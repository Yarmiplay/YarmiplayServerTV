//! LAN discovery for YarmiplayTV: a UDP responder on the Syncplay port number.
//! A client at home broadcasts `{"yarmiplay":"discover","protocol":1}` and
//! learns this server's id, port, certificate name and home addresses, so it
//! can dial a LAN address and still verify the certificate by name when the
//! router doesn't loop the public address back in.

use super::room::ServerState;
use crate::net::ip::is_local_source;
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;
use tracing::{debug, warn};

/// Replies sent per second at most.
const MAX_REPLIES_PER_SECOND: u32 = 10;

/// The reply to one datagram, or `None` to ignore it.
pub fn reply(
    request: &[u8],
    src: IpAddr,
    st: &ServerState,
    server_id: &str,
    port: u16,
) -> Option<Vec<u8>> {
    if !is_local_source(src) || st.vanilla() {
        return None;
    }
    let req: Value = serde_json::from_slice(request).ok()?;
    if req.get("yarmiplay").and_then(Value::as_str) != Some("discover") {
        return None;
    }
    let body = json!({
        "yarmiplay": "server",
        "server": "YarmiplayServerTV",
        "version": env!("CARGO_PKG_VERSION"),
        "serverId": server_id,
        "port": port,
        "tls": st.tls_message(),
    });
    serde_json::to_vec(&body).ok()
}

struct RateLimit {
    window: Instant,
    sent: u32,
}

impl RateLimit {
    fn allow(&mut self, now: Instant) -> bool {
        if now.duration_since(self.window) >= Duration::from_secs(1) {
            self.window = now;
            self.sent = 0;
        }
        self.sent += 1;
        self.sent <= MAX_REPLIES_PER_SECOND
    }
}

/// Bind the responder; discovery is optional, so failure only logs.
pub async fn bind(port: u16) -> Option<UdpSocket> {
    match UdpSocket::bind(SocketAddr::from(([0, 0, 0, 0], port))).await {
        Ok(s) => Some(s),
        Err(e) => {
            warn!(error = %e, port, "LAN discovery unavailable");
            None
        }
    }
}

pub async fn run(socket: UdpSocket, state: Weak<Mutex<ServerState>>, server_id: String, port: u16) {
    let mut buf = [0u8; 512];
    let mut limit = RateLimit {
        window: Instant::now(),
        sent: 0,
    };
    loop {
        let Ok((n, src)) = socket.recv_from(&mut buf).await else {
            continue;
        };
        let Some(state) = state.upgrade() else {
            return;
        };
        let answer = reply(&buf[..n], src.ip(), &state.lock(), &server_id, port);
        drop(state);
        if let Some(answer) = answer {
            if limit.allow(Instant::now()) {
                debug!(%src, "LAN discovery reply");
                let _ = socket.send_to(&answer, src).await;
            }
        }
    }
}

/// Start the responder on `port`, for as long as the returned task runs.
pub async fn start(
    port: u16,
    state: &Arc<Mutex<ServerState>>,
    server_id: String,
) -> Option<tokio::task::JoinHandle<()>> {
    let socket = bind(port).await?;
    Some(tokio::spawn(run(
        socket,
        Arc::downgrade(state),
        server_id,
        port,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syncplay::ext::TlsInfo;
    use crate::syncplay::SyncplayOptions;

    fn state(vanilla: bool) -> ServerState {
        let mut st = ServerState::new(SyncplayOptions {
            vanilla_mode: vanilla,
            ..Default::default()
        });
        st.set_tls(Some(TlsInfo {
            name: "home.duckdns.org".into(),
            lan: vec!["192.168.1.20:8999".into()],
        }));
        st
    }

    const REQ: &[u8] = br#"{"yarmiplay":"discover","protocol":1}"#;

    #[test]
    fn answers_home_devices_only() {
        let st = state(false);
        let lan: IpAddr = "192.168.1.30".parse().unwrap();
        let out = reply(REQ, lan, &st, "abc", 8999).unwrap();
        let v: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["yarmiplay"], "server");
        assert_eq!(v["serverId"], "abc");
        assert_eq!(v["port"], 8999);
        assert_eq!(v["tls"]["name"], "home.duckdns.org");
        assert_eq!(v["tls"]["lan"][0], "192.168.1.20:8999");

        assert!(reply(REQ, "8.8.8.8".parse().unwrap(), &st, "abc", 8999).is_none());
        assert!(reply(b"junk", lan, &st, "abc", 8999).is_none());
        assert!(reply(br#"{"yarmiplay":"other"}"#, lan, &st, "abc", 8999).is_none());
        assert!(
            reply(REQ, lan, &state(true), "abc", 8999).is_none(),
            "off in vanilla mode"
        );
    }

    #[test]
    fn rate_limited() {
        let start = Instant::now();
        let mut l = RateLimit {
            window: start,
            sent: 0,
        };
        assert!((0..MAX_REPLIES_PER_SECOND).all(|_| l.allow(start)));
        assert!(!l.allow(start));
        assert!(l.allow(start + Duration::from_secs(1)));
    }

    #[tokio::test]
    async fn responds_over_udp() {
        let st = Arc::new(Mutex::new(state(false)));
        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let port = socket.local_addr().unwrap().port();
        let task = tokio::spawn(run(socket, Arc::downgrade(&st), "abc".into(), port));
        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        client.send_to(REQ, ("127.0.0.1", port)).await.unwrap();
        let mut buf = [0u8; 512];
        let (n, _) = tokio::time::timeout(Duration::from_secs(5), client.recv_from(&mut buf))
            .await
            .unwrap()
            .unwrap();
        let v: Value = serde_json::from_slice(&buf[..n]).unwrap();
        assert_eq!(v["port"], port);
        task.abort();
    }
}
