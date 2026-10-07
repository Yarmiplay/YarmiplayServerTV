//! User settings, stored as JSON in the OS config folder. Secrets (DuckDNS
//! token, Jellyfin access token) are not here; see [`crate::secrets`].

use crate::paths::write_atomic;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    pub syncplay: SyncplaySettings,
    pub jellyfin: JellyfinSettings,
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
    /// The password, or a device the host approved.
    Password,
    /// Only devices the host approved (YarmiplayTV).
    Approved,
}

impl SyncplayAccess {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Password => "password",
            Self::Approved => "approved",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct SyncplaySettings {
    pub enabled: bool,
    pub port: u16,
    pub access: SyncplayAccess,
    /// Plain text; shown in the UI so it can be shared with friends. Used in
    /// `password` mode only.
    pub password: String,
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
        }
    }
}

impl Settings {
    /// Jellyfin sharing as configured; it still needs Jellyfin running and signed in.
    pub fn share_effective(&self) -> bool {
        self.jellyfin.share_with_syncplay && !self.syncplay.vanilla_mode
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct TlsSettings {
    pub enabled: bool,
    /// `name` or `name.duckdns.org`.
    pub duckdns_domain: String,
    pub email: String,
    pub staging: bool,
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
        let ports = [
            ("Syncplay", self.syncplay.port),
            ("Jellyfin HTTP", self.jellyfin.http_port),
            ("Jellyfin HTTPS", self.jellyfin.https_port),
        ];
        for (name, port) in ports {
            if port < 1024 {
                return Err(format!("{name} port must be between 1024 and 65535"));
            }
        }
        if self.syncplay.port == self.jellyfin.http_port
            || self.syncplay.port == self.jellyfin.https_port
        {
            return Err("Syncplay and Jellyfin need different ports".into());
        }
        if self.jellyfin.http_port == self.jellyfin.https_port {
            return Err("Jellyfin HTTP and HTTPS ports must differ".into());
        }
        if self.syncplay.max_chat_message_length == 0 || self.syncplay.max_username_length == 0 {
            return Err("length limits must be at least 1".into());
        }
        if !(1..=2000).contains(&self.syncplay.relay_cache_gb) {
            return Err("the relay cache must be between 1 and 2000 GB".into());
        }
        match self.syncplay.access {
            SyncplayAccess::Password if self.syncplay.password.is_empty() => {
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
    }

    #[test]
    fn access_mode_validation() {
        let mut s = Settings::default();
        s.syncplay.access = SyncplayAccess::Password;
        assert!(s.validate().is_err());
        s.syncplay.password = "pw".into();
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
