//! Owns every service and keeps them matching the saved settings. All
//! changes go through [`App::reconcile`], serialized by an async mutex.

use crate::config::Settings;
use crate::jellyfin::api::JellyfinApi;
use crate::jellyfin::{JellyfinDesired, JellyfinManager, JellyfinStatus};
use crate::net::acme::DuckDns;
use crate::net::proxy::LocalProxy;
use crate::net::upnp::{UpnpManager, UpnpStatus};
use crate::paths::AppPaths;
use crate::plex::api::PlexApi;
use crate::plex::tv::PlexTv;
use crate::plex::{PlexManager, PlexStatus};
use crate::relay::{Relay, RelayStatus};
use crate::secrets::{self, Secrets};
use crate::syncplay::devices::{DeviceStore, DevicesStatus};
use crate::syncplay::ext::{JellyfinShare, PlexShare};
use crate::syncplay::{
    AuthorizeFn, Extensions, PlexInviteFn, RoomInfo, SyncplayOptions, SyncplayServer,
};
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
/// How often to ask plex.tv whether the user entered the link code.
const PLEX_LINK_POLL: Duration = Duration::from_secs(2);
/// Relay Plex over the Syncplay port. Off: Plex 1.42 reads the relayed
/// viewer's address from `X-Forwarded-For` but still marks them local
/// (`Player.local=true`) because the connection comes from 127.0.0.1, so the
/// relay would sidestep Plex's remote-streaming rules. Only direct
/// connections are shared until Plex counts relayed viewers as remote (check
/// with the `plex_spike` test).
const PLEX_RELAY: bool = false;

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

/// A plex.tv link code waiting for the user to enter it at plex.tv/link.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PlexLink {
    pub code: String,
    /// Unix seconds.
    pub expires_at: u64,
}

/// The host's plex.tv account as the Plex page shows it.
#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PlexAccount {
    pub signed_in: bool,
    pub link: Option<PlexLink>,
    /// The running Plex belongs to the linked account; None until checked.
    pub owns_server: Option<bool>,
    pub error: Option<String>,
}

/// What Plex sharing last applied: sharing on/off, the picked libraries and
/// the relay URLs advertised to plex.tv.
type PlexApplied = (bool, Option<Vec<String>>, Vec<String>);

