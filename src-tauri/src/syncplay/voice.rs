//! Room voice relay. Opus frames on a private channel (see `docs/VOICE_PROTOCOL.md`).
//!
//! The hub is a table plus the sockets. It does not read or write the disk, and it
//! does not log packet contents, tokens, or keys. Plaintext exists only inside
//! [`VoiceHub::ingest`], long enough to seal a copy for each other seat.

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use parking_lot::Mutex;
use rand::Rng;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};
use tokio::task::JoinHandle;

use super::room::ConnId;

pub const VERSION: u64 = 1;
pub const PORT: u16 = 9000;
pub const MAX_SEATS: usize = 8;
pub const MAX_PACKET: usize = 1200;
pub const HEADER_LEN: usize = 40;
pub const TAG_LEN: usize = 16;
pub const MIN_PACKET: usize = HEADER_LEN + TAG_LEN;
pub const WS_PATH: &str = "/yarmiplay/voice";
/// `voiceActivity` messages accepted per seat per second.
pub const ACTIVITY_PER_SEC: usize = 5;
const SEQ_WINDOW: u32 = 64;
const AUDIO: u8 = 1;
const KEEPALIVE: u8 = 2;
const BYE: u8 = 3;

#[derive(Clone)]
struct Down {
    tx: UnboundedSender<Vec<u8>>,
    gen: u64,
}

enum Route {
    None,
    Udp(SocketAddr),
    Stream(Down),
}

struct Seat {
    conn: ConnId,
    room: String,
    name: String,
    token: u64,
    key: [u8; 32],
    speaking: bool,
    seq_init: bool,
    seq_base: u32,
    seq_bits: u64,
    activity_ms: VecDeque<u64>,
    route: Route,
}

struct Inner {
    listening: bool,
    seats: HashMap<ConnId, Seat>,
    by_token: HashMap<u64, ConnId>,
    next_gen: u64,
}

pub struct VoiceHub {
    udp_port: u16,
    tcp_port: u16,
    fingerprint: String,
    cert_der: Vec<u8>,
    inner: Mutex<Inner>,
}

#[derive(Debug)]
pub struct SeatSecrets {
    pub participant_id: String,
    pub username: String,
    pub room: String,
    pub token_hex: String,
    pub key_b64: String,
    pub replaced: bool,
    pub udp_port: u16,
    pub tcp_port: u16,
    pub fingerprint: String,
}

#[derive(Debug)]
pub enum Join {
    Joined(SeatSecrets),
    Full,
    Version,
}

pub enum Activity {
    /// No seat, or the flag did not change.
    Quiet,
    /// Over the per-second cap. Not forwarded.
    Dropped,
    Update {
        room: String,
        participant_id: String,
        username: String,
        speaking: bool,
    },
}

pub struct VoiceRuntime {
    pub hub: Arc<VoiceHub>,
    udp: JoinHandle<()>,
    tcp: JoinHandle<()>,
}

impl VoiceHub {
    fn new(
        udp_port: u16,
        tcp_port: u16,
        fingerprint: String,
        cert_der: Vec<u8>,
        listening: bool,
    ) -> Arc<Self> {
        Arc::new(Self {
            udp_port,
            tcp_port,
            fingerprint,
            cert_der,
            inner: Mutex::new(Inner {
                listening,
                seats: HashMap::new(),
                by_token: HashMap::new(),
                next_gen: 1,
            }),
        })
    }

    /// In-memory table for tests. Reports the real default ports and claims to be listening.
    pub fn open_for_test() -> Arc<Self> {
        Self::new(PORT, PORT, "ab".repeat(32), Vec::new(), true)
    }

    pub fn listening(&self) -> bool {
        self.inner.lock().listening
    }

    pub fn udp_port(&self) -> u16 {
        self.udp_port
    }

    pub fn tcp_port(&self) -> u16 {
        self.tcp_port
    }

    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    pub fn cert_der(&self) -> &[u8] {
        &self.cert_der
    }

    pub fn descriptor(&self) -> Value {
        json!({
            "version": VERSION,
            "codec": "opus",
            "clockRate": 48000,
            "channels": 1,
            "ptime": 20,
            "maxBitrate": 32000,
            "fec": true,
            "dtx": true,
            "transport": "yv1",
            "udpPort": self.udp_port,
            "tcpPort": self.tcp_port,
            "wsPath": WS_PATH,
            "maxParticipants": MAX_SEATS,
            "fingerprint": self.fingerprint,
        })
    }

