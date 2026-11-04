//! Stress for everything that shares one Syncplay connection: JSON lines and
//! voice frames, many rooms, slow readers, TLS, and a viewer who leaves voice
//! and keeps pausing and playing. The simulated clients behave like Syncplay's
//! own: every server State is answered, and ignoringOnTheFly is echoed.

use super::protocol::line;
use super::room::SyncplayOptions;
use super::server::{Extensions, SyncplayServer};
use super::voice::{encode_frame, read_record, seal, Record, VoiceRuntime, AUDIO};
use crate::tls::CertStore;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use parking_lot::Mutex;
use serde_json::{json, Map, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{
    AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader, ReadHalf, WriteHalf,
};
use tokio::net::TcpStream;
use tokio::sync::Mutex as AsyncMutex;
use tokio::task::JoinHandle;

trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}
type Conn = Box<dyn Io>;
type Writer = Arc<AsyncMutex<WriteHalf<Conn>>>;

/// How fast a client takes bytes off its socket.
#[derive(Clone, Copy)]
enum Pace {
    Fast,
    /// At most this many bytes per second.
    Throttled(u64),
}

/// The client's player and its half of Syncplay's ignoringOnTheFly.
struct Player {
    paused: bool,
    position: f64,
    server_ign: u64,
    client_ign: u64,
}

#[derive(Default)]
struct Seen {
    voice: usize,
    last_voice: Option<Instant>,
    closed: Option<Instant>,
    joined: Option<Value>,
    session: bool,
    broken: Option<String>,
}

struct Client {
    name: String,
    w: Writer,
    player: Arc<Mutex<Player>>,
    seen: Arc<Mutex<Seen>>,
    /// Stop taking bytes off the socket while set.
    hold: Arc<AtomicBool>,
    talking: Arc<AtomicBool>,
    reader: JoinHandle<()>,
}

struct Tls {
    store: CertStore,
    root: rustls::pki_types::CertificateDer<'static>,
}

fn tls_store() -> Tls {
    let params = rcgen::CertificateParams::new(vec!["localhost".into()]).unwrap();
    let key = rcgen::KeyPair::generate().unwrap();
    let cert = params.self_signed(&key).unwrap();
    let config = crate::tls::server_config(&cert.pem(), &key.serialize_pem()).unwrap();
    let store: CertStore = Arc::new(parking_lot::RwLock::new(Some(Arc::new(
        crate::tls::CertBundle {
            host: "localhost".into(),
            names: vec!["localhost".into()],
            cert_pem: cert.pem(),
            key_pem: key.serialize_pem(),
            not_before: 0,
            not_after: i64::MAX,
            staging: true,
            fingerprint: String::new(),
            config,
        },
    ))));
    Tls {
        store,
        root: cert.der().clone(),
    }
}

async fn start(tls: Option<&Tls>) -> SyncplayServer {
    crate::net::install_crypto_provider();
    let store = tls
        .map(|t| t.store.clone())
        .unwrap_or_else(|| Arc::new(parking_lot::RwLock::new(None)));
    let server = SyncplayServer::start(
        0,
        SyncplayOptions::default(),
        store,
        Arc::new(|| {}),
        Extensions::default(),
    )
    .await
    .unwrap();
    server.install_voice(VoiceRuntime::start(server.port));
    server
}

async fn open(port: u16, tls: Option<&Tls>, pace: Pace) -> Conn {
    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    if let Pace::Throttled(_) = pace {
        // A slow link holds little in flight; loopback would otherwise buffer megabytes.
        // The kernel doubles this. Together with the server's send buffer it
        // is the only audio that can sit ahead of a pause on the loopback.
        socket.set_recv_buffer_size(4 * 1024).unwrap();
    }
    let mut tcp: TcpStream = socket.connect(([127, 0, 0, 1], port).into()).await.unwrap();
    tcp.set_nodelay(true).unwrap();
    let Some(tls) = tls else {
        return Box::new(tcp);
    };
    tcp.write_all(line(&json!({ "TLS": { "startTLS": "send" } })).as_bytes())
        .await
        .unwrap();
    let mut reply = Vec::new();
    loop {
        let mut byte = [0u8; 1];
        tcp.read_exact(&mut byte).await.unwrap();
        reply.push(byte[0]);
        if byte[0] == b'\n' {
            break;
        }
    }
    assert!(String::from_utf8_lossy(&reply).contains("\"true\""));
    let mut roots = rustls::RootCertStore::empty();
    roots.add(tls.root.clone()).unwrap();
    let config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let stream = tokio_rustls::TlsConnector::from(Arc::new(config))
        .connect(
            rustls::pki_types::ServerName::try_from("localhost").unwrap(),
            tcp,
        )
        .await
        .unwrap();
    Box::new(stream)
}

