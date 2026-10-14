//! Jellyfin's HTTPS port, served by the app: TLS with the same certificate
//! and chain the Syncplay port sends (Let's Encrypt's full chain as issued,
//! cross-signed root included), then a reverse proxy to Jellyfin's plain
//! HTTP port. Jellyfin's own HTTPS (Kestrel) builds its chain from the
//! host's trust store and stops at the root the host trusts, leaving out the
//! cross-signed certificate older devices need. A renewed certificate is
//! used from the next connection on, without restarting Jellyfin.

use crate::net::proxy::LocalProxy;
use crate::syncplay::mux::{ConnMeta, HttpSender, MuxListener};
use crate::tls::CertStore;
use axum::body::Body;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::Response;
use axum::Router;
use parking_lot::Mutex;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

pub struct HttpsFront {
    store: CertStore,
    proxy: Arc<LocalProxy>,
    /// (port, accept loop, HTTP server).
    running: Mutex<Option<(u16, JoinHandle<()>, JoinHandle<()>)>>,
    error: Mutex<Option<String>>,
}

impl HttpsFront {
    pub fn new(store: CertStore) -> Self {
        Self {
            store,
            proxy: Arc::new(LocalProxy::new("Jellyfin")),
            running: Mutex::new(None),
            error: Mutex::new(None),
        }
    }

    /// Serve `https_port`, forwarding to Jellyfin on `http_port`; `None` stops.
    pub fn apply(&self, want: Option<(u16, u16)>) {
        let Some((https_port, http_port)) = want else {
            self.proxy.set_target(None);
            if let Some((_, accept, http)) = self.running.lock().take() {
                accept.abort();
                http.abort();
                info!("Jellyfin HTTPS stopped");
            }
            *self.error.lock() = None;
            return;
        };
        self.proxy.set_target(Some(http_port));
        let mut running = self.running.lock();
        if running.as_ref().is_some_and(|(p, _, _)| *p == https_port) {
            return;
        }
        if let Some((_, accept, http)) = running.take() {
            accept.abort();
            http.abort();
        }
        let listener = match crate::syncplay::server::bind_dual_stack(https_port) {
            Ok(l) => l,
            Err(e) => {
                warn!(port = https_port, error = %e, "Jellyfin HTTPS port");
                *self.error.lock() = Some(format!("Couldn't listen on the Jellyfin HTTPS port {https_port}: {e}"));
                return;
            }
        };
        *self.error.lock() = None;
        let local = SocketAddr::from(([0, 0, 0, 0], https_port));
        let (tx, http) = serve(self.proxy.clone(), local);
        let store = self.store.clone();
        let accept = tokio::spawn(accept_loop(listener, store, tx));
        info!(port = https_port, "Jellyfin HTTPS listening");
        *running = Some((https_port, accept, http));
    }

    /// Whether the port is being served.
    pub fn active(&self) -> bool {
        self.running.lock().is_some()
    }

    pub fn error(&self) -> Option<String> {
        self.error.lock().clone()
    }
}

impl Drop for HttpsFront {
    fn drop(&mut self) {
        if let Some((_, accept, http)) = self.running.lock().take() {
            accept.abort();
            http.abort();
        }
    }
}

/// The HTTP side: every request goes to Jellyfin.
fn serve(proxy: Arc<LocalProxy>, local: SocketAddr) -> (HttpSender, JoinHandle<()>) {
    async fn forward(
        State(proxy): State<Arc<LocalProxy>>,
        ConnectInfo(meta): ConnectInfo<ConnMeta>,
        req: Request,
    ) -> Response<Body> {
        proxy.forward(req, meta.peer.ip(), true).await
    }
    let (tx, listener) = MuxListener::new(local);
    let app = Router::new()
        .fallback(forward)
        .layer(axum::extract::DefaultBodyLimit::disable())
        .with_state(proxy)
        .into_make_service_with_connect_info::<ConnMeta>();
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (tx, task)
}

