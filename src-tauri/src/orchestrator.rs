//! Owns every service and keeps them matching the saved settings. All
//! changes go through [`App::reconcile`], serialized by an async mutex.

use crate::config::Settings;
use crate::jellyfin::api::JellyfinApi;
use crate::jellyfin::proxy::JellyfinProxy;
use crate::jellyfin::{JellyfinDesired, JellyfinManager, JellyfinStatus};
use crate::net::acme::DuckDns;
use crate::net::upnp::{UpnpManager, UpnpStatus};
use crate::paths::AppPaths;
use crate::relay::{Relay, RelayStatus};
use crate::secrets::{self, Secrets};
use crate::syncplay::devices::{DeviceStore, DevicesStatus};
use crate::syncplay::ext::JellyfinShare;
use crate::syncplay::{AuthorizeFn, Extensions, RoomInfo, SyncplayOptions, SyncplayServer};
use crate::tls::{IpSource, Notify, TlsManager, TlsRequest, TlsStatus};
use crate::updates::{UpdateStatus, Updates};
use parking_lot::{Mutex, RwLock};
use serde::Serialize;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{info, warn};

const UPNP_RECHECK: Duration = Duration::from_secs(10 * 60);
/// After a failed guest-account update, wait this long before trying again.
const SHARE_RETRY: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SyncplayStatus {
    pub running: bool,
    pub port: Option<u16>,
    pub error: Option<String>,
    pub users: usize,
    pub rooms: Vec<RoomInfo>,
    pub tls: bool,
    pub relay: RelayStatus,
    pub devices: DevicesStatus,
}

/// Jellyfin sharing with Syncplay users, as it actually is right now.
#[derive(Debug, Clone, Serialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ShareStatus {
    /// Offered to Syncplay users (sharing on, Jellyfin running, signed in, guest ready).
    pub active: bool,
    pub error: Option<String>,
}

#[derive(Default)]
struct ShareState {
    /// The guest account's enabled flag as last set in Jellyfin.
    applied: Option<bool>,
    failed_at: Option<Instant>,
    error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Addresses {
    pub lan_ip: Option<String>,
    /// DuckDNS name when TLS is set up, otherwise the router's public IP (if known).
    pub public_host: Option<String>,
    pub syncplay_lan: Option<String>,
    pub syncplay_public: Option<String>,
    pub jellyfin_local: Option<String>,
    pub jellyfin_lan: Option<String>,
    pub jellyfin_public: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub version: &'static str,
    pub settings: Settings,
    pub duckdns_token_set: bool,
    pub jellyfin_signed_in: bool,
    pub syncplay: SyncplayStatus,
    pub jellyfin: JellyfinStatus,
    pub tls: TlsStatus,
    pub upnp: UpnpStatus,
    pub addresses: Addresses,
    pub update: UpdateStatus,
    pub jellyfin_share: ShareStatus,
}

struct PfxState {
    fingerprint: String,
    password: String,
}

pub struct App {
    pub paths: AppPaths,
    secrets: Secrets,
    settings: RwLock<Settings>,
    duckdns_token: RwLock<Option<String>>,
    jellyfin_token: RwLock<Option<String>>,
    reconcile_lock: tokio::sync::Mutex<()>,
    syncplay: Mutex<Option<SyncplayServer>>,
    syncplay_error: RwLock<Option<String>>,
    pub tls: Arc<TlsManager>,
    pub jellyfin: Arc<JellyfinManager>,
    pub upnp: Arc<UpnpManager>,
    pub updates: Arc<Updates>,
    upnp_desired: Mutex<BTreeMap<u16, String>>,
    pfx: Mutex<Option<PfxState>>,
    applied_fingerprint: Mutex<Option<String>>,
    notify: Notify,
    pub relay: Relay,
    proxy: Arc<JellyfinProxy>,
    share: Mutex<ShareState>,
    share_lock: tokio::sync::Mutex<()>,
    devices: Arc<DeviceStore>,
}

impl App {
    pub fn new(paths: AppPaths, notify: Notify) -> Arc<Self> {
        if let Err(e) = paths.ensure() {
            warn!(error = %e, "could not create app folders");
        }
        let mut settings = Settings::load(&paths.settings_file());
        if settings.jellyfin.device_id.is_empty() {
            settings.jellyfin.device_id = uuid::Uuid::new_v4().simple().to_string();
            let _ = settings.save(&paths.settings_file());
        }
        let secrets = Secrets::new(paths.config.clone());
        let duckdns_token = secrets.get(secrets::DUCKDNS_TOKEN);
        let jellyfin_token = secrets.get(secrets::JELLYFIN_TOKEN);
        let jellyfin = JellyfinManager::new(paths.jellyfin_root(), notify.clone());
        let relay = Relay::new(
            paths.relay_cache_dir(),
            u64::from(settings.syncplay.relay_cache_gb) << 30,
            settings.syncplay.relay_effective(),
        );
        let devices = Arc::new(DeviceStore::load(paths.syncplay_devices_file()));
        Arc::new(Self {
            relay,
            devices,
            proxy: Arc::new(JellyfinProxy::new()),
            share: Mutex::new(ShareState::default()),
            share_lock: tokio::sync::Mutex::new(()),
            tls: TlsManager::new(paths.acme_dir()),
            jellyfin,
            upnp: Arc::new(UpnpManager::default()),
            updates: Updates::new(notify.clone()),
            paths,
            secrets,
            settings: RwLock::new(settings),
            duckdns_token: RwLock::new(duckdns_token),
            jellyfin_token: RwLock::new(jellyfin_token),
            reconcile_lock: tokio::sync::Mutex::new(()),
            syncplay: Mutex::new(None),
            syncplay_error: RwLock::new(None),
            upnp_desired: Mutex::new(BTreeMap::new()),
            pfx: Mutex::new(None),
            applied_fingerprint: Mutex::new(None),
            notify,
        })
    }