    pub fn join(&self, conn: ConnId, room: &str, name: &str, version: u64) -> Join {
        if version != VERSION {
            return Join::Version;
        }
        let mut g = self.inner.lock();
        let replaced = g.seats.contains_key(&conn);
        if !replaced {
            let n = g.seats.values().filter(|s| s.room == room).count();
            if n >= MAX_SEATS {
                return Join::Full;
            }
        }
        if let Some(old) = g.seats.remove(&conn) {
            g.by_token.remove(&old.token);
        }
        let mut rng = rand::thread_rng();
        let mut token = rng.gen::<u64>();
        while token == 0 || g.by_token.contains_key(&token) {
            token = rng.gen::<u64>();
        }
        let key: [u8; 32] = rng.gen();
        g.by_token.insert(token, conn);
        g.seats.insert(
            conn,
            Seat {
                conn,
                room: room.to_string(),
                name: name.to_string(),
                token,
                key,
                speaking: false,
                seq_init: false,
                seq_base: 0,
                seq_bits: 0,
                activity_ms: VecDeque::new(),
                route: Route::None,
            },
        );
        Join::Joined(SeatSecrets {
            participant_id: participant_id(conn),
            username: name.to_string(),
            room: room.to_string(),
            token_hex: format!("{token:016x}"),
            key_b64: B64.encode(key),
            replaced,
            udp_port: self.udp_port,
            tcp_port: self.tcp_port,
            fingerprint: self.fingerprint.clone(),
        })
    }

    /// Drop the seat. False if it was not in voice.
    pub fn leave(&self, conn: ConnId) -> bool {
        let mut g = self.inner.lock();
        let Some(seat) = g.seats.remove(&conn) else {
            return false;
        };
        g.by_token.remove(&seat.token);
        true
    }

    pub fn close_all(&self) -> Vec<ConnId> {
        let mut g = self.inner.lock();
        let mut ids: Vec<ConnId> = g.seats.keys().copied().collect();
        ids.sort_unstable();
        g.seats.clear();
        g.by_token.clear();
        ids
    }

    pub fn room_of(&self, conn: ConnId) -> Option<String> {
        self.inner.lock().seats.get(&conn).map(|s| s.room.clone())
    }

    pub fn conns_in(&self, room: &str) -> Vec<ConnId> {
        let g = self.inner.lock();
        let mut ids: Vec<ConnId> = g
            .seats
            .values()
            .filter(|s| s.room == room)
            .map(|s| s.conn)
            .collect();
        ids.sort_unstable();
        ids
    }

    pub fn peers_message(&self, room: &str, self_id: ConnId) -> Value {
        let g = self.inner.lock();
        let mut peers: Vec<Value> = g
            .seats
            .values()
            .filter(|s| s.room == room)
            .map(|s| {
                json!({
                    "participantId": participant_id(s.conn),
                    "username": s.name,
                    "speaking": s.speaking,
                })
            })
            .collect();
        peers.sort_by(|a, b| {
            a["participantId"]
                .as_str()
                .unwrap_or("")
                .cmp(b["participantId"].as_str().unwrap_or(""))
        });
        json!({
            "room": room,
            "self": participant_id(self_id),
            "peers": peers,
        })
    }

    /// `now_ms` is a monotonic millisecond clock the caller owns (wall time is fine).
    pub fn activity(&self, conn: ConnId, speaking: bool, now_ms: u64) -> Activity {
        let mut g = self.inner.lock();
        let Some(seat) = g.seats.get_mut(&conn) else {
            return Activity::Quiet;
        };
        while seat
            .activity_ms
            .front()
            .is_some_and(|t| now_ms.saturating_sub(*t) >= 1000)
        {
            seat.activity_ms.pop_front();
        }
        if seat.activity_ms.len() >= ACTIVITY_PER_SEC {
            return Activity::Dropped;
        }
        seat.activity_ms.push_back(now_ms);
        if seat.speaking == speaking {
            return Activity::Quiet;
        }
        seat.speaking = speaking;
        Activity::Update {
            room: seat.room.clone(),
            participant_id: participant_id(conn),
            username: seat.name.clone(),
            speaking,
        }
    }

    pub fn has_token(&self, token: u64) -> bool {
        self.inner.lock().by_token.contains_key(&token)
    }

    /// Attach the TCP or WebSocket path. The returned generation is passed back
    /// to [`detach_sender`] so an older socket cannot detach a newer one.
    pub fn attach_sender(&self, token: u64, tx: UnboundedSender<Vec<u8>>) -> Option<u64> {
        let mut g = self.inner.lock();
        let conn = *g.by_token.get(&token)?;
        let gen = g.next_gen;
        g.next_gen = g.next_gen.wrapping_add(1);
        if let Some(seat) = g.seats.get_mut(&conn) {
            seat.route = Route::Stream(Down { tx, gen });
            Some(gen)
        } else {
            None
        }
    }

    pub fn detach_sender(&self, token: u64, gen: u64) {
        let mut g = self.inner.lock();
        let Some(conn) = g.by_token.get(&token).copied() else {
            return;
        };
        if let Some(seat) = g.seats.get_mut(&conn) {
            if let Route::Stream(down) = &seat.route {
                if down.gen == gen {
                    seat.route = Route::None;
                }
            }
        }
    }

