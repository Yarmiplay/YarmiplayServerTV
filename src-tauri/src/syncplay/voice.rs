//! Room voice relay. Opus frames multiplexed on the Syncplay connection
//! (see `docs/VOICE_PROTOCOL.md`). There is no separate voice port.
//!
//! The hub is an in-memory table. It does not read or write the disk, and it
//! does not log packet contents, tokens, or keys. Plaintext exists only inside
//! [`VoiceHub::ingest`], long enough to seal a copy for each other seat.
//! Failures (a frame that will not open, a connection that will not take a
//! copy) are logged without those secrets.

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use parking_lot::Mutex;
use rand::Rng;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, BufReader};
use tokio::sync::mpsc::UnboundedSender;
use tracing::{error, warn};

use super::room::ConnId;

pub const VERSION: u64 = 1;
pub const MAX_SEATS: usize = 8;
pub const MAX_PACKET: usize = 1200;
pub const HEADER_LEN: usize = 40;
pub const TAG_LEN: usize = 16;
pub const MIN_PACKET: usize = HEADER_LEN + TAG_LEN;
pub const WS_PATH: &str = "/yarmiplay/voice";
/// Shown to clients when the host has voice switched off, including in `voiceClosed`.
pub const DISABLED_MESSAGE: &str = "voice disabled on this server";
/// ASCII Record Separator. A Syncplay JSON line never starts with this byte.
pub const FRAME_MARKER: u8 = 0x1E;
/// `voiceActivity` messages accepted per seat per second.
pub const ACTIVITY_PER_SEC: usize = 5;
pub const DEFAULT_BITRATE: u32 = 32_000;
/// Host-adjustable ranges. The packet ceiling is the yv1 maximum.
pub const PARTICIPANTS_MIN: u32 = 1;
pub const PARTICIPANTS_MAX: u32 = 32;
pub const BITRATE_MIN: u32 = 6_000;
pub const BITRATE_MAX: u32 = 128_000;
pub const ACTIVITY_MIN: u32 = 1;
pub const ACTIVITY_MAX: u32 = 30;

/// Limits the Voice page can change. The relay reads them on every join and frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VoicePolicy {
    pub max_participants: usize,
    pub max_bitrate: u32,
    pub max_packet: usize,
    pub activity_per_sec: usize,
    pub diagnostics: bool,
    pub fec: bool,
    pub dtx: bool,
}

impl Default for VoicePolicy {
    fn default() -> Self {
        Self {
            max_participants: MAX_SEATS,
            max_bitrate: DEFAULT_BITRATE,
            max_packet: MAX_PACKET,
            activity_per_sec: ACTIVITY_PER_SEC,
            diagnostics: true,
            fec: true,
            dtx: true,
        }
    }
}

impl VoicePolicy {
    /// Keep a bad value inside the ranges the Voice page allows.
    pub fn clamped(self) -> Self {
        Self {
            max_participants: self
                .max_participants
                .clamp(PARTICIPANTS_MIN as usize, PARTICIPANTS_MAX as usize),
            max_bitrate: self.max_bitrate.clamp(BITRATE_MIN, BITRATE_MAX),
            max_packet: self.max_packet.clamp(MIN_PACKET, MAX_PACKET),
            activity_per_sec: self
                .activity_per_sec
                .clamp(ACTIVITY_MIN as usize, ACTIVITY_MAX as usize),
            ..self
        }
    }

    fn advertised(self) -> (usize, u32, bool, bool) {
        (self.max_participants, self.max_bitrate, self.fec, self.dtx)
    }
}

/// How many voice seats are in one Syncplay room right now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VoiceRoomCount {
    pub room: String,
    pub users: usize,
}
const SEQ_WINDOW: u32 = 64;
pub(super) const AUDIO: u8 = 1;
const KEEPALIVE: u8 = 2;
const BYE: u8 = 3;

#[derive(Clone)]
struct Down {
    tx: UnboundedSender<Vec<u8>>,
    gen: u64,
}

/// Hands one yv1 packet to the seat's Syncplay connection. The connection
/// writer adds the `0x1E` framing. The closure must not log the packet.
pub type MuxSend = Arc<dyn Fn(Vec<u8>) + Send + Sync>;

enum Route {
    None,
    Stream(Down),
    Mux(MuxSend),
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
    policy: VoicePolicy,
    seats: HashMap<ConnId, Seat>,
    by_token: HashMap<u64, ConnId>,
    next_gen: u64,
}