async fn connect(port: u16, name: &str, room: &str, tls: Option<&Tls>, pace: Pace) -> Client {
    let conn = open(port, tls, pace).await;
    let (rd, wr) = tokio::io::split(conn);
    let w: Writer = Arc::new(AsyncMutex::new(wr));
    let hello = json!({
        "Hello": {
            "username": name,
            "room": { "name": room },
            "version": "1.2.255",
            "realversion": "1.7.4",
            "features": { "yarmiplay": { "protocol": 1 } }
        }
    });
    w.lock()
        .await
        .write_all(line(&hello).as_bytes())
        .await
        .unwrap();
    let player = Arc::new(Mutex::new(Player {
        paused: true,
        position: 0.0,
        server_ign: 0,
        client_ign: 0,
    }));
    let seen = Arc::new(Mutex::new(Seen::default()));
    let hold = Arc::new(AtomicBool::new(false));
    let reader = tokio::spawn(read_loop(
        BufReader::new(rd),
        w.clone(),
        player.clone(),
        seen.clone(),
        hold.clone(),
        pace,
    ));
    let c = Client {
        name: name.to_string(),
        w,
        player,
        seen,
        hold,
        talking: Arc::new(AtomicBool::new(false)),
        reader,
    };
    wait_for(&format!("{name}'s session"), Duration::from_secs(5), || {
        c.seen.lock().session
    })
    .await;
    c
}

async fn read_loop(
    mut r: BufReader<ReadHalf<Conn>>,
    w: Writer,
    player: Arc<Mutex<Player>>,
    seen: Arc<Mutex<Seen>>,
    hold: Arc<AtomicBool>,
    pace: Pace,
) {
    let began = Instant::now();
    let mut taken: u64 = 0;
    loop {
        while hold.load(Ordering::Relaxed) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let rec = match read_record(&mut r).await {
            Ok(Some(rec)) => rec,
            Ok(None) => break,
            Err(e) => {
                seen.lock().broken = Some(e.to_string());
                break;
            }
        };
        if let Pace::Throttled(rate) = pace {
            taken += match &rec {
                Record::Voice(p) => p.len() as u64 + 3,
                Record::Line(l) => l.len() as u64 + 2,
            };
            let due = began + Duration::from_secs_f64(taken as f64 / rate as f64);
            tokio::time::sleep_until(due.into()).await;
        }
        match rec {
            Record::Voice(_) => {
                let mut s = seen.lock();
                s.voice += 1;
                s.last_voice = Some(Instant::now());
            }
            Record::Line(text) => {
                let v: Value = match serde_json::from_str(&text) {
                    Ok(v) => v,
                    Err(e) => {
                        seen.lock().broken = Some(format!("broken JSON line: {e}"));
                        break;
                    }
                };
                if let Some(y) = v.get("Yarmiplay") {
                    let mut s = seen.lock();
                    if y.get("session").is_some() {
                        s.session = true;
                    }
                    if let Some(j) = y.get("voiceJoined") {
                        s.joined = Some(j.clone());
                        s.closed = None;
                    }
                    if y.get("voiceClosed").is_some() {
                        s.closed = Some(Instant::now());
                    }
                }
                if let Some(st) = v.get("State") {
                    let mut w = w.lock().await;
                    let reply = answer(&player, st);
                    if w.write_all(line(&reply).as_bytes()).await.is_err()
                        || w.flush().await.is_err()
                    {
                        break;
                    }
                }
            }
        }
    }
}