async fn accept_loop(listener: TcpListener, store: CertStore, tx: HttpSender) {
    loop {
        let Ok((stream, peer)) = listener.accept().await else { continue };
        let Some(bundle) = store.read().clone() else {
            // No certificate (yet): nothing to serve.
            continue;
        };
        let tx = tx.clone();
        tokio::spawn(async move {
            let mut config = (*bundle.config).clone();
            config.alpn_protocols = vec![b"http/1.1".to_vec()];
            let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
            match tokio::time::timeout(Duration::from_secs(15), acceptor.accept(stream)).await {
                Ok(Ok(tls)) => {
                    let _ = tx.send((Box::new(tls), ConnMeta { peer, tls: true })).await;
                }
                Ok(Err(e)) => debug!(%peer, error = %e, "Jellyfin HTTPS handshake failed"),
                Err(_) => debug!(%peer, "Jellyfin HTTPS handshake timed out"),
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustls::pki_types::{CertificateDer, ServerName};

    /// The chain on the wire is the PEM chain exactly, cross-signed root
    /// included; requests reach Jellyfin's HTTP port.
    #[tokio::test]
    async fn sends_the_whole_chain_and_forwards() {
        crate::net::install_crypto_provider();
        // A root, an intermediate it signed, and a leaf the intermediate signed;
        // plus a "cross-signed" copy of the intermediate under another root.
        let ca_key = rcgen::KeyPair::generate().unwrap();
        let mut ca = rcgen::CertificateParams::new(vec![]).unwrap();
        ca.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        ca.distinguished_name.push(rcgen::DnType::CommonName, "Test Root");
        let ca_cert = ca.self_signed(&ca_key).unwrap();
        let ca_issuer = rcgen::Issuer::new(ca, ca_key);
        let leaf_key = rcgen::KeyPair::generate().unwrap();
        let leaf = rcgen::CertificateParams::new(vec!["jellyfin.test".into()])
            .unwrap()
            .signed_by(&leaf_key, &ca_issuer)
            .unwrap();
        let cross_key = rcgen::KeyPair::generate().unwrap();
        let mut cross = rcgen::CertificateParams::new(vec![]).unwrap();
        cross.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        cross.distinguished_name.push(rcgen::DnType::CommonName, "Cross Signed");
        let cross = cross.self_signed(&cross_key).unwrap();
        let chain_pem = format!("{}{}{}", leaf.pem(), ca_cert.pem(), cross.pem());
        let config = crate::tls::server_config(&chain_pem, &leaf_key.serialize_pem()).unwrap();
        let store: CertStore = Arc::default();
        *store.write() = Some(Arc::new(crate::tls::CertBundle {
            host: "jellyfin.test".into(),
            names: vec!["jellyfin.test".into()],
            cert_pem: chain_pem.clone(),
            key_pem: leaf_key.serialize_pem(),
            not_before: 0,
            not_after: i64::MAX,
            staging: false,
            fingerprint: "x".into(),
            config,
        }));

        // A stand-in for Jellyfin's HTTP port.
        let jf = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let jf_port = jf.local_addr().unwrap().port();
        tokio::spawn(async move {
            let app = Router::new().fallback(|| async { "jellyfin says hi" });
            let _ = axum::serve(jf, app).await;
        });

        let https_port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let front = HttpsFront::new(store);
        front.apply(Some((https_port, jf_port)));
        assert!(front.active(), "{:?}", front.error());

        // Connect, trusting only the test root, and look at what was sent.
        let mut roots = rustls::RootCertStore::empty();
        roots.add(ca_cert.der().clone()).unwrap();
        let client = rustls::ClientConfig::builder().with_root_certificates(roots).with_no_client_auth();
        let connector = tokio_rustls::TlsConnector::from(Arc::new(client));
        let tcp = tokio::net::TcpStream::connect(("127.0.0.1", https_port)).await.unwrap();
        let mut tls = connector.connect(ServerName::try_from("jellyfin.test").unwrap(), tcp).await.unwrap();
        let sent: Vec<CertificateDer> = tls.get_ref().1.peer_certificates().unwrap().to_vec();
        let expected: Vec<CertificateDer> = rustls_pemfile::certs(&mut chain_pem.as_bytes()).collect::<Result<_, _>>().unwrap();
        assert_eq!(sent, expected, "the whole chain, in order");

        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        tls.write_all(b"GET / HTTP/1.1\r\nHost: jellyfin.test\r\nConnection: close\r\n\r\n").await.unwrap();
        let mut body = String::new();
        tls.read_to_string(&mut body).await.unwrap();
        assert!(body.contains("jellyfin says hi"), "{body}");

        front.apply(None);
        assert!(!front.active());
    }
}
