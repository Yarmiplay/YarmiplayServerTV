//! Secrets live in the OS credential store (Windows Credential Manager, macOS
//! Keychain, Secret Service on Linux). If no credential store is available
//! (e.g. a headless Linux session) they fall back to an owner-only file in
//! the app config folder. Values are registered with the log redactor.

use crate::logs;
use crate::paths::write_atomic;
use std::collections::BTreeMap;
use std::path::PathBuf;

const SERVICE: &str = "YarmiplayServerTV";
pub const DUCKDNS_TOKEN: &str = "duckdns-token";
pub const JELLYFIN_TOKEN: &str = "jellyfin-token";

pub struct Secrets {
    fallback_file: PathBuf,
}

impl Secrets {
    pub fn new(config_dir: PathBuf) -> Self {
        let s = Self { fallback_file: config_dir.join("secrets.json") };
        for key in [DUCKDNS_TOKEN, JELLYFIN_TOKEN] {
            if let Some(v) = s.get(key) {
                logs::register_secret(&v);
            }
        }
        s
    }

    /// Env override for local testing; never persisted.
    fn env_override(key: &str) -> Option<String> {
        let var = match key {
            DUCKDNS_TOKEN => "YARMIPLAYSERVERTV_DUCKDNS_TOKEN",
            _ => return None,
        };
        std::env::var(var).ok().filter(|v| !v.trim().is_empty())
    }

    pub fn get(&self, key: &str) -> Option<String> {
        if let Some(v) = Self::env_override(key) {
            logs::register_secret(&v);
            return Some(v);
        }
        if let Ok(entry) = keyring::Entry::new(SERVICE, key) {
            match entry.get_password() {
                Ok(v) => return Some(v),
                Err(keyring::Error::NoEntry) => {}
                Err(e) => tracing::debug!(error = %e, key, "credential store read failed"),
            }
        }
        self.read_fallback().remove(key)
    }

    pub fn is_set(&self, key: &str) -> bool {
        self.get(key).is_some()
    }

    pub fn set(&self, key: &str, value: &str) -> Result<(), String> {
        logs::register_secret(value);
        let stored = keyring::Entry::new(SERVICE, key)
            .and_then(|e| e.set_password(value))
            .is_ok();
        let mut map = self.read_fallback();
        if stored {
            if map.remove(key).is_some() {
                self.write_fallback(&map)?;
            }
            return Ok(());
        }
        tracing::warn!(key, "OS credential store unavailable, storing secret in an owner-only file");
        map.insert(key.to_string(), value.to_string());
        self.write_fallback(&map)
    }

    pub fn delete(&self, key: &str) -> Result<(), String> {
        if let Ok(entry) = keyring::Entry::new(SERVICE, key) {
            let _ = entry.delete_credential();
        }
        let mut map = self.read_fallback();
        if map.remove(key).is_some() {
            self.write_fallback(&map)?;
        }
        Ok(())
    }

    fn read_fallback(&self) -> BTreeMap<String, String> {
        std::fs::read_to_string(&self.fallback_file)
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default()
    }

    fn write_fallback(&self, map: &BTreeMap<String, String>) -> Result<(), String> {
        if map.is_empty() {
            let _ = std::fs::remove_file(&self.fallback_file);
            return Ok(());
        }
        let json = serde_json::to_vec(map).map_err(|e| e.to_string())?;
        write_atomic(&self.fallback_file, &json)
    }
}