    /// First reconcile plus the periodic UPnP re-check.
    pub fn start(self: &Arc<Self>) {
        let me = self.clone();
        tokio::spawn(async move {
            me.reconcile().await;
            let mut tick = tokio::time::interval(UPNP_RECHECK);
            tick.tick().await;
            loop {
                tick.tick().await;
                let desired = me.upnp_desired.lock().clone();
                me.upnp.recheck(desired).await;
                (me.notify)();
            }
        });
    }

    pub fn settings(&self) -> Settings {
        self.settings.read().clone()
    }

    /// Save new settings from the UI (server-managed Jellyfin fields are kept)
    /// and apply them.
    pub async fn update_settings(self: &Arc<Self>, mut next: Settings) -> Result<Snapshot, String> {
        {
            let cur = self.settings.read();
            next.jellyfin.setup_complete = cur.jellyfin.setup_complete;
            next.jellyfin.admin_user = cur.jellyfin.admin_user.clone();
            next.jellyfin.admin_user_id = cur.jellyfin.admin_user_id.clone();
            next.jellyfin.device_id = cur.jellyfin.device_id.clone();
            next.jellyfin.guest_user_id = cur.jellyfin.guest_user_id.clone();
        }
        next.tls.duckdns_domain = next.tls.duckdns_domain.trim().to_string();
        next.tls.email = next.tls.email.trim().to_string();
        next.validate()?;
        if !next.tls.duckdns_domain.is_empty() {
            crate::net::acme::normalize_domain(&next.tls.duckdns_domain)?;
        }
        self.save_settings(next)?;
        self.reconcile().await;
        Ok(self.snapshot())
    }

    fn save_settings(&self, next: Settings) -> Result<(), String> {
        next.save(&self.paths.settings_file())?;
        *self.settings.write() = next;
        Ok(())
    }

    fn modify_settings(&self, f: impl FnOnce(&mut Settings)) -> Result<(), String> {
        let mut next = self.settings();
        f(&mut next);
        self.save_settings(next)
    }