    /// Decrypt one packet and fan the audio out. UDP destinations are returned.
    /// TCP and WebSocket copies are pushed onto the seat's sender.
    /// Nothing in here is logged.
    ///
    /// `from` is installed as the UDP return path only after the tag checks out.
    /// `stream` is a TCP or WebSocket sender and the generation already registered
    /// for that socket (`None` until the first accepted packet). A failed tag does
    /// not move the path: the token sits in the clear, and a copied header must
    /// not be enough to steal where audio is sent. The returned generation is the
    /// one `detach_sender` needs for this socket.
    pub fn ingest(
        &self,
        packet: &[u8],
        from: Option<SocketAddr>,
        stream: Option<(&UnboundedSender<Vec<u8>>, Option<u64>)>,
    ) -> (Vec<(SocketAddr, Vec<u8>)>, Option<u64>) {
        let parsed = match packet_token(packet) {
            Some(t) => t,
            None => return (Vec::new(), None),
        };
        let mut g = self.inner.lock();
        let Some(conn) = g.by_token.get(&parsed).copied() else {
            return (Vec::new(), None);
        };
        let key = match g.seats.get(&conn) {
            Some(s) if s.token == parsed => s.key,
            _ => return (Vec::new(), None),
        };
        let Ok(opened) = open(&key, packet) else {
            return (Vec::new(), None);
        };
        if opened.sender != 0 || !plain_ok(opened.ty, &opened.plain) {
            return (Vec::new(), None);
        }
        let (keep_gen, room, seq, ts, plain, ty) = {
            let Some(seat) = g.seats.get_mut(&conn) else {
                return (Vec::new(), None);
            };
            if !accept_seq(seat, opened.seq) {
                return (Vec::new(), None);
            }
            let keep_gen = stream.as_ref().and_then(|(_, existing)| {
                (*existing)
                    .filter(|gen| matches!(&seat.route, Route::Stream(down) if down.gen == *gen))
            });
            if stream.is_none() {
                if let Some(addr) = from {
                    seat.route = Route::Udp(addr);
                }
            }
            (
                keep_gen,
                seat.room.clone(),
                opened.seq,
                opened.ts,
                opened.plain,
                opened.ty,
            )
        };
        let stream_gen = if let Some((tx, _)) = stream {
            if let Some(gen) = keep_gen {
                Some(gen)
            } else {
                let gen = g.next_gen;
                g.next_gen = g.next_gen.wrapping_add(1);
                if let Some(seat) = g.seats.get_mut(&conn) {
                    seat.route = Route::Stream(Down {
                        tx: tx.clone(),
                        gen,
                    });
                }
                Some(gen)
            }
        } else {
            None
        };
        if ty != AUDIO {
            return (Vec::new(), stream_gen);
        }
        let targets: Vec<(u64, [u8; 32], RouteCopy)> = g
            .seats
            .values()
            .filter(|s| s.conn != conn && s.room == room)
            .map(|s| {
                let route = match &s.route {
                    Route::Udp(addr) => RouteCopy::Udp(*addr),
                    Route::Stream(d) => RouteCopy::Stream(d.tx.clone()),
                    Route::None => RouteCopy::None,
                };
                (s.token, s.key, route)
            })
            .collect();
        drop(g);
        let mut udp = Vec::new();
        for (token, key, route) in targets {
            let Some(pkt) = seal(&key, AUDIO, token, conn, seq, ts, &plain) else {
                continue;
            };
            match route {
                RouteCopy::Udp(addr) => udp.push((addr, pkt)),
                RouteCopy::Stream(tx) => {
                    let _ = tx.send(pkt);
                }
                RouteCopy::None => {}
            }
        }
        (udp, stream_gen)
    }
}

enum RouteCopy {
    None,
    Udp(SocketAddr),
    Stream(UnboundedSender<Vec<u8>>),
}

struct Opened {
    ty: u8,
    sender: u64,
    seq: u32,
    ts: u32,
    plain: Vec<u8>,
}

fn participant_id(conn: ConnId) -> String {
    format!("c{conn}")
}

pub fn parse_token(text: &str) -> Option<u64> {
    let lower_hex = |b: u8| b.is_ascii_digit() || (b'a'..=b'f').contains(&b);
    if text.len() != 16 || !text.bytes().all(lower_hex) {
        return None;
    }
    u64::from_str_radix(text, 16).ok().filter(|t| *t != 0)
}

fn packet_token(packet: &[u8]) -> Option<u64> {
    if packet.len() < MIN_PACKET || packet.len() > MAX_PACKET {
        return None;
    }
    if packet[0] != b'Y' || packet[1] != b'V' || packet[2] != 1 {
        return None;
    }
    let mut token = [0u8; 8];
    token.copy_from_slice(&packet[4..12]);
    let token = u64::from_be_bytes(token);
    (token != 0).then_some(token)
}

fn plain_ok(ty: u8, plain: &[u8]) -> bool {
    match ty {
        AUDIO => !plain.is_empty(),
        KEEPALIVE => plain == [0x00],
        BYE => plain.len() <= 32,
        _ => false,
    }
}

fn accept_seq(seat: &mut Seat, seq: u32) -> bool {
    if !seat.seq_init {
        seat.seq_init = true;
        seat.seq_base = seq;
        seat.seq_bits = 1;
        return true;
    }
    let ahead = seq.wrapping_sub(seat.seq_base);
    if ahead == 0 {
        return false;
    }
    if ahead < 0x8000_0000 {
        if ahead >= SEQ_WINDOW {
            seat.seq_bits = 1;
        } else {
            seat.seq_bits = (seat.seq_bits << ahead) | 1;
        }
        seat.seq_base = seq;
        return true;
    }
    let age = seat.seq_base.wrapping_sub(seq);
    if age == 0 || age >= SEQ_WINDOW {
        return false;
    }
    let bit = 1u64 << age;
    if seat.seq_bits & bit != 0 {
        return false;
    }
    seat.seq_bits |= bit;
    true
}

