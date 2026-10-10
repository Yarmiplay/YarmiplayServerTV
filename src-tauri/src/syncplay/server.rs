//! TCP listener for the Syncplay server, with optional STARTTLS using the
//! current certificate from the [`CertStore`] (read per connection, so a
//! renewed certificate applies to new connections without a restart).
//! Unless vanilla mode is on, the same port also answers HTTP(S) for the
//! YarmiplayTV extensions (see [`super::mux`]).

use super::devices::{self, Decision, DeviceStore};
use super::ext::{self, Effect, JellyfinShare};
use super::mux::{self, ConnMeta, HttpSender, Sniff};
use super::protocol::{line, now_secs, MAX_LINE_LENGTH};
use super::room::{Access, ConnId, Out, RoomInfo, ServerState, SyncplayOptions};
use crate::config::SyncplayAccess;
use crate::jellyfin::proxy::JellyfinProxy;
use crate::relay::Relay;
use crate::tls::CertStore;
use futures_util::future::BoxFuture;
use parking_lot::Mutex;
use serde_json::{json, Map, Value};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

/// A YarmiplayTV client must answer a device challenge within this.
const AUTH_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a device may wait for the host to approve it.
const PENDING_LIMIT: Duration = Duration::from_secs(10 * 60);
const PENDING_KEEPALIVE: Duration = Duration::from_secs(60);
const AUTH_FAILED: &str = "Device authentication failed";

pub type ChangeNotify = Arc<dyn Fn() + Send + Sync>;
/// Approve a Jellyfin Quick Connect code for the guest account.
pub type AuthorizeFn = Arc<dyn Fn(String) -> BoxFuture<'static, Result<(), String>> + Send + Sync>;

/// The async parts behind the YarmiplayTV extensions. All optional: without
/// them the server is a plain Syncplay server.
#[derive(Clone, Default)]
pub struct Extensions {
    pub relay: Option<Relay>,
    pub proxy: Option<Arc<JellyfinProxy>>,
    pub authorize: Option<AuthorizeFn>,
    /// Approved device keys; without it nobody gets a device challenge.
    pub devices: Option<Arc<DeviceStore>>,
}

pub struct SyncplayServer {
    pub port: u16,
    shared: Arc<Shared>,
    accept: JoinHandle<()>,
    heartbeat: JoinHandle<()>,
    http: JoinHandle<()>,
}

struct Shared {
    state: Arc<Mutex<ServerState>>,
    tls: CertStore,
    next_id: AtomicU64,
    notify: ChangeNotify,
    ext: Extensions,
    http: HttpSender,
}

impl Shared {
    fn relay_on(&self, opts: &SyncplayOptions) {
        if let Some(r) = &self.ext.relay {
            r.set_enabled(opts.file_relay && !opts.vanilla_mode);
        }
    }

    /// Hand queued [`Effect`]s to the relay and Jellyfin.
    fn dispatch(self: &Arc<Self>) {
        let effects = self.state.lock().take_effects();
        for effect in effects {
            match effect {
                Effect::AuthorizeJellyfin { conn, code } => {
                    let shared = self.clone();
                    tokio::spawn(async move { shared.authorize(conn, code).await });
                }
                other => {
                    if let Some(r) = &self.ext.relay {
                        r.handle(&other);
                    }
                }
            }
        }
    }

    async fn authorize(&self, conn: ConnId, code: String) {
        let result = match &self.ext.authorize {
            Some(f) => f(code.clone()).await,
            None => Err("Jellyfin sharing is off on this server".into()),
        };
        let reply = match result {
            Ok(()) => json!({ "code": code, "ok": true }),
            Err(e) => {
                debug!(error = %e, "Quick Connect approval failed");
                json!({ "code": code, "ok": false, "error": e })
            }
        };
        self.state.lock().send_ext(conn, "jellyfinAuthorize", reply);
    }
}