    pub async fn set_duckdns_token(
        self: &Arc<Self>,
        token: Option<String>,
    ) -> Result<Snapshot, String> {
        match token
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
        {
            Some(t) => {
                DuckDns::new("x", &t)?;
                self.secrets.set(secrets::DUCKDNS_TOKEN, &t)?;
                *self.duckdns_token.write() = Some(t);
            }
            None => {
                self.secrets.delete(secrets::DUCKDNS_TOKEN)?;
                *self.duckdns_token.write() = None;
            }
        }
        self.reconcile().await;
        Ok(self.snapshot())
    }

    pub fn renew_certificate(&self) -> Result<(), String> {
        self.tls.renew_now()
    }

    fn ip_source(&self) -> IpSource {
        let upnp = self.upnp.clone();
        Arc::new(move || upnp.external_ip())
    }

    fn ensure_pfx(&self, bundle: &crate::tls::CertBundle) -> Result<(String, String), String> {
        let path = self.jellyfin.pfx_path();
        let mut pfx = self.pfx.lock();
        if let Some(state) = pfx.as_ref() {
            if state.fingerprint == bundle.fingerprint && path.is_file() {
                return Ok((path.to_string_lossy().into_owned(), state.password.clone()));
            }
        }
        let password = hex::encode(rand::random::<[u8; 24]>());
        let bytes =
            crate::jellyfin::netconfig::pem_to_pfx(&bundle.cert_pem, &bundle.key_pem, &password)?;
        crate::paths::write_atomic(&path, &bytes)?;
        *pfx = Some(PfxState {
            fingerprint: bundle.fingerprint.clone(),
            password: password.clone(),
        });
        Ok((path.to_string_lossy().into_owned(), password))
    }

    /// Bring every service in line with the current settings.
    pub async fn reconcile(self: &Arc<Self>) {
        self.reconcile_services().await;
        self.sync_share().await;
    }

    fn extensions(self: &Arc<Self>) -> Extensions {
        let weak = Arc::downgrade(self);
        let authorize: AuthorizeFn = Arc::new(move |code: String| {
            let weak = weak.clone();
            Box::pin(async move {
                match weak.upgrade() {
                    Some(app) => app.authorize_guest(&code).await,
                    None => Err("The server is shutting down".into()),
                }
            })
        });
        Extensions {
            relay: Some(self.relay.clone()),
            proxy: Some(self.proxy.clone()),
            authorize: Some(authorize),
            devices: Some(self.devices.clone()),
        }
    }