pub struct VoiceHub {
    /// The Syncplay listener voice is multiplexed on. Not a second socket.
    syncplay_port: u16,
    /// Copied out of the policy so a dropped frame can be logged without the seat lock.
    diagnostics: AtomicBool,
    max_packet: AtomicUsize,
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
    pub port: u16,
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
}

impl VoiceHub {
    fn new(syncplay_port: u16) -> Arc<Self> {
        let policy = VoicePolicy::default();
        Arc::new(Self {
            syncplay_port,
            diagnostics: AtomicBool::new(policy.diagnostics),
            max_packet: AtomicUsize::new(policy.max_packet),
            inner: Mutex::new(Inner {
                listening: true,
                policy,
                seats: HashMap::new(),
                by_token: HashMap::new(),
                next_gen: 1,
            }),
        })
    }

    /// In-memory table for tests. Voice is multiplexed on port 8999.
    pub fn open_for_test() -> Arc<Self> {
        Self::new(8999)
    }

    pub fn listening(&self) -> bool {
        self.inner.lock().listening
    }

    pub fn syncplay_port(&self) -> u16 {
        self.syncplay_port
    }

    pub fn descriptor(&self) -> Value {
        let policy = self.inner.lock().policy;
        json!({
            "version": VERSION,
            "enabled": true,
            "codec": "opus",
            "clockRate": 48000,
            "channels": 1,
            "ptime": 20,
            "maxBitrate": policy.max_bitrate,
            "fec": policy.fec,
            "dtx": policy.dtx,
            "transport": "yv1",
            "mux": "syncplay",
            "port": self.syncplay_port,
            "wsPath": WS_PATH,
            "maxParticipants": policy.max_participants,
        })
    }

    /// Replace the live limits. True when clients must be told (participants, bitrate, FEC, DTX).
    pub fn set_policy(&self, policy: VoicePolicy) -> bool {
        let policy = policy.clamped();
        self.diagnostics
            .store(policy.diagnostics, Ordering::Relaxed);
        self.max_packet.store(policy.max_packet, Ordering::Relaxed);
        let mut g = self.inner.lock();
        let changed = g.policy.advertised() != policy.advertised();
        g.policy = policy;
        changed
    }

    pub fn occupancy(&self) -> Vec<VoiceRoomCount> {
        let g = self.inner.lock();
        let mut counts: HashMap<String, usize> = HashMap::new();
        for seat in g.seats.values() {
            *counts.entry(seat.room.clone()).or_default() += 1;
        }
        let mut rooms: Vec<VoiceRoomCount> = counts
            .into_iter()
            .map(|(room, users)| VoiceRoomCount { room, users })
            .collect();
        rooms.sort_by(|a, b| a.room.cmp(&b.room));
        rooms
    }

    fn note_drop(&self, message: &str) {
        if self.diagnostics.load(Ordering::Relaxed) {
            warn!("{message}");
        }
    }

    pub fn join(&self, conn: ConnId, room: &str, name: &str, version: u64) -> Join {
        if version != VERSION {
            return Join::Version;
        }
        let mut g = self.inner.lock();
        let cap = g.policy.max_participants;
        let replaced = g.seats.contains_key(&conn);
        if !replaced {
            let n = g.seats.values().filter(|s| s.room == room).count();
            if n >= cap {
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
            port: self.syncplay_port,
        })
    }