#[derive(Default)]
struct PlexShareState {
    applied: Option<PlexApplied>,
    failed_at: Option<Instant>,
    error: Option<String>,
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
    pub plex_local: Option<String>,
    pub plex_lan: Option<String>,
    pub plex_public: Option<String>,
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
    pub plex: PlexStatus,
    pub plex_account: PlexAccount,
    pub plex_share: ShareStatus,
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
    proxy: Arc<LocalProxy>,
    share: Mutex<ShareState>,
    share_lock: tokio::sync::Mutex<()>,
    devices: Arc<DeviceStore>,
    pub plex: Arc<PlexManager>,
    plex_token: RwLock<Option<String>>,
    plex_proxy: Arc<LocalProxy>,
    plex_link: Mutex<Option<(PlexLink, tokio::task::AbortHandle)>>,
    plex_link_error: Mutex<Option<String>>,
    /// (machine id, owned by the linked account), checked with plex.tv.
    plex_owned: Mutex<Option<(String, bool)>>,
    plex_owned_failed_at: Mutex<Option<Instant>>,
    plex_share: Mutex<PlexShareState>,
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
        if settings.plex.client_id.is_empty() {
            settings.plex.client_id = uuid::Uuid::new_v4().to_string();
            let _ = settings.save(&paths.settings_file());
        }
        let secrets = Secrets::new(paths.config.clone());
        let duckdns_token = secrets.get(secrets::DUCKDNS_TOKEN);
        let jellyfin_token = secrets.get(secrets::JELLYFIN_TOKEN);
        let plex_token = secrets.get(secrets::PLEX_TOKEN);
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
            plex: PlexManager::new(notify.clone()),
            plex_token: RwLock::new(plex_token),
            plex_proxy: Arc::new(LocalProxy::new("Plex")),
            plex_link: Mutex::new(None),
            plex_link_error: Mutex::new(None),
            plex_owned: Mutex::new(None),
            plex_owned_failed_at: Mutex::new(None),
            plex_share: Mutex::new(PlexShareState::default()),
            proxy: Arc::new(LocalProxy::new("Jellyfin")),
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
            next.plex.client_id = cur.plex.client_id.clone();
            next.plex.account_user = cur.plex.account_user.clone();
            next.plex.machine_id = cur.plex.machine_id.clone();
            next.plex.invited = cur.plex.invited.clone();
            next.plex.saved_custom_connections = cur.plex.saved_custom_connections.clone();
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
        self.sync_plex_share().await;
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
        let weak = Arc::downgrade(self);
        let plex_invite: PlexInviteFn = Arc::new(move |username: String, syncplay_name: String| {
            let weak = weak.clone();
            Box::pin(async move {
                match weak.upgrade() {
                    Some(app) => app.invite_plex_user(&username, &syncplay_name).await,
                    None => Err("The server is shutting down".into()),
                }
            })
        });
        Extensions {
            relay: Some(self.relay.clone()),
            proxy: Some(self.proxy.clone()),
            authorize: Some(authorize),
            devices: Some(self.devices.clone()),
            plex_proxy: Some(self.plex_proxy.clone()),
            plex_invite: Some(plex_invite),
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

        // Plex: only watched, never started or stopped.
        self.plex.apply(
            s.plex.enabled.then_some(s.plex.port),
            &s.plex.client_id,
            self.plex_token.read().clone(),
        );

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
        if s.plex.enabled && s.plex.upnp {
            desired.insert(s.plex.port, "Plex".to_string());
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
            self.sync_plex_share().await;
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
        if s.plex.enabled {
            addresses.plex_local = Some(format!("http://localhost:{}/web", s.plex.port));
            addresses.plex_lan = lan_ip
                .as_ref()
                .map(|ip| format!("http://{ip}:{}", s.plex.port));
            addresses.plex_public = self
                .upnp
                .status()
                .external_ip
                .map(|ip| format!("http://{ip}:{}", s.plex.port));
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
            plex: self.plex.status(),
            plex_account: self.plex_account(),
            plex_share: self.plex_share_status(),
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

    // ------------------------------------------------------------ Plex

    fn plex_tv(&self) -> Result<PlexTv, String> {
        let token = self
            .plex_token
            .read()
            .clone()
            .ok_or("Link your Plex account first")?;
        Ok(PlexTv::new(
            &self.settings.read().plex.client_id,
            Some(token),
        ))
    }

    fn plex_api(&self) -> Result<PlexApi, String> {
        let s = self.settings.read();
        if !s.plex.enabled {
            return Err("Plex is switched off here".into());
        }
        match self.plex.status().phase {
            "running" => {}
            "unclaimed" => return Err("Finish Plex's own setup first".into()),
            _ => return Err("Plex Media Server is not running".into()),
        }
        let token = self
            .plex_token
            .read()
            .clone()
            .ok_or("Link your Plex account first")?;
        if self
            .plex_owned
            .lock()
            .as_ref()
            .is_some_and(|(_, owned)| !owned)
        {
            return Err("This Plex server belongs to another plex.tv account".into());
        }
        Ok(PlexApi::new(s.plex.port, &s.plex.client_id, Some(token)))
    }

    /// A 401 from plex.tv or Plex means the account token is gone.
    async fn with_plex_auth<T>(&self, res: Result<T, String>) -> Result<T, String> {
        if let Err(e) = &res {
            if e.contains("(401)") {
                let _ = self.secrets.delete(secrets::PLEX_TOKEN);
                *self.plex_token.write() = None;
                *self.plex_owned.lock() = None;
                let _ = self.modify_settings(|s| s.plex.account_user = None);
                (self.notify)();
                return Err("Your Plex sign-in expired. Link your Plex account again.".into());
            }
        }
        res
    }

    pub fn plex_account(&self) -> PlexAccount {
        let s = self.settings.read();
        let machine = self.plex.status().machine_id;
        PlexAccount {
            signed_in: self.plex_token.read().is_some(),
            link: self.plex_link.lock().as_ref().map(|(l, _)| l.clone()),
            owns_server: self
                .plex_owned
                .lock()
                .as_ref()
                .filter(|(m, _)| Some(m) == machine.as_ref())
                .map(|(_, owned)| *owned)
                .filter(|_| s.plex.enabled),
            error: self.plex_link_error.lock().clone(),
        }
    }

    pub fn plex_share_status(&self) -> ShareStatus {
        let s = self.settings.read();
        let st = self.plex_share.lock();
        ShareStatus {
            active: self.plex_proxy.target().is_some()
                || (s.plex_share_effective() && st.applied.as_ref().is_some_and(|a| a.0)),
            error: s.plex_share_effective().then(|| st.error.clone()).flatten(),
        }
    }

    /// Where friends reach Plex through the Syncplay port.
    fn plex_relay_urls(&self, s: &Settings) -> Vec<String> {
        if !s.syncplay.enabled || !PLEX_RELAY {
            return Vec::new();
        }
        let port = s.syncplay.port;
        let mut urls = Vec::new();
        match self.tls.current() {
            Some(cert) => urls.push(format!("https://{}:{port}", cert.host)),
            None => {
                if let Some(ip) = self.upnp.status().external_ip {
                    urls.push(format!("http://{ip}:{port}"));
                }
            }
        }
        if let Some(ip) = crate::net::ip::primary_lan_ipv4() {
            urls.push(format!("http://{ip}:{port}"));
        }
        urls
    }

    /// Whether the running Plex belongs to the linked account (asked once per
    /// server and account).
    async fn plex_owns(&self, machine_id: &str) -> Option<bool> {
        if let Some((m, owned)) = self.plex_owned.lock().clone() {
            if m == machine_id {
                return Some(owned);
            }
        }
        if self
            .plex_owned_failed_at
            .lock()
            .is_some_and(|t| t.elapsed() < SHARE_RETRY)
        {
            return None;
        }
        let tv = self.plex_tv().ok()?;
        match self.with_plex_auth(tv.resources().await).await {
            Ok(resources) => {
                let owned = resources.iter().any(|r| {
                    r.client_identifier == machine_id && r.owned && r.provides.contains("server")
                });
                if !owned {
                    warn!("the local Plex server is not owned by the linked plex.tv account");
                }
                *self.plex_owned.lock() = Some((machine_id.to_string(), owned));
                *self.plex_owned_failed_at.lock() = None;
                (self.notify)();
                Some(owned)
            }
            Err(e) => {
                warn!(error = %e, "could not look up the Plex server on plex.tv");
                *self.plex_owned_failed_at.lock() = Some(Instant::now());
                None
            }
        }
    }

    /// Keep invites, Plex's advertised connections, the relay and what
    /// Syncplay clients are told in line with "share Plex" and Plex's state.
    async fn sync_plex_share(self: &Arc<Self>) {
        let _guard = self.share_lock.lock().await;
        let s = self.settings();
        let want = s.plex_share_effective();
        let status = self.plex.status();
        let machine_id = status
            .machine_id
            .clone()
            .filter(|_| status.phase == "running");
        if let Some(m) = &machine_id {
            if s.plex.machine_id.as_ref() != Some(m) {
                let _ = self.modify_settings(|n| n.plex.machine_id = Some(m.clone()));
            }
        }
        let linked = self.plex_token.read().is_some();
        let owned = match (&machine_id, linked) {
            (Some(m), true) => self.plex_owns(m).await,
            _ => None,
        };
        let ready = s.plex.enabled && owned == Some(true);
        let relay = self.plex_relay_urls(&s);
        let target: PlexApplied = (want, s.plex.shared_sections.clone(), relay.clone());

        let pending = {
            let st = self.plex_share.lock();
            st.applied.as_ref() != Some(&target)
                && st.failed_at.is_none_or(|t| t.elapsed() >= SHARE_RETRY)
        };
        // Removing invites only needs plex.tv, so it happens even while Plex is down.
        let unshare = !want && linked && !s.plex.invited.is_empty();
        if pending && (ready || unshare) {
            let result = self.apply_plex_share(&s, want, ready, &relay).await;
            let mut st = self.plex_share.lock();
            match result {
                Ok(()) => {
                    info!(sharing = want, "Plex sharing updated");
                    *st = PlexShareState {
                        applied: ready.then_some(target),
                        ..Default::default()
                    };
                }
                Err(e) => {
                    warn!(error = %e, "could not update Plex sharing");
                    st.failed_at = Some(Instant::now());
                    st.error = Some(e);
                }
            }
        } else if !ready {
            // Re-apply once Plex is reachable again.
            let mut st = self.plex_share.lock();
            st.applied = None;
        }

        let active = want && ready && self.plex_share.lock().applied.as_ref().is_some_and(|a| a.0);
        let relaying = active && PLEX_RELAY && s.syncplay.enabled;
        self.plex_proxy.set_target(relaying.then_some(s.plex.port));
        let info = active.then(|| {
            let a = self.addresses(&s, &self.jellyfin.status());
            PlexShare {
                machine_id: status.machine_id.clone().unwrap_or_default(),
                server_name: status.server_name.clone().unwrap_or_default(),
                proxy: relaying,
                addresses: [a.plex_public, a.plex_lan].into_iter().flatten().collect(),
            }
        });
        if let Some(srv) = self.syncplay.lock().as_ref() {
            srv.set_plex(info);
        }
    }

    async fn apply_plex_share(
        &self,
        s: &Settings,
        want: bool,
        ready: bool,
        relay: &[String],
    ) -> Result<(), String> {
        let tv = self.plex_tv()?;
        if !want {
            if !s.plex.invited.is_empty() {
                let (kept, error) = crate::plex::share::unshare_all(&tv, &s.plex.invited).await;
                self.modify_settings(|n| n.plex.invited = kept)?;
                if let Some(e) = error {
                    return self.with_plex_auth(Err(e)).await;
                }
            }
            if ready {
                if let Some(original) = &s.plex.saved_custom_connections {
                    let api = self.plex_api()?;
                    self.with_plex_auth(api.set_pref("customConnections", original).await)
                        .await?;
                    self.modify_settings(|n| n.plex.saved_custom_connections = None)?;
                }
            }
            return Ok(());
        }

        // Advertise the relay through plex.tv, keeping the host's own entries.
        let api = self.plex_api()?;
        let original = match &s.plex.saved_custom_connections {
            Some(o) => o.clone(),
            None => {
                let current = self
                    .with_plex_auth(api.pref("customConnections").await)
                    .await?
                    .unwrap_or_default();
                self.modify_settings(|n| n.plex.saved_custom_connections = Some(current.clone()))?;
                current
            }
        };
        let value = crate::plex::share::with_relay(&original, relay);
        self.with_plex_auth(api.set_pref("customConnections", &value).await)
            .await?;

        // People already invited follow the picked libraries.
        let sections_changed = self
            .plex_share
            .lock()
            .applied
            .as_ref()
            .is_none_or(|a| a.1 != s.plex.shared_sections);
        if sections_changed && !s.plex.invited.is_empty() {
            let machine_id = s.plex.machine_id.clone().unwrap_or_default();
            let ids = self
                .with_plex_auth(tv.section_ids(&machine_id).await)
                .await?;
            let sections = crate::plex::share::section_ids(s.plex.shared_sections.as_deref(), &ids);
            for i in &s.plex.invited {
                self.with_plex_auth(tv.update_share(&machine_id, &i.share_id, &sections).await)
                    .await?;
            }
        }
        Ok(())
    }

    /// A YarmiplayTV user asked for the host's Plex with their plex.tv username.
    async fn invite_plex_user(&self, username: &str, syncplay_name: &str) -> Result<(), String> {
        let s = self.settings();
        let active = s.plex_share_effective()
            && self.plex_share.lock().applied.as_ref().is_some_and(|a| a.0);
        if !active {
            return Err("Plex sharing is off on this server".into());
        }
        let machine_id = s.plex.machine_id.clone().ok_or("Plex is not running")?;
        let tv = self.plex_tv()?;
        let ids = self
            .with_plex_auth(tv.section_ids(&machine_id).await)
            .await?;
        let sections = crate::plex::share::section_ids(s.plex.shared_sections.as_deref(), &ids);
        let invite = crate::plex::share::invite(
            &tv,
            &machine_id,
            &sections,
            username,
            syncplay_name,
            &s.plex.invited,
        )
        .await;
        let invite = self.with_plex_auth(invite).await?;
        if !s.plex.invited.iter().any(|i| i.share_id == invite.share_id) {
            info!(user = %invite.username, syncplay = %syncplay_name, "invited a Syncplay user to Plex");
            self.modify_settings(|n| n.plex.invited.push(invite))?;
            (self.notify)();
        }
        Ok(())
    }

    pub async fn plex_remove_invite(&self, share_id: String) -> Result<Snapshot, String> {
        let tv = self.plex_tv()?;
        self.with_plex_auth(tv.unshare(&share_id).await).await?;
        self.modify_settings(|n| n.plex.invited.retain(|i| i.share_id != share_id))?;
        (self.notify)();
        Ok(self.snapshot())
    }

    /// Ask plex.tv for a link code and wait (in the background) until the
    /// user enters it at plex.tv/link.
    pub async fn plex_link_start(self: &Arc<Self>) -> Result<Snapshot, String> {
        if self.plex_link.lock().is_some() {
            return Ok(self.snapshot());
        }
        let client_id = self.settings.read().plex.client_id.clone();
        let anon = PlexTv::new(&client_id, None);
        let pin = anon.create_pin().await?;
        let expires_in = Duration::from_secs(pin.expires_in.clamp(60, 1800));
        let link = PlexLink {
            code: pin.code.clone(),
            expires_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0)
                + expires_in.as_secs(),
        };
        let me = Arc::downgrade(self);
        let task = tokio::spawn(async move {
            let deadline = Instant::now() + expires_in;
            let result = loop {
                tokio::time::sleep(PLEX_LINK_POLL).await;
                if Instant::now() >= deadline {
                    break Err("The link code expired. Get a new one.".to_string());
                }
                match anon.check_pin(pin.id).await {
                    Ok(Some(token)) => break Ok(token),
                    Ok(None) => {}
                    Err(e) => warn!(error = %e, "Plex link check failed"),
                }
            };
            let Some(app) = me.upgrade() else { return };
            *app.plex_link.lock() = None;
            match result {
                Ok(token) => {
                    if let Err(e) = app.store_plex_token(token).await {
                        *app.plex_link_error.lock() = Some(e);
                    }
                    app.reconcile().await;
                }
                Err(e) => *app.plex_link_error.lock() = Some(e),
            }
            (app.notify)();
        });
        *self.plex_link_error.lock() = None;
        *self.plex_link.lock() = Some((link, task.abort_handle()));
        (self.notify)();
        Ok(self.snapshot())
    }

    async fn store_plex_token(&self, token: String) -> Result<(), String> {
        let client_id = self.settings.read().plex.client_id.clone();
        let account = PlexTv::new(&client_id, Some(token.clone()))
            .account()
            .await?;
        self.secrets.set(secrets::PLEX_TOKEN, &token)?;
        *self.plex_token.write() = Some(token);
        *self.plex_owned.lock() = None;
        info!(user = %account.username, "linked a plex.tv account");
        self.modify_settings(|s| s.plex.account_user = Some(account.username))
    }

    pub fn plex_link_cancel(&self) -> Snapshot {
        if let Some((_, task)) = self.plex_link.lock().take() {
            task.abort();
        }
        *self.plex_link_error.lock() = None;
        (self.notify)();
        self.snapshot()
    }

    pub async fn plex_logout(self: &Arc<Self>) -> Result<Snapshot, String> {
        self.secrets.delete(secrets::PLEX_TOKEN)?;
        *self.plex_token.write() = None;
        *self.plex_owned.lock() = None;
        self.modify_settings(|s| s.plex.account_user = None)?;
        self.reconcile().await;
        Ok(self.snapshot())
    }

    pub fn plex_refresh(&self) {
        *self.plex_owned.lock() = None;
        *self.plex_owned_failed_at.lock() = None;
        self.plex.refresh();
    }

    pub async fn plex_libraries(&self) -> Result<Vec<crate::plex::api::Library>, String> {
        let api = self.plex_api()?;
        self.with_plex_auth(api.libraries().await).await
    }

    pub async fn plex_add_library(
        &self,
        name: String,
        kind: String,
        path: String,
    ) -> Result<(), String> {
        if name.trim().is_empty() || path.trim().is_empty() {
            return Err("A library needs a name and a folder".into());
        }
        let kind = crate::plex::api::LibraryKind::parse(&kind)?;
        let api = self.plex_api()?;
        self.with_plex_auth(api.add_library(name.trim(), kind, path.trim()).await)
            .await
    }

    pub async fn plex_remove_library(&self, key: String) -> Result<(), String> {
        let api = self.plex_api()?;
        self.with_plex_auth(api.remove_library(&key).await).await?;
        let _ = self.modify_settings(|s| {
            if let Some(picked) = s.plex.shared_sections.as_mut() {
                picked.retain(|k| *k != key);
            }
        });
        Ok(())
    }

    pub async fn plex_add_path(&self, key: String, path: String) -> Result<(), String> {
        let api = self.plex_api()?;
        self.with_plex_auth(api.add_path(&key, path.trim()).await)
            .await
    }

    pub async fn plex_remove_path(&self, key: String, path: String) -> Result<(), String> {
        let api = self.plex_api()?;
        self.with_plex_auth(api.remove_path(&key, &path).await)
            .await
    }

    pub async fn plex_rescan(&self) -> Result<(), String> {
        let api = self.plex_api()?;
        self.with_plex_auth(api.rescan().await).await
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