    async fn reconcile_services(self: &Arc<Self>) {
        let _guard = self.reconcile_lock.lock().await;
        let s = self.settings();
        self.relay
            .set_limit(u64::from(s.syncplay.relay_cache_gb) << 30);

        // TLS first so Syncplay and Jellyfin see the right certificate.
        let token = self.duckdns_token.read().clone();
        if s.tls.enabled {
            let request = match (&token, s.tls.duckdns_domain.is_empty()) {
                (_, true) => Err("Enter your DuckDNS domain".to_string()),
                (None, _) => Err("Enter your DuckDNS token".to_string()),
                (Some(t), false) => {
                    DuckDns::new(&s.tls.duckdns_domain, t).map(|duckdns| TlsRequest {
                        duckdns,
                        staging: s.tls.staging,
                        email: Some(s.tls.email.clone()).filter(|e| !e.is_empty()),
                    })
                }
            };
            match request {
                Ok(req) => self
                    .tls
                    .apply(Some(req), None, self.ip_source(), self.notify.clone()),
                Err(msg) => self
                    .tls
                    .apply(None, Some(msg), self.ip_source(), self.notify.clone()),
            }
        } else {
            self.tls
                .apply(None, None, self.ip_source(), self.notify.clone());
        }

        // Syncplay.
        let want_port = s.syncplay.enabled.then_some(s.syncplay.port);
        let opts = SyncplayOptions::from(&s.syncplay);
        let running_port = self.syncplay.lock().as_ref().map(|srv| srv.port);
        if running_port.is_some() && running_port != want_port {
            if let Some(srv) = self.syncplay.lock().take() {
                srv.stop();
            }
        }
        match want_port {
            Some(port) if running_port == Some(port) => {
                if let Some(srv) = self.syncplay.lock().as_ref() {
                    srv.set_options(opts);
                }
            }
            Some(port) => {
                match SyncplayServer::start(
                    port,
                    opts,
                    self.tls.store(),
                    self.notify.clone(),
                    self.extensions(),
                )
                .await
                {
                    Ok(srv) => {
                        *self.syncplay.lock() = Some(srv);
                        *self.syncplay_error.write() = None;
                    }
                    Err(e) => {
                        let msg = if e.kind() == std::io::ErrorKind::AddrInUse {
                            format!("Port {port} is already in use by another program")
                        } else {
                            format!("Could not listen on port {port}: {e}")
                        };
                        warn!(port, error = %e, "Syncplay server failed to start");
                        *self.syncplay_error.write() = Some(msg);
                    }
                }
            }
            None => *self.syncplay_error.write() = None,
        }

        // Jellyfin.
        let cert = if s.tls.enabled {
            self.tls.current()
        } else {
            None
        };
        *self.applied_fingerprint.lock() = cert.as_ref().map(|c| c.fingerprint.clone());
        let https = cert.as_ref().and_then(|c| match self.ensure_pfx(c) {
            Ok(v) => Some(v),
            Err(e) => {
                warn!(error = %e, "could not prepare the Jellyfin HTTPS certificate");
                None
            }
        });
        let https_on = https.is_some();
        if s.jellyfin.enabled {
            let token = self.jellyfin_token.read().clone();
            self.jellyfin.set_api(
                token.map(|t| {
                    JellyfinApi::new(s.jellyfin.http_port, &s.jellyfin.device_id, Some(t))
                }),
            );
            self.jellyfin.apply(Some(JellyfinDesired {
                http_port: s.jellyfin.http_port,
                https_port: s.jellyfin.https_port,
                https,
            }));
        } else {
            self.jellyfin.apply(None);
        }

        // UPnP runs in the background; the router can be slow.
        let mut desired = BTreeMap::new();
        if s.syncplay.enabled && s.syncplay.upnp {
            desired.insert(s.syncplay.port, "Syncplay".to_string());
        }
        if s.jellyfin.enabled && s.jellyfin.upnp {
            desired.insert(s.jellyfin.http_port, "Jellyfin HTTP".to_string());
            if https_on {
                desired.insert(s.jellyfin.https_port, "Jellyfin HTTPS".to_string());
            }
        }
        let changed = *self.upnp_desired.lock() != desired;
        *self.upnp_desired.lock() = desired.clone();
        if changed {
            let me = self.clone();
            tokio::spawn(async move {
                me.upnp.reconcile(desired).await;
                (me.notify)();
            });
        }
        (self.notify)();
    }

    /// Called (debounced) after any status change: a new or renewed
    /// certificate needs a fresh Jellyfin PFX.
    pub async fn after_change(self: &Arc<Self>) {
        let current = if self.settings.read().tls.enabled {
            self.tls.current().map(|c| c.fingerprint.clone())
        } else {
            None
        };
        if *self.applied_fingerprint.lock() != current {
            info!("certificate changed, updating services");
            self.reconcile().await;
        } else {
            self.sync_share().await;
        }
    }

