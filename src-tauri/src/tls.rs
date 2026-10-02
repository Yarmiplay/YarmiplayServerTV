//! Certificate lifecycle for the DuckDNS name: load the cached certificate,
//! issue or renew through ACME, keep the DuckDNS address current, and publish
//! the active certificate in a [`CertStore`]. The Syncplay server reads the
//! store on every STARTTLS, so renewed certificates are picked up without a
//! restart; Jellyfin gets a PFX and a restart from the orchestrator.

use crate::net::acme::{self, AcmeOptions, DuckDns, IssuedCert};
use parking_lot::{Mutex, RwLock};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{info, warn};

pub struct CertBundle {
    pub host: String,
    pub cert_pem: String,
    pub key_pem: String,
    pub not_before: i64,
    pub not_after: i64,
    pub staging: bool,
    /// SHA-256 of the certificate chain PEM.
    pub fingerprint: String,
    pub config: Arc<rustls::ServerConfig>,
}

pub type CertStore = Arc<RwLock<Option<Arc<CertBundle>>>>;

pub fn server_config(cert_pem: &str, key_pem: &str) -> Result<Arc<rustls::ServerConfig>, String> {
    let certs = rustls_pemfile::certs(&mut cert_pem.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("certificate PEM: {e}"))?;
    if certs.is_empty() {
        return Err("no certificate in PEM".into());
    }
    let key = rustls_pemfile::private_key(&mut key_pem.as_bytes())
        .map_err(|e| format!("private key PEM: {e}"))?
        .ok_or("no private key in PEM")?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| format!("TLS protocols: {e}"))?
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| format!("TLS certificate: {e}"))?;
    Ok(Arc::new(config))
}

fn bundle(cert: &IssuedCert, staging: bool) -> Result<CertBundle, String> {
    Ok(CertBundle {
        host: cert.host.clone(),
        cert_pem: cert.cert_pem.clone(),
        key_pem: cert.key_pem.clone(),
        not_before: cert.not_before,
        not_after: cert.not_after,
        staging,
        fingerprint: hex::encode(Sha256::digest(cert.cert_pem.as_bytes())),
        config: server_config(&cert.cert_pem, &cert.key_pem)?,
    })
}

#[derive(Debug, Clone, PartialEq)]
pub struct TlsRequest {
    pub duckdns: DuckDns,
    pub staging: bool,
    pub email: Option<String>,
}

#[derive(Debug, Clone, Serialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TlsStatus {
    /// `off`, `incomplete` (missing domain/token), `issuing`, `ready` or `error`.
    pub phase: &'static str,
    pub host: Option<String>,
    pub not_after: Option<i64>,
    pub renew_at: Option<i64>,
    pub staging: bool,
    pub error: Option<String>,
    /// Address DuckDNS has on record for the name.
    pub duckdns_ip: Option<String>,
}

pub type Notify = Arc<dyn Fn() + Send + Sync>;
pub type IpSource = Arc<dyn Fn() -> Option<IpAddr> + Send + Sync>;

struct Running {
    request: TlsRequest,
    task: JoinHandle<()>,
    renew: mpsc::UnboundedSender<()>,
}

pub struct TlsManager {
    acme_dir: PathBuf,
    store: CertStore,
    status: RwLock<TlsStatus>,
    running: Mutex<Option<Running>>,
}