impl SyncplayServer {
    pub async fn start(
        port: u16,
        opts: SyncplayOptions,
        tls: CertStore,
        notify: ChangeNotify,
        ext: Extensions,
    ) -> std::io::Result<Self> {
        let listener = TcpListener::bind(SocketAddr::from(([0, 0, 0, 0], port))).await?;
        let local = listener.local_addr()?;
        let port = local.port();
        let mut st = ServerState::new(opts.clone());
        st.set_https(tls.read().is_some());
        let state = Arc::new(Mutex::new(st));
        if let Some(r) = &ext.relay {
            r.attach(Arc::downgrade(&state));
        }
        let (http_tx, http) = mux::start_http(Arc::downgrade(&state), ext.clone(), local);
        let shared = Arc::new(Shared {
            state: state.clone(),
            tls,
            next_id: AtomicU64::new(1),
            notify: notify.clone(),
            ext,
            http: http_tx,
        });
        shared.relay_on(&opts);

        let accept = {
            let shared = shared.clone();
            tokio::spawn(async move {
                let mut conns = tokio::task::JoinSet::new();
                loop {
                    tokio::select! {
                        accepted = listener.accept() => {
                            let Ok((stream, peer)) = accepted else { continue };
                            let _ = stream.set_nodelay(true);
                            let shared = shared.clone();
                            conns.spawn(async move { serve_conn(stream, peer, shared).await });
                        }
                        Some(_) = conns.join_next(), if !conns.is_empty() => {}
                    }
                }
            })
        };

        let heartbeat = {
            let shared = shared.clone();
            tokio::spawn(async move {
                let mut tick = tokio::time::interval(Duration::from_secs(1));
                loop {
                    tick.tick().await;
                    let https = shared.tls.read().is_some();
                    let dropped = {
                        let mut st = shared.state.lock();
                        st.set_https(https);
                        st.tick(now_secs())
                    };
                    shared.dispatch();
                    if !dropped.is_empty() {
                        notify();
                    }
                }
            })
        };

        info!(port, "Syncplay server listening");
        Ok(Self {
            port,
            shared,
            accept,
            heartbeat,
            http,
        })
    }

    pub fn set_options(&self, opts: SyncplayOptions) {
        self.shared.state.lock().set_options(opts.clone());
        self.shared.relay_on(&opts);
        self.shared.dispatch();
    }

    /// What extension sessions are told about the shared Jellyfin.
    pub fn set_jellyfin(&self, share: Option<JellyfinShare>) {
        self.shared.state.lock().set_jellyfin(share);
    }

    /// Disconnect every login that used this device key.
    pub fn kick_device(&self, fingerprint: &str) {
        let kicked = self.shared.state.lock().kick_device(fingerprint);
        self.shared.dispatch();
        if kicked > 0 {
            (self.shared.notify)();
        }
    }

    pub fn user_count(&self) -> usize {
        self.shared.state.lock().user_count()
    }

    pub fn rooms(&self) -> Vec<RoomInfo> {
        self.shared.state.lock().rooms()
    }

    pub fn stop(self) {
        self.abort_tasks();
        self.shared.state.lock().close_all();
        info!(port = self.port, "Syncplay server stopped");
    }

    fn abort_tasks(&self) {
        self.accept.abort();
        self.heartbeat.abort();
        self.http.abort();
        if let Some(r) = &self.shared.ext.relay {
            r.detach();
        }
    }
}

impl Drop for SyncplayServer {
    fn drop(&mut self) {
        self.abort_tasks();
    }
}

/// Read one `\n`-terminated line (cancel-safe accumulation into `buf`).
async fn read_line<R: AsyncRead + Unpin>(
    reader: &mut BufReader<R>,
    buf: &mut Vec<u8>,
) -> std::io::Result<Option<Value>> {
    loop {
        buf.clear();
        let n = reader.read_until(b'\n', buf).await?;
        if n == 0 {
            return Ok(None);
        }
        if buf.len() > MAX_LINE_LENGTH {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "line too long",
            ));
        }
        let text = String::from_utf8_lossy(buf);
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        return match serde_json::from_str::<Value>(text) {
            Ok(v) if v.is_object() => Ok(Some(v)),
            _ => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "not a JSON object",
            )),
        };
    }
}

/// What the first byte says, unless vanilla mode keeps the port Syncplay-only.
async fn sniff_http(stream: TcpStream, peer: SocketAddr, shared: &Shared) -> Option<TcpStream> {
    if shared.state.lock().vanilla() {
        return Some(stream);
    }
    let mut first = [0u8; 1];
    match tokio::time::timeout(Duration::from_secs(15), stream.peek(&mut first)).await {
        Ok(Ok(1)) => {}
        _ => return None,
    }
    match mux::sniff(first[0]) {
        Sniff::Syncplay => Some(stream),
        Sniff::Http => {
            let meta = ConnMeta { peer, tls: false };
            let _ = shared.http.send((Box::new(stream), meta)).await;
            None
        }
        Sniff::Tls => {
            let bundle = shared.tls.read().clone()?;
            let mut config = (*bundle.config).clone();
            config.alpn_protocols = vec![b"http/1.1".to_vec()];
            let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
            match tokio::time::timeout(Duration::from_secs(15), acceptor.accept(stream)).await {
                Ok(Ok(tls)) => {
                    let meta = ConnMeta { peer, tls: true };
                    let _ = shared.http.send((Box::new(tls), meta)).await;
                }
                Ok(Err(e)) => debug!(%peer, error = %e, "HTTPS handshake failed"),
                Err(_) => debug!(%peer, "HTTPS handshake timed out"),
            }
            None
        }
    }
}