    /// Keep the guest account, the proxy and what Syncplay clients are told
    /// in line with "share Jellyfin" and Jellyfin's state.
    async fn sync_share(self: &Arc<Self>) {
        let _guard = self.share_lock.lock().await;
        let s = self.settings();
        let want = s.share_effective();
        let ready = s.jellyfin.enabled
            && self.jellyfin.is_running()
            && self.jellyfin_token.read().is_some();
        if !ready {
            // Re-apply once Jellyfin is reachable again.
            *self.share.lock() = ShareState::default();
        } else {
            let pending = {
                let st = self.share.lock();
                st.applied != Some(want) && st.failed_at.is_none_or(|t| t.elapsed() >= SHARE_RETRY)
            };
            if pending {
                let result = match self.jellyfin_api() {
                    Ok(api) => {
                        if want {
                            if let Err(e) = api.enable_quick_connect().await {
                                warn!(error = %e, "could not enable Quick Connect");
                            }
                        }
                        let res = crate::jellyfin::share::ensure_guest(
                            &api,
                            s.jellyfin.guest_user_id.as_deref(),
                            s.jellyfin.admin_user_id.as_deref(),
                            want,
                        )
                        .await;
                        self.with_auth(res).await
                    }
                    Err(e) => Err(e),
                };
                match result {
                    Ok(id) => {
                        if id.is_some() && id != s.jellyfin.guest_user_id {
                            let _ = self.modify_settings(|n| n.jellyfin.guest_user_id = id.clone());
                        }
                        info!(sharing = want, "Jellyfin guest account updated");
                        *self.share.lock() = ShareState {
                            applied: Some(want),
                            ..Default::default()
                        };
                    }
                    Err(e) => {
                        warn!(error = %e, "could not update the Jellyfin guest account");
                        let mut st = self.share.lock();
                        st.failed_at = Some(Instant::now());
                        st.error = Some(e);
                    }
                }
            }
        }

        let active = want
            && ready
            && self.share.lock().applied == Some(true)
            && self.settings.read().jellyfin.guest_user_id.is_some();
        self.proxy
            .set_target(active.then_some(s.jellyfin.http_port));
        let info = active.then(|| {
            let j = self.jellyfin.status();
            let a = self.addresses(&s, &j);
            JellyfinShare {
                server_id: j.server_id.unwrap_or_default(),
                server_name: j.server_name.unwrap_or_default(),
                proxy: true,
                addresses: [a.jellyfin_public, a.jellyfin_lan]
                    .into_iter()
                    .flatten()
                    .collect(),
            }
        });
        if let Some(srv) = self.syncplay.lock().as_ref() {
            srv.set_jellyfin(info);
        }
    }

    /// Approve a Quick Connect code from a Syncplay user for the guest account.
    async fn authorize_guest(&self, code: &str) -> Result<(), String> {
        let s = self.settings();
        let guest = s
            .jellyfin
            .guest_user_id
            .clone()
            .filter(|_| s.share_effective() && self.share.lock().applied == Some(true))
            .ok_or("Jellyfin sharing is off on this server")?;
        let api = self.jellyfin_api()?;
        let res = api.quick_connect_authorize(code, &guest).await;
        self.with_auth(res).await?;
        info!("approved a Jellyfin Quick Connect sign-in for a Syncplay guest");
        Ok(())
    }

    // ------------------------------------------------------------ Syncplay devices

    pub fn approve_device(&self, fingerprint: &str) -> Result<Snapshot, String> {
        let result = self.devices.approve(fingerprint);
        (self.notify)();
        result.map(|()| self.snapshot())
    }

    pub fn deny_device(&self, fingerprint: &str) -> Result<Snapshot, String> {
        self.devices.deny(fingerprint)?;
        (self.notify)();
        Ok(self.snapshot())
    }

    /// Forget an approved device and disconnect it.
    pub fn remove_device(&self, fingerprint: &str) -> Result<Snapshot, String> {
        self.devices.remove(fingerprint)?;
        if let Some(srv) = self.syncplay.lock().as_ref() {
            srv.kick_device(fingerprint);
        }
        (self.notify)();
        Ok(self.snapshot())
    }

    pub fn rename_device(&self, fingerprint: &str, name: &str) -> Result<Snapshot, String> {
        self.devices.rename(fingerprint, name)?;
        (self.notify)();
        Ok(self.snapshot())
    }

    pub fn share_status(&self) -> ShareStatus {
        let s = self.settings.read();
        let st = self.share.lock();
        ShareStatus {
            active: self.proxy.target().is_some(),
            error: s.share_effective().then(|| st.error.clone()).flatten(),
        }
    }