fn header(
    ty: u8,
    token: u64,
    sender: u64,
    seq: u32,
    ts: u32,
    nonce: &[u8; 12],
) -> [u8; HEADER_LEN] {
    let mut h = [0u8; HEADER_LEN];
    h[0] = b'Y';
    h[1] = b'V';
    h[2] = 1;
    h[3] = ty;
    h[4..12].copy_from_slice(&token.to_be_bytes());
    h[12..20].copy_from_slice(&sender.to_be_bytes());
    h[20..24].copy_from_slice(&seq.to_be_bytes());
    h[24..28].copy_from_slice(&ts.to_be_bytes());
    h[28..40].copy_from_slice(nonce);
    h
}

fn seal(
    key: &[u8; 32],
    ty: u8,
    token: u64,
    sender: u64,
    seq: u32,
    ts: u32,
    plain: &[u8],
) -> Option<Vec<u8>> {
    let nonce_bytes: [u8; 12] = rand::thread_rng().gen();
    let h = header(ty, token, sender, seq, ts, &nonce_bytes);
    let cipher = Aes256Gcm::new_from_slice(key).ok()?;
    let ct = cipher
        .encrypt(
            Nonce::from_slice(&nonce_bytes),
            Payload {
                msg: plain,
                aad: &h,
            },
        )
        .ok()?;
    let mut out = Vec::with_capacity(HEADER_LEN + ct.len());
    out.extend_from_slice(&h);
    out.extend_from_slice(&ct);
    Some(out)
}

fn open(key: &[u8; 32], packet: &[u8]) -> Result<Opened, ()> {
    if packet.len() < MIN_PACKET || packet.len() > MAX_PACKET {
        return Err(());
    }
    let (h, ct) = packet.split_at(HEADER_LEN);
    if h[0] != b'Y' || h[1] != b'V' || h[2] != 1 {
        return Err(());
    }
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| ())?;
    let plain = cipher
        .decrypt(Nonce::from_slice(&h[28..40]), Payload { msg: ct, aad: h })
        .map_err(|_| ())?;
    let mut sender = [0u8; 8];
    sender.copy_from_slice(&h[12..20]);
    let mut seq = [0u8; 4];
    seq.copy_from_slice(&h[20..24]);
    let mut ts = [0u8; 4];
    ts.copy_from_slice(&h[24..28]);
    Ok(Opened {
        ty: h[3],
        sender: u64::from_be_bytes(sender),
        seq: u32::from_be_bytes(seq),
        ts: u32::from_be_bytes(ts),
        plain,
    })
}

impl SeatSecrets {
    pub fn joined_message(&self) -> Value {
        json!({
            "version": VERSION,
            "participantId": self.participant_id,
            "username": self.username,
            "room": self.room,
            "token": self.token_hex,
            "key": self.key_b64,
            "udpPort": self.udp_port,
            "tcpPort": self.tcp_port,
            "wsPath": WS_PATH,
            "fingerprint": self.fingerprint,
            "codec": "opus",
            "clockRate": 48000,
            "channels": 1,
            "ptime": 20,
        })
    }
}

fn mint_cert() -> Result<(String, String, String, Vec<u8>), String> {
    let params =
        rcgen::CertificateParams::new(vec!["voice.local".into()]).map_err(|e| e.to_string())?;
    let key = rcgen::KeyPair::generate().map_err(|e| e.to_string())?;
    let cert = params.self_signed(&key).map_err(|e| e.to_string())?;
    let der = cert.der().to_vec();
    let fingerprint = hex::encode(Sha256::digest(&der));
    Ok((cert.pem(), key.serialize_pem(), fingerprint, der))
}

fn bind_udp(port: u16) -> std::io::Result<tokio::net::UdpSocket> {
    use socket2::{Domain, Protocol, Socket, Type};
    let v6 = || -> std::io::Result<std::net::UdpSocket> {
        let s = Socket::new(Domain::IPV6, Type::DGRAM, Some(Protocol::UDP))?;
        s.set_only_v6(false)?;
        s.bind(&SocketAddr::from((std::net::Ipv6Addr::UNSPECIFIED, port)).into())?;
        s.set_nonblocking(true)?;
        Ok(s.into())
    };
    let std = match v6() {
        Ok(s) => s,
        Err(_) => {
            let s = std::net::UdpSocket::bind(SocketAddr::from(([0, 0, 0, 0], port)))?;
            s.set_nonblocking(true)?;
            s
        }
    };
    tokio::net::UdpSocket::from_std(std)
}

