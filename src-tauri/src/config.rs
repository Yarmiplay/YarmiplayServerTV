//! User settings, stored as JSON in the OS config folder. Secrets (DuckDNS
//! token, Jellyfin and Plex access tokens) are not here; see [`crate::secrets`].

use crate::paths::write_atomic;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    pub syncplay: SyncplaySettings,
    pub jellyfin: JellyfinSettings,
    pub plex: PlexSettings,
    pub tls: TlsSettings,
    pub browser: BrowserSettings,
    pub updates: UpdateSettings,
}

/// Updates from the latest GitHub release (see `crate::updates`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct UpdateSettings {
    /// Check GitHub on a schedule, download what's new and install it when
    /// nobody is using Syncplay. Off until the user opts in.
    pub auto: bool,
}

/// Access to the control panel from a web browser on this PC (see `crate::web`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct BrowserSettings {
    pub enabled: bool,
}

impl Default for BrowserSettings {
    fn default() -> Self {
        Self { enabled: true }
    }
}

/// Who may join the Syncplay server.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum SyncplayAccess {
    #[default]
    Open,
    /// The password, for everyone; device keys aren't checked.
    #[serde(rename = "passwordOnly")]
    PasswordOnly,
    /// The password, or a device the host approved.
    Password,
    /// Only devices the host approved (YarmiplayTV).
    Approved,
}

impl SyncplayAccess {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::PasswordOnly => "passwordOnly",
            Self::Password => "password",
            Self::Approved => "approved",
        }
    }

    pub fn uses_password(self) -> bool {
        matches!(self, Self::PasswordOnly | Self::Password)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct SyncplaySettings {
    pub enabled: bool,
    pub port: u16,
    pub access: SyncplayAccess,
    /// Plain text; shown in the UI so it can be shared with friends. Used in
    /// the `passwordOnly` and `password` modes only.
    pub password: String,
    /// Welcome message, sent to clients as the Hello reply's `motd`.
    pub motd: String,
    pub isolate_rooms: bool,
    pub disable_chat: bool,
    pub disable_ready: bool,
    pub max_chat_message_length: u32,
    pub max_username_length: u32,
    pub upnp: bool,
    /// Behave exactly like the official server: no YarmiplayTV extensions,
    /// no file relay, no Jellyfin on the Syncplay port.
    pub vanilla_mode: bool,
    /// Let YarmiplayTV clients stream each other's files through this server.
    pub file_relay: bool,
    /// Disk space the relay cache may use, in GB.
    pub relay_cache_gb: u32,
    /// Let Syncplay users create managed rooms, where only operators control playback.
    pub managed_rooms: bool,
    /// Origins of web pages allowed to join from a browser (`GET /yarmiplay/ws`)
    /// and to read the relay and MyAnimeList passthrough cross-origin.
    pub web_origins: Vec<String>,
}

impl SyncplaySettings {
    pub fn relay_effective(&self) -> bool {
        self.file_relay && !self.vanilla_mode
    }
}

impl Default for SyncplaySettings {
    fn default() -> Self {
        Self {
            enabled: false,
            port: 8999,
            access: SyncplayAccess::Open,
            password: String::new(),
            motd: String::new(),
            isolate_rooms: false,
            disable_chat: false,
            disable_ready: false,
            max_chat_message_length: 150,
            max_username_length: 150,
            upnp: false,
            vanilla_mode: false,
            file_relay: true,
            relay_cache_gb: 10,
            managed_rooms: true,
            web_origins: default_web_origins(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct JellyfinSettings {
    pub enabled: bool,
    pub http_port: u16,
    pub https_port: u16,
    pub upnp: bool,
    /// Set once the built-in first-run setup has completed.
    pub setup_complete: bool,
    pub admin_user: Option<String>,
    pub admin_user_id: Option<String>,
    /// Stable id this app uses when it talks to the Jellyfin API.
    pub device_id: String,
    /// Offer this Jellyfin to YarmiplayTV users on the Syncplay server, signed
    /// in as a hidden guest account.
    pub share_with_syncplay: bool,
    /// The guest account's Jellyfin user id; managed by the app.
    pub guest_user_id: Option<String>,
    /// Library folders whose loose episodes are sorted into show folders
    /// as they arrive.
    pub keep_sorted: Vec<String>,
}

impl Default for JellyfinSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            http_port: 8096,
            https_port: 8920,
            upnp: false,
            setup_complete: false,
            admin_user: None,
            admin_user_id: None,
            device_id: String::new(),
            share_with_syncplay: false,
            guest_user_id: None,
            keep_sorted: Vec::new(),
        }
    }
}

/// A Plex Media Server the user installed themselves. The app never installs,
/// starts or stops Plex; it watches it, manages libraries and shares it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct PlexSettings {
    /// Watch and manage the local Plex.
    pub enabled: bool,
    /// The port Plex listens on (32400 unless the user changed it in Plex).
    pub port: u16,
    pub upnp: bool,
    /// Stable `X-Plex-Client-Identifier` this app uses with plex.tv and Plex.
    pub client_id: String,
    /// The plex.tv account linked in the app; managed by the app.
    pub account_user: Option<String>,
    /// The Plex server's machine identifier, once seen; managed by the app.
    pub machine_id: Option<String>,
    /// Invite YarmiplayTV users on the Syncplay server as Plex friends.
    pub share_with_syncplay: bool,
    /// Library section ids shared with them; None shares every library.
    pub shared_sections: Option<Vec<String>>,
    /// People this app invited; the host's own Plex friends are never listed
    /// or touched. Managed by the app.
    pub invited: Vec<PlexInvite>,
    /// Plex's `customConnections` before sharing set it, restored when
    /// sharing is switched off. Managed by the app.
    pub saved_custom_connections: Option<String>,
}

impl Default for PlexSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            port: 32400,
            upnp: false,
            client_id: String::new(),
            account_user: None,
            machine_id: None,
            share_with_syncplay: false,
            shared_sections: None,
            invited: Vec::new(),
            saved_custom_connections: None,
        }
    }
}