/// Syncplay's client: apply the room's state unless our own change is in
/// flight, then report ours and echo the ignore counters.
fn answer(player: &Mutex<Player>, st: &Value) -> Value {
    let mut p = player.lock();
    if let Some(ign) = st.get("ignoringOnTheFly") {
        if let Some(n) = ign.get("server").and_then(Value::as_u64) {
            p.server_ign = n;
            p.client_ign = 0;
        } else if let Some(n) = ign.get("client").and_then(Value::as_u64) {
            if n == p.client_ign {
                p.client_ign = 0;
            }
        }
    }
    if p.client_ign == 0 {
        if let Some(ps) = st.get("playstate") {
            if let Some(paused) = ps.get("paused").and_then(Value::as_bool) {
                p.paused = paused;
            }
            if let Some(pos) = ps.get("position").and_then(Value::as_f64) {
                p.position = pos;
            }
        }
    }
    let mut state = json!({ "ping": {
        "latencyCalculation": st["ping"]["latencyCalculation"],
        "clientRtt": 0.0,
    } });
    if p.client_ign == 0 || p.server_ign != 0 {
        state["playstate"] = json!({ "position": p.position, "paused": p.paused });
    }
    let mut ign = Map::new();
    if p.server_ign != 0 {
        ign.insert("server".into(), json!(p.server_ign));
        p.server_ign = 0;
    }
    if p.client_ign != 0 {
        ign.insert("client".into(), json!(p.client_ign));
    }
    if !ign.is_empty() {
        state["ignoringOnTheFly"] = Value::Object(ign);
    }
    json!({ "State": state })
}

impl Client {
    async fn send(&self, v: Value) {
        let mut w = self.w.lock().await;
        w.write_all(line(&v).as_bytes()).await.unwrap();
        w.flush().await.unwrap();
    }

    /// The user pressed pause or play.
    async fn set_paused(&self, paused: bool) {
        let mut w = self.w.lock().await;
        let msg = {
            let mut p = self.player.lock();
            p.paused = paused;
            let mut state = json!({ "playstate": { "position": p.position, "paused": paused } });
            p.client_ign += 1;
            let mut ign = json!({ "client": p.client_ign });
            if p.server_ign != 0 {
                ign["server"] = json!(p.server_ign);
                p.server_ign = 0;
            }
            state["ignoringOnTheFly"] = ign;
            json!({ "State": state })
        };
        w.write_all(line(&msg).as_bytes()).await.unwrap();
        w.flush().await.unwrap();
    }

    fn paused(&self) -> bool {
        self.player.lock().paused
    }

    async fn join_voice(&self) {
        self.send(json!({ "Yarmiplay": { "voiceJoin": { "version": 1, "mux": true } } }))
            .await;
        wait_for(
            &format!("{} to join voice", self.name),
            Duration::from_secs(5),
            || self.seen.lock().joined.is_some(),
        )
        .await;
    }

    async fn leave_voice(&self) {
        self.talking.store(false, Ordering::Relaxed);
        self.send(json!({ "Yarmiplay": { "voiceLeave": {} } }))
            .await;
        wait_for(
            &format!("{} to leave voice", self.name),
            Duration::from_secs(5),
            || self.seen.lock().closed.is_some(),
        )
        .await;
    }

    /// 50 Opus-sized frames a second on the Syncplay connection.
    fn talk(&self) -> JoinHandle<()> {
        let joined = self.seen.lock().joined.clone().unwrap();
        let token = u64::from_str_radix(joined["token"].as_str().unwrap(), 16).unwrap();
        let key: [u8; 32] = B64
            .decode(joined["key"].as_str().unwrap())
            .unwrap()
            .try_into()
            .unwrap();
        let w = self.w.clone();
        let talking = self.talking.clone();
        talking.store(true, Ordering::Relaxed);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_millis(20));
            let mut seq: u32 = 1;
            while talking.load(Ordering::Relaxed) {
                tick.tick().await;
                let pkt = seal(
                    &key,
                    AUDIO,
                    token,
                    0,
                    seq,
                    seq.wrapping_mul(960),
                    &[0x55; 100],
                )
                .unwrap();
                seq += 1;
                let frame = encode_frame(&pkt).unwrap();
                let mut w = w.lock().await;
                if w.write_all(&frame).await.is_err() || w.flush().await.is_err() {
                    break;
                }
            }
        })
    }

    fn broken(&self) -> Option<String> {
        self.seen.lock().broken.clone()
    }
}