impl VoiceRuntime {
    /// Bind UDP and TCP. A bind error leaves nothing listening; the caller keeps Syncplay up.
    pub async fn bind(udp_port: u16, tcp_port: u16) -> std::io::Result<Self> {
        crate::net::install_crypto_provider();
        let udp = bind_udp(udp_port)?;
        let tcp = super::server::bind_dual_stack(tcp_port)?;
        let udp_port = udp.local_addr()?.port();
        let tcp_port = tcp.local_addr()?.port();
        let (pem, key_pem, fingerprint, der) = mint_cert().map_err(std::io::Error::other)?;
        let config = crate::tls::server_config(&pem, &key_pem).map_err(std::io::Error::other)?;
        let hub = VoiceHub::new(udp_port, tcp_port, fingerprint, der, true);
        let udp_hub = hub.clone();
        let tcp_hub = hub.clone();
        let udp = tokio::spawn(async move { udp_loop(udp, udp_hub).await });
        let tcp = tokio::spawn(async move { tcp_loop(tcp, config, tcp_hub).await });
        Ok(Self { hub, udp, tcp })
    }

    pub fn shutdown(self) {
        self.udp.abort();
        self.tcp.abort();
    }
}

async fn udp_loop(sock: tokio::net::UdpSocket, hub: Arc<VoiceHub>) {
    let mut buf = vec![0u8; 2048];
    loop {
        let Ok((n, addr)) = sock.recv_from(&mut buf).await else {
            break;
        };
        if n > MAX_PACKET {
            continue;
        }
        for (dest, pkt) in hub.ingest(&buf[..n], Some(addr), None).0 {
            let _ = sock.send_to(&pkt, dest).await;
        }
    }
}

async fn tcp_loop(
    listener: tokio::net::TcpListener,
    config: Arc<rustls::ServerConfig>,
    hub: Arc<VoiceHub>,
) {
    let acceptor = tokio_rustls::TlsAcceptor::from(config);
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            break;
        };
        let _ = stream.set_nodelay(true);
        let acceptor = acceptor.clone();
        let hub = hub.clone();
        tokio::spawn(async move {
            if let Ok(stream) = acceptor.accept(stream).await {
                serve_tls(stream, hub).await;
            }
        });
    }
}

async fn serve_tls<S>(stream: S, hub: Arc<VoiceHub>)
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (mut rd, mut wr) = tokio::io::split(stream);
    let (tx, mut rx) = unbounded_channel::<Vec<u8>>();
    let writer = tokio::spawn(async move {
        while let Some(pkt) = rx.recv().await {
            let len = pkt.len() as u16;
            if wr.write_all(&len.to_be_bytes()).await.is_err()
                || wr.write_all(&pkt).await.is_err()
                || wr.flush().await.is_err()
            {
                break;
            }
        }
    });
    let mut registered: Option<(u64, u64)> = None;
    loop {
        let frame = match read_frame(&mut rd).await {
            Ok(f) => f,
            Err(_) => break,
        };
        let Some(token) = packet_token(&frame) else {
            continue;
        };
        if registered.is_some_and(|(have, _)| have != token) {
            continue;
        }
        let existing = registered.map(|(_, gen)| gen);
        let (_, gen) = hub.ingest(&frame, None, Some((&tx, existing)));
        if let Some(gen) = gen {
            registered = Some((token, gen));
        }
    }
    if let Some((token, gen)) = registered {
        hub.detach_sender(token, gen);
    }
    drop(tx);
    let _ = writer.await;
}

async fn read_frame<R: AsyncRead + Unpin>(rd: &mut R) -> std::io::Result<Vec<u8>> {
    let mut len_buf = [0u8; 2];
    rd.read_exact(&mut len_buf).await?;
    let len = u16::from_be_bytes(len_buf) as usize;
    if !(MIN_PACKET..=MAX_PACKET).contains(&len) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "voice frame",
        ));
    }
    let mut buf = vec![0u8; len];
    rd.read_exact(&mut buf).await?;
    Ok(buf)
}

