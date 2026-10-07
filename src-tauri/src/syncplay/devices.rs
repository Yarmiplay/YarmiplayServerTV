//! Devices the host approved for Syncplay access. A YarmiplayTV client keeps
//! one ECDSA P-256 key per server (chosen by this server's random `serverId`)
//! and proves it holds the key by signing a challenge; see
//! `docs/yarmiplaytv-device-access-prompt.md`.
//!
//! Approved keys are saved in `syncplay-devices.json`. Requests waiting for
//! the host live in memory only, each with a channel that carries the
//! decision to the connection that is waiting.

use crate::paths::write_atomic;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::watch;
use tracing::warn;

/// First line of the bytes a device signs.
pub const SIGNED_PREFIX: &str = "YarmiplayServerTV-device-auth-v1";
pub const MAX_DEVICE_NAME: usize = 60;
const MAX_PENDING: usize = 50;
const MAX_PENDING_PER_IP: usize = 3;
/// A request is forgotten this long after its device was last connected.
const PENDING_TTL: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Waiting,
    Approved,
    Denied,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ApprovedDevice {
    pub fingerprint: String,
    /// Base64 SubjectPublicKeyInfo DER.
    pub public_key: String,
    pub name: String,
    /// Unix seconds.
    pub approved_at: i64,
    pub last_seen: i64,
    pub last_username: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PendingDevice {
    pub fingerprint: String,
    pub name: String,
    pub username: String,
    pub ip: String,
    pub requested_at: i64,
    /// The device is still connected and waiting.
    pub connected: bool,
}

#[derive(Debug, Clone, Serialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DevicesStatus {
    pub pending: Vec<PendingDevice>,
    pub approved: Vec<ApprovedDevice>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct DeviceFile {
    server_id: String,
    devices: Vec<ApprovedDevice>,
}

struct Pending {
    view: PendingDevice,
    public_key: String,
    decision: watch::Sender<Decision>,
    connections: usize,
    last_connected: Instant,
}

struct Inner {
    file: DeviceFile,
    pending: Vec<Pending>,
}

pub struct DeviceStore {
    path: Option<PathBuf>,
    inner: Mutex<Inner>,
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn clean_name(name: &str) -> String {
    let name: String = name
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_DEVICE_NAME)
        .collect();
    let name = name.trim();
    if name.is_empty() {
        "Unnamed device".into()
    } else {
        name.to_string()
    }
}

impl DeviceStore {
    /// Load the saved devices, creating this server's id on first use.
    pub fn load(path: PathBuf) -> Self {
        let mut file: DeviceFile = std::fs::read(&path)
            .ok()
            .and_then(|raw| match serde_json::from_slice(&raw) {
                Ok(f) => Some(f),
                Err(e) => {
                    warn!(error = %e, "Syncplay device list unreadable, starting empty");
                    None
                }
            })
            .unwrap_or_default();
        let fresh = file.server_id.is_empty();
        if fresh {
            file.server_id = hex::encode(rand::random::<[u8; 16]>());
        }
        let store = Self {
            path: Some(path),
            inner: Mutex::new(Inner {
                file,
                pending: Vec::new(),
            }),
        };
        if fresh {
            if let Err(e) = store.save(&store.inner.lock()) {
                warn!(error = %e, "could not save the Syncplay device list");
            }
        }
        store
    }

    /// Not saved anywhere (tests).
    pub fn in_memory() -> Self {
        Self {
            path: None,
            inner: Mutex::new(Inner {
                file: DeviceFile {
                    server_id: hex::encode(rand::random::<[u8; 16]>()),
                    devices: Vec::new(),
                },
                pending: Vec::new(),
            }),
        }
    }

    fn save(&self, inner: &Inner) -> Result<(), String> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let json = serde_json::to_vec_pretty(&inner.file).map_err(|e| e.to_string())?;
        write_atomic(path, &json)
    }

    pub fn server_id(&self) -> String {
        self.inner.lock().file.server_id.clone()
    }

    pub fn is_approved(&self, fingerprint: &str) -> bool {
        self.inner
            .lock()
            .file
            .devices
            .iter()
            .any(|d| d.fingerprint == fingerprint)
    }

    /// An approved device logged in.
    pub fn seen(&self, fingerprint: &str, username: &str) {
        let mut inner = self.inner.lock();
        let Some(d) = inner
            .file
            .devices
            .iter_mut()
            .find(|d| d.fingerprint == fingerprint)
        else {
            return;
        };
        d.last_seen = unix_now();
        d.last_username = username.to_string();
        if let Err(e) = self.save(&inner) {
            warn!(error = %e, "could not save the Syncplay device list");
        }
    }

    /// Ask the host to approve a device; the receiver gets the decision. A
    /// device that asks again joins its earlier request.
    pub fn request(
        &self,
        fingerprint: &str,
        public_key: &str,
        name: &str,
        username: &str,
        ip: &str,
    ) -> Result<watch::Receiver<Decision>, String> {
        let mut inner = self.inner.lock();
        let now = Instant::now();
        inner
            .pending
            .retain(|p| p.connections > 0 || now.duration_since(p.last_connected) < PENDING_TTL);
        if let Some(p) = inner
            .pending
            .iter_mut()
            .find(|p| p.view.fingerprint == fingerprint)
        {
            p.view.name = clean_name(name);
            p.view.username = username.to_string();
            p.view.ip = ip.to_string();
            p.connections += 1;
            p.last_connected = now;
            return Ok(p.decision.subscribe());
        }
        if inner.pending.len() >= MAX_PENDING
            || inner.pending.iter().filter(|p| p.view.ip == ip).count() >= MAX_PENDING_PER_IP
        {
            return Err("Too many devices are waiting for approval; try again later".into());
        }
        let (tx, rx) = watch::channel(Decision::Waiting);
        inner.pending.push(Pending {
            view: PendingDevice {
                fingerprint: fingerprint.to_string(),
                name: clean_name(name),
                username: username.to_string(),
                ip: ip.to_string(),
                requested_at: unix_now(),
                connected: true,
            },
            public_key: public_key.to_string(),
            decision: tx,
            connections: 1,
            last_connected: now,
        });
        Ok(rx)
    }

    /// A connection waiting on `fingerprint`'s request went away.
    pub fn disconnected(&self, fingerprint: &str) {
        let mut inner = self.inner.lock();
        if let Some(p) = inner
            .pending
            .iter_mut()
            .find(|p| p.view.fingerprint == fingerprint)
        {
            p.connections = p.connections.saturating_sub(1);
            p.last_connected = Instant::now();
        }
    }

    fn take_pending(inner: &mut Inner, fingerprint: &str) -> Result<Pending, String> {
        let i = inner
            .pending
            .iter()
            .position(|p| p.view.fingerprint == fingerprint)
            .ok_or("That request is no longer waiting")?;
        Ok(inner.pending.remove(i))
    }

    pub fn approve(&self, fingerprint: &str) -> Result<(), String> {
        let mut inner = self.inner.lock();
        let p = Self::take_pending(&mut inner, fingerprint)?;
        let now = unix_now();
        inner.file.devices.push(ApprovedDevice {
            fingerprint: p.view.fingerprint.clone(),
            public_key: p.public_key.clone(),
            name: p.view.name.clone(),
            approved_at: now,
            last_seen: now,
            last_username: p.view.username.clone(),
        });
        let saved = self.save(&inner);
        let _ = p.decision.send(Decision::Approved);
        saved
    }

    pub fn deny(&self, fingerprint: &str) -> Result<(), String> {
        let p = Self::take_pending(&mut self.inner.lock(), fingerprint)?;
        let _ = p.decision.send(Decision::Denied);
        Ok(())
    }

    /// Forget an approved device. The caller disconnects it.
    pub fn remove(&self, fingerprint: &str) -> Result<(), String> {
        let mut inner = self.inner.lock();
        let before = inner.file.devices.len();
        inner.file.devices.retain(|d| d.fingerprint != fingerprint);
        if inner.file.devices.len() == before {
            return Err("No such device".into());
        }
        self.save(&inner)
    }

    pub fn rename(&self, fingerprint: &str, name: &str) -> Result<(), String> {
        let mut inner = self.inner.lock();
        let d = inner
            .file
            .devices
            .iter_mut()
            .find(|d| d.fingerprint == fingerprint)
            .ok_or("No such device")?;
        d.name = clean_name(name);
        self.save(&inner)
    }

    pub fn status(&self) -> DevicesStatus {
        let inner = self.inner.lock();
        let now = Instant::now();
        let mut pending: Vec<PendingDevice> = inner
            .pending
            .iter()
            .filter(|p| p.connections > 0 || now.duration_since(p.last_connected) < PENDING_TTL)
            .map(|p| PendingDevice {
                connected: p.connections > 0,
                ..p.view.clone()
            })
            .collect();
        pending.sort_by_key(|p| std::cmp::Reverse(p.requested_at));
        let mut approved = inner.file.devices.clone();
        approved.sort_by_key(|d| d.name.to_lowercase());
        DevicesStatus { pending, approved }
    }
}