/// A Plex friend invite made for a YarmiplayTV user on the Syncplay server.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct PlexInvite {
    /// The invitee's plex.tv user id.
    pub plex_user_id: String,
    pub username: String,
    /// Their Syncplay username when they asked.
    pub syncplay_name: String,
    /// plex.tv's id for the share, used to update or remove it.
    pub share_id: String,
    /// Unix seconds.
    pub at: u64,
}

impl Settings {
    /// Jellyfin sharing as configured; it still needs Jellyfin running and signed in.
    pub fn share_effective(&self) -> bool {
        self.jellyfin.share_with_syncplay && !self.syncplay.vanilla_mode
    }

    /// Plex sharing as configured; it still needs Plex running and a linked account.
    pub fn plex_share_effective(&self) -> bool {
        self.plex.enabled && self.plex.share_with_syncplay && !self.syncplay.vanilla_mode
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct TlsSettings {
    pub enabled: bool,
    /// Where the certificate's name comes from.
    pub provider: TlsProvider,
    /// `name` or `name.duckdns.org`.
    pub duckdns_domain: String,
    /// The user's own domain (`tv.example.com`), for `TlsProvider::Domain`.
    pub own_domain: String,
    pub email: String,
    pub staging: bool,
    /// Publish this PC's public IPv6 address on DuckDNS too.
    pub ipv6: bool,
    /// A second DuckDNS name (same account) kept pointing at this PC's LAN
    /// address and added to the certificate, so browsers at home can connect
    /// locally. Empty for none.
    pub lan_domain: String,
}

impl Default for TlsSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: TlsProvider::DuckDns,
            duckdns_domain: String::new(),
            own_domain: String::new(),
            email: String::new(),
            staging: false,
            ipv6: true,
            lan_domain: String::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub enum TlsProvider {
    /// A DuckDNS name, validated through DuckDNS's API (no inbound port).
    #[default]
    DuckDns,
    /// A domain of the user's own pointed at this PC, validated on port 80.
    Domain,
}

/// Web pages allowed to use the Syncplay port from a browser (WebSocket and CORS).
pub fn default_web_origins() -> Vec<String> {
    [
        "https://tv.yarmiplay.com",
        "http://localhost:5173",
        "http://127.0.0.1:5173",
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

/// `https://Example.com/` -> `https://example.com`; `None` unless it's an http(s) origin.
pub fn normalize_origin(origin: &str) -> Option<String> {
    let o = origin.trim().trim_end_matches('/').to_ascii_lowercase();
    let rest = o
        .strip_prefix("https://")
        .or_else(|| o.strip_prefix("http://"))?;
    let valid = !rest.is_empty()
        && !rest.contains(['/', '?', '#', ' ', '@'])
        && rest
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':' | '[' | ']'));
    valid.then_some(o)
}

impl Settings {
    pub fn load(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(raw) => Self::parse(&raw).unwrap_or_else(|e| {
                tracing::warn!(error = %e, "settings file unreadable, using defaults");
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    fn parse(raw: &str) -> Result<Self, serde_json::Error> {
        let value: serde_json::Value = serde_json::from_str(raw)?;
        let has_access = value.pointer("/syncplay/access").is_some();
        let mut s: Self = serde_json::from_value(value)?;
        // Settings from before access modes: a password meant password mode.
        if !has_access && !s.syncplay.password.is_empty() {
            s.syncplay.access = SyncplayAccess::Password;
        }
        Ok(s)
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        let json = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        write_atomic(path, &json)
    }

    pub fn validate(&self) -> Result<(), String> {
        let mut ports = vec![
            ("Syncplay", self.syncplay.port),
            ("Jellyfin HTTP", self.jellyfin.http_port),
            ("Jellyfin HTTPS", self.jellyfin.https_port),
        ];
        if self.plex.enabled {
            ports.push(("Plex", self.plex.port));
        }
        for (name, port) in &ports {
            if *port < 1024 {
                return Err(format!("{name} port must be between 1024 and 65535"));
            }
        }
        for (i, (a, pa)) in ports.iter().enumerate() {
            if let Some((b, _)) = ports[i + 1..].iter().find(|(_, pb)| pb == pa) {
                return Err(format!("{a} and {b} need different ports"));
            }
        }
        if self.syncplay.max_chat_message_length == 0 || self.syncplay.max_username_length == 0 {
            return Err("length limits must be at least 1".into());
        }
        if !(1..=2000).contains(&self.syncplay.relay_cache_gb) {
            return Err("the relay cache must be between 1 and 2000 GB".into());
        }
        match self.syncplay.access {
            a if a.uses_password() && self.syncplay.password.is_empty() => {
                return Err("Enter a Syncplay password, or choose who can join another way".into())
            }
            SyncplayAccess::Approved if self.syncplay.vanilla_mode => {
                return Err("Approved devices only needs vanilla Syncplay mode off".into())
            }
            _ => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upnp_is_off_by_default() {
        let s = Settings::default();
        assert!(!s.syncplay.upnp);
        assert!(!s.jellyfin.upnp);
        assert!(!s.syncplay.enabled && !s.jellyfin.enabled && !s.tls.enabled);
    }

    #[test]
    fn partial_json_fills_defaults() {
        let s: Settings = serde_json::from_str(r#"{"syncplay":{"port":9000}}"#).unwrap();
        assert_eq!(s.syncplay.port, 9000);
        assert_eq!(s.jellyfin.http_port, 8096);
        assert_eq!(s.syncplay.max_chat_message_length, 150);
        assert!(s.browser.enabled);
        assert!(!s.updates.auto);
        assert!(s.syncplay.file_relay && !s.syncplay.vanilla_mode);
        assert_eq!(s.syncplay.relay_cache_gb, 10);
        assert!(!s.jellyfin.share_with_syncplay);
    }

    #[test]
    fn vanilla_mode_turns_the_extensions_off() {
        let mut s = Settings::default();
        s.jellyfin.share_with_syncplay = true;
        assert!(s.syncplay.relay_effective() && s.share_effective());
        s.syncplay.vanilla_mode = true;
        assert!(!s.syncplay.relay_effective() && !s.share_effective());
    }

    #[test]
    fn validation_rejects_port_clashes() {
        let mut s = Settings::default();
        assert!(s.validate().is_ok());
        s.syncplay.port = 8096;
        assert!(s.validate().is_err());
        s.syncplay.port = 80;
        assert!(s.validate().is_err());

        let mut s = Settings::default();
        s.syncplay.port = 32400;
        assert!(
            s.validate().is_ok(),
            "Plex's port only counts while Plex is on"
        );
        s.plex.enabled = true;
        assert_eq!(
            s.validate().unwrap_err(),
            "Syncplay and Plex need different ports"
        );
        s.syncplay.port = 8999;
        s.jellyfin.https_port = 8096;
        assert_eq!(
            s.validate().unwrap_err(),
            "Jellyfin HTTP and Jellyfin HTTPS need different ports"
        );
    }

    #[test]
    fn plex_sharing_needs_plex_on_and_vanilla_mode_off() {
        let mut s = Settings::default();
        assert_eq!(s.plex.port, 32400);
        s.plex.share_with_syncplay = true;
        assert!(!s.plex_share_effective());
        s.plex.enabled = true;
        assert!(s.plex_share_effective());
        s.syncplay.vanilla_mode = true;
        assert!(!s.plex_share_effective());
    }

    #[test]
    fn access_mode_comes_from_the_old_password() {
        let s = Settings::parse(r#"{"syncplay":{"password":"pw"}}"#).unwrap();
        assert_eq!(s.syncplay.access, SyncplayAccess::Password);
        let s = Settings::parse(r#"{"syncplay":{}}"#).unwrap();
        assert_eq!(s.syncplay.access, SyncplayAccess::Open);
        let s = Settings::parse(r#"{"syncplay":{"password":"pw","access":"open"}}"#).unwrap();
        assert_eq!(s.syncplay.access, SyncplayAccess::Open);
        let s = Settings::parse(r#"{"syncplay":{"access":"approved"}}"#).unwrap();
        assert_eq!(s.syncplay.access, SyncplayAccess::Approved);
        let s = Settings::parse(r#"{"syncplay":{"access":"passwordOnly"}}"#).unwrap();
        assert_eq!(s.syncplay.access, SyncplayAccess::PasswordOnly);
        assert!(serde_json::to_string(&s)
            .unwrap()
            .contains(r#""access":"passwordOnly""#));
    }

    #[test]
    fn access_mode_validation() {
        let mut s = Settings::default();
        s.syncplay.access = SyncplayAccess::Password;
        assert!(s.validate().is_err());
        s.syncplay.access = SyncplayAccess::PasswordOnly;
        assert!(s.validate().is_err());
        s.syncplay.password = "pw".into();
        assert!(s.validate().is_ok());
        s.syncplay.access = SyncplayAccess::Password;
        assert!(s.validate().is_ok());
        s.syncplay.access = SyncplayAccess::Approved;
        assert!(s.validate().is_ok());
        s.syncplay.vanilla_mode = true;
        assert_eq!(
            s.validate().unwrap_err(),
            "Approved devices only needs vanilla Syncplay mode off"
        );
    }

    #[test]
    fn save_and_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let mut s = Settings::default();
        s.syncplay.motd = "hi".into();
        s.save(&path).unwrap();
        assert_eq!(Settings::load(&path), s);
    }
}