/// Browser media socket. Each binary message is one packet.
pub async fn serve_ws(socket: axum::extract::ws::WebSocket, hub: Arc<VoiceHub>, token: u64) {
    use axum::extract::ws::Message;
    use futures_util::{SinkExt, StreamExt};
    let (tx, mut rx) = unbounded_channel::<Vec<u8>>();
    let (mut sink, mut stream) = socket.split();
    let writer = tokio::spawn(async move {
        while let Some(pkt) = rx.recv().await {
            if sink.send(Message::Binary(pkt.into())).await.is_err() {
                break;
            }
        }
    });
    let mut generation = None;
    while let Some(item) = stream.next().await {
        match item {
            Ok(Message::Binary(b)) => {
                if packet_token(&b) != Some(token) {
                    continue;
                }
                let (_, gen) = hub.ingest(&b, None, Some((&tx, generation)));
                if gen.is_some() {
                    generation = gen;
                }
            }
            Ok(Message::Close(_)) | Err(_) => break,
            _ => {}
        }
    }
    if let Some(gen) = generation {
        hub.detach_sender(token, gen);
    }
    writer.abort();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn joined(hub: &VoiceHub, conn: ConnId, room: &str, name: &str) -> SeatSecrets {
        match hub.join(conn, room, name, VERSION) {
            Join::Joined(s) => s,
            other => panic!("expected a seat, got {other:?}"),
        }
    }

    fn key_of(s: &SeatSecrets) -> [u8; 32] {
        B64.decode(&s.key_b64).unwrap().try_into().unwrap()
    }

    fn token_of(s: &SeatSecrets) -> u64 {
        parse_token(&s.token_hex).unwrap()
    }

    fn packet(s: &SeatSecrets, ty: u8, seq: u32, plain: &[u8]) -> Vec<u8> {
        seal(
            &key_of(s),
            ty,
            token_of(s),
            0,
            seq,
            seq.wrapping_mul(960),
            plain,
        )
        .unwrap()
    }

    #[test]
    fn two_seats_in_a_room_hear_each_other_and_another_room_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let hub = VoiceHub::open_for_test();
        let a = joined(&hub, 1, "movie", "alice");
        let b = joined(&hub, 2, "movie", "bob");
        let c = joined(&hub, 3, "other", "carol");
        let addr_a: SocketAddr = "127.0.0.1:11".parse().unwrap();
        let addr_b: SocketAddr = "127.0.0.1:12".parse().unwrap();
        let addr_c: SocketAddr = "127.0.0.1:13".parse().unwrap();
        assert!(hub
            .ingest(&packet(&a, KEEPALIVE, 1, &[0x00]), Some(addr_a), None)
            .0
            .is_empty());
        assert!(hub
            .ingest(&packet(&b, KEEPALIVE, 1, &[0x00]), Some(addr_b), None)
            .0
            .is_empty());
        assert!(hub
            .ingest(&packet(&c, KEEPALIVE, 1, &[0x00]), Some(addr_c), None)
            .0
            .is_empty());

        let outs = hub
            .ingest(&packet(&a, AUDIO, 2, b"opus-frame"), Some(addr_a), None)
            .0;
        assert_eq!(outs.len(), 1, "only bob is in alice's room and has a path");
        assert_eq!(outs[0].0, addr_b);
        let heard = open(&key_of(&b), &outs[0].1).unwrap();
        assert_eq!(heard.plain, b"opus-frame");
        assert_eq!(heard.sender, 1);
        assert_eq!(heard.ty, AUDIO);

        assert!(
            hub.ingest(&packet(&a, AUDIO, 2, b"opus-frame"), Some(addr_a), None)
                .0
                .is_empty(),
            "replay"
        );
        assert!(
            hub.ingest(&packet(&a, AUDIO, 3, b""), Some(addr_a), None)
                .0
                .is_empty(),
            "empty audio"
        );
        assert!(std::fs::read_dir(dir.path()).unwrap().next().is_none());
    }

    #[test]
    fn a_stream_seat_receives_without_udp_and_the_ninth_seat_is_refused() {
        let hub = VoiceHub::open_for_test();
        for i in 1..=8 {
            assert!(matches!(hub.join(i, "full", "u", VERSION), Join::Joined(_)));
        }
        assert!(matches!(hub.join(9, "full", "late", VERSION), Join::Full));
        assert!(matches!(hub.join(1, "full", "u", 2), Join::Version));
        hub.close_all();

        let a = joined(&hub, 1, "movie", "alice");
        let b = joined(&hub, 2, "movie", "bob");
        let (tx, mut rx) = unbounded_channel();
        assert!(hub.attach_sender(token_of(&b), tx).is_some());
        let addr_a: SocketAddr = "127.0.0.1:21".parse().unwrap();
        hub.ingest(&packet(&a, KEEPALIVE, 1, &[0x00]), Some(addr_a), None);
        hub.ingest(&packet(&a, AUDIO, 2, b"frame"), Some(addr_a), None);
        let got = rx.try_recv().unwrap();
        assert_eq!(open(&key_of(&b), &got).unwrap().plain, b"frame");
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn activity_over_five_a_second_is_dropped() {
        let hub = VoiceHub::open_for_test();
        let _a = joined(&hub, 1, "movie", "alice");
        assert!(matches!(
            hub.activity(1, true, 0),
            Activity::Update { speaking: true, .. }
        ));
        for i in 1..5 {
            assert!(matches!(hub.activity(1, true, i), Activity::Quiet));
        }
        assert!(matches!(hub.activity(1, false, 6), Activity::Dropped));
        assert!(matches!(
            hub.activity(1, false, 1000),
            Activity::Update {
                speaking: false,
                ..
            }
        ));
        assert!(matches!(hub.activity(99, true, 1000), Activity::Quiet));
    }

    #[test]
    fn leaving_stops_forwarding() {
        let hub = VoiceHub::open_for_test();
        let a = joined(&hub, 1, "movie", "alice");
        let b = joined(&hub, 2, "movie", "bob");
        let addr_a: SocketAddr = "127.0.0.1:31".parse().unwrap();
        let addr_b: SocketAddr = "127.0.0.1:32".parse().unwrap();
        hub.ingest(&packet(&a, KEEPALIVE, 1, &[0x00]), Some(addr_a), None);
        hub.ingest(&packet(&b, KEEPALIVE, 1, &[0x00]), Some(addr_b), None);
        assert!(hub.leave(2));
        assert!(hub
            .ingest(&packet(&a, AUDIO, 2, b"x"), Some(addr_a), None)
            .0
            .is_empty());
        assert!(hub.room_of(2).is_none());
        assert_eq!(hub.conns_in("movie"), vec![1]);
    }

    #[test]
    fn a_copied_header_does_not_steal_the_return_path() {
        let hub = VoiceHub::open_for_test();
        let a = joined(&hub, 1, "movie", "alice");
        let b = joined(&hub, 2, "movie", "bob");
        let (real, mut rx) = unbounded_channel();
        let gen = hub.attach_sender(token_of(&b), real).unwrap();
        let mut forged = packet(&a, KEEPALIVE, 1, &[0x00]);
        forged[4..12].copy_from_slice(&token_of(&b).to_be_bytes());
        let (thief, mut stolen) = unbounded_channel();
        let (outs, took) = hub.ingest(&forged, None, Some((&thief, None)));
        assert!(outs.is_empty());
        assert!(took.is_none());
        let addr_a: SocketAddr = "127.0.0.1:41".parse().unwrap();
        hub.ingest(&packet(&a, KEEPALIVE, 1, &[0x00]), Some(addr_a), None);
        hub.ingest(&packet(&a, AUDIO, 2, b"still-bob"), Some(addr_a), None);
        assert_eq!(
            open(&key_of(&b), &rx.try_recv().unwrap()).unwrap().plain,
            b"still-bob"
        );
        assert!(stolen.try_recv().is_err());
        hub.detach_sender(token_of(&b), gen);
        hub.ingest(&packet(&a, AUDIO, 3, b"gone"), Some(addr_a), None);
        assert!(rx.try_recv().is_err());
        assert!(parse_token(&b.token_hex.to_ascii_uppercase()).is_none());
    }

    #[tokio::test]
    async fn udp_and_tcp_loopback_deliver_a_frame() {
        crate::net::install_crypto_provider();
        let rt = VoiceRuntime::bind(0, 0).await.unwrap();
        let a = joined(&rt.hub, 1, "movie", "alice");
        let b = joined(&rt.hub, 2, "movie", "bob");

        let bob = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let alice = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let server = SocketAddr::from(([127, 0, 0, 1], rt.hub.udp_port()));
        bob.send_to(&packet(&b, KEEPALIVE, 1, &[0x00]), server)
            .await
            .unwrap();
        alice
            .send_to(&packet(&a, KEEPALIVE, 1, &[0x00]), server)
            .await
            .unwrap();
        alice
            .send_to(&packet(&a, AUDIO, 2, b"udp-opus"), server)
            .await
            .unwrap();
        let mut buf = [0u8; 1500];
        let n = tokio::time::timeout(Duration::from_secs(2), bob.recv(&mut buf))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(open(&key_of(&b), &buf[..n]).unwrap().plain, b"udp-opus");

        let tcp_server = SocketAddr::from(([127, 0, 0, 1], rt.hub.tcp_port()));
        let mut roots = rustls::RootCertStore::empty();
        roots.add(rt.hub.cert_der().to_vec().into()).unwrap();
        let client_cfg = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let connector = tokio_rustls::TlsConnector::from(Arc::new(client_cfg));
        let stream = tokio::net::TcpStream::connect(tcp_server).await.unwrap();
        let mut tls = connector
            .connect("voice.local".try_into().unwrap(), stream)
            .await
            .unwrap();
        let der = rt.hub.cert_der();
        let got = hex::encode(Sha256::digest(der));
        assert_eq!(got, rt.hub.fingerprint());
        let frame = packet(&b, KEEPALIVE, 7, &[0x00]);
        tls.write_all(&(frame.len() as u16).to_be_bytes())
            .await
            .unwrap();
        tls.write_all(&frame).await.unwrap();
        tls.flush().await.unwrap();
        // Alice's next UDP audio is forwarded on TCP now that bob attached there.
        tokio::time::sleep(Duration::from_millis(150)).await;
        alice
            .send_to(&packet(&a, AUDIO, 3, b"tcp-opus"), server)
            .await
            .unwrap();
        let mut len_buf = [0u8; 2];
        tokio::time::timeout(Duration::from_secs(2), tls.read_exact(&mut len_buf))
            .await
            .unwrap()
            .unwrap();
        let len = u16::from_be_bytes(len_buf) as usize;
        let mut body = vec![0u8; len];
        tls.read_exact(&mut body).await.unwrap();
        assert_eq!(open(&key_of(&b), &body).unwrap().plain, b"tcp-opus");
        rt.shutdown();
    }

    #[tokio::test]
    async fn browser_websocket_forwards_one_frame() {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;
        use tokio_tungstenite::tungstenite::Message as WsMessage;
        crate::net::install_crypto_provider();
        let server = crate::syncplay::server::SyncplayServer::start(
            0,
            super::super::room::SyncplayOptions::default(),
            Arc::new(parking_lot::RwLock::new(None)),
            Arc::new(|| {}),
            crate::syncplay::server::Extensions::default(),
        )
        .await
        .unwrap();
        let rt = VoiceRuntime::bind(0, 0).await.unwrap();
        server.install_voice(rt);

        async fn session(
            port: u16,
            name: &str,
        ) -> tokio_tungstenite::WebSocketStream<tokio::net::TcpStream> {
            let tcp = tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .unwrap();
            let mut req = format!("ws://127.0.0.1:{port}/yarmiplay/ws")
                .into_client_request()
                .unwrap();
            req.headers_mut().insert(
                "origin",
                axum::http::HeaderValue::from_static("http://localhost:5173"),
            );
            let (mut ws, _) = tokio_tungstenite::client_async(req, tcp).await.unwrap();
            let hello = serde_json::json!({
                "Hello": {
                    "username": name,
                    "room": { "name": "lobby" },
                    "version": "1.2.255",
                    "realversion": "1.7.4",
                    "features": { "yarmiplay": { "protocol": 1 } }
                }
            });
            ws.send(WsMessage::Text(hello.to_string().into()))
                .await
                .unwrap();
            ws
        }
        async fn yarm(
            ws: &mut tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
            key: &str,
        ) -> Value {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            loop {
                let left = deadline.saturating_duration_since(std::time::Instant::now());
                let msg = tokio::time::timeout(left, ws.next())
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap();
                if let WsMessage::Text(t) = msg {
                    let v: Value = serde_json::from_str(&t).unwrap();
                    if v.get("Yarmiplay").and_then(|y| y.get(key)).is_some() {
                        return v;
                    }
                }
            }
        }

        let mut alice = session(server.port, "alice").await;
        let mut bob = session(server.port, "bob").await;
        let caps = yarm(&mut alice, "session").await;
        assert_eq!(caps["Yarmiplay"]["capabilities"]["voice"], true);
        assert!(caps["Yarmiplay"]["capabilities"]
            .get("voiceSignal")
            .is_none());
        yarm(&mut bob, "session").await;
        alice
            .send(WsMessage::Text(
                serde_json::json!({"Yarmiplay":{"voiceJoin":{"version":1}}})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
        bob.send(WsMessage::Text(
            serde_json::json!({"Yarmiplay":{"voiceJoin":{"version":1}}})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
        let a_joined = yarm(&mut alice, "voiceJoined").await["Yarmiplay"]["voiceJoined"].clone();
        let b_joined = yarm(&mut bob, "voiceJoined").await["Yarmiplay"]["voiceJoined"].clone();
        let a = SeatSecrets {
            participant_id: a_joined["participantId"].as_str().unwrap().into(),
            username: "alice".into(),
            room: "lobby".into(),
            token_hex: a_joined["token"].as_str().unwrap().into(),
            key_b64: a_joined["key"].as_str().unwrap().into(),
            replaced: false,
            udp_port: a_joined["udpPort"].as_u64().unwrap() as u16,
            tcp_port: a_joined["tcpPort"].as_u64().unwrap() as u16,
            fingerprint: a_joined["fingerprint"].as_str().unwrap().into(),
        };
        let b = SeatSecrets {
            participant_id: b_joined["participantId"].as_str().unwrap().into(),
            username: "bob".into(),
            room: "lobby".into(),
            token_hex: b_joined["token"].as_str().unwrap().into(),
            key_b64: b_joined["key"].as_str().unwrap().into(),
            replaced: false,
            udp_port: b_joined["udpPort"].as_u64().unwrap() as u16,
            tcp_port: b_joined["tcpPort"].as_u64().unwrap() as u16,
            fingerprint: b_joined["fingerprint"].as_str().unwrap().into(),
        };
        let b_key = key_of(&b);

        let bad = format!("ws://127.0.0.1:{}/yarmiplay/voice", server.port)
            .into_client_request()
            .unwrap();
        let tcp = tokio::net::TcpStream::connect(("127.0.0.1", server.port))
            .await
            .unwrap();
        let err = tokio_tungstenite::client_async(bad, tcp).await;
        assert!(err.is_err(), "a voice socket without a token is rejected");

        async fn voice_sock(
            port: u16,
            token: &str,
        ) -> tokio_tungstenite::WebSocketStream<tokio::net::TcpStream> {
            let tcp = tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .unwrap();
            let mut req = format!("ws://127.0.0.1:{port}/yarmiplay/voice?t={token}")
                .into_client_request()
                .unwrap();
            req.headers_mut().insert(
                "origin",
                axum::http::HeaderValue::from_static("http://localhost:5173"),
            );
            let (ws, resp) = tokio_tungstenite::client_async(req, tcp).await.unwrap();
            assert_eq!(resp.status(), axum::http::StatusCode::SWITCHING_PROTOCOLS);
            ws
        }
        let mut a_media = voice_sock(server.port, &a.token_hex).await;
        let mut b_media = voice_sock(server.port, &b.token_hex).await;
        // The return path is installed only after a packet authenticates.
        b_media
            .send(WsMessage::Binary(packet(&b, KEEPALIVE, 1, &[0x00]).into()))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        a_media
            .send(WsMessage::Binary(packet(&a, KEEPALIVE, 1, &[0x00]).into()))
            .await
            .unwrap();
        a_media
            .send(WsMessage::Binary(packet(&a, AUDIO, 2, b"ws-opus").into()))
            .await
            .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        loop {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            let msg = tokio::time::timeout(left, b_media.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            if let WsMessage::Binary(buf) = msg {
                if open(&b_key, &buf).unwrap().plain == b"ws-opus" {
                    break;
                }
            }
        }
        // The public probe does not advertise voice.
        let info = reqwest::get(format!("http://127.0.0.1:{}/yarmiplay/info", server.port))
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert!(!info.contains("\"voice\""));
        server.stop();
    }
}