/// SHA-256 of the SubjectPublicKeyInfo DER: the first 16 hex digits in
/// uppercase, in groups of four (`3F2A-91BC-04DE-7710`).
pub fn fingerprint(spki: &[u8]) -> String {
    let hex = hex::encode_upper(&Sha256::digest(spki)[..8]);
    hex.as_bytes()
        .chunks(4)
        .map(|c| String::from_utf8_lossy(c).into_owned())
        .collect::<Vec<_>>()
        .join("-")
}

/// The bytes a device signs for a challenge.
pub fn signed_message(server_id: &str, nonce: &str) -> String {
    format!("{SIGNED_PREFIX}\n{server_id}\n{nonce}")
}

/// The uncompressed point of a P-256 SubjectPublicKeyInfo.
fn p256_point(spki: &[u8]) -> Option<Vec<u8>> {
    use x509_parser::oid_registry::{OID_EC_P256, OID_KEY_TYPE_EC_PUBLIC_KEY};
    use x509_parser::prelude::FromDer;
    use x509_parser::x509::SubjectPublicKeyInfo;
    let (rest, info) = SubjectPublicKeyInfo::from_der(spki).ok()?;
    if !rest.is_empty() || info.algorithm.algorithm != OID_KEY_TYPE_EC_PUBLIC_KEY {
        return None;
    }
    let curve = info.algorithm.parameters.as_ref()?.as_oid().ok()?;
    (curve == OID_EC_P256).then(|| info.subject_public_key.data.to_vec())
}