async fn serve_conn(stream: TcpStream, peer: SocketAddr, shared: Arc<Shared>) {
    let Some(stream) = sniff_http(stream, peer, &shared).await else {
        return;
    };
    let id = shared.next_id.fetch_add(1, Ordering::Relaxed);
    debug!(%peer, id, "Syncplay connection");
    let mut reader = BufReader::new(stream);
    let mut buf = Vec::new();
    let first = match read_line(&mut reader, &mut buf).await {
        Ok(Some(v)) => v,
        _ => return,
    };

    // STARTTLS is only offered as the very first message, before Hello.
    if first.get("TLS").is_some() {
        let bundle = shared.tls.read().clone();
        let Some(bundle) = bundle else {
            if reader
                .get_mut()
                .write_all(line(&json!({ "TLS": { "startTLS": "false" } })).as_bytes())
                .await
                .is_err()
            {
                return;
            }
            return run_session(reader, None, id, peer, false, shared).await;
        };
        if reader
            .get_mut()
            .write_all(line(&json!({ "TLS": { "startTLS": "true" } })).as_bytes())
            .await
            .is_err()
        {
            return;
        }
        if !reader.buffer().is_empty() {
            warn!(%peer, "client sent data before the TLS handshake; dropping");
            return;
        }
        let tcp = reader.into_inner();
        let acceptor = tokio_rustls::TlsAcceptor::from(bundle.config.clone());
        match tokio::time::timeout(std::time::Duration::from_secs(15), acceptor.accept(tcp)).await {
            Ok(Ok(tls)) => {
                debug!(%peer, "Syncplay TLS established");
                run_session(BufReader::new(tls), None, id, peer, true, shared).await
            }
            Ok(Err(e)) => debug!(%peer, error = %e, "Syncplay TLS handshake failed"),
            Err(_) => debug!(%peer, "Syncplay TLS handshake timed out"),
        }
        return;
    }
    run_session(reader, Some(first), id, peer, false, shared).await
}

async fn run_session<S>(
    reader: BufReader<S>,
    mut pending: Option<Value>,
    id: ConnId,
    peer: SocketAddr,
    tls: bool,
    shared: Arc<Shared>,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (tx, mut rx) = unbounded_channel::<Out>();
    let stream = reader;
    let (rd, mut wr) = tokio::io::split(stream);
    let mut reader = BufReader::new(rd);

    let writer = tokio::spawn(async move {
        while let Some(out) = rx.recv().await {
            match out {
                Out::Line(l) => {
                    if wr.write_all(l.as_bytes()).await.is_err() {
                        break;
                    }
                }
                Out::Close => break,
            }
        }
        let _ = wr.shutdown().await;
    });

    let drop_with_error = |message: &str| {
        let _ = tx.send(Out::Line(line(&json!({ "Error": { "message": message } }))));
        let _ = tx.send(Out::Close);
    };

    let mut buf = Vec::new();
    let mut logged = false;
    loop {
        let message = match pending.take() {
            Some(m) => m,
            None => match read_line(&mut reader, &mut buf).await {
                Ok(Some(m)) => m,
                Ok(None) => break,
                Err(e) => {
                    debug!(%peer, error = %e, "Syncplay connection error");
                    break;
                }
            },
        };
        let Some(obj) = message.as_object() else {
            continue;
        };

        if !logged {
            if obj.contains_key("TLS") {
                let _ = tx.send(Out::Line(line(&json!({ "TLS": { "startTLS": "false" } }))));
                continue;
            }
            let Some(hello) = obj.get("Hello") else {
                drop_with_error("You must be known to server before sending this command");
                break;
            };
            let devices = shared.ext.devices.clone();
            let access = shared.state.lock().check_access(hello, devices.is_some());
            let device = match (access, &devices) {
                (Access::Admit, _) => None,
                (Access::Reject(msg), _) => {
                    debug!(%peer, error = %msg, "Syncplay Hello rejected");
                    drop_with_error(&msg);
                    break;
                }
                (Access::Challenge(mode), Some(store)) => {
                    let shake = Handshake {
                        tx: &tx,
                        store,
                        hello,
                        mode,
                        peer,
                        shared: &shared,
                    };
                    match shake.run(&mut reader, &mut buf).await {
                        Some(device) => device,
                        None => break,
                    }
                }
                (Access::Challenge(_), None) => {
                    drop_with_error(AUTH_FAILED);
                    break;
                }
            };
            let result =
                shared
                    .state
                    .lock()
                    .login(id, hello, tx.clone(), now_secs(), device.clone());
            match result {
                Ok(()) => {
                    logged = true;
                    if let (Some(fp), Some(store)) = (&device, &devices) {
                        let name = hello.get("username").and_then(Value::as_str).unwrap_or("");
                        store.seen(fp, name.trim());
                    }
                    debug!(%peer, tls, device = ?device, "Syncplay client logged in");
                    shared.dispatch();
                    (shared.notify)();
                    let rest: Map<String, Value> = obj
                        .iter()
                        .filter(|(k, _)| k.as_str() != "Hello")
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect();
                    if !rest.is_empty() {
                        let keep = shared.state.lock().handle_message(id, &rest, now_secs());
                        shared.dispatch();
                        if !keep {
                            break;
                        }
                    }
                }
                Err(msg) => {
                    debug!(%peer, error = %msg, "Syncplay Hello rejected");
                    drop_with_error(&msg);
                    break;
                }
            }
            continue;
        }

        if obj.contains_key("TLS") {
            let _ = tx.send(Out::Line(line(&json!({ "TLS": { "startTLS": "false" } }))));
        }
        let (keep, rooms_changed) = {
            let mut state = shared.state.lock();
            let before = state.rooms();
            let keep = state.handle_message(id, obj, now_secs());
            (keep, keep && state.rooms() != before)
        };
        shared.dispatch();
        if rooms_changed {
            (shared.notify)();
        }
        if !keep {
            break;
        }
    }

    let was_logged = shared.state.lock().is_logged(id);
    shared.state.lock().remove(id);
    shared.dispatch();
    if was_logged {
        (shared.notify)();
    }
    drop(tx);
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), writer).await;
}