    fn addresses(&self, s: &Settings, jellyfin: &JellyfinStatus) -> Addresses {
        let lan_ip = crate::net::ip::primary_lan_ipv4().map(|ip| ip.to_string());
        let cert_host = self.tls.current().map(|c| c.host.clone());
        let public_host = cert_host.clone().or_else(|| self.upnp.status().external_ip);
        let mut addresses = Addresses {
            lan_ip: lan_ip.clone(),
            public_host: public_host.clone(),
            ..Default::default()
        };
        if s.syncplay.enabled {
            addresses.syncplay_lan = lan_ip
                .as_ref()
                .map(|ip| format!("{ip}:{}", s.syncplay.port));
            addresses.syncplay_public = public_host
                .as_ref()
                .map(|h| format!("{h}:{}", s.syncplay.port));
        }
        if s.jellyfin.enabled {
            addresses.jellyfin_local = Some(format!("http://localhost:{}", s.jellyfin.http_port));
            addresses.jellyfin_lan = lan_ip
                .as_ref()
                .map(|ip| format!("http://{ip}:{}", s.jellyfin.http_port));
            addresses.jellyfin_public = match (&cert_host, jellyfin.https) {
                (Some(host), true) => Some(format!("https://{host}:{}", s.jellyfin.https_port)),
                _ => public_host
                    .as_ref()
                    .map(|h| format!("http://{h}:{}", s.jellyfin.http_port)),
            };
        }
        addresses
    }

    pub fn snapshot(&self) -> Snapshot {
        let s = self.settings();
        let syncplay = {
            let guard = self.syncplay.lock();
            match guard.as_ref() {
                Some(srv) => SyncplayStatus {
                    running: true,
                    port: Some(srv.port),
                    error: None,
                    users: srv.user_count(),
                    rooms: srv.rooms(),
                    tls: self.tls.current().is_some(),
                    relay: self.relay.status(),
                    devices: self.devices.status(),
                },
                None => SyncplayStatus {
                    error: self.syncplay_error.read().clone(),
                    relay: self.relay.status(),
                    devices: self.devices.status(),
                    ..Default::default()
                },
            }
        };
        let tls = self.tls.status();
        let upnp = self.upnp.status();
        let jellyfin = self.jellyfin.status();
        let addresses = self.addresses(&s, &jellyfin);

        Snapshot {
            jellyfin_share: self.share_status(),
            version: env!("CARGO_PKG_VERSION"),
            duckdns_token_set: self.duckdns_token.read().is_some(),
            jellyfin_signed_in: self.jellyfin_token.read().is_some(),
            settings: s,
            syncplay,
            jellyfin,
            tls,
            upnp,
            addresses,
            update: self.updates.status(),
        }
    }

    // ------------------------------------------------------------ Jellyfin

    fn jellyfin_port(&self) -> Result<u16, String> {
        if !self.jellyfin.is_running() {
            return Err("Jellyfin is not running yet".into());
        }
        Ok(self.settings.read().jellyfin.http_port)
    }

    fn jellyfin_api(&self) -> Result<JellyfinApi, String> {
        let port = self.jellyfin_port()?;
        let token = self
            .jellyfin_token
            .read()
            .clone()
            .ok_or("Sign in to Jellyfin first")?;
        Ok(JellyfinApi::new(
            port,
            &self.settings.read().jellyfin.device_id,
            Some(token),
        ))
    }

    fn store_session(
        &self,
        port: u16,
        session: crate::jellyfin::api::Session,
    ) -> Result<(), String> {
        self.secrets.set(secrets::JELLYFIN_TOKEN, &session.token)?;
        *self.jellyfin_token.write() = Some(session.token.clone());
        let device_id = self.settings.read().jellyfin.device_id.clone();
        self.jellyfin.set_api(Some(JellyfinApi::new(
            port,
            &device_id,
            Some(session.token),
        )));
        self.modify_settings(|s| {
            s.jellyfin.setup_complete = true;
            s.jellyfin.admin_user = Some(session.user_name);
            s.jellyfin.admin_user_id = Some(session.user_id);
        })
    }