    /// Send later audio on this seat's Syncplay connection.
    /// `force` replaces a legacy WebSocket path. A mux path that is already
    /// installed is left in place so every audio frame does not rebuild it.
    pub fn attach_mux(&self, conn: ConnId, send: MuxSend, force: bool) {
        let mut g = self.inner.lock();
        let Some(seat) = g.seats.get_mut(&conn) else {
            return;
        };
        if matches!(seat.route, Route::Mux(_)) {
            return;
        }
        if !force && !matches!(seat.route, Route::None) {
            return;
        }
        seat.route = Route::Mux(send);
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
        let cap = g.policy.activity_per_sec;
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
        if seat.activity_ms.len() >= cap {
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

    /// A frame that arrived on the Syncplay connection `conn`. The packet token
    /// must belong to that connection. The path is not moved here; [`attach_mux`]
    /// does that.
    pub fn ingest_connection(&self, conn: ConnId, packet: &[u8]) {
        if packet.len() > self.max_packet.load(Ordering::Relaxed) {
            self.note_drop("voice relay dropped a frame: larger than the packet limit");
            return;
        }
        let Some(token) = packet_token(packet) else {
            self.note_drop(
                "voice relay dropped a frame on the Syncplay connection: not a yv1 packet",
            );
            return;
        };
        let owner = self.inner.lock().by_token.get(&token).copied();
        if owner != Some(conn) {
            self.note_drop(
                "voice relay dropped a frame: its token does not belong to this Syncplay connection",
            );
            return;
        }
        self.ingest(packet, None);
    }

    /// Decrypt one packet and fan the audio out to the other seats' paths.
    ///
    /// `stream` is a legacy WebSocket sender and the generation already registered
    /// for that socket (`None` until the first accepted packet). A failed tag does
    /// not move the path: the token sits in the clear, and a copied header must
    /// not be enough to steal where audio is sent. The returned generation is the
    /// one `detach_sender` needs for this socket.
    ///
    /// Rejected frames and delivery failures are logged. The packet, token, key,
    /// and plaintext are not.
    pub fn ingest(
        &self,
        packet: &[u8],
        stream: Option<(&UnboundedSender<Vec<u8>>, Option<u64>)>,
    ) -> Option<u64> {
        if packet.len() > self.max_packet.load(Ordering::Relaxed) {
            self.note_drop("voice relay dropped a frame: larger than the packet limit");
            return None;
        }
        let log = self.diagnostics.load(Ordering::Relaxed);
        let parsed = match packet_token(packet) {
            Some(t) => t,
            None => {
                if log {
                    warn!("voice relay dropped a frame: not a yv1 packet");
                }
                return None;
            }
        };
        let mut g = self.inner.lock();
        let Some(conn) = g.by_token.get(&parsed).copied() else {
            if log {
                warn!("voice relay dropped a frame: unknown seat");
            }
            return None;
        };
        let key = match g.seats.get(&conn) {
            Some(s) if s.token == parsed => s.key,
            _ => {
                if log {
                    warn!("voice relay dropped a frame: seat and token disagree");
                }
                return None;
            }
        };
        let Ok(opened) = open(&key, packet) else {
            if log {
                warn!("voice relay dropped a frame: authentication failed");
            }
            return None;
        };
        if opened.sender != 0 || !plain_ok(opened.ty, &opened.plain) {
            if log {
                warn!("voice relay dropped a frame: rejected plaintext or sender");
            }
            return None;
        }
        let (keep_gen, room, seq, ts, plain, ty) = {
            let Some(seat) = g.seats.get_mut(&conn) else {
                if log {
                    warn!("voice relay dropped a frame: seat disappeared");
                }
                return None;
            };
            if !accept_seq(seat, opened.seq) {
                if log {
                    warn!("voice relay dropped a frame: replayed or old sequence");
                }
                return None;
            }
            let keep_gen = stream.as_ref().and_then(|(_, existing)| {
                (*existing)
                    .filter(|gen| matches!(&seat.route, Route::Stream(down) if down.gen == *gen))
            });
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
            return stream_gen;
        }
        let targets: Vec<(u64, [u8; 32], RouteCopy)> = g
            .seats
            .values()
            .filter(|s| s.conn != conn && s.room == room)
            .map(|s| {
                let route = match &s.route {
                    Route::Stream(d) => RouteCopy::Stream(d.tx.clone()),
                    Route::Mux(send) => RouteCopy::Mux(send.clone()),
                    Route::None => RouteCopy::None,
                };
                (s.token, s.key, route)
            })
            .collect();
        drop(g);
        for (token, key, route) in targets {
            let Some(pkt) = seal(&key, AUDIO, token, conn, seq, ts, &plain) else {
                error!("voice relay failed to seal a frame for another seat");
                continue;
            };
            match route {
                RouteCopy::Stream(tx) => {
                    if tx.send(pkt).is_err() {
                        error!(
                            "voice relay failed to deliver a frame on the legacy voice WebSocket"
                        );
                    }
                }
                RouteCopy::Mux(send) => send(pkt),
                RouteCopy::None => {}
            }
        }
        stream_gen
    }
}

enum RouteCopy {
    None,
    Stream(UnboundedSender<Vec<u8>>),
    Mux(MuxSend),
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

pub(super) fn seal(
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
            "transport": "yv1",
            "mux": "syncplay",
            "port": self.port,
            "wsPath": WS_PATH,
            "codec": "opus",
            "clockRate": 48000,
            "channels": 1,
            "ptime": 20,
        })
    }
}