/// Check a device's answer to a challenge: `signature` (base64 DER ECDSA)
/// over [`signed_message`] by the key in `public_key` (base64 SPKI DER).
/// Returns the key's fingerprint.
pub fn verify(public_key: &str, signature: &str, server_id: &str, nonce: &str) -> Option<String> {
    let spki = B64.decode(public_key.trim()).ok()?;
    let sig = B64.decode(signature.trim()).ok()?;
    let point = p256_point(&spki)?;
    ring::signature::UnparsedPublicKey::new(&ring::signature::ECDSA_P256_SHA256_ASN1, point)
        .verify(signed_message(server_id, nonce).as_bytes(), &sig)
        .ok()?;
    Some(fingerprint(&spki))
}

pub fn new_nonce() -> String {
    B64.encode(rand::random::<[u8; 32]>())
}

#[cfg(test)]
pub mod testkey {
    //! A YarmiplayTV-style device key for tests.
    use super::*;
    use ring::rand::SystemRandom;
    use ring::signature::{EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_ASN1_SIGNING};

    /// SubjectPublicKeyInfo header for an uncompressed P-256 point.
    const SPKI_PREFIX: &str = "3059301306072a8648ce3d020106082a8648ce3d030107034200";

    pub struct DeviceKey {
        pair: EcdsaKeyPair,
        rng: SystemRandom,
    }

    impl Default for DeviceKey {
        fn default() -> Self {
            Self::new()
        }
    }

