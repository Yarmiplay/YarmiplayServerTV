//! YarmiplayTV extensions to Syncplay 1.7 (wire protocol v1).
//!
//! A client opts in with `features.yarmiplay: {"protocol": N}` in its Hello;
//! the server answers with its own `features.yarmiplay` and a per-connection
//! session token. Everything else travels as `{"Yarmiplay": {<sub>: ...}}`
//! lines, which only opted-in clients ever receive. Clients that don't opt in
//! get a byte-for-byte official server. See `docs/client-integration-prompt.md`.

use rand::Rng;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::VecDeque;

use super::room::ConnId;

pub const PROTOCOL: u64 = 1;
/// Jellyfin Quick Connect approvals a connection may ask for per minute.
pub const AUTHORIZE_PER_MINUTE: usize = 5;
/// Plex invites a connection may ask for per minute.
pub const INVITE_PER_MINUTE: usize = 3;
/// Longest plex.tv username or email accepted in a Plex invite request.
pub const MAX_PLEX_USERNAME: usize = 100;
/// Files one client may offer at once.
pub const MAX_OFFERED_FILES: usize = 200;

/// What this server shares with the Syncplay clients right now.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JellyfinShare {
    pub server_id: String,
    pub server_name: String,
    /// Jellyfin answers on the Syncplay port too.
    pub proxy: bool,
    /// Direct Jellyfin URLs (LAN, public, DuckDNS).
    pub addresses: Vec<String>,
}

/// The host's Plex Media Server, as shared with the Syncplay clients.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlexShare {
    /// Plex's `machineIdentifier`; `/identity` must report the same.
    pub machine_id: String,
    pub server_name: String,
    /// Plex answers on the Syncplay port too.
    pub proxy: bool,
    /// Direct Plex URLs (LAN, public).
    pub addresses: Vec<String>,
}

/// How clients at home can reach this server with TLS when the router doesn't
/// loop connections to the public address back in.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TlsInfo {
    /// The name on the certificate; clients verify it whatever address they dial.
    pub name: String,
    /// `ip:port` addresses on the host's network (and its public IPv6).
    pub lan: Vec<String>,
}

/// A connection's extension session.
#[derive(Debug)]
pub struct ExtSession {
    pub protocol: u64,
    pub token: String,
    authorize_times: VecDeque<f64>,
    invite_times: VecDeque<f64>,
}

/// Sliding one-minute window: true (and counted) if fewer than `limit`
/// requests happened in the last minute.
fn allow_in_window(times: &mut VecDeque<f64>, now: f64, limit: usize) -> bool {
    while times.front().is_some_and(|t| now - t >= 60.0) {
        times.pop_front();
    }
    if times.len() >= limit {
        return false;
    }
    times.push_back(now);
    true
}

impl ExtSession {
    pub fn new(protocol: u64) -> Self {
        Self {
            protocol,
            token: new_token(),
            authorize_times: VecDeque::new(),
            invite_times: VecDeque::new(),
        }
    }

    /// Sliding one-minute window for Quick Connect approvals.
    pub fn allow_authorize(&mut self, now: f64) -> bool {
        allow_in_window(&mut self.authorize_times, now, AUTHORIZE_PER_MINUTE)
    }

    /// Sliding one-minute window for Plex invites.
    pub fn allow_invite(&mut self, now: f64) -> bool {
        allow_in_window(&mut self.invite_times, now, INVITE_PER_MINUTE)
    }
}

/// A plex.tv username or email from a `plexInvite` request, or None if it
/// can't be one.
pub fn plex_username(value: &Value) -> Option<String> {
    let name = value.get("username")?.as_str()?.trim();
    let ok = !name.is_empty()
        && name.chars().count() <= MAX_PLEX_USERNAME
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || "@._-+".contains(c));
    ok.then(|| name.to_string())
}

pub fn new_token() -> String {
    let bytes: [u8; 24] = rand::thread_rng().gen();
    hex::encode(bytes)
}

/// The protocol a client asks for in its Hello features, if it opts in.
pub fn requested_protocol(features: &Value) -> Option<u64> {
    features
        .get("yarmiplay")?
        .get("protocol")?
        .as_u64()
        .filter(|p| *p >= 1)
}

/// Strip any `yarmiplay` claim and put back the negotiated one, so peers see
/// exactly which watchers have a session.
pub fn normalize_features(features: &mut Value, session: Option<&ExtSession>) {
    let Some(obj) = features.as_object_mut() else {
        return;
    };
    obj.remove("yarmiplay");
    if let Some(s) = session {
        obj.insert("yarmiplay".into(), json!({ "protocol": s.protocol }));
    }
}