/// One voice record on the Syncplay byte stream: `0x1E`, big-endian length, yv1 packet.
/// `None` when `packet` is not a legal yv1 size. That refusal is logged.
pub fn encode_frame(packet: &[u8]) -> Option<Vec<u8>> {
    if !(MIN_PACKET..=MAX_PACKET).contains(&packet.len()) {
        error!(
            bytes = packet.len(),
            "voice relay refused to write a frame whose size is outside {MIN_PACKET}..={MAX_PACKET}"
        );
        return None;
    }
    let mut out = Vec::with_capacity(3 + packet.len());
    out.push(FRAME_MARKER);
    out.extend_from_slice(&(packet.len() as u16).to_be_bytes());
    out.extend_from_slice(packet);
    Some(out)
}

/// Audio older than this is not worth sending: the listener's jitter buffer
/// has moved on, and it would only delay the Syncplay lines behind it.
pub const QUEUED_MAX_AGE: std::time::Duration = std::time::Duration::from_millis(300);
/// Frames held for one connection at most (eight talkers for about 300 ms).
pub const QUEUED_MAX: usize = 128;

/// Voice waiting to be written to one client. A connection that can't keep up
/// loses its oldest audio instead of growing a backlog that the room's
/// pause, play and seek messages would have to wait behind.
#[derive(Default)]
pub struct VoiceQueue {
    frames: VecDeque<(std::time::Instant, Vec<u8>)>,
    dropped: u64,
}

impl VoiceQueue {
    pub fn push(&mut self, queued: std::time::Instant, packet: Vec<u8>) {
        self.frames.push_back((queued, packet));
        while self.frames.len() > QUEUED_MAX {
            self.frames.pop_front();
            self.dropped += 1;
        }
    }

    /// The oldest frame still fresh at `now`. Stale ones are dropped.
    pub fn pop(&mut self, now: std::time::Instant) -> Option<Vec<u8>> {
        while let Some((queued, packet)) = self.frames.pop_front() {
            if now.saturating_duration_since(queued) <= QUEUED_MAX_AGE {
                return Some(packet);
            }
            self.dropped += 1;
        }
        None
    }

    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// Frames dropped since the last call.
    pub fn take_dropped(&mut self) -> u64 {
        std::mem::take(&mut self.dropped)
    }
}

/// What the next record on a Syncplay connection is.
pub enum Record {
    /// One JSON object, without the trailing newline.
    Line(String),
    /// One yv1 packet, with the `0x1E` marker and the length already removed.
    Voice(Vec<u8>),
}

/// Read one JSON line or one voice frame. Blank lines are skipped.
/// A voice length outside 56..=1200 is an error so the caller can log and close.
pub async fn read_record<R: AsyncRead + Unpin>(
    reader: &mut BufReader<R>,
) -> std::io::Result<Option<Record>> {
    loop {
        let first = {
            let buf = reader.fill_buf().await?;
            if buf.is_empty() {
                return Ok(None);
            }
            buf[0]
        };
        if first == FRAME_MARKER {
            reader.consume(1);
            let mut len_buf = [0u8; 2];
            reader.read_exact(&mut len_buf).await?;
            let len = u16::from_be_bytes(len_buf) as usize;
            if !(MIN_PACKET..=MAX_PACKET).contains(&len) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!(
                        "voice frame length {len} is outside {MIN_PACKET}..={MAX_PACKET} on the Syncplay connection"
                    ),
                ));
            }
            let mut pkt = vec![0u8; len];
            reader.read_exact(&mut pkt).await?;
            return Ok(Some(Record::Voice(pkt)));
        }
        let mut line = String::new();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            return Ok(None);
        }
        if line.len() > super::protocol::MAX_LINE_LENGTH {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "line too long",
            ));
        }
        let text = line.trim();
        if text.is_empty() {
            continue;
        }
        return Ok(Some(Record::Line(text.to_string())));
    }
}

impl VoiceRuntime {
    /// Voice rides on the Syncplay listener. Nothing else is bound.
    pub fn start(syncplay_port: u16) -> Self {
        Self {
            hub: VoiceHub::new(syncplay_port),
        }
    }

    pub fn shutdown(self) {}
}