    impl DeviceKey {
        pub fn new() -> Self {
            let rng = SystemRandom::new();
            let pkcs8 =
                EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &rng).unwrap();
            let pair =
                EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, pkcs8.as_ref(), &rng)
                    .unwrap();
            Self { pair, rng }
        }

        pub fn spki(&self) -> Vec<u8> {
            let mut der = hex::decode(SPKI_PREFIX).unwrap();
            der.extend_from_slice(self.pair.public_key().as_ref());
            der
        }

        pub fn public_key(&self) -> String {
            B64.encode(self.spki())
        }

        pub fn fingerprint(&self) -> String {
            fingerprint(&self.spki())
        }

        pub fn sign(&self, server_id: &str, nonce: &str) -> String {
            let sig = self
                .pair
                .sign(&self.rng, signed_message(server_id, nonce).as_bytes())
                .unwrap();
            B64.encode(sig.as_ref())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testkey::DeviceKey;
    use super::*;

    #[test]
    fn signatures_are_checked() {
        let key = DeviceKey::new();
        let sig = key.sign("abc", "nonce");
        assert_eq!(
            verify(&key.public_key(), &sig, "abc", "nonce"),
            Some(key.fingerprint())
        );
        assert_eq!(verify(&key.public_key(), &sig, "abc", "other"), None);
        assert_eq!(verify(&key.public_key(), &sig, "xyz", "nonce"), None);
        let other = DeviceKey::new();
        assert_eq!(verify(&other.public_key(), &sig, "abc", "nonce"), None);
        assert_eq!(verify("not base64!", &sig, "abc", "nonce"), None);
        // An Ed25519 SubjectPublicKeyInfo is refused.
        let ed = B64
            .encode(hex::decode(format!("302a300506032b6570032100{}", "11".repeat(32))).unwrap());
        assert_eq!(verify(&ed, &sig, "abc", "nonce"), None);
    }

    #[test]
    fn fingerprint_format() {
        let fp = fingerprint(b"anything");
        assert_eq!(fp.len(), 19);
        assert_eq!(fp.matches('-').count(), 3);
        assert!(fp
            .chars()
            .all(|c| c == '-' || c.is_ascii_digit() || c.is_ascii_uppercase()));
        let digest = hex::encode_upper(Sha256::digest(b"anything"));
        assert_eq!(fp.replace('-', ""), digest[..16]);
    }

    #[test]
    fn approve_deny_and_limits() {
        let store = DeviceStore::in_memory();
        let rx = store
            .request("A", "ka", "Phone\u{7}", "ana", "1.1.1.1")
            .unwrap();
        assert_eq!(store.status().pending[0].name, "Phone");
        // Asking again joins the same request.
        let rx2 = store.request("A", "ka", "Phone", "ana", "1.1.1.1").unwrap();
        assert_eq!(store.status().pending.len(), 1);
        store.approve("A").unwrap();
        assert_eq!(*rx.borrow(), Decision::Approved);
        assert_eq!(*rx2.borrow(), Decision::Approved);
        assert!(store.is_approved("A"));
        assert!(store.status().pending.is_empty());
        assert_eq!(store.status().approved[0].last_username, "ana");

        let rx = store.request("B", "kb", "TV", "bo", "1.1.1.1").unwrap();
        store.deny("B").unwrap();
        assert_eq!(*rx.borrow(), Decision::Denied);
        assert!(!store.is_approved("B"));
        assert!(store.deny("B").is_err());

        for fp in ["C", "D", "E"] {
            store.request(fp, "k", "x", "u", "2.2.2.2").unwrap();
        }
        assert!(store.request("F", "k", "x", "u", "2.2.2.2").is_err());
        assert!(store.request("F", "k", "x", "u", "3.3.3.3").is_ok());

        store.rename("A", &"n".repeat(100)).unwrap();
        assert_eq!(store.status().approved[0].name.len(), MAX_DEVICE_NAME);
        store.remove("A").unwrap();
        assert!(!store.is_approved("A"));
    }

    #[test]
    fn saved_devices_survive_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("devices.json");
        let store = DeviceStore::load(path.clone());
        let id = store.server_id();
        assert_eq!(id.len(), 32);
        store.request("A", "ka", "Phone", "ana", "1.1.1.1").unwrap();
        store.approve("A").unwrap();
        store.request("B", "kb", "TV", "bo", "1.1.1.1").unwrap();
        let again = DeviceStore::load(path);
        assert_eq!(again.server_id(), id);
        assert!(again.is_approved("A"));
        assert!(again.status().pending.is_empty());
    }
}