    pub async fn jellyfin_setup(
        &self,
        server_name: String,
        user: String,
        password: String,
    ) -> Result<Snapshot, String> {
        let user = user.trim().to_string();
        if user.is_empty() {
            return Err("Choose an administrator username".into());
        }
        if password.chars().count() < 4 {
            return Err("Use a password of at least 4 characters".into());
        }
        let port = self.jellyfin_port()?;
        let device_id = self.settings.read().jellyfin.device_id.clone();
        let api = JellyfinApi::new(port, &device_id, None);
        let info = api.public_info().await?;
        if info.startup_wizard_completed {
            return Err("This Jellyfin server is already set up. Sign in with its administrator account instead.".into());
        }
        let name = if server_name.trim().is_empty() {
            "YarmiplayServerTV".to_string()
        } else {
            server_name.trim().to_string()
        };
        api.run_startup(&name, &user, &password).await?;
        let session = api.authenticate(&user, &password).await?;
        let authed = JellyfinApi::new(port, &device_id, Some(session.token.clone()));
        if let Err(e) = authed.enable_quick_connect().await {
            warn!(error = %e, "could not enable Quick Connect");
        }
        self.store_session(port, session)?;
        info!(%user, "Jellyfin first-run setup complete");
        self.jellyfin.refresh_info(port).await;
        Ok(self.snapshot())
    }

    pub async fn jellyfin_login(&self, user: String, password: String) -> Result<Snapshot, String> {
        let port = self.jellyfin_port()?;
        let device_id = self.settings.read().jellyfin.device_id.clone();
        let session = JellyfinApi::new(port, &device_id, None)
            .authenticate(user.trim(), &password)
            .await?;
        self.store_session(port, session)?;
        self.jellyfin.refresh_info(port).await;
        Ok(self.snapshot())
    }

    pub fn jellyfin_logout(&self) -> Result<Snapshot, String> {
        self.secrets.delete(secrets::JELLYFIN_TOKEN)?;
        *self.jellyfin_token.write() = None;
        self.jellyfin.set_api(None);
        self.modify_settings(|s| {
            s.jellyfin.admin_user = None;
            s.jellyfin.admin_user_id = None;
        })?;
        Ok(self.snapshot())
    }

    async fn with_auth<T>(&self, res: Result<T, String>) -> Result<T, String> {
        if let Err(e) = &res {
            if e.contains("(401)") {
                let _ = self.secrets.delete(secrets::JELLYFIN_TOKEN);
                *self.jellyfin_token.write() = None;
                (self.notify)();
                return Err("Your Jellyfin sign-in expired. Sign in again.".into());
            }
        }
        res
    }

    pub async fn jellyfin_libraries(&self) -> Result<Vec<crate::jellyfin::api::Library>, String> {
        let api = self.jellyfin_api()?;
        self.with_auth(api.libraries().await).await
    }

    pub async fn jellyfin_add_library(
        &self,
        name: String,
        collection_type: String,
        path: String,
    ) -> Result<(), String> {
        if name.trim().is_empty() || path.trim().is_empty() {
            return Err("A library needs a name and a folder".into());
        }
        let api = self.jellyfin_api()?;
        self.with_auth(
            api.add_library(name.trim(), &collection_type, path.trim())
                .await,
        )
        .await
    }

    pub async fn jellyfin_remove_library(&self, name: String) -> Result<(), String> {
        let api = self.jellyfin_api()?;
        self.with_auth(api.remove_library(&name).await).await
    }

    pub async fn jellyfin_add_path(&self, library: String, path: String) -> Result<(), String> {
        let api = self.jellyfin_api()?;
        self.with_auth(api.add_path(&library, path.trim()).await)
            .await
    }

    pub async fn jellyfin_remove_path(&self, library: String, path: String) -> Result<(), String> {
        let api = self.jellyfin_api()?;
        self.with_auth(api.remove_path(&library, &path).await).await
    }

    pub async fn jellyfin_rescan(&self) -> Result<(), String> {
        let api = self.jellyfin_api()?;
        self.with_auth(api.rescan().await).await
    }

    // ------------------------------------------------------------ quit

    pub async fn shutdown(&self) {
        info!("shutting down");
        if let Some(srv) = self.syncplay.lock().take() {
            srv.stop();
        }
        let upnp = self.upnp.clone();
        let jellyfin = self.jellyfin.clone();
        let _ = tokio::time::timeout(Duration::from_secs(25), async move {
            tokio::join!(jellyfin.shutdown(), upnp.shutdown());
        })
        .await;
    }
}