/// `features` as a vanilla client should see them.
pub fn without_ext(features: &Value) -> Value {
    let mut f = features.clone();
    if let Some(obj) = f.as_object_mut() {
        obj.remove("yarmiplay");
    }
    f
}

#[derive(Debug, Clone, PartialEq)]
pub struct OfferedFile {
    pub name: String,
    pub size: u64,
    pub duration: f64,
    /// SHA-256 (hex) over the first and last MiB and the size; see `relay::quick_hash`.
    pub quick_hash: String,
}

pub fn parse_offer(value: &Value) -> Vec<OfferedFile> {
    value
        .get("files")
        .and_then(Value::as_array)
        .map(|files| {
            files
                .iter()
                .filter_map(|f| {
                    let name = f.get("name")?.as_str()?.trim();
                    let size = f.get("size")?.as_u64().filter(|s| *s > 0)?;
                    let hash = f.get("quickHash")?.as_str()?.to_ascii_lowercase();
                    let valid_hash =
                        hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit());
                    (!name.is_empty() && valid_hash).then(|| OfferedFile {
                        name: super::protocol::truncate(name, super::protocol::MAX_FILENAME_LENGTH),
                        size,
                        duration: f.get("duration").and_then(Value::as_f64).unwrap_or(0.0),
                        quick_hash: hash,
                    })
                })
                .take(MAX_OFFERED_FILES)
                .collect()
        })
        .unwrap_or_default()
}

/// Work the pure [`super::room::ServerState`] hands to the async side.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// An extension session entered `room` (login or room change).
    Joined {
        conn: ConnId,
        room: String,
    },
    /// The session ended (logout, timeout, or revoked by vanilla mode).
    Left {
        conn: ConnId,
    },
    Offer {
        conn: ConnId,
        room: String,
        files: Vec<OfferedFile>,
    },
    AuthorizeJellyfin {
        conn: ConnId,
        code: String,
    },
    /// Invite a plex.tv user to the host's Plex.
    InvitePlex {
        conn: ConnId,
        /// plex.tv username or email.
        username: String,
        /// Who asked, as known on the Syncplay server.
        syncplay_name: String,
    },
    UploadFailed {
        conn: ConnId,
        upload: String,
        error: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limit_is_a_sliding_minute() {
        let mut s = ExtSession::new(1);
        for i in 0..5 {
            assert!(s.allow_authorize(i as f64));
        }
        assert!(!s.allow_authorize(30.0));
        assert!(s.allow_authorize(60.5));
        assert!(!s.allow_authorize(60.6));
        for i in 0..3 {
            assert!(s.allow_invite(i as f64), "invites have their own budget");
        }
        assert!(!s.allow_invite(3.0));
    }

    #[test]
    fn plex_usernames_are_checked() {
        let name = |v: Value| plex_username(&json!({ "username": v }));
        assert_eq!(name(json!(" alice_99 ")).as_deref(), Some("alice_99"));
        assert_eq!(
            name(json!("a.b+c@example.com")).as_deref(),
            Some("a.b+c@example.com")
        );
        assert_eq!(name(json!("")), None);
        assert_eq!(name(json!("a b")), None);
        assert_eq!(name(json!("a/../b")), None);
        assert_eq!(name(json!("x".repeat(101))), None);
        assert_eq!(name(json!(5)), None);
    }

    #[test]
    fn offers_are_validated() {
        let h = "a".repeat(64);
        let files = parse_offer(&json!({ "files": [
            { "name": "a.mkv", "size": 10, "duration": 5.0, "quickHash": h },
            { "name": "", "size": 10, "quickHash": h },
            { "name": "b.mkv", "size": 0, "quickHash": h },
            { "name": "c.mkv", "size": 10, "quickHash": "zz" },
        ] }));
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].name, "a.mkv");
    }

    #[test]
    fn features_are_normalized() {
        let mut f = json!({ "chat": true, "yarmiplay": { "protocol": 7, "x": 1 } });
        normalize_features(&mut f, None);
        assert_eq!(f, json!({ "chat": true }));
        let s = ExtSession::new(1);
        normalize_features(&mut f, Some(&s));
        assert_eq!(f["yarmiplay"], json!({ "protocol": 1 }));
        assert_eq!(without_ext(&f), json!({ "chat": true }));
        assert_eq!(
            requested_protocol(&json!({ "yarmiplay": { "protocol": 1 } })),
            Some(1)
        );
        assert_eq!(requested_protocol(&json!({ "yarmiplay": true })), None);
    }
}