async fn wait_for(what: &str, limit: Duration, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + limit;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// Press pause or play on `by` and time how long until every client in
/// `room` plays along. Panics past `limit`.
async fn toggle(by: &Client, room: &[&Client], paused: bool, limit: Duration) -> Duration {
    let t0 = Instant::now();
    by.set_paused(paused).await;
    let deadline = t0 + limit;
    loop {
        let behind: Vec<&str> = room
            .iter()
            .filter(|c| c.paused() != paused)
            .map(|c| c.name.as_str())
            .collect();
        if behind.is_empty() {
            return t0.elapsed();
        }
        assert!(
            Instant::now() < deadline,
            "{} pressed {} but {behind:?} still disagree after {limit:?}",
            by.name,
            if paused { "pause" } else { "play" },
        );
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn leaving_voice_then_pausing_and_playing_keeps_everyone_in_step() {
    let tls = tls_store();
    let server = start(Some(&tls)).await;
    let port = server.port;
    let viewer = connect(port, "viewer", "film", Some(&tls), Pace::Fast).await;
    let host = connect(port, "host", "film", Some(&tls), Pace::Fast).await;
    let mut talkers = Vec::new();
    for (i, secure) in [false, false, true, true].into_iter().enumerate() {
        let tls = secure.then_some(&tls);
        talkers.push(connect(port, &format!("talker{i}"), "film", tls, Pace::Fast).await);
    }
    viewer.join_voice().await;
    host.join_voice().await;
    let mut voices = Vec::new();
    for t in &talkers {
        t.join_voice().await;
        voices.push(t.talk());
    }
    voices.push(viewer.talk());
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(viewer.seen.lock().voice > 100, "the viewer heard voice");

    viewer.leave_voice().await;
    let left = viewer.seen.lock().closed.unwrap();
    let heard_before = viewer.seen.lock().voice;

    let everyone: Vec<&Client> = [&viewer, &host].into_iter().chain(talkers.iter()).collect();
    let mut worst = Duration::ZERO;
    for i in 0..24 {
        let by = if i % 3 == 0 { &viewer } else { &host };
        let paused = !by.paused();
        worst = worst.max(toggle(by, &everyone, paused, Duration::from_secs(2)).await);
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    eprintln!("pause/play reached everyone within {worst:?} at worst");
    assert!(
        worst < Duration::from_millis(500),
        "slowest pause/play took {worst:?}"
    );

    let seen = viewer.seen.lock();
    let stray = seen.voice - heard_before;
    let last = seen.last_voice.unwrap();
    assert!(
        last <= left + Duration::from_millis(200),
        "voice kept arriving {:?} after the viewer left ({stray} frames)",
        last.saturating_duration_since(left)
    );
    drop(seen);
    for c in &everyone {
        assert_eq!(c.broken(), None, "{}'s stream broke", c.name);
    }
    for t in &talkers {
        t.talking.store(false, Ordering::Relaxed);
    }
    for v in voices {
        let _ = v.await;
    }
    server.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_client_that_stops_reading_does_not_hold_up_the_room() {
    let server = start(None).await;
    let port = server.port;
    let host = connect(port, "host", "film", None, Pace::Fast).await;
    let stuck = connect(port, "stuck", "film", None, Pace::Fast).await;
    let mut talkers = Vec::new();
    for i in 0..6 {
        talkers.push(connect(port, &format!("talker{i}"), "film", None, Pace::Fast).await);
    }
    stuck.join_voice().await;
    let mut voices = Vec::new();
    for t in &talkers {
        t.join_voice().await;
        voices.push(t.talk());
    }
    stuck.hold.store(true, Ordering::Relaxed);

    let awake: Vec<&Client> = std::iter::once(&host).chain(talkers.iter()).collect();
    let mut worst = Duration::ZERO;
    for i in 0..16 {
        worst = worst.max(toggle(&host, &awake, i % 2 == 0, Duration::from_secs(2)).await);
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    eprintln!("with one client not reading, the rest followed within {worst:?}");
    assert!(
        worst < Duration::from_millis(500),
        "slowest pause/play took {worst:?}"
    );

    stuck.hold.store(false, Ordering::Relaxed);
    let want = host.paused();
    let t0 = Instant::now();
    wait_for(
        "the stuck client to catch up",
        Duration::from_secs(10),
        || stuck.paused() == want,
    )
    .await;
    eprintln!(
        "the stuck client caught up {:?} after it read again",
        t0.elapsed()
    );
    for t in &talkers {
        t.talking.store(false, Ordering::Relaxed);
    }
    for v in voices {
        let _ = v.await;
    }
    for c in awake.iter().chain([&&stuck]) {
        assert_eq!(c.broken(), None, "{}'s stream broke", c.name);
    }
    server.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_slow_link_still_gets_pause_and_play_promptly() {
    let server = start(None).await;
    let port = server.port;
    let host = connect(port, "host", "film", None, Pace::Fast).await;
    // Half of what six talkers send.
    let slow = connect(port, "slow", "film", None, Pace::Throttled(24_000)).await;
    let mut talkers = Vec::new();
    for i in 0..6 {
        talkers.push(connect(port, &format!("talker{i}"), "film", None, Pace::Fast).await);
    }
    slow.join_voice().await;
    let mut voices = Vec::new();
    for t in &talkers {
        t.join_voice().await;
        voices.push(t.talk());
    }
    tokio::time::sleep(Duration::from_secs(3)).await;

    let room = [&host, &slow];
    let mut worst = Duration::ZERO;
    for i in 0..8 {
        let took = toggle(&host, &room, i % 2 == 0, Duration::from_secs(30)).await;
        eprintln!("pause/play {i} reached the slow link in {took:?}");
        worst = worst.max(took);
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    for t in &talkers {
        t.talking.store(false, Ordering::Relaxed);
    }
    for v in voices {
        let _ = v.await;
    }
    assert_eq!(slow.broken(), None);
    assert!(
        worst < Duration::from_millis(1500),
        "the slow link waited {worst:?} for pause/play behind queued voice"
    );
    server.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn many_rooms_with_voice_stay_in_step() {
    let server = start(None).await;
    let port = server.port;
    let mut rooms: Vec<Vec<Client>> = Vec::new();
    let mut voices = Vec::new();
    for r in 0..6 {
        let name = format!("room{r}");
        let mut members = Vec::new();
        for i in 0..8 {
            let c = connect(port, &format!("r{r}u{i}"), &name, None, Pace::Fast).await;
            if i < 4 {
                c.join_voice().await;
                voices.push(c.talk());
            }
            members.push(c);
        }
        rooms.push(members);
    }
    let worst = Arc::new(Mutex::new(Duration::ZERO));
    let rooms = Arc::new(rooms);
    let mut jobs = Vec::new();
    for r in 0..rooms.len() {
        let rooms = rooms.clone();
        let worst = worst.clone();
        jobs.push(tokio::spawn(async move {
            let members: Vec<&Client> = rooms[r].iter().collect();
            for i in 0..12 {
                let by = members[i % members.len()];
                let paused = !by.paused();
                let took = toggle(by, &members, paused, Duration::from_secs(3)).await;
                {
                    let mut w = worst.lock();
                    *w = (*w).max(took);
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }));
    }
    for j in jobs {
        j.await.unwrap();
    }
    let worst = *worst.lock();
    eprintln!("48 clients in 6 rooms: pause/play reached a whole room within {worst:?}");
    for room in rooms.iter() {
        for c in room {
            c.talking.store(false, Ordering::Relaxed);
            assert_eq!(c.broken(), None, "{}'s stream broke", c.name);
        }
    }
    for v in voices {
        let _ = v.await;
    }
    assert!(
        worst < Duration::from_millis(750),
        "slowest pause/play took {worst:?}"
    );
    for room in rooms.iter() {
        for c in room {
            c.reader.abort();
        }
    }
    server.stop();
}
