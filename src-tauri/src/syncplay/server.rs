//! TCP listener for the Syncplay server, with optional STARTTLS using the
//! current certificate from the [`CertStore`] (read per connection, so a
//! renewed certificate applies to new connections without a restart).

use super::protocol::{line, now_secs, MAX_LINE_LENGTH};
use super::room::{ConnId, Out, RoomInfo, ServerState, SyncplayOptions};
use crate::tls::CertStore;
use parking_lot::Mutex;
use serde_json::{json, Map, Value};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc::unbounded_channel;
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

pub type ChangeNotify = Arc<dyn Fn() + Send + Sync>;

pub struct SyncplayServer {
    pub port: u16,
    state: Arc<Mutex<ServerState>>,
    accept: JoinHandle<()>,
    heartbeat: JoinHandle<()>,
}

struct Shared {
    state: Arc<Mutex<ServerState>>,
    tls: CertStore,
    next_id: AtomicU64,
    notify: ChangeNotify,
}

impl SyncplayServer {
    pub async fn start(port: u16, opts: SyncplayOptions, tls: CertStore, notify: ChangeNotify) -> std::io::Result<Self> {
        let listener = TcpListener::bind(SocketAddr::from(([0, 0, 0, 0], port))).await?;
        let port = listener.local_addr()?.port();
        let state = Arc::new(Mutex::new(ServerState::new(opts)));
        let shared = Arc::new(Shared { state: state.clone(), tls, next_id: AtomicU64::new(1), notify: notify.clone() });

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
            let state = state.clone();
            tokio::spawn(async move {
                let mut tick = tokio::time::interval(std::time::Duration::from_secs(1));
                loop {
                    tick.tick().await;
                    let dropped = state.lock().tick(now_secs());
                    if !dropped.is_empty() {
                        notify();
                    }
                }
            })
        };

        info!(port, "Syncplay server listening");
        Ok(Self { port, state, accept, heartbeat })
    }

    pub fn set_options(&self, opts: SyncplayOptions) {
        self.state.lock().set_options(opts);
    }

    pub fn user_count(&self) -> usize {
        self.state.lock().user_count()
    }

    pub fn rooms(&self) -> Vec<RoomInfo> {
        self.state.lock().rooms()
    }

    pub fn stop(self) {
        self.accept.abort();
        self.heartbeat.abort();
        self.state.lock().close_all();
        info!(port = self.port, "Syncplay server stopped");
    }
}

impl Drop for SyncplayServer {
    fn drop(&mut self) {
        self.accept.abort();
        self.heartbeat.abort();
    }
}

/// Read one `\n`-terminated line (cancel-safe accumulation into `buf`).
async fn read_line<R: AsyncRead + Unpin>(reader: &mut BufReader<R>, buf: &mut Vec<u8>) -> std::io::Result<Option<Value>> {
    loop {
        buf.clear();
        let n = reader.read_until(b'\n', buf).await?;
        if n == 0 {
            return Ok(None);
        }
        if buf.len() > MAX_LINE_LENGTH {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "line too long"));
        }
        let text = String::from_utf8_lossy(buf);
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        return match serde_json::from_str::<Value>(text) {
            Ok(v) if v.is_object() => Ok(Some(v)),
            _ => Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "not a JSON object")),
        };
    }
}