impl TlsManager {
    pub fn new(acme_dir: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            acme_dir,
            store: Arc::default(),
            status: RwLock::new(TlsStatus {
                phase: "off",
                ..Default::default()
            }),
            running: Mutex::new(None),
        })
    }

    pub fn store(&self) -> CertStore {
        self.store.clone()
    }

    pub fn current(&self) -> Option<Arc<CertBundle>> {
        self.store.read().clone()
    }

    pub fn status(&self) -> TlsStatus {
        self.status.read().clone()
    }

    /// Start, restart or stop the certificate task so it matches `request`.
    /// `incomplete` marks TLS as switched on but missing a domain or token.
    pub fn apply(
        self: &Arc<Self>,
        request: Option<TlsRequest>,
        incomplete: Option<String>,
        ip: IpSource,
        notify: Notify,
    ) {
        let mut running = self.running.lock();
        if let (Some(req), Some(r)) = (&request, running.as_ref()) {
            if &r.request == req {
                return;
            }
        }
        if let Some(r) = running.take() {
            r.task.abort();
        }
        let Some(request) = request else {
            let had_cert = self.store.write().take().is_some();
            let status = match incomplete {
                Some(msg) => TlsStatus {
                    phase: "incomplete",
                    error: Some(msg),
                    ..Default::default()
                },
                None => TlsStatus {
                    phase: "off",
                    ..Default::default()
                },
            };
            let changed = *self.status.read() != status;
            *self.status.write() = status;
            if had_cert || changed {
                drop(running);
                notify();
            }
            return;
        };
        // A different name or environment invalidates the published certificate.
        let keep = self
            .store
            .read()
            .as_ref()
            .is_some_and(|b| b.host == request.duckdns.fqdn() && b.staging == request.staging);
        if !keep {
            self.store.write().take();
        }
        *self.status.write() = TlsStatus {
            phase: "issuing",
            host: Some(request.duckdns.fqdn()),
            staging: request.staging,
            ..Default::default()
        };
        // Publish a still-valid cached certificate right away so services
        // started in the same reconcile already use it.
        if self.store.read().is_none() {
            if let Some(cached) = acme::load_cached(&self.options(&request)) {
                if acme::unix_now() < cached.not_after
                    && self.install(&cached, request.staging).is_ok()
                {
                    self.set_status(|s| s.phase = "ready");
                }
            }
        }
        let (tx, rx) = mpsc::unbounded_channel();
        let me = self.clone();
        let req = request.clone();
        let task = tokio::spawn(async move { me.run(req, rx, ip, notify).await });
        *running = Some(Running {
            request,
            task,
            renew: tx,
        });
    }

    pub fn renew_now(&self) -> Result<(), String> {
        match self.running.lock().as_ref() {
            Some(r) => r
                .renew
                .send(())
                .map_err(|_| "certificate task stopped".into()),
            None => Err("TLS is not enabled".into()),
        }
    }

    fn set_status(&self, f: impl FnOnce(&mut TlsStatus)) {
        let mut s = self.status.write();
        f(&mut s);
        if let Some(b) = self.store.read().as_ref() {
            s.host = Some(b.host.clone());
            s.not_after = Some(b.not_after);
            s.renew_at = Some(b.not_before + (b.not_after - b.not_before) / 2);
            s.staging = b.staging;
        }
    }

    fn install(&self, cert: &IssuedCert, staging: bool) -> Result<bool, String> {
        let next = bundle(cert, staging)?;
        let changed = self
            .store
            .read()
            .as_ref()
            .map(|b| b.fingerprint != next.fingerprint)
            .unwrap_or(true);
        if changed {
            *self.store.write() = Some(Arc::new(next));
        }
        Ok(changed)
    }

    fn options(&self, req: &TlsRequest) -> AcmeOptions {
        AcmeOptions {
            dir: self.acme_dir.clone(),
            staging: req.staging,
            email: req.email.clone(),
            duckdns: req.duckdns.clone(),
            duckdns_api: None,
            directory: None,
        }
    }

    async fn run(
        self: Arc<Self>,
        req: TlsRequest,
        mut renew: mpsc::UnboundedReceiver<()>,
        ip: IpSource,
        notify: Notify,
    ) {
        let opts = self.options(&req);
        let host = req.duckdns.fqdn();
        if let Some(b) = self.current() {
            info!(%host, not_after = b.not_after, "using cached certificate");
        }
        notify();

        self.refresh_ip(&opts, &ip).await;
        notify();

        let mut failed = false;
        let mut force = false;
        let mut retry_at: i64 = 0;
        let mut ip_tick = tokio::time::interval(acme::DUCKDNS_IP_REFRESH);
        ip_tick.tick().await;
        loop {
            let now = acme::unix_now();
            let due = force
                || (now >= retry_at
                    && self.current().map_or(true, |b| {
                        acme::needs_renewal(b.not_before, b.not_after, now)
                    }));
            if due {
                self.set_status(|s| {
                    s.phase = "issuing";
                    s.error = None;
                });
                notify();
                match acme::load_or_issue(&opts, force).await {
                    Ok(cert) => match self.install(&cert, req.staging) {
                        Ok(changed) => {
                            if changed {
                                info!(%host, not_after = cert.not_after, "certificate active");
                            }
                            failed = false;
                            self.set_status(|s| {
                                s.phase = "ready";
                                s.error = None;
                            });
                        }
                        Err(e) => {
                            warn!(error = %e, "certificate could not be loaded");
                            failed = true;
                            self.set_status(|s| {
                                s.phase = "error";
                                s.error = Some(e);
                            });
                        }
                    },
                    Err(e) => {
                        warn!(error = %e, "certificate issuance failed");
                        failed = true;
                        let has_cert = self.current().is_some();
                        self.set_status(|s| {
                            s.phase = if has_cert { "ready" } else { "error" };
                            s.error = Some(e);
                        });
                    }
                }
                force = false;
                retry_at = if failed {
                    acme::unix_now() + acme::RETRY_AFTER_FAILURE.as_secs() as i64
                } else {
                    0
                };
                notify();
            }

            let delay = match self.current() {
                Some(b) => acme::next_check_delay(
                    &IssuedCert {
                        host: String::new(),
                        cert_pem: String::new(),
                        key_pem: String::new(),
                        not_before: b.not_before,
                        not_after: b.not_after,
                    },
                    acme::unix_now(),
                    failed,
                ),
                None => acme::RETRY_AFTER_FAILURE,
            };
            tokio::select! {
                _ = tokio::time::sleep(delay) => {}
                _ = ip_tick.tick() => {
                    self.refresh_ip(&opts, &ip).await;
                    notify();
                }
                msg = renew.recv() => {
                    if msg.is_none() { return; }
                    force = true;
                }
            }
        }
    }

    async fn refresh_ip(&self, opts: &AcmeOptions, ip: &IpSource) {
        match acme::duckdns_set_ip(opts.duckdns_api(), &opts.duckdns, ip()).await {
            Ok(recorded) => self.set_status(|s| s.duckdns_ip = recorded.map(|i| i.to_string())),
            Err(e) => {
                warn!(error = %e, "DuckDNS address update failed");
                self.set_status(|s| {
                    if s.phase != "issuing" {
                        s.error = Some(e);
                    }
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_config_from_self_signed() {
        let params = rcgen::CertificateParams::new(vec!["example.duckdns.org".into()]).unwrap();
        let key = rcgen::KeyPair::generate().unwrap();
        let cert = params.self_signed(&key).unwrap();
        assert!(server_config(&cert.pem(), &key.serialize_pem()).is_ok());
        assert!(server_config("", &key.serialize_pem()).is_err());
    }
}