/// The device challenge a YarmiplayTV client goes through before its Hello
/// is answered (`password` and `approved` access).
struct Handshake<'a> {
    tx: &'a UnboundedSender<Out>,
    store: &'a DeviceStore,
    hello: &'a Value,
    mode: SyncplayAccess,
    peer: SocketAddr,
    shared: &'a Shared,
}

impl Handshake<'_> {
    fn send(&self, value: Value) {
        let _ = self.tx.send(Out::Line(line(&value)));
    }

    fn status(&self, state: &str, fingerprint: &str) {
        self.send(
            json!({ "Yarmiplay": { "status": { "state": state, "fingerprint": fingerprint } } }),
        );
    }

    fn fail(&self, message: &str) {
        debug!(peer = %self.peer, error = message, "Syncplay device not admitted");
        self.send(json!({ "Error": { "message": message } }));
        let _ = self.tx.send(Out::Close);
    }

    /// `Some(device)` to log in (`None` inside: admitted by password without
    /// an approved key); `None` when the connection is done.
    async fn run<R: AsyncRead + Unpin>(
        &self,
        reader: &mut BufReader<R>,
        buf: &mut Vec<u8>,
    ) -> Option<Option<String>> {
        let server_id = self.store.server_id();
        let nonce = devices::new_nonce();
        self.send(json!({ "Yarmiplay": { "challenge": {
            "protocol": ext::PROTOCOL,
            "serverId": server_id,
            "nonce": nonce,
            "access": self.mode.as_str(),
        } } }));
        let auth = match tokio::time::timeout(AUTH_TIMEOUT, read_line(reader, buf)).await {
            Ok(Ok(Some(m))) => m.get("Yarmiplay").and_then(|y| y.get("auth")).cloned(),
            Ok(_) => return None,
            Err(_) => {
                self.fail("Device authentication timed out");
                return None;
            }
        };
        let Some(auth) = auth else {
            self.fail(AUTH_FAILED);
            return None;
        };
        let field = |k: &str| auth.get(k).and_then(Value::as_str).unwrap_or("");
        let Some(fp) = devices::verify(field("publicKey"), field("signature"), &server_id, &nonce)
        else {
            self.fail(AUTH_FAILED);
            return None;
        };
        if self.store.is_approved(&fp) {
            self.status("approved", &fp);
            return Some(Some(fp));
        }
        let request = auth
            .get("requestAccess")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if self.mode == SyncplayAccess::Password {
            match self.shared.state.lock().password_ok(self.hello) {
                Ok(()) => return Some(None),
                Err(e) if !request => {
                    self.fail(&e);
                    return None;
                }
                Err(_) => {}
            }
        } else if !request {
            self.status("required", &fp);
            self.fail("This device isn't approved on this server");
            return None;
        }

        let username = self
            .hello
            .get("username")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        let ip = self.peer.ip().to_canonical().to_string();
        let mut decision =
            match self
                .store
                .request(&fp, field("publicKey"), field("deviceName"), username, &ip)
            {
                Ok(rx) => rx,
                Err(e) => {
                    self.fail(&e);
                    return None;
                }
            };
        info!(fingerprint = %fp, "Syncplay device is waiting for approval");
        self.status("pending", &fp);
        (self.shared.notify)();

        let deadline = tokio::time::sleep(PENDING_LIMIT);
        tokio::pin!(deadline);
        let start = tokio::time::Instant::now() + PENDING_KEEPALIVE;
        let mut keepalive = tokio::time::interval_at(start, PENDING_KEEPALIVE);
        let outcome = loop {
            tokio::select! {
                changed = decision.changed() => match changed.map(|_| *decision.borrow_and_update()) {
                    Ok(Decision::Waiting) => {}
                    Ok(Decision::Approved) => {
                        self.status("approved", &fp);
                        break Some(Some(fp.clone()));
                    }
                    Ok(Decision::Denied) => {
                        self.status("denied", &fp);
                        self.fail("The host denied this device");
                        break None;
                    }
                    Err(_) => {
                        self.status("expired", &fp);
                        self.fail("The request is no longer waiting; try again");
                        break None;
                    }
                },
                _ = keepalive.tick() => self.status("pending", &fp),
                line = read_line(reader, buf) => match line {
                    Ok(Some(_)) => {}
                    _ => break None,
                },
                _ = &mut deadline => {
                    self.status("expired", &fp);
                    self.fail("Nobody approved this device in time");
                    break None;
                }
            }
        };
        self.store.disconnected(&fp);
        (self.shared.notify)();
        outcome
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;

    async fn read_msg<R: AsyncRead + Unpin>(r: &mut BufReader<R>) -> Value {
        let mut buf = Vec::new();
        tokio::time::timeout(std::time::Duration::from_secs(5), read_line(r, &mut buf))
            .await
            .expect("timeout")
            .unwrap()
            .expect("eof")
    }

    async fn read_until_key<R: AsyncRead + Unpin>(r: &mut BufReader<R>, key: &str) -> Value {
        loop {
            let m = read_msg(r).await;
            if m.get(key).is_some() {
                return m;
            }
        }
    }

    fn hello(name: &str) -> String {
        line(
            &json!({ "Hello": { "username": name, "room": { "name": "lobby" }, "version": "1.2.255", "realversion": "1.7.4" } }),
        )
    }

    #[tokio::test]
    async fn two_clients_over_tcp() {
        let server = SyncplayServer::start(
            0,
            SyncplayOptions::default(),
            Arc::default(),
            Arc::new(|| {}),
            Extensions::default(),
        )
        .await
        .unwrap();
        let port = server.port;

        let mut a = BufReader::new(TcpStream::connect(("127.0.0.1", port)).await.unwrap());
        a.get_mut()
            .write_all(hello("alice").as_bytes())
            .await
            .unwrap();
        assert_eq!(
            read_until_key(&mut a, "Hello").await["Hello"]["username"],
            "alice"
        );

        let mut b = BufReader::new(TcpStream::connect(("127.0.0.1", port)).await.unwrap());
        b.get_mut()
            .write_all(hello("bob").as_bytes())
            .await
            .unwrap();
        read_until_key(&mut b, "Hello").await;

        let joined = read_until_key(&mut a, "Set").await;
        assert!(joined["Set"]["user"]["bob"]["event"]["joined"]
            .as_bool()
            .unwrap());
        assert_eq!(server.user_count(), 2);

        b.get_mut()
            .write_all(line(&json!({ "Chat": "hi" })).as_bytes())
            .await
            .unwrap();
        let chat = read_until_key(&mut a, "Chat").await;
        assert_eq!(chat["Chat"]["username"], "bob");

        // Heartbeat State arrives within about a second.
        let st = read_until_key(&mut a, "State").await;
        assert!(st["State"]["ping"]["latencyCalculation"].as_f64().is_some());

        drop(b);
        let left = loop {
            let m = read_until_key(&mut a, "Set").await;
            if m["Set"]["user"]["bob"]["event"]["left"].as_bool() == Some(true) {
                break m;
            }
        };
        assert!(left.is_object());
        server.stop();
    }

    #[tokio::test]
    async fn starttls_refused_without_certificate_then_plain_works() {
        let server = SyncplayServer::start(
            0,
            SyncplayOptions::default(),
            Arc::default(),
            Arc::new(|| {}),
            Extensions::default(),
        )
        .await
        .unwrap();
        let mut a = BufReader::new(
            TcpStream::connect(("127.0.0.1", server.port))
                .await
                .unwrap(),
        );
        a.get_mut()
            .write_all(line(&json!({ "TLS": { "startTLS": "send" } })).as_bytes())
            .await
            .unwrap();
        assert_eq!(read_msg(&mut a).await["TLS"]["startTLS"], "false");
        a.get_mut()
            .write_all(hello("alice").as_bytes())
            .await
            .unwrap();
        read_until_key(&mut a, "Hello").await;
        server.stop();
    }

    #[tokio::test]
    async fn starttls_with_certificate() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let params = rcgen::CertificateParams::new(vec!["localhost".into()]).unwrap();
        let key = rcgen::KeyPair::generate().unwrap();
        let cert = params.self_signed(&key).unwrap();
        let config = crate::tls::server_config(&cert.pem(), &key.serialize_pem()).unwrap();
        let store: CertStore = Arc::new(parking_lot::RwLock::new(Some(Arc::new(
            crate::tls::CertBundle {
                host: "localhost".into(),
                cert_pem: cert.pem(),
                key_pem: key.serialize_pem(),
                not_before: 0,
                not_after: i64::MAX,
                staging: true,
                fingerprint: String::new(),
                config,
            },
        ))));
        let server = SyncplayServer::start(
            0,
            SyncplayOptions::default(),
            store,
            Arc::new(|| {}),
            Extensions::default(),
        )
        .await
        .unwrap();

        let mut tcp = TcpStream::connect(("127.0.0.1", server.port))
            .await
            .unwrap();
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
        roots.add(cert.der().clone()).unwrap();
        let client = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let connector = tokio_rustls::TlsConnector::from(Arc::new(client));
        let tls = connector
            .connect(
                rustls::pki_types::ServerName::try_from("localhost").unwrap(),
                tcp,
            )
            .await
            .expect("TLS handshake");
        let mut tls = BufReader::new(tls);
        tls.get_mut()
            .write_all(hello("secure").as_bytes())
            .await
            .unwrap();
        assert_eq!(
            read_until_key(&mut tls, "Hello").await["Hello"]["username"],
            "secure"
        );
        server.stop();
    }

    #[tokio::test]
    async fn non_hello_before_login_is_rejected() {
        let server = SyncplayServer::start(
            0,
            SyncplayOptions::default(),
            Arc::default(),
            Arc::new(|| {}),
            Extensions::default(),
        )
        .await
        .unwrap();
        let mut a = BufReader::new(
            TcpStream::connect(("127.0.0.1", server.port))
                .await
                .unwrap(),
        );
        a.get_mut()
            .write_all(line(&json!({ "List": null })).as_bytes())
            .await
            .unwrap();
        assert!(read_msg(&mut a).await["Error"]["message"]
            .as_str()
            .unwrap()
            .contains("known"));
        server.stop();
    }

    use crate::syncplay::devices::testkey::DeviceKey;
    use crate::syncplay::protocol::md5_hex;
    use crate::syncplay::room::APPROVED_ONLY;

    type Conn = BufReader<TcpStream>;

    async fn start_with(
        access: SyncplayAccess,
        vanilla_mode: bool,
        store: &Arc<DeviceStore>,
    ) -> SyncplayServer {
        let opts = SyncplayOptions {
            access,
            password: if access.uses_password() {
                "pw".into()
            } else {
                String::new()
            },
            vanilla_mode,
            ..SyncplayOptions::default()
        };
        let ext = Extensions {
            devices: Some(store.clone()),
            ..Extensions::default()
        };
        SyncplayServer::start(0, opts, Arc::default(), Arc::new(|| {}), ext)
            .await
            .unwrap()
    }

    async fn connect(server: &SyncplayServer) -> Conn {
        BufReader::new(
            TcpStream::connect(("127.0.0.1", server.port))
                .await
                .unwrap(),
        )
    }

    async fn send(c: &mut Conn, v: Value) {
        c.get_mut().write_all(line(&v).as_bytes()).await.unwrap();
    }

    /// The next message, or `None` once the server closed the connection.
    async fn next(c: &mut Conn) -> Option<Value> {
        let mut buf = Vec::new();
        tokio::time::timeout(Duration::from_secs(5), read_line(c, &mut buf))
            .await
            .expect("timeout")
            .ok()
            .flatten()
    }

    /// Every message until the connection closes.
    async fn rest(c: &mut Conn) -> Vec<Value> {
        let mut out = Vec::new();
        while let Some(m) = next(c).await {
            out.push(m);
        }
        out
    }

    fn hello_with(name: &str, ext: bool, password: Option<&str>) -> Value {
        let mut h = json!({ "username": name, "room": { "name": "lobby" }, "version": "1.2.255",
            "realversion": "1.7.4", "features": { "sharedPlaylists": true, "chat": true } });
        if ext {
            h["features"]["yarmiplay"] =
                json!({ "protocol": 1, "client": "YarmiplayTV", "version": "2.0.0" });
        }
        if let Some(p) = password {
            h["password"] = json!(md5_hex(p));
        }
        json!({ "Hello": h })
    }

    /// Answer the device challenge with `key`.
    async fn answer(c: &mut Conn, key: &DeviceKey, request_access: bool, access: &str) {
        let m = next(c).await.unwrap();
        let ch = &m["Yarmiplay"]["challenge"];
        assert_eq!(ch["protocol"], 1);
        assert_eq!(ch["access"], access);
        let sig = key.sign(
            ch["serverId"].as_str().unwrap(),
            ch["nonce"].as_str().unwrap(),
        );
        send(
            c,
            json!({ "Yarmiplay": { "auth": {
            "publicKey": key.public_key(), "signature": sig,
            "deviceName": "Test device", "requestAccess": request_access,
        } } }),
        )
        .await;
    }

    async fn expect_status(c: &mut Conn, state: &str, key: &DeviceKey) {
        let m = next(c).await.unwrap();
        assert_eq!(m["Yarmiplay"]["status"]["state"], state, "{m}");
        assert_eq!(m["Yarmiplay"]["status"]["fingerprint"], key.fingerprint());
    }

    fn approve(store: &DeviceStore, key: &DeviceKey) {
        store
            .request(
                &key.fingerprint(),
                &key.public_key(),
                "Phone",
                "ana",
                "10.0.0.9",
            )
            .unwrap();
        store.approve(&key.fingerprint()).unwrap();
    }

    fn error_of(msgs: &[Value]) -> String {
        msgs.iter()
            .find_map(|m| m["Error"]["message"].as_str())
            .unwrap_or_default()
            .to_string()
    }

    #[tokio::test]
    async fn approved_devices_skip_the_password() {
        let store = Arc::new(DeviceStore::in_memory());
        let key = DeviceKey::new();
        approve(&store, &key);
        let server = start_with(SyncplayAccess::Password, false, &store).await;

        let mut c = connect(&server).await;
        send(&mut c, hello_with("ana", true, None)).await;
        answer(&mut c, &key, false, "password").await;
        expect_status(&mut c, "approved", &key).await;
        let h = read_until_key(&mut c, "Hello").await;
        assert_eq!(h["Hello"]["features"]["yarmiplay"]["device"], "approved");
        assert_eq!(h["Hello"]["features"]["yarmiplay"]["access"], "password");
        assert_eq!(store.status().approved[0].last_username, "ana");

        // An unknown key still needs the password...
        let other = DeviceKey::new();
        let mut c = connect(&server).await;
        send(&mut c, hello_with("bo", true, None)).await;
        answer(&mut c, &other, false, "password").await;
        assert_eq!(error_of(&rest(&mut c).await), "Password required");
        // ...and with it gets in without an approved key.
        let mut c = connect(&server).await;
        send(&mut c, hello_with("cy", true, Some("pw"))).await;
        answer(&mut c, &other, false, "password").await;
        let h = read_until_key(&mut c, "Hello").await;
        assert_eq!(h["Hello"]["features"]["yarmiplay"]["device"], "none");
        // A wrong password with requestAccess waits for the host.
        let mut c = connect(&server).await;
        send(&mut c, hello_with("di", true, Some("nope"))).await;
        answer(&mut c, &other, true, "password").await;
        expect_status(&mut c, "pending", &other).await;
        server.stop();
    }

    #[tokio::test]
    async fn password_only_needs_the_password_from_approved_devices_too() {
        let store = Arc::new(DeviceStore::in_memory());
        let key = DeviceKey::new();
        approve(&store, &key);
        let server = start_with(SyncplayAccess::PasswordOnly, false, &store).await;

        let mut c = connect(&server).await;
        send(&mut c, hello_with("ana", true, None)).await;
        let out = rest(&mut c).await;
        assert!(out.iter().all(|m| m.get("Yarmiplay").is_none()), "no challenge");
        assert_eq!(error_of(&out), "Password required");

        let mut c = connect(&server).await;
        send(&mut c, hello_with("ana", true, Some("pw"))).await;
        let h = read_until_key(&mut c, "Hello").await;
        let marker = &h["Hello"]["features"]["yarmiplay"];
        assert_eq!(marker["access"], "passwordOnly");
        assert_eq!(marker["device"], "none");
        assert!(store.status().pending.is_empty());
        server.stop();
    }

    #[tokio::test]
    async fn pending_devices_get_in_once_approved() {
        let store = Arc::new(DeviceStore::in_memory());
        let server = start_with(SyncplayAccess::Approved, false, &store).await;
        let key = DeviceKey::new();
        let mut c = connect(&server).await;
        send(&mut c, hello_with("ana", true, None)).await;
        answer(&mut c, &key, true, "approved").await;
        expect_status(&mut c, "pending", &key).await;
        let pending = store.status().pending;
        assert_eq!(pending[0].fingerprint, key.fingerprint());
        assert_eq!(pending[0].name, "Test device");
        assert_eq!(pending[0].username, "ana");
        assert_eq!(pending[0].ip, "127.0.0.1");

        store.approve(&key.fingerprint()).unwrap();
        expect_status(&mut c, "approved", &key).await;
        let h = read_until_key(&mut c, "Hello").await;
        assert_eq!(h["Hello"]["features"]["yarmiplay"]["device"], "approved");
        assert_eq!(server.user_count(), 1);
        server.stop();
    }

    #[tokio::test]
    async fn denied_unrequested_and_forged_devices_are_refused() {
        let store = Arc::new(DeviceStore::in_memory());
        let server = start_with(SyncplayAccess::Approved, false, &store).await;
        let key = DeviceKey::new();

        let mut c = connect(&server).await;
        send(&mut c, hello_with("ana", true, None)).await;
        answer(&mut c, &key, true, "approved").await;
        expect_status(&mut c, "pending", &key).await;
        store.deny(&key.fingerprint()).unwrap();
        expect_status(&mut c, "denied", &key).await;
        assert!(error_of(&rest(&mut c).await).contains("denied"));

        let mut c = connect(&server).await;
        send(&mut c, hello_with("ana", true, None)).await;
        answer(&mut c, &key, false, "approved").await;
        expect_status(&mut c, "required", &key).await;
        assert!(error_of(&rest(&mut c).await).contains("isn't approved"));

        let mut c = connect(&server).await;
        send(&mut c, hello_with("ana", true, None)).await;
        next(&mut c).await.unwrap();
        let forged = key.sign("someone else's server", "nonce");
        send(&mut c, json!({ "Yarmiplay": { "auth": {
            "publicKey": key.public_key(), "signature": forged, "deviceName": "x", "requestAccess": true,
        } } }))
        .await;
        assert_eq!(error_of(&rest(&mut c).await), AUTH_FAILED);
        assert!(store.status().pending.is_empty());
        assert_eq!(server.user_count(), 0);
        server.stop();
    }

    #[tokio::test]
    async fn official_clients_and_vanilla_mode_see_an_official_server() {
        let store = Arc::new(DeviceStore::in_memory());
        let key = DeviceKey::new();
        approve(&store, &key);
        let no_ext = |msgs: &[Value]| {
            msgs.iter().all(|m| {
                m.get("Yarmiplay").is_none() && m["Hello"]["features"].get("yarmiplay").is_none()
            })
        };

        // Approved devices only: an official client is refused, readably.
        let server = start_with(SyncplayAccess::Approved, false, &store).await;
        let mut c = connect(&server).await;
        send(&mut c, hello_with("v", false, Some("pw"))).await;
        let out = rest(&mut c).await;
        assert_eq!(error_of(&out), APPROVED_ONLY);
        assert!(no_ext(&out));
        server.stop();

        // Password mode: an official client gets today's replies.
        let server = start_with(SyncplayAccess::Password, false, &store).await;
        let mut c = connect(&server).await;
        send(&mut c, hello_with("v", false, Some("pw"))).await;
        let mut out = Vec::new();
        loop {
            let m = next(&mut c).await.unwrap();
            let done = m.get("Hello").is_some();
            out.push(m);
            if done {
                break;
            }
        }
        assert!(no_ext(&out));
        server.stop();

        // Open mode: YarmiplayTV gets no challenge, only the marker.
        let server = start_with(SyncplayAccess::Open, false, &store).await;
        let mut c = connect(&server).await;
        send(&mut c, hello_with("y", true, None)).await;
        let first = next(&mut c).await.unwrap();
        assert!(first["Yarmiplay"].get("challenge").is_none());
        let h = read_until_key(&mut c, "Hello").await;
        assert_eq!(h["Hello"]["features"]["yarmiplay"]["access"], "open");
        server.stop();

        // Vanilla mode: no challenge, no marker, and approved keys don't skip the password.
        let server = start_with(SyncplayAccess::Password, true, &store).await;
        let mut c = connect(&server).await;
        send(&mut c, hello_with("y", true, None)).await;
        let out = rest(&mut c).await;
        assert_eq!(error_of(&out), "Password required");
        assert!(no_ext(&out));
        let mut c = connect(&server).await;
        send(&mut c, hello_with("y", true, Some("pw"))).await;
        let mut out = Vec::new();
        loop {
            let m = next(&mut c).await.unwrap();
            let done = m.get("Hello").is_some();
            out.push(m);
            if done {
                break;
            }
        }
        assert!(no_ext(&out));
        server.stop();
    }

    #[tokio::test]
    async fn removed_devices_are_disconnected() {
        let store = Arc::new(DeviceStore::in_memory());
        let key = DeviceKey::new();
        approve(&store, &key);
        let server = start_with(SyncplayAccess::Approved, false, &store).await;
        let mut c = connect(&server).await;
        send(&mut c, hello_with("ana", true, None)).await;
        answer(&mut c, &key, false, "approved").await;
        expect_status(&mut c, "approved", &key).await;
        read_until_key(&mut c, "Hello").await;

        store.remove(&key.fingerprint()).unwrap();
        server.kick_device(&key.fingerprint());
        let out = rest(&mut c).await;
        assert!(out
            .iter()
            .any(|m| m["Yarmiplay"]["status"]["state"] == "revoked"));
        assert!(error_of(&out).contains("removed"));
        assert_eq!(server.user_count(), 0);
        server.stop();
    }
}