async fn serve_conn(stream: TcpStream, peer: SocketAddr, shared: Arc<Shared>) {
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
            if reader.get_mut().write_all(line(&json!({ "TLS": { "startTLS": "false" } })).as_bytes()).await.is_err() {
                return;
            }
            return run_session(reader, None, id, peer, false, shared).await;
        };
        if reader.get_mut().write_all(line(&json!({ "TLS": { "startTLS": "true" } })).as_bytes()).await.is_err() {
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
        let Some(obj) = message.as_object() else { continue };

        if !logged {
            if obj.contains_key("TLS") {
                let _ = tx.send(Out::Line(line(&json!({ "TLS": { "startTLS": "false" } }))));
                continue;
            }
            let Some(hello) = obj.get("Hello") else {
                drop_with_error("You must be known to server before sending this command");
                break;
            };
            let result = shared.state.lock().handle_hello(id, hello, tx.clone(), now_secs());
            match result {
                Ok(()) => {
                    logged = true;
                    debug!(%peer, tls, "Syncplay client logged in");
                    (shared.notify)();
                    let rest: Map<String, Value> = obj.iter().filter(|(k, _)| k.as_str() != "Hello").map(|(k, v)| (k.clone(), v.clone())).collect();
                    if !rest.is_empty() && !shared.state.lock().handle_message(id, &rest, now_secs()) {
                        break;
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
        if rooms_changed {
            (shared.notify)();
        }
        if !keep {
            break;
        }
    }

    let was_logged = shared.state.lock().is_logged(id);
    shared.state.lock().remove(id);
    if was_logged {
        (shared.notify)();
    }
    drop(tx);
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), writer).await;
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
        line(&json!({ "Hello": { "username": name, "room": { "name": "lobby" }, "version": "1.2.255", "realversion": "1.7.4" } }))
    }

    #[tokio::test]
    async fn two_clients_over_tcp() {
        let server = SyncplayServer::start(0, SyncplayOptions::default(), Arc::default(), Arc::new(|| {})).await.unwrap();
        let port = server.port;

        let mut a = BufReader::new(TcpStream::connect(("127.0.0.1", port)).await.unwrap());
        a.get_mut().write_all(hello("alice").as_bytes()).await.unwrap();
        assert_eq!(read_until_key(&mut a, "Hello").await["Hello"]["username"], "alice");

        let mut b = BufReader::new(TcpStream::connect(("127.0.0.1", port)).await.unwrap());
        b.get_mut().write_all(hello("bob").as_bytes()).await.unwrap();
        read_until_key(&mut b, "Hello").await;

        let joined = read_until_key(&mut a, "Set").await;
        assert!(joined["Set"]["user"]["bob"]["event"]["joined"].as_bool().unwrap());
        assert_eq!(server.user_count(), 2);

        b.get_mut().write_all(line(&json!({ "Chat": "hi" })).as_bytes()).await.unwrap();
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
        let server = SyncplayServer::start(0, SyncplayOptions::default(), Arc::default(), Arc::new(|| {})).await.unwrap();
        let mut a = BufReader::new(TcpStream::connect(("127.0.0.1", server.port)).await.unwrap());
        a.get_mut().write_all(line(&json!({ "TLS": { "startTLS": "send" } })).as_bytes()).await.unwrap();
        assert_eq!(read_msg(&mut a).await["TLS"]["startTLS"], "false");
        a.get_mut().write_all(hello("alice").as_bytes()).await.unwrap();
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
        let store: CertStore = Arc::new(parking_lot::RwLock::new(Some(Arc::new(crate::tls::CertBundle {
            host: "localhost".into(),
            cert_pem: cert.pem(),
            key_pem: key.serialize_pem(),
            not_before: 0,
            not_after: i64::MAX,
            staging: true,
            fingerprint: String::new(),
            config,
        }))));
        let server = SyncplayServer::start(0, SyncplayOptions::default(), store, Arc::new(|| {})).await.unwrap();

        let mut tcp = TcpStream::connect(("127.0.0.1", server.port)).await.unwrap();
        tcp.write_all(line(&json!({ "TLS": { "startTLS": "send" } })).as_bytes()).await.unwrap();
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
        let client = rustls::ClientConfig::builder().with_root_certificates(roots).with_no_client_auth();
        let connector = tokio_rustls::TlsConnector::from(Arc::new(client));
        let tls = connector
            .connect(rustls::pki_types::ServerName::try_from("localhost").unwrap(), tcp)
            .await
            .expect("TLS handshake");
        let mut tls = BufReader::new(tls);
        tls.get_mut().write_all(hello("secure").as_bytes()).await.unwrap();
        assert_eq!(read_until_key(&mut tls, "Hello").await["Hello"]["username"], "secure");
        server.stop();
    }

    #[tokio::test]
    async fn non_hello_before_login_is_rejected() {
        let server = SyncplayServer::start(0, SyncplayOptions::default(), Arc::default(), Arc::new(|| {})).await.unwrap();
        let mut a = BufReader::new(TcpStream::connect(("127.0.0.1", server.port)).await.unwrap());
        a.get_mut().write_all(line(&json!({ "List": null })).as_bytes()).await.unwrap();
        assert!(read_msg(&mut a).await["Error"]["message"].as_str().unwrap().contains("known"));
        server.stop();
    }
}