/// Legacy browser media socket on the Syncplay port (`GET /yarmiplay/voice`).
/// Each binary message is one yv1 packet. New clients multiplex on `/yarmiplay/ws`
/// or the Syncplay TCP connection instead.
pub async fn serve_ws(socket: axum::extract::ws::WebSocket, hub: Arc<VoiceHub>, token: u64) {
    use axum::extract::ws::Message;
    use futures_util::{SinkExt, StreamExt};
    use tokio::sync::mpsc::unbounded_channel;
    let (tx, mut rx) = unbounded_channel::<Vec<u8>>();
    let (mut sink, mut stream) = socket.split();
    let writer = tokio::spawn(async move {
        while let Some(pkt) = rx.recv().await {
            if sink.send(Message::Binary(pkt.into())).await.is_err() {
                error!("voice relay failed to write a frame on the legacy voice WebSocket");
                break;
            }
        }
    });
    let mut generation = None;
    while let Some(item) = stream.next().await {
        match item {
            Ok(Message::Binary(b)) => {
                if packet_token(&b) != Some(token) {
                    hub.note_drop("voice relay dropped a legacy WebSocket frame: token mismatch");
                    continue;
                }
                if let Some(gen) = hub.ingest(&b, Some((&tx, generation))) {
                    generation = Some(gen);
                }
            }
            Ok(Message::Close(_)) => break,
            Err(e) => {
                error!(error = %e, "voice relay legacy WebSocket failed");
                break;
            }
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
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};
    use tokio::io::BufReader;
    use tokio::sync::mpsc::unbounded_channel;

    #[test]
    fn a_slow_client_loses_old_voice_instead_of_growing_a_backlog() {
        let mut q = VoiceQueue::default();
        let t = Instant::now();
        for i in 0..QUEUED_MAX + 40 {
            q.push(t, vec![i as u8]);
        }
        assert_eq!(q.take_dropped(), 40);
        assert!(q
            .pop(t + QUEUED_MAX_AGE + Duration::from_millis(1))
            .is_none());
        assert_eq!(q.take_dropped(), QUEUED_MAX as u64);
        q.push(t, b"fresh".to_vec());
        assert_eq!(q.pop(t).as_deref(), Some(&b"fresh"[..]));
    }

    fn mux_receiver(hub: &VoiceHub, conn: ConnId) -> tokio::sync::mpsc::UnboundedReceiver<Vec<u8>> {
        let (tx, rx) = unbounded_channel();
        let failed = AtomicBool::new(false);
        hub.attach_mux(
            conn,
            Arc::new(move |pkt| {
                if tx.send(pkt).is_err() && !failed.swap(true, Ordering::Relaxed) {
                    error!("voice relay failed to deliver a frame on the Syncplay connection");
                }
            }),
            true,
        );
        rx
    }

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
        let mut bob = mux_receiver(&hub, 2);
        let mut carol = mux_receiver(&hub, 3);
        hub.ingest(&packet(&a, KEEPALIVE, 1, &[0x00]), None);
        assert!(bob.try_recv().is_err(), "keepalives are not forwarded");
        hub.ingest(&packet(&a, AUDIO, 2, b"opus-frame"), None);
        let heard = open(&key_of(&b), &bob.try_recv().unwrap()).unwrap();
        assert_eq!(heard.plain, b"opus-frame");
        assert_eq!(heard.sender, 1);
        assert_eq!(heard.ty, AUDIO);
        assert!(carol.try_recv().is_err(), "another room hears nothing");
        hub.ingest(&packet(&a, AUDIO, 2, b"opus-frame"), None);
        assert!(bob.try_recv().is_err(), "replay");
        hub.ingest(&packet(&a, AUDIO, 3, b""), None);
        assert!(bob.try_recv().is_err(), "empty audio");
        assert!(std::fs::read_dir(dir.path()).unwrap().next().is_none());
        let _ = (a, c);
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
        hub.ingest(&packet(&a, KEEPALIVE, 1, &[0x00]), None);
        hub.ingest(&packet(&a, AUDIO, 2, b"frame"), None);
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
    fn the_voice_page_limits_apply_without_a_new_relay() {
        let hub = VoiceHub::open_for_test();
        assert!(hub.set_policy(VoicePolicy {
            max_participants: 1,
            max_bitrate: 16_000,
            max_packet: 80,
            activity_per_sec: 1,
            diagnostics: false,
            fec: false,
            dtx: false,
        }));
        let described = hub.descriptor();
        assert_eq!(described["maxParticipants"], 1);
        assert_eq!(described["maxBitrate"], 16_000);
        assert_eq!(described["fec"], false);
        assert_eq!(described["dtx"], false);
        let _a = joined(&hub, 1, "movie", "alice");
        assert!(matches!(hub.join(2, "movie", "bob", VERSION), Join::Full));
        assert!(matches!(
            hub.activity(1, true, 0),
            Activity::Update { speaking: true, .. }
        ));
        assert!(matches!(hub.activity(1, false, 10), Activity::Dropped));
        let rooms = hub.occupancy();
        assert_eq!(rooms.len(), 1);
        assert_eq!(rooms[0].room, "movie");
        assert_eq!(rooms[0].users, 1);
        assert!(hub.ingest(&vec![0u8; 200], None).is_none());
        assert!(!hub.set_policy(VoicePolicy {
            max_participants: 1,
            max_bitrate: 16_000,
            max_packet: 90,
            activity_per_sec: 2,
            diagnostics: true,
            fec: false,
            dtx: false,
        }));
    }

    #[test]
    fn leaving_stops_forwarding() {
        let hub = VoiceHub::open_for_test();
        let a = joined(&hub, 1, "movie", "alice");
        let b = joined(&hub, 2, "movie", "bob");
        let mut bob = mux_receiver(&hub, 2);
        hub.ingest(&packet(&a, KEEPALIVE, 1, &[0x00]), None);
        hub.ingest(&packet(&b, KEEPALIVE, 1, &[0x00]), None);
        assert!(hub.leave(2));
        hub.ingest(&packet(&a, AUDIO, 2, b"x"), None);
        assert!(bob.try_recv().is_err());
        assert!(hub.room_of(2).is_none());
        assert_eq!(hub.conns_in("movie"), vec![1]);
        let _ = (a, b);
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
        let took = hub.ingest(&forged, Some((&thief, None)));
        assert!(took.is_none());
        hub.ingest(&packet(&a, KEEPALIVE, 1, &[0x00]), None);
        hub.ingest(&packet(&a, AUDIO, 2, b"still-bob"), None);
        assert_eq!(
            open(&key_of(&b), &rx.try_recv().unwrap()).unwrap().plain,
            b"still-bob"
        );
        assert!(stolen.try_recv().is_err());
        hub.detach_sender(token_of(&b), gen);
        hub.ingest(&packet(&a, AUDIO, 3, b"gone"), None);
        assert!(rx.try_recv().is_err());
        assert!(parse_token(&b.token_hex.to_ascii_uppercase()).is_none());
    }

    #[tokio::test]
    async fn a_voice_frame_with_a_newline_inside_stays_one_record() {
        let hub = VoiceHub::open_for_test();
        let a = joined(&hub, 1, "movie", "alice");
        let mut pkt = packet(&a, AUDIO, 1, b"opus");
        pkt[10] = b'\n';
        pkt[40] = b'\n';
        assert!(pkt.contains(&b'\n'));
        let frame = encode_frame(&pkt).unwrap();
        let mut stream = Vec::new();
        stream.extend_from_slice(b"{\"Hello\":{}}\r\n");
        stream.extend_from_slice(&frame);
        stream.extend_from_slice(b"\r\n{\"List\":null}\r\n");
        let mut reader = BufReader::new(&stream[..]);
        let Record::Line(hello) = read_record(&mut reader).await.unwrap().unwrap() else {
            panic!("json");
        };
        assert!(hello.starts_with('{'));
        let Record::Voice(got) = read_record(&mut reader).await.unwrap().unwrap() else {
            panic!("voice");
        };
        assert_eq!(got, pkt);
        let Record::Line(list) = read_record(&mut reader).await.unwrap().unwrap() else {
            panic!("json after voice");
        };
        assert!(list.contains("List"));
        assert!(read_record(&mut reader).await.unwrap().is_none());
        assert!(encode_frame(&[0, 1, 2]).is_none());
    }

    #[tokio::test]
    async fn mux_on_the_syncplay_connection_delivers_a_frame() {
        use tokio::io::{AsyncWriteExt, BufReader};
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
        server.install_voice(VoiceRuntime::start(server.port));

        async fn connect(
            port: u16,
            name: &str,
        ) -> (
            tokio::net::tcp::OwnedWriteHalf,
            BufReader<tokio::net::tcp::OwnedReadHalf>,
        ) {
            let stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .unwrap();
            let (rd, mut wr) = stream.into_split();
            let hello = serde_json::json!({
                "Hello": {
                    "username": name,
                    "room": { "name": "lobby" },
                    "version": "1.2.255",
                    "realversion": "1.7.4",
                    "features": { "yarmiplay": { "protocol": 1 } }
                }
            });
            wr.write_all(super::super::protocol::line(&hello).as_bytes())
                .await
                .unwrap();
            (wr, BufReader::new(rd))
        }
        async fn until_yarm(
            reader: &mut BufReader<tokio::net::tcp::OwnedReadHalf>,
            key: &str,
        ) -> Value {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            loop {
                let left = deadline.saturating_duration_since(std::time::Instant::now());
                let rec = tokio::time::timeout(left, read_record(reader))
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap();
                let Record::Line(text) = rec else { continue };
                let v: Value = serde_json::from_str(&text).unwrap();
                if v.get("Yarmiplay").and_then(|y| y.get(key)).is_some() {
                    return v;
                }
            }
        }

        let (mut alice_w, mut alice_r) = connect(server.port, "alice").await;
        let (mut bob_w, mut bob_r) = connect(server.port, "bob").await;
        let caps = until_yarm(&mut alice_r, "session").await;
        assert_eq!(caps["Yarmiplay"]["capabilities"]["voice"], true);
        assert_eq!(caps["Yarmiplay"]["voice"]["mux"], "syncplay");
        assert_eq!(caps["Yarmiplay"]["voice"]["port"], server.port);
        assert!(caps["Yarmiplay"]["voice"].get("udpPort").is_none());
        assert!(caps["Yarmiplay"]["voice"].get("tcpPort").is_none());
        until_yarm(&mut bob_r, "session").await;
        let join = super::super::protocol::line(
            &serde_json::json!({"Yarmiplay":{"voiceJoin":{"version":1,"mux":true}}}),
        );
        alice_w.write_all(join.as_bytes()).await.unwrap();
        bob_w.write_all(join.as_bytes()).await.unwrap();
        let a_joined =
            until_yarm(&mut alice_r, "voiceJoined").await["Yarmiplay"]["voiceJoined"].clone();
        let b_joined =
            until_yarm(&mut bob_r, "voiceJoined").await["Yarmiplay"]["voiceJoined"].clone();
        assert_eq!(a_joined["mux"], "syncplay");
        assert_eq!(a_joined["port"], server.port);
        let a = SeatSecrets {
            participant_id: a_joined["participantId"].as_str().unwrap().into(),
            username: "alice".into(),
            room: "lobby".into(),
            token_hex: a_joined["token"].as_str().unwrap().into(),
            key_b64: a_joined["key"].as_str().unwrap().into(),
            replaced: false,
            port: server.port,
        };
        let b = SeatSecrets {
            participant_id: b_joined["participantId"].as_str().unwrap().into(),
            username: "bob".into(),
            room: "lobby".into(),
            token_hex: b_joined["token"].as_str().unwrap().into(),
            key_b64: b_joined["key"].as_str().unwrap().into(),
            replaced: false,
            port: server.port,
        };
        let b_key = key_of(&b);
        let frame = encode_frame(&packet(&a, AUDIO, 2, b"mux-\nopus")).unwrap();
        alice_w.write_all(&frame).await.unwrap();
        alice_w.flush().await.unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        loop {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            let rec = tokio::time::timeout(left, read_record(&mut bob_r))
                .await
                .expect("bob timed out waiting for audio")
                .unwrap()
                .unwrap();
            if let Record::Voice(buf) = rec {
                assert_eq!(open(&b_key, &buf).unwrap().plain, b"mux-\nopus");
                break;
            }
        }
        server.stop();
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
        server.install_voice(VoiceRuntime::start(server.port));

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
            port: a_joined["port"].as_u64().unwrap() as u16,
        };
        assert_eq!(a.port, server.port);
        assert!(a_joined.get("udpPort").is_none());
        let b = SeatSecrets {
            participant_id: b_joined["participantId"].as_str().unwrap().into(),
            username: "bob".into(),
            room: "lobby".into(),
            token_hex: b_joined["token"].as_str().unwrap().into(),
            key_b64: b_joined["key"].as_str().unwrap().into(),
            replaced: false,
            port: b_joined["port"].as_u64().unwrap() as u16,
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
