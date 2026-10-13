//! Server-side Syncplay state: watchers, rooms, playstate and shared
//! playlists. Mirrors the behaviour of the official server
//! (`syncplay/server.py`, `SyncServerProtocol`) so the official clients and
//! YarmiplayTV see exactly what they expect, including message order.
//!
//! Pure and synchronous: callers pass the current time, and outgoing lines go
//! to each connection's channel. Work for the async side (file relay,
//! Jellyfin) is queued as [`Effect`]s.
//!
//! The YarmiplayTV extensions ([`super::ext`]) only exist for clients that ask
//! for them; everyone else sees the official server.

use super::ext::{self, Effect, ExtSession, JellyfinShare, PlexShare, TlsInfo};
use super::managed;
use super::protocol::{
    self, line, truncate, PingService, MAX_FILENAME_LENGTH, MAX_ROOM_NAME_LENGTH,
};
use crate::config::SyncplayAccess;
use serde::Serialize;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use tokio::sync::mpsc::UnboundedSender;

pub type ConnId = u64;

#[derive(Debug)]
pub enum Out {
    Line(String),
    Close,
}

pub const APPROVED_ONLY: &str = "This server only admits devices approved by the host. Connect with YarmiplayTV and request access.";

/// What to do with a Hello.
#[derive(Debug, Clone, PartialEq)]
pub enum Access {
    Admit,
    /// Ask the YarmiplayTV client to prove its device key first.
    Challenge(SyncplayAccess),
    Reject(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct SyncplayOptions {
    /// Device challenges happen in `password` and `approved` mode only.
    pub access: SyncplayAccess,
    /// Checked whenever it is set, except in `approved` mode.
    pub password: String,
    /// Welcome message sent in the Hello reply.
    pub motd: String,
    pub isolate_rooms: bool,
    pub disable_chat: bool,
    pub disable_ready: bool,
    pub max_chat_message_length: usize,
    pub max_username_length: usize,
    /// No extension sessions at all; turning it on revokes the existing ones.
    pub vanilla_mode: bool,
    /// Advertise the file relay to extension sessions.
    pub file_relay: bool,
    /// Managed rooms (`+name:HASH`), where only operators control playback.
    pub managed_rooms: bool,
    /// Secret the managed-room names are derived from; kept across restarts.
    pub room_salt: String,
    /// Origins of web pages allowed to use the port from a browser (normalized).
    pub web_origins: Vec<String>,
}

impl Default for SyncplayOptions {
    fn default() -> Self {
        Self {
            access: SyncplayAccess::Open,
            password: String::new(),
            motd: String::new(),
            isolate_rooms: false,
            disable_chat: false,
            disable_ready: false,
            max_chat_message_length: 150,
            max_username_length: 150,
            vanilla_mode: false,
            file_relay: false,
            managed_rooms: true,
            room_salt: String::new(),
            web_origins: crate::config::default_web_origins(),
        }
    }
}

struct Watcher {
    name: String,
    room: String,
    file: Option<Value>,
    ready: Option<bool>,
    features: Value,
    version: String,
    position: Option<f64>,
    last_updated: f64,
    ping: PingService,
    client_latency_calc: f64,
    client_latency_arrival: f64,
    server_ignoring: u32,
    client_ignoring: u32,
    ext: Option<ExtSession>,
    /// Fingerprint of the approved device key this login used.
    device: Option<String>,
    tx: UnboundedSender<Out>,
}

impl Watcher {
    fn send(&self, value: Value) {
        let _ = self.tx.send(Out::Line(line(&value)));
    }

    /// Peer `features` as this watcher may see them.
    fn view_features(&self, features: &Value) -> Value {
        if self.ext.is_some() {
            features.clone()
        } else {
            ext::without_ext(features)
        }
    }

    fn send_set(&self, setting: Value) {
        self.send(json!({ "Set": setting }));
    }

    /// Position extrapolated while the room plays.
    fn position_at(&self, now: f64, room_paused: bool) -> Option<f64> {
        let p = self.position?;
        Some(if room_paused {
            p
        } else {
            p + (now - self.last_updated)
        })
    }

    /// Syncplay's `Watcher.__lt__` usability: needs a position and a file.
    fn comparable(&self) -> bool {
        self.position.is_some() && self.file.is_some()
    }

    fn send_state(
        &mut self,
        position: Option<f64>,
        paused: bool,
        do_seek: bool,
        set_by: Option<&str>,
        forced: bool,
        now: f64,
    ) {
        let processing = if self.client_latency_arrival > 0.0 {
            now - self.client_latency_arrival
        } else {
            0.0
        };
        let mut ping = json!({ "latencyCalculation": now, "serverRtt": self.ping.rtt() });
        if self.client_latency_calc != 0.0 {
            ping["clientLatencyCalculation"] = json!(self.client_latency_calc + processing);
            self.client_latency_calc = 0.0;
        }
        let mut state = json!({
            "ping": ping,
            "playstate": {
                "position": position.filter(|p| *p != 0.0).unwrap_or(0.0),
                "paused": paused,
                "doSeek": do_seek,
                "setBy": set_by,
            }
        });
        if forced {
            self.server_ignoring += 1;
        }
        if self.server_ignoring > 0 || self.client_ignoring > 0 {
            let mut ignoring = Map::new();
            if self.server_ignoring > 0 {
                ignoring.insert("server".into(), json!(self.server_ignoring));
            }
            if self.client_ignoring > 0 {
                ignoring.insert("client".into(), json!(self.client_ignoring));
                self.client_ignoring = 0;
            }
            state["ignoringOnTheFly"] = Value::Object(ignoring);
        }
        self.send(json!({ "State": state }));
    }
}

struct Room {
    members: Vec<ConnId>,
    paused: bool,
    set_by: Option<String>,
    position: Option<f64>,
    last_update: f64,
    playlist: Vec<Value>,
    playlist_index: Value,
    /// A managed room: only `controllers` may play, pause, seek or change the playlist.
    controlled: bool,
    controllers: Vec<ConnId>,
}

impl Room {
    fn new(now: f64, controlled: bool) -> Self {
        Self {
            members: Vec::new(),
            paused: true,
            set_by: None,
            position: Some(0.0),
            last_update: now,
            playlist: Vec::new(),
            playlist_index: Value::Null,
            controlled,
            controllers: Vec::new(),
        }
    }

    /// The official server's `canControl`: everyone in an ordinary room.
    fn can_control(&self, id: ConnId) -> bool {
        !self.controlled || self.controllers.contains(&id)
    }
}

/// Settings that are a room's, the same for everyone in it (`{"Yarmiplay": {"roomSettings": …}}`).
/// Kept by room name while the server runs, also while the room is empty.
#[derive(Debug, Clone, Default, PartialEq)]
struct RoomSettings {
    /// Skip anime openings and endings automatically.
    auto_skip: bool,
    /// Who changed them last.
    set_by: Option<String>,
}

impl RoomSettings {
    fn message(&self) -> Value {
        json!({ "autoSkip": self.auto_skip, "setBy": self.set_by })
    }
}

/// How many rooms' settings are kept; beyond it the settings of rooms nobody is in are forgotten.
const MAX_ROOM_SETTINGS: usize = 500;

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RoomInfo {
    pub name: String,
    pub users: Vec<String>,
    pub paused: bool,
    /// A managed room (`+name:HASH`).
    pub controlled: bool,
    /// Its operators.
    pub controllers: Vec<String>,
}

/// Username, room and client version from a Hello.
fn hello_args(hello: &Value) -> Result<(&str, &str, &str), String> {
    let username = hello
        .get("username")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let room = hello
        .get("room")
        .and_then(|r| r.get("name"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let version = hello
        .get("realversion")
        .or_else(|| hello.get("version"))
        .and_then(Value::as_str);
    match (username, room, version) {
        (Some(u), Some(r), Some(v)) => Ok((u, r, v)),
        _ => Err("Not enough Hello arguments".into()),
    }
}

pub struct ServerState {
    opts: SyncplayOptions,
    password_md5: Option<String>,
    watchers: HashMap<ConnId, Watcher>,
    rooms: HashMap<String, Room>,
    room_settings: HashMap<String, RoomSettings>,
    jellyfin: Option<JellyfinShare>,
    plex: Option<PlexShare>,
    tls: Option<TlsInfo>,
    effects: Vec<Effect>,
}

impl ServerState {
    pub fn new(opts: SyncplayOptions) -> Self {
        let mut s = Self {
            opts: SyncplayOptions::default(),
            password_md5: None,
            watchers: HashMap::new(),
            rooms: HashMap::new(),
            room_settings: HashMap::new(),
            jellyfin: None,
            plex: None,
            tls: None,
            effects: Vec::new(),
        };
        s.set_options(opts);
        s
    }

    pub fn set_options(&mut self, opts: SyncplayOptions) {
        self.password_md5 = (!opts.password.is_empty()).then(|| protocol::md5_hex(&opts.password));
        let revoke = opts.vanilla_mode && !self.opts.vanilla_mode;
        let switched_to = |mode| opts.access == mode && self.opts.access != mode;
        let approved_only = switched_to(SyncplayAccess::Approved);
        let password_only = switched_to(SyncplayAccess::PasswordOnly);
        let before = self.capabilities();
        if !opts.managed_rooms {
            for room in self.rooms.values_mut() {
                room.controlled = false;
                room.controllers.clear();
            }
        }
        self.opts = opts;
        if revoke {
            self.revoke_sessions();
        } else if self.capabilities() != before {
            self.broadcast_ext_state();
        }
        if approved_only {
            self.drop_where(|w| w.device.is_none(), APPROVED_ONLY);
        }
        if password_only {
            self.drop_where(|w| w.device.is_some(), "Password required");
        }
    }

    fn drop_where(&mut self, pred: impl Fn(&Watcher) -> bool, message: &str) {
        let ids: Vec<ConnId> = self
            .watchers
            .iter()
            .filter(|(_, w)| pred(w))
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            self.drop_watcher(id, None, message);
        }
    }

    /// Send `status` (to an extension session) and an `Error`, then disconnect.
    fn drop_watcher(&mut self, id: ConnId, status: Option<Value>, message: &str) {
        let Some(w) = self.watchers.get(&id) else {
            return;
        };
        if let (Some(status), Some(_)) = (status, &w.ext) {
            w.send(json!({ "Yarmiplay": { "status": status } }));
        }
        w.send(json!({ "Error": { "message": message } }));
        let _ = w.tx.send(Out::Close);
        self.remove(id);
    }

    /// The host removed a device: disconnect every login that used it.
    pub fn kick_device(&mut self, fingerprint: &str) -> usize {
        let ids: Vec<ConnId> = self
            .watchers
            .iter()
            .filter(|(_, w)| w.device.as_deref() == Some(fingerprint))
            .map(|(id, _)| *id)
            .collect();
        for id in &ids {
            let status = json!({ "state": "revoked", "fingerprint": fingerprint });
            self.drop_watcher(*id, Some(status), "The host removed this device");
        }
        ids.len()
    }

    /// Check the Hello's password (when one is set).
    pub fn password_ok(&self, hello: &Value) -> Result<(), String> {
        let Some(expected) = &self.password_md5 else {
            return Ok(());
        };
        match hello
            .get("password")
            .and_then(Value::as_str)
            .filter(|p| !p.is_empty())
        {
            None => Err("Password required".into()),
            Some(p) if !p.eq_ignore_ascii_case(expected) => Err("Wrong password supplied".into()),
            Some(_) => Ok(()),
        }
    }

    /// Whether the Hello carries the YarmiplayTV extension opt-in that this server honours.
    pub fn extended(&self, hello: &Value) -> bool {
        !self.opts.vanilla_mode
            && hello
                .get("features")
                .and_then(ext::requested_protocol)
                .is_some()
    }

    /// Decide on a Hello before logging in. `devices` says whether device
    /// keys can be checked at all.
    pub fn check_access(&self, hello: &Value, devices: bool) -> Access {
        if let Err(e) = hello_args(hello) {
            return Access::Reject(e);
        }
        let mode = self.opts.access;
        if mode == SyncplayAccess::Approved && (self.opts.vanilla_mode || !devices) {
            return Access::Reject(APPROVED_ONLY.into());
        }
        let challenges = matches!(mode, SyncplayAccess::Password | SyncplayAccess::Approved);
        if devices && challenges && self.extended(hello) {
            return Access::Challenge(mode);
        }
        if mode == SyncplayAccess::Approved {
            return Access::Reject(APPROVED_ONLY.into());
        }
        match self.password_ok(hello) {
            Ok(()) => Access::Admit,
            Err(e) => Access::Reject(e),
        }
    }

    pub fn vanilla(&self) -> bool {
        self.opts.vanilla_mode
    }

    /// Whether a web page at `origin` may use the port from a browser.
    pub fn origin_allowed(&self, origin: &str) -> bool {
        crate::config::normalize_origin(origin).is_some_and(|o| self.opts.web_origins.contains(&o))
    }

    /// What extension sessions are told about the shared Jellyfin.
    pub fn set_jellyfin(&mut self, share: Option<JellyfinShare>) {
        if self.jellyfin != share {
            self.jellyfin = share;
            self.broadcast_ext_state();
        }
    }

    /// What extension sessions are told about the shared Plex.
    pub fn set_plex(&mut self, share: Option<PlexShare>) {
        if self.plex != share {
            self.plex = share;
            self.broadcast_ext_state();
        }
    }

    /// The loaded certificate's name and the home addresses (`None` = no
    /// certificate, so no TLS on the Syncplay port).
    pub fn set_tls(&mut self, tls: Option<TlsInfo>) {
        if self.tls != tls {
            self.tls = tls;
            self.broadcast_ext_state();
        }
    }

    /// `tls` as clients see it: `null` without a certificate or in vanilla mode.
    pub fn tls_message(&self) -> Value {
        match (&self.tls, self.opts.vanilla_mode) {
            (Some(t), false) => serde_json::to_value(t).unwrap_or(Value::Null),
            _ => Value::Null,
        }
    }

    /// `GET /yarmiplay/info`: what a client would see in Hello, without logging in.
    pub fn ext_info(&self) -> Value {
        json!({
            "server": "YarmiplayServerTV",
            "version": env!("CARGO_PKG_VERSION"),
            "protocol": ext::PROTOCOL,
            "capabilities": self.capabilities(),
            "tls": self.tls_message(),
        })
    }

    pub fn take_effects(&mut self) -> Vec<Effect> {
        std::mem::take(&mut self.effects)
    }

    fn capabilities(&self) -> Value {
        let on = !self.opts.vanilla_mode;
        json!({
            "fileRelay": on && self.opts.file_relay,
            "jellyfin": on && self.jellyfin.is_some(),
            "plex": on && self.plex.is_some(),
            "https": on && self.tls.is_some(),
            "roomSettings": on,
            "websocket": on,
            "mal": on,
        })
    }

    /// `room`'s shared settings as sent to clients.
    fn room_settings_message(&self, room: &str) -> Value {
        self.room_settings
            .get(room)
            .cloned()
            .unwrap_or_default()
            .message()
    }

    /// `{"roomSettings": {"autoSkip": …}}` from `id`: changes its room's settings for everyone in it.
    /// In a managed room only operators may; anyone else just gets the room's settings back.
    fn set_room_settings(&mut self, id: ConnId, value: &Value) {
        let Some(w) = self.watchers.get(&id) else {
            return;
        };
        let room = w.room.clone();
        if !self.can_control(id) {
            let current = self.room_settings_message(&room);
            self.send_ext(id, "roomSettings", current);
            return;
        }
        let name = w.name.clone();
        if self.room_settings.len() >= MAX_ROOM_SETTINGS && !self.room_settings.contains_key(&room)
        {
            let rooms = &self.rooms;
            self.room_settings.retain(|r, _| rooms.contains_key(r));
        }
        let settings = self.room_settings.entry(room.clone()).or_default();
        if let Some(on) = value.get("autoSkip").and_then(Value::as_bool) {
            settings.auto_skip = on;
        }
        settings.set_by = Some(name);
        let message = settings.message();
        self.send_ext_room(&room, "roomSettings", &message);
    }

    fn plex_message(&self) -> Value {
        match (&self.plex, self.opts.vanilla_mode) {
            (Some(p), false) => {
                let mut v = serde_json::to_value(p).unwrap_or_else(|_| json!({}));
                v["available"] = json!(true);
                v
            }
            _ => json!({ "available": false }),
        }
    }

    fn jellyfin_message(&self) -> Value {
        match (&self.jellyfin, self.opts.vanilla_mode) {
            (Some(j), false) => {
                let mut v = serde_json::to_value(j).unwrap_or_else(|_| json!({}));
                v["available"] = json!(true);
                v
            }
            _ => json!({ "available": false }),
        }
    }

    fn broadcast_ext_state(&self) {
        let caps = self.capabilities();
        let jf = self.jellyfin_message();
        let plex = self.plex_message();
        let tls = self.tls_message();
        for w in self.watchers.values().filter(|w| w.ext.is_some()) {
            w.send(json!({ "Yarmiplay": { "capabilities": caps, "jellyfin": jf, "plex": plex, "tls": tls } }));
        }
    }

    /// Vanilla mode switched on: tell every session the extensions are gone,
    /// then forget their tokens.
    fn revoke_sessions(&mut self) {
        let caps = self.capabilities();
        let mut ids: Vec<ConnId> = self
            .watchers
            .iter()
            .filter(|(_, w)| w.ext.is_some())
            .map(|(id, _)| *id)
            .collect();
        ids.sort_unstable();
        for id in ids {
            let w = self.watchers.get_mut(&id).unwrap();
            w.send(json!({ "Yarmiplay": { "capabilities": caps } }));
            w.ext = None;
            ext::normalize_features(&mut w.features, None);
            self.effects.push(Effect::Left { conn: id });
        }
    }

    /// Send `{"Yarmiplay": {sub: value}}` to `id` if it has a session.
    pub fn send_ext(&self, id: ConnId, sub: &str, value: Value) -> bool {
        match self.watchers.get(&id) {
            Some(w) if w.ext.is_some() => {
                w.send(json!({ "Yarmiplay": { sub: value } }));
                true
            }
            _ => false,
        }
    }

    /// Send to every session in `room`.
    pub fn send_ext_room(&self, room: &str, sub: &str, value: &Value) {
        let Some(r) = self.rooms.get(room) else {
            return;
        };
        for id in &r.members {
            self.send_ext(*id, sub, value.clone());
        }
    }

    /// The connection and room a session token belongs to.
    pub fn session_for_token(&self, token: &str) -> Option<(ConnId, String)> {
        if token.is_empty() {
            return None;
        }
        self.watchers.iter().find_map(|(id, w)| {
            w.ext
                .as_ref()
                .filter(|s| s.token == token)
                .map(|_| (*id, w.room.clone()))
        })
    }

    pub fn is_logged(&self, id: ConnId) -> bool {
        self.watchers.contains_key(&id)
    }

    pub fn user_count(&self) -> usize {
        self.watchers.len()
    }

    pub fn rooms(&self) -> Vec<RoomInfo> {
        let mut rooms: Vec<RoomInfo> = self
            .rooms
            .iter()
            .map(|(name, room)| RoomInfo {
                name: name.clone(),
                users: room
                    .members
                    .iter()
                    .filter_map(|id| self.watchers.get(id))
                    .map(|w| w.name.clone())
                    .collect(),
                paused: room.paused,
                controlled: room.controlled,
                controllers: room
                    .controllers
                    .iter()
                    .filter_map(|id| self.watchers.get(id))
                    .map(|w| w.name.clone())
                    .collect(),
            })
            .collect();
        rooms.sort_by(|a, b| a.name.cmp(&b.name));
        rooms
    }

    fn features(&self, session: Option<&ExtSession>, device: bool) -> Value {
        let mut f = json!({
            "isolateRooms": self.opts.isolate_rooms,
            "readiness": !self.opts.disable_ready,
            "managedRooms": self.opts.managed_rooms,
            "persistentRooms": false,
            "chat": !self.opts.disable_chat,
            "maxChatMessageLength": self.opts.max_chat_message_length,
            "maxUsernameLength": self.opts.max_username_length,
            "maxRoomNameLength": MAX_ROOM_NAME_LENGTH,
            "maxFilenameLength": MAX_FILENAME_LENGTH,
        });
        if let Some(s) = session {
            f["yarmiplay"] = json!({
                "server": "YarmiplayServerTV",
                "version": env!("CARGO_PKG_VERSION"),
                "protocol": s.protocol,
                "capabilities": self.capabilities(),
                "tls": self.tls_message(),
                "access": self.opts.access.as_str(),
                "device": if device { "approved" } else { "none" },
            });
        }
        f
    }

    /// Watchers that `sender` "sees": its room when rooms are isolated, else everyone.
    fn audience(&self, sender: ConnId) -> Vec<ConnId> {
        if self.opts.isolate_rooms {
            self.room_members(sender)
        } else {
            let mut ids: Vec<ConnId> = self.watchers.keys().copied().collect();
            ids.sort_unstable();
            ids
        }
    }

    fn room_members(&self, sender: ConnId) -> Vec<ConnId> {
        self.watchers
            .get(&sender)
            .and_then(|w| self.rooms.get(&w.room))
            .map(|r| r.members.clone())
            .unwrap_or_default()
    }

    /// Whether `id` may change its room's playstate and playlist.
    fn can_control(&self, id: ConnId) -> bool {
        self.watchers
            .get(&id)
            .and_then(|w| self.rooms.get(&w.room))
            .is_none_or(|r| r.can_control(id))
    }

    /// The official server's `isController`: an operator of a managed room.
    fn is_controller(&self, id: ConnId) -> bool {
        self.watchers
            .get(&id)
            .and_then(|w| self.rooms.get(&w.room))
            .is_some_and(|r| r.controlled && r.controllers.contains(&id))
    }

    fn send_to(&self, ids: &[ConnId], value: &Value) {
        for id in ids {
            if let Some(w) = self.watchers.get(id) {
                w.send(value.clone());
            }
        }
    }

    fn user_setting(&self, id: ConnId, file: Option<&Value>, event: Option<Value>) -> Value {
        let w = &self.watchers[&id];
        let mut user = json!({ "room": { "name": w.room } });
        if let Some(f) = file {
            user["file"] = f.clone();
        }
        if let Some(e) = event {
            user["event"] = e;
        }
        json!({ "Set": { "user": { w.name.clone(): user } } })
    }

    fn ready_message(&self, id: ConnId, manually_initiated: bool) -> Value {
        let w = &self.watchers[&id];
        json!({ "Set": { "ready": { "username": w.name, "isReady": w.ready, "manuallyInitiated": manually_initiated } } })
    }

    fn free_username(&self, wanted: &str) -> String {
        let mut name = truncate(wanted, self.opts.max_username_length);
        let taken: Vec<String> = self
            .watchers
            .values()
            .map(|w| w.name.to_lowercase())
            .collect();
        while taken.contains(&name.to_lowercase()) {
            name.push('_');
        }
        name
    }

    /// Validate a Hello (without device keys) and log the client in. On error
    /// the caller sends `{"Error": {"message": ...}}` and closes the connection.
    pub fn handle_hello(
        &mut self,
        id: ConnId,
        hello: &Value,
        tx: UnboundedSender<Out>,
        now: f64,
    ) -> Result<(), String> {
        match self.check_access(hello, false) {
            Access::Admit => self.login(id, hello, tx, now, None),
            Access::Reject(e) => Err(e),
            Access::Challenge(_) => Err("Device authentication failed".into()),
        }
    }

    /// Log in a client whose access was already checked; `device` is the
    /// approved key it proved, if any.
    pub fn login(
        &mut self,
        id: ConnId,
        hello: &Value,
        tx: UnboundedSender<Out>,
        now: f64,
        device: Option<String>,
    ) -> Result<(), String> {
        let (username, room, version) = hello_args(hello)?;
        let name = self.free_username(username);
        let mut features = hello
            .get("features")
            .cloned()
            .filter(Value::is_object)
            .unwrap_or_else(|| json!({}));
        let session = ext::requested_protocol(&features)
            .filter(|_| !self.opts.vanilla_mode)
            .map(|p| ExtSession::new(p.min(ext::PROTOCOL)));
        ext::normalize_features(&mut features, session.as_ref());
        self.watchers.insert(
            id,
            Watcher {
                name: name.clone(),
                room: String::new(),
                file: None,
                ready: None,
                features,
                version: version.to_string(),
                position: None,
                last_updated: now,
                ping: PingService::default(),
                client_latency_calc: 0.0,
                client_latency_arrival: 0.0,
                server_ignoring: 0,
                client_ignoring: 0,
                ext: session,
                device,
                tx,
            },
        );
        self.set_watcher_room(id, room, true, now);

        let w = &self.watchers[&id];
        w.send(json!({
            "Hello": {
                "username": w.name,
                "room": { "name": w.room },
                "version": w.version,
                "realversion": protocol::SERVER_VERSION,
                "motd": truncate(&self.opts.motd, protocol::MAX_MOTD_LENGTH),
                "features": self.features(w.ext.as_ref(), w.device.is_some()),
            }
        }));
        if let Some(s) = &w.ext {
            // The room's settings ride with the session: a client takes them only once it has one.
            w.send(json!({ "Yarmiplay": {
                "session": { "token": s.token, "protocol": s.protocol },
                "capabilities": self.capabilities(),
                "jellyfin": self.jellyfin_message(),
                "plex": self.plex_message(),
                "tls": self.tls_message(),
                "roomSettings": self.room_settings_message(&w.room),
            } }));
        }
        tracing::info!(user = %name, room = %w.room, client = %version, ext = w.ext.is_some(), "Syncplay user joined");
        Ok(())
    }

    fn move_watcher(&mut self, id: ConnId, room_name: &str, now: f64) {
        let old_room = self.watchers[&id].room.clone();
        if self.opts.isolate_rooms && !old_room.is_empty() {
            let event = self.user_setting(id, None, Some(json!({ "left": true })));
            let others: Vec<ConnId> = self
                .room_members(id)
                .into_iter()
                .filter(|m| *m != id)
                .collect();
            self.send_to(&others, &event);
        }
        self.detach(id);
        let controlled = self.opts.managed_rooms && managed::is_controlled(room_name).is_some();
        let room = self
            .rooms
            .entry(room_name.to_string())
            .or_insert_with(|| Room::new(now, controlled));
        let had_members = !room.members.is_empty();
        room.members.push(id);
        self.watchers.get_mut(&id).unwrap().room = room_name.to_string();
        if had_members {
            let pos = self.room_position(room_name, now);
            self.watchers.get_mut(&id).unwrap().position = pos;
        }
    }

    fn detach(&mut self, id: ConnId) {
        let Some(old) = self.watchers.get(&id).map(|w| w.room.clone()) else {
            return;
        };
        if let Some(room) = self.rooms.get_mut(&old) {
            room.members.retain(|m| *m != id);
            room.controllers.retain(|m| *m != id);
            if room.members.is_empty() {
                self.rooms.remove(&old);
            }
        }
    }

    fn set_watcher_room(&mut self, id: ConnId, room_name: &str, as_join: bool, now: f64) {
        let room_name = truncate(room_name, MAX_ROOM_NAME_LENGTH);
        self.move_watcher(id, &room_name, now);
        if as_join {
            let w = &self.watchers[&id];
            let others: Vec<ConnId> = self.audience(id).into_iter().filter(|m| *m != id).collect();
            for other in others {
                let Some(o) = self.watchers.get(&other) else {
                    continue;
                };
                let event = json!({ "joined": true, "version": w.version, "features": o.view_features(&w.features) });
                o.send(self.user_setting(id, None, Some(event)));
            }
        } else {
            let msg = self.user_setting(id, None, None);
            self.send_to(&self.audience(id), &msg);
        }
        let ready = self.ready_message(id, false);
        self.send_to(&self.room_members(id), &ready);

        let room = &self.rooms[&room_name];
        let w = &self.watchers[&id];
        w.send_set(json!({ "playlistChange": { "user": room.set_by, "files": room.playlist } }));
        w.send_set(
            json!({ "playlistIndex": { "user": room.set_by, "index": room.playlist_index } }),
        );
        if w.ext.is_some() {
            // On logging in they come with the session instead.
            if !as_join {
                let settings = self.room_settings_message(&room_name);
                self.send_ext(id, "roomSettings", settings);
            }
            self.effects.push(Effect::Joined {
                conn: id,
                room: room_name,
            });
        }
    }

    /// Room position, re-anchored on the furthest-behind watcher at most once per second.
    /// In a managed room only its operators count, and without any the position runs on.
    fn room_position(&mut self, room_name: &str, now: f64) -> Option<f64> {
        let room = self.rooms.get(room_name)?;
        let age = now - room.last_update;
        let anchors = if room.controlled {
            &room.controllers
        } else {
            &room.members
        };
        if !anchors.is_empty() && age > 1.0 {
            let paused = room.paused;
            let mut best: Option<ConnId> = None;
            for id in anchors {
                let Some(w) = self.watchers.get(id) else {
                    continue;
                };
                best = match best {
                    None => Some(*id),
                    Some(b) => {
                        let bw = &self.watchers[&b];
                        let less = w.comparable()
                            && (!bw.comparable()
                                || w.position_at(now, paused) < bw.position_at(now, paused));
                        if less {
                            Some(*id)
                        } else {
                            Some(b)
                        }
                    }
                };
            }
            let best = best?;
            let (name, pos) = {
                let w = &self.watchers[&best];
                (w.name.clone(), w.position_at(now, paused))
            };
            let room = self.rooms.get_mut(room_name)?;
            room.set_by = Some(name);
            room.position = pos;
            room.last_update = now;
            pos
        } else {
            room.position
                .map(|p| p + if room.paused { 0.0 } else { age })
        }
    }

    fn set_room_position(&mut self, room_name: &str, position: Option<f64>, set_by: &str) {
        let Some(room) = self.rooms.get_mut(room_name) else {
            return;
        };
        room.position = position;
        if !room.members.is_empty() {
            room.set_by = Some(set_by.to_string());
        }
        for id in room.members.clone() {
            if let Some(w) = self.watchers.get_mut(&id) {
                w.position = position;
            }
        }
    }

    /// Handle one message from a logged-in client. Returns false to drop the connection.
    pub fn handle_message(&mut self, id: ConnId, message: &Map<String, Value>, now: f64) -> bool {
        for (command, value) in message {
            match command.as_str() {
                "Set" => self.handle_set(id, value, now),
                "State" => self.handle_state(id, value, now),
                "List" => self.send_list(id),
                "Chat" => self.handle_chat(id, value),
                "Yarmiplay" => self.handle_ext(id, value, now),
                "Error" => {
                    tracing::debug!(error = %value, "Syncplay client reported an error");
                    return false;
                }
                "Hello" | "TLS" => {}
                other => tracing::debug!(command = other, "ignoring unknown Syncplay command"),
            }
            if !self.watchers.contains_key(&id) {
                return false;
            }
        }
        true
    }

    /// `{"Yarmiplay": {...}}` from a client. Without a session it is ignored,
    /// as the official server ignores unknown commands.
    fn handle_ext(&mut self, id: ConnId, value: &Value, now: f64) {
        let Some(commands) = value.as_object() else {
            return;
        };
        if self.watchers.get(&id).is_none_or(|w| w.ext.is_none()) {
            return;
        }
        for (command, value) in commands {
            match command.as_str() {
                "offer" => {
                    let room = self.watchers[&id].room.clone();
                    self.effects.push(Effect::Offer {
                        conn: id,
                        room,
                        files: ext::parse_offer(value),
                    });
                }
                "jellyfinAuthorize" => {
                    let code: String = value
                        .get("code")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .chars()
                        .filter(char::is_ascii_alphanumeric)
                        .take(32)
                        .collect();
                    let refuse = if self.jellyfin.is_none() {
                        Some("Jellyfin sharing is off on this server")
                    } else if code.is_empty() {
                        Some("No Quick Connect code")
                    } else if !self
                        .watchers
                        .get_mut(&id)
                        .and_then(|w| w.ext.as_mut())
                        .is_some_and(|s| s.allow_authorize(now))
                    {
                        Some("Too many attempts; wait a minute")
                    } else {
                        None
                    };
                    match refuse {
                        Some(error) => {
                            self.send_ext(
                                id,
                                "jellyfinAuthorize",
                                json!({ "code": code, "ok": false, "error": error }),
                            );
                        }
                        None => self
                            .effects
                            .push(Effect::AuthorizeJellyfin { conn: id, code }),
                    }
                }
                "plexInvite" => {
                    let username = ext::plex_username(value);
                    let refuse = if self.plex.is_none() {
                        Some("Plex sharing is off on this server")
                    } else if username.is_none() {
                        Some("No plex.tv username")
                    } else if !self
                        .watchers
                        .get_mut(&id)
                        .and_then(|w| w.ext.as_mut())
                        .is_some_and(|s| s.allow_invite(now))
                    {
                        Some("Too many attempts; wait a minute")
                    } else {
                        None
                    };
                    match (refuse, username) {
                        (None, Some(username)) => {
                            let syncplay_name = self.watchers[&id].name.clone();
                            self.effects.push(Effect::InvitePlex {
                                conn: id,
                                username,
                                syncplay_name,
                            });
                        }
                        (error, _) => {
                            self.send_ext(
                                id,
                                "plexInvite",
                                json!({ "ok": false, "error": error.unwrap_or("No plex.tv username") }),
                            );
                        }
                    }
                }
                "roomSettings" => self.set_room_settings(id, value),
                "uploadFailed" => {
                    let upload = value.get("id").and_then(Value::as_str).unwrap_or("");
                    let error = value.get("error").and_then(Value::as_str).unwrap_or("");
                    self.effects.push(Effect::UploadFailed {
                        conn: id,
                        upload: upload.to_string(),
                        error: truncate(error, 200),
                    });
                }
                other => tracing::debug!(command = other, "ignoring unknown Yarmiplay command"),
            }
        }
    }

    fn handle_set(&mut self, id: ConnId, settings: &Value, now: f64) {
        let Some(settings) = settings.as_object() else {
            return;
        };
        for (command, value) in settings {
            match command.as_str() {
                "room" => {
                    if let Some(name) = value
                        .get("name")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                    {
                        self.set_watcher_room(id, name, false, now);
                    }
                }
                "file" => self.set_file(id, value.clone()),
                "ready" => {
                    let is_ready = value.get("isReady").and_then(Value::as_bool);
                    let manually = value
                        .get("manuallyInitiated")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    let other = value.get("username").and_then(Value::as_str);
                    let own_name = self.watchers[&id].name.clone();
                    if let Some(other) = other.filter(|u| *u != own_name) {
                        self.set_others_ready(id, other, is_ready, manually);
                        continue;
                    }
                    self.watchers.get_mut(&id).unwrap().ready = is_ready;
                    let msg = self.ready_message(id, manually);
                    self.send_to(&self.room_members(id), &msg);
                }
                "playlistChange" => {
                    let files = value
                        .get("files")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    self.set_playlist(id, files);
                }
                "playlistIndex" => {
                    let index = value.get("index").cloned().unwrap_or(Value::Null);
                    self.set_playlist_index(id, index);
                }
                "features" => {
                    if value.is_object() {
                        let w = self.watchers.get_mut(&id).unwrap();
                        w.features = value.clone();
                        ext::normalize_features(&mut w.features, w.ext.as_ref());
                    }
                }
                "controllerAuth" => {
                    let room = value.get("room").and_then(Value::as_str);
                    let password = value.get("password").and_then(Value::as_str);
                    self.auth_room_controller(id, room, password);
                }
                _ => {}
            }
        }
    }

    fn set_file(&mut self, id: ConnId, mut file: Value) {
        if let Some(name) = file.get("name").and_then(Value::as_str) {
            file["name"] = json!(truncate(name, MAX_FILENAME_LENGTH));
        }
        let has = file.is_object() && file.as_object().is_some_and(|o| !o.is_empty());
        self.watchers.get_mut(&id).unwrap().file = has.then(|| file.clone());
        if has {
            let msg = self.user_setting(id, Some(&file), None);
            self.send_to(&self.audience(id), &msg);
        }
    }

    fn set_playlist(&mut self, id: ConnId, files: Vec<Value>) {
        let total: usize = files
            .iter()
            .filter_map(Value::as_str)
            .map(|f| f.chars().count())
            .sum();
        let valid = files.len() <= protocol::PLAYLIST_MAX_ITEMS
            && total <= protocol::PLAYLIST_MAX_CHARACTERS
            && files.iter().all(Value::is_string)
            && self.can_control(id);
        let (name, room_name) = {
            let w = &self.watchers[&id];
            (w.name.clone(), w.room.clone())
        };
        if valid {
            if let Some(room) = self.rooms.get_mut(&room_name) {
                room.playlist = files.clone();
                room.set_by = Some(name.clone());
            }
            let msg = json!({ "Set": { "playlistChange": { "user": name, "files": files } } });
            self.send_to(&self.room_members(id), &msg);
        } else if let Some(room) = self.rooms.get(&room_name) {
            let w = &self.watchers[&id];
            w.send_set(json!({ "playlistChange": { "user": room_name, "files": room.playlist } }));
            w.send_set(
                json!({ "playlistIndex": { "user": room_name, "index": room.playlist_index } }),
            );
        }
    }

    fn set_playlist_index(&mut self, id: ConnId, index: Value) {
        let (name, room_name) = {
            let w = &self.watchers[&id];
            (w.name.clone(), w.room.clone())
        };
        if !self.can_control(id) {
            if let Some(room) = self.rooms.get(&room_name) {
                self.watchers[&id].send_set(
                    json!({ "playlistIndex": { "user": room_name, "index": room.playlist_index } }),
                );
            }
            return;
        }
        if let Some(room) = self.rooms.get_mut(&room_name) {
            room.playlist_index = index.clone();
            room.set_by = Some(name.clone());
        }
        let msg = json!({ "Set": { "playlistIndex": { "user": name, "index": index } } });
        self.send_to(&self.room_members(id), &msg);
    }

    fn handle_chat(&mut self, id: ConnId, value: &Value) {
        if self.opts.disable_chat {
            return;
        }
        let Some(text) = value.as_str() else { return };
        let message = truncate(text, self.opts.max_chat_message_length);
        let msg = json!({ "Chat": { "message": message, "username": self.watchers[&id].name } });
        self.send_to(&self.room_members(id), &msg);
    }

    fn send_list(&self, id: ConnId) {
        let ids: Vec<ConnId> = if self.opts.isolate_rooms {
            self.room_members(id)
        } else {
            self.audience(id)
        };
        let me = &self.watchers[&id];
        let mut list = Map::new();
        for wid in ids {
            let Some(w) = self.watchers.get(&wid) else {
                continue;
            };
            let room = list.entry(w.room.clone()).or_insert_with(|| json!({}));
            room[w.name.clone()] = json!({
                "file": w.file.clone().unwrap_or_else(|| json!({})),
                "controller": self.is_controller(wid),
                "isReady": w.ready,
                "features": me.view_features(&w.features),
            });
        }
        me.send(json!({ "List": list }));
    }

    fn handle_state(&mut self, id: ConnId, state: &Value, now: f64) {
        let mut position = None;
        let mut paused = None;
        let mut do_seek = None;
        {
            let w = self.watchers.get_mut(&id).unwrap();
            if let Some(ignore) = state.get("ignoringOnTheFly") {
                if let Some(server) = ignore.get("server").and_then(Value::as_u64) {
                    if u64::from(w.server_ignoring) == server {
                        w.server_ignoring = 0;
                    }
                }
                if let Some(client) = ignore.get("client").and_then(Value::as_u64) {
                    w.client_ignoring = client as u32;
                }
            }
            if let Some(ps) = state.get("playstate") {
                position = Some(ps.get("position").and_then(Value::as_f64).unwrap_or(0.0));
                paused = ps.get("paused").and_then(Value::as_bool);
                do_seek = ps.get("doSeek").and_then(Value::as_bool);
            }
            if let Some(ping) = state.get("ping") {
                let latency = ping
                    .get("latencyCalculation")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0);
                let client_rtt = ping.get("clientRtt").and_then(Value::as_f64).unwrap_or(0.0);
                w.client_latency_calc = ping
                    .get("clientLatencyCalculation")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0);
                w.client_latency_arrival = now;
                w.ping.receive(latency, client_rtt, now);
            }
            if w.server_ignoring != 0 {
                return;
            }
        }
        self.update_state(id, position, paused, do_seek, now);
    }

    fn update_state(
        &mut self,
        id: ConnId,
        position: Option<f64>,
        paused: Option<bool>,
        do_seek: Option<bool>,
        now: f64,
    ) {
        let room_name = self.watchers[&id].room.clone();
        let Some(room_paused) = self.rooms.get(&room_name).map(|r| r.paused) else {
            return;
        };
        let pause_changed = paused.is_some_and(|p| p != room_paused);
        let (name, message_age) = {
            let w = self.watchers.get_mut(&id).unwrap();
            w.last_updated = now;
            (w.name.clone(), w.ping.forward_delay())
        };
        let can_control = self.can_control(id);
        if pause_changed && can_control {
            let room = self.rooms.get_mut(&room_name).unwrap();
            room.paused = paused.unwrap();
            room.set_by = Some(name.clone());
        }
        if let Some(mut p) = position {
            if paused != Some(true) {
                p += message_age;
            }
            self.watchers.get_mut(&id).unwrap().position = Some(p);
        }
        if do_seek == Some(true) || pause_changed {
            if can_control {
                self.force_position_update(id, do_seek == Some(true), now);
            } else {
                self.revert_watcher(id, paused.unwrap_or(room_paused), now);
            }
        }
    }

    /// A managed room's non-operator paused or seeked: put only them back
    /// (the official server's `forcePositionUpdate` for `not canControl`).
    fn revert_watcher(&mut self, id: ConnId, own_paused: bool, now: f64) {
        let room_name = self.watchers[&id].room.clone();
        let position = self.room_position(&room_name, now);
        let Some((paused, set_by)) = self
            .rooms
            .get(&room_name)
            .map(|r| (r.paused, r.set_by.clone()))
        else {
            return;
        };
        let w = self.watchers.get_mut(&id).unwrap();
        let name = w.name.clone();
        w.send_state(position, own_paused, false, Some(&name), true, now);
        w.send_state(position, paused, true, set_by.as_deref(), true, now);
    }

    /// `Set.controllerAuth`: identify as an operator of a managed room, or
    /// (for an ordinary room) get the name of a new managed room.
    fn auth_room_controller(&mut self, id: ConnId, room: Option<&str>, password: Option<&str>) {
        let (name, current) = {
            let w = &self.watchers[&id];
            (w.name.clone(), w.room.clone())
        };
        let status = |success: bool| json!({ "Set": { "controllerAuth": { "user": name, "room": current, "success": success } } });
        if !self.opts.managed_rooms {
            self.watchers[&id].send(status(false));
            return;
        }
        let password = password.unwrap_or("");
        if !managed::password_valid(password) {
            self.send_to(&self.room_members(id), &status(false));
            return;
        }
        let room_name = room
            .filter(|r| !r.is_empty())
            .unwrap_or(&current)
            .to_string();
        if managed::is_controlled(&room_name).is_none() {
            let new_name = managed::controlled_name(&room_name, password, &self.opts.room_salt);
            self.watchers[&id].send_set(
                json!({ "newControlledRoom": { "password": password, "roomName": new_name } }),
            );
            return;
        }
        // Unlike the official server, the password must be for the room the user is in.
        let success =
            room_name == current && managed::check(&room_name, password, &self.opts.room_salt);
        if success {
            if let Some(r) = self.rooms.get_mut(&current) {
                if r.controlled && !r.controllers.contains(&id) {
                    r.controllers.push(id);
                }
            }
            tracing::info!(user = %name, room = %current, "Syncplay room operator identified");
        }
        self.send_to(&self.audience(id), &status(success));
    }

    /// `Set.ready` for someone else: only a managed room's operators may.
    fn set_others_ready(
        &mut self,
        id: ConnId,
        username: &str,
        is_ready: Option<bool>,
        manually: bool,
    ) {
        if !self.is_controller(id) {
            return;
        }
        let setter = self.watchers[&id].name.clone();
        let members = self.room_members(id);
        let Some(target) = members
            .iter()
            .copied()
            .find(|m| self.watchers.get(m).is_some_and(|w| w.name == username))
        else {
            return;
        };
        let w = self.watchers.get_mut(&target).unwrap();
        w.ready = is_ready;
        let target_name = w.name.clone();
        let msg = json!({ "Set": { "ready": {
            "username": target_name, "isReady": is_ready, "manuallyInitiated": manually, "setBy": setter,
        } } });
        self.send_to(&members, &msg);
        let text = if is_ready == Some(true) {
            format!("I have set {target_name} as ready.")
        } else {
            format!("I have set {target_name} as not ready.")
        };
        let chat = json!({ "Chat": { "message": text, "username": setter } });
        for m in &members {
            // Clients that show `setBy` themselves skip the chat line.
            if let Some(w) = self.watchers.get(m) {
                if w.features.get("setOthersReadiness") != Some(&json!(true)) {
                    w.send(chat.clone());
                }
            }
        }
    }

    fn force_position_update(&mut self, id: ConnId, do_seek: bool, now: f64) {
        let (room_name, name) = {
            let w = &self.watchers[&id];
            (w.room.clone(), w.name.clone())
        };
        let Some(paused) = self.rooms.get(&room_name).map(|r| r.paused) else {
            return;
        };
        let position = self.watchers[&id].position_at(now, paused);
        self.set_room_position(&room_name, position, &name);
        for member in self.room_members(id) {
            if let Some(w) = self.watchers.get_mut(&member) {
                w.send_state(position, paused, do_seek, Some(&name), true, now);
            }
        }
    }

    /// Once per second: send every watcher the room state and drop silent clients.
    /// Returns the connections that timed out.
    pub fn tick(&mut self, now: f64) -> Vec<ConnId> {
        let mut ids: Vec<ConnId> = self.watchers.keys().copied().collect();
        ids.sort_unstable();
        let mut dropped = Vec::new();
        for id in ids {
            let room_name = self.watchers[&id].room.clone();
            let position = self.room_position(&room_name, now);
            let Some((paused, set_by)) = self
                .rooms
                .get(&room_name)
                .map(|r| (r.paused, r.set_by.clone()))
            else {
                continue;
            };
            let w = self.watchers.get_mut(&id).unwrap();
            w.send_state(position, paused, false, set_by.as_deref(), false, now);
            if now - w.last_updated > protocol::PROTOCOL_TIMEOUT {
                let _ = w.tx.send(Out::Close);
                dropped.push(id);
            }
        }
        for id in &dropped {
            tracing::info!(id, "Syncplay client timed out");
            self.remove(*id);
        }
        dropped
    }

    pub fn remove(&mut self, id: ConnId) {
        if !self.watchers.contains_key(&id) {
            return;
        }
        let msg = self.user_setting(id, None, Some(json!({ "left": true })));
        let others: Vec<ConnId> = self.audience(id).into_iter().filter(|m| *m != id).collect();
        self.send_to(&others, &msg);
        self.detach(id);
        if let Some(w) = self.watchers.remove(&id) {
            if w.ext.is_some() {
                self.effects.push(Effect::Left { conn: id });
            }
            tracing::info!(user = %w.name, room = %w.room, "Syncplay user left");
        }
    }

    /// Connections with an extension session, and their rooms.
    pub fn ext_rooms(&self) -> Vec<(ConnId, String)> {
        let mut v: Vec<(ConnId, String)> = self
            .watchers
            .iter()
            .filter(|(_, w)| w.ext.is_some())
            .map(|(id, w)| (*id, w.room.clone()))
            .collect();
        v.sort_unstable();
        v
    }

    /// Close every connection (server stopping).
    pub fn close_all(&mut self) {
        for w in self.watchers.values() {
            let _ = w.tx.send(Out::Close);
        }
        self.watchers.clear();
        self.rooms.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};

    fn drain(rx: &mut UnboundedReceiver<Out>) -> Vec<Value> {
        let mut out = Vec::new();
        while let Ok(o) = rx.try_recv() {
            if let Out::Line(l) = o {
                assert!(l.ends_with("\r\n"));
                out.push(serde_json::from_str(l.trim_end()).unwrap());
            }
        }
        out
    }

    fn hello(name: &str, room: &str) -> Value {
        json!({ "Hello": { "username": name, "room": { "name": room }, "version": "1.2.255", "realversion": "1.7.4", "features": { "sharedPlaylists": true, "chat": true } } })["Hello"].clone()
    }

    fn join(
        s: &mut ServerState,
        id: ConnId,
        name: &str,
        room: &str,
        now: f64,
    ) -> UnboundedReceiver<Out> {
        let (tx, rx) = unbounded_channel();
        s.handle_hello(id, &hello(name, room), tx, now).unwrap();
        rx
    }

    fn msg(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn hello_reply_matches_official_order_and_fields() {
        let mut s = ServerState::new(SyncplayOptions {
            motd: "Welcome".into(),
            ..Default::default()
        });
        let mut a = join(&mut s, 1, "alice", "movie", 100.0);
        let out = drain(&mut a);
        // Ready, playlistChange, playlistIndex come before the Hello reply, as in syncplay/server.py.
        assert!(out[0]["Set"]["ready"].is_object());
        assert_eq!(out[0]["Set"]["ready"]["username"], "alice");
        assert!(out[1]["Set"]["playlistChange"]["files"]
            .as_array()
            .unwrap()
            .is_empty());
        assert!(out[2]["Set"]["playlistIndex"].is_object());
        let h = &out[3]["Hello"];
        assert_eq!(h["username"], "alice");
        assert_eq!(h["room"]["name"], "movie");
        assert_eq!(h["version"], "1.7.4");
        assert_eq!(h["realversion"], protocol::SERVER_VERSION);
        assert_eq!(h["motd"], "Welcome");
        assert_eq!(h["features"]["chat"], true);
        assert_eq!(h["features"]["maxRoomNameLength"], 35);
    }

    #[test]
    fn duplicate_names_get_underscores_and_join_is_broadcast() {
        let mut s = ServerState::new(SyncplayOptions::default());
        let mut a = join(&mut s, 1, "alice", "movie", 0.0);
        drain(&mut a);
        let mut b = join(&mut s, 2, "Alice", "movie", 0.0);
        let to_a = drain(&mut a);
        assert!(to_a[0]["Set"]["user"]["Alice_"]["event"]["joined"]
            .as_bool()
            .unwrap());
        assert_eq!(to_a[1]["Set"]["ready"]["username"], "Alice_");
        assert_eq!(drain(&mut b).last().unwrap()["Hello"]["username"], "Alice_");
    }

    #[test]
    fn password_is_checked_as_md5() {
        let mut s = ServerState::new(SyncplayOptions {
            password: "secret".into(),
            ..Default::default()
        });
        let (tx, _rx) = unbounded_channel();
        let mut h = hello("a", "r");
        assert_eq!(
            s.handle_hello(1, &h, tx.clone(), 0.0).unwrap_err(),
            "Password required"
        );
        h["password"] = json!(protocol::md5_hex("nope"));
        assert_eq!(
            s.handle_hello(1, &h, tx.clone(), 0.0).unwrap_err(),
            "Wrong password supplied"
        );
        h["password"] = json!(protocol::md5_hex("secret"));
        assert!(s.handle_hello(1, &h, tx, 0.0).is_ok());
        let (tx2, _) = unbounded_channel();
        assert!(s
            .handle_hello(2, &json!({ "username": "x" }), tx2, 0.0)
            .is_err());
    }

    #[test]
    fn pause_and_seek_are_forwarded_to_room_with_ignoring_on_the_fly() {
        let mut s = ServerState::new(SyncplayOptions::default());
        let mut a = join(&mut s, 1, "alice", "movie", 0.0);
        let mut b = join(&mut s, 2, "bob", "movie", 0.0);
        let mut c = join(&mut s, 3, "carol", "other", 0.0);
        drain(&mut a);
        drain(&mut b);
        drain(&mut c);

        // Alice unpauses at 10s.
        s.handle_message(1, &msg(json!({ "State": { "playstate": { "position": 10.0, "paused": false, "doSeek": false } } })), 1.0);
        let to_b = drain(&mut b);
        let st = &to_b[0]["State"];
        assert_eq!(st["playstate"]["paused"], false);
        assert_eq!(st["playstate"]["setBy"], "alice");
        assert!((st["playstate"]["position"].as_f64().unwrap() - 10.0).abs() < 0.01);
        assert_eq!(st["ignoringOnTheFly"]["server"], 1);
        assert!(drain(&mut c).is_empty(), "other rooms are not told");

        // Until Bob acknowledges, his own state reports are ignored.
        s.handle_message(
            2,
            &msg(json!({ "State": { "playstate": { "position": 3.0, "paused": true } } })),
            1.5,
        );
        assert!(drain(&mut a)
            .iter()
            .all(|m| m["State"]["playstate"]["paused"] == false));
        s.handle_message(2, &msg(json!({ "State": { "ignoringOnTheFly": { "server": 1 }, "playstate": { "position": 10.5, "paused": false } } })), 1.6);

        // Bob seeks to 100.
        drain(&mut a);
        s.handle_message(2, &msg(json!({ "State": { "playstate": { "position": 100.0, "paused": false, "doSeek": true } } })), 2.0);
        let to_a = drain(&mut a);
        assert_eq!(to_a[0]["State"]["playstate"]["doSeek"], true);
        assert!((to_a[0]["State"]["playstate"]["position"].as_f64().unwrap() - 100.0).abs() < 0.01);
    }

    const SALT: &str = "QWERTYUIOP";
    const ROOM: &str = "+movie night:D6A85289CDB5";
    const PW: &str = "AB-123-456";

    fn managed_opts() -> SyncplayOptions {
        SyncplayOptions {
            room_salt: SALT.into(),
            ..Default::default()
        }
    }

    fn auth(s: &mut ServerState, id: ConnId, room: Option<&str>, password: &str, now: f64) {
        let mut v = json!({ "password": password });
        if let Some(r) = room {
            v["room"] = json!(r);
        }
        s.handle_message(id, &msg(json!({ "Set": { "controllerAuth": v } })), now);
    }

    fn find<'a>(out: &'a [Value], set: &str) -> Vec<&'a Value> {
        out.iter().filter_map(|m| m["Set"].get(set)).collect()
    }

    #[test]
    fn managed_rooms_are_advertised() {
        let mut s = ServerState::new(managed_opts());
        let mut a = join(&mut s, 1, "alice", "movie", 0.0);
        let out = drain(&mut a);
        let hello = out.iter().find(|m| m.get("Hello").is_some()).unwrap();
        assert_eq!(hello["Hello"]["features"]["managedRooms"], true);
    }

    #[test]
    fn controller_auth_in_an_ordinary_room_names_a_new_managed_room() {
        let mut s = ServerState::new(managed_opts());
        let mut a = join(&mut s, 1, "alice", "movie night", 0.0);
        let mut b = join(&mut s, 2, "bob", "movie night", 0.0);
        drain(&mut a);
        drain(&mut b);
        auth(&mut s, 1, Some("movie night"), PW, 1.0);
        let out = drain(&mut a);
        let created = find(&out, "newControlledRoom");
        assert_eq!(created.len(), 1);
        assert_eq!(created[0]["roomName"], ROOM);
        assert_eq!(created[0]["password"], PW);
        assert!(drain(&mut b).is_empty(), "only the sender is told");
    }

    #[test]
    fn operators_are_identified_by_password() {
        let mut s = ServerState::new(managed_opts());
        let mut a = join(&mut s, 1, "alice", ROOM, 0.0);
        let mut b = join(&mut s, 2, "bob", ROOM, 0.0);
        let mut c = join(&mut s, 3, "carol", "other", 0.0);
        drain(&mut a);
        drain(&mut b);
        drain(&mut c);

        // A wrong password is broadcast as a failure.
        auth(&mut s, 2, Some(ROOM), "AB-123-457", 1.0);
        let fail = drain(&mut c);
        assert_eq!(find(&fail, "controllerAuth")[0]["success"], false);
        assert_eq!(find(&fail, "controllerAuth")[0]["user"], "bob");

        // A malformed one only reaches the room.
        auth(&mut s, 2, None, "nope", 1.0);
        assert_eq!(find(&drain(&mut a), "controllerAuth").len(), 2);
        assert!(drain(&mut c).is_empty());
        drain(&mut b);

        auth(&mut s, 1, Some(ROOM), PW, 2.0);
        for rx in [&mut a, &mut b, &mut c] {
            let out = drain(rx);
            let st = find(&out, "controllerAuth");
            assert_eq!(st.len(), 1);
            assert_eq!(st[0]["success"], true);
            assert_eq!(st[0]["user"], "alice");
            assert_eq!(st[0]["room"], ROOM);
        }
        s.handle_message(2, &msg(json!({ "List": null })), 2.0);
        let list = drain(&mut b);
        assert_eq!(list[0]["List"][ROOM]["alice"]["controller"], true);
        assert_eq!(list[0]["List"][ROOM]["bob"]["controller"], false);

        let info = s.rooms().into_iter().find(|r| r.name == ROOM).unwrap();
        assert!(info.controlled);
        assert_eq!(info.controllers, vec!["alice".to_string()]);

        // Leaving the room drops control; coming back needs the password again.
        s.handle_message(
            1,
            &msg(json!({ "Set": { "room": { "name": "other" } } })),
            3.0,
        );
        s.handle_message(1, &msg(json!({ "Set": { "room": { "name": ROOM } } })), 3.0);
        assert!(s.rooms().iter().all(|r| r.controllers.is_empty()));
    }

    #[test]
    fn a_password_for_another_room_does_not_grant_control() {
        let mut s = ServerState::new(managed_opts());
        let other = managed::controlled_name("other", PW, SALT);
        let mut a = join(&mut s, 1, "alice", &other, 0.0);
        drain(&mut a);
        auth(&mut s, 1, Some(ROOM), PW, 1.0);
        assert_eq!(find(&drain(&mut a), "controllerAuth")[0]["success"], false);
        assert!(s.rooms()[0].controllers.is_empty());
    }

    #[test]
    fn only_operators_control_a_managed_room() {
        let mut s = ServerState::new(managed_opts());
        let mut a = join(&mut s, 1, "alice", ROOM, 0.0);
        let mut b = join(&mut s, 2, "bob", ROOM, 0.0);
        auth(&mut s, 1, Some(ROOM), PW, 0.5);
        drain(&mut a);
        drain(&mut b);

        // Bob unpauses: nobody else hears of it, and Bob is put back.
        s.handle_message(2, &msg(json!({ "State": { "playstate": { "position": 10.0, "paused": false, "doSeek": false } } })), 1.0);
        assert!(drain(&mut a).is_empty());
        let back = drain(&mut b);
        assert_eq!(back.len(), 2);
        assert_eq!(back[0]["State"]["playstate"]["paused"], false);
        assert_eq!(back[0]["State"]["playstate"]["doSeek"], false);
        assert_eq!(back[0]["State"]["playstate"]["setBy"], "bob");
        assert_eq!(back[1]["State"]["playstate"]["paused"], true);
        assert_eq!(back[1]["State"]["playstate"]["doSeek"], true);
        assert_eq!(back[1]["State"]["ignoringOnTheFly"]["server"], 2);
        assert!(s.rooms()[0].paused);

        // Bob's playlist change is undone for him only.
        s.handle_message(
            2,
            &msg(json!({ "Set": { "playlistChange": { "files": ["a.mkv"] } } })),
            1.1,
        );
        let undo = drain(&mut b);
        assert_eq!(find(&undo, "playlistChange")[0]["files"], json!([]));
        s.handle_message(
            2,
            &msg(json!({ "Set": { "playlistIndex": { "index": 3 } } })),
            1.1,
        );
        assert_eq!(
            find(&drain(&mut b), "playlistIndex")[0]["index"],
            Value::Null
        );
        assert!(drain(&mut a).is_empty());

        // Alice unpauses: everyone follows.
        s.handle_message(1, &msg(json!({ "State": { "playstate": { "position": 5.0, "paused": false, "doSeek": false } } })), 2.0);
        let to_b = drain(&mut b);
        assert_eq!(to_b[0]["State"]["playstate"]["paused"], false);
        assert_eq!(to_b[0]["State"]["playstate"]["setBy"], "alice");
        s.handle_message(
            1,
            &msg(json!({ "Set": { "playlistChange": { "files": ["a.mkv"] } } })),
            2.1,
        );
        assert_eq!(
            find(&drain(&mut b), "playlistChange")[0]["files"],
            json!(["a.mkv"])
        );
    }

    #[test]
    fn operators_can_set_others_ready() {
        let mut s = ServerState::new(managed_opts());
        let mut a = join(&mut s, 1, "alice", ROOM, 0.0);
        let mut b = join(&mut s, 2, "bob", ROOM, 0.0);
        drain(&mut a);
        drain(&mut b);

        // Not an operator yet: ignored.
        s.handle_message(1, &msg(json!({ "Set": { "ready": { "username": "bob", "isReady": true, "manuallyInitiated": true } } })), 1.0);
        assert!(drain(&mut b).is_empty());

        auth(&mut s, 1, None, PW, 1.0);
        drain(&mut a);
        drain(&mut b);
        s.handle_message(1, &msg(json!({ "Set": { "ready": { "username": "bob", "isReady": true, "manuallyInitiated": true } } })), 2.0);
        let out = drain(&mut b);
        let ready = find(&out, "ready");
        assert_eq!(ready[0]["username"], "bob");
        assert_eq!(ready[0]["isReady"], true);
        assert_eq!(ready[0]["setBy"], "alice");
        let chat = out.iter().find(|m| m.get("Chat").is_some()).unwrap();
        assert_eq!(chat["Chat"]["message"], "I have set bob as ready.");
        assert_eq!(chat["Chat"]["username"], "alice");
    }

    #[test]
    fn managed_rooms_off_behaves_as_before() {
        let mut s = ServerState::new(SyncplayOptions {
            managed_rooms: false,
            ..managed_opts()
        });
        let mut a = join(&mut s, 1, "alice", ROOM, 0.0);
        let mut b = join(&mut s, 2, "bob", ROOM, 0.0);
        let hello = drain(&mut a);
        let hello = hello.iter().find(|m| m.get("Hello").is_some()).unwrap();
        assert_eq!(hello["Hello"]["features"]["managedRooms"], false);
        drain(&mut b);

        auth(&mut s, 1, Some(ROOM), PW, 1.0);
        let out = drain(&mut a);
        assert_eq!(find(&out, "controllerAuth")[0]["success"], false);
        assert!(drain(&mut b).is_empty());

        // Anyone may control the room.
        s.handle_message(2, &msg(json!({ "State": { "playstate": { "position": 1.0, "paused": false, "doSeek": false } } })), 2.0);
        assert_eq!(drain(&mut a)[0]["State"]["playstate"]["setBy"], "bob");
    }

    #[test]
    fn switching_managed_rooms_off_frees_existing_rooms() {
        let mut s = ServerState::new(managed_opts());
        let mut a = join(&mut s, 1, "alice", ROOM, 0.0);
        let mut b = join(&mut s, 2, "bob", ROOM, 0.0);
        auth(&mut s, 1, None, PW, 0.5);
        drain(&mut a);
        drain(&mut b);
        s.set_options(SyncplayOptions {
            managed_rooms: false,
            ..managed_opts()
        });
        assert!(!s.rooms()[0].controlled);
        s.handle_message(2, &msg(json!({ "State": { "playstate": { "position": 1.0, "paused": false, "doSeek": false } } })), 2.0);
        assert_eq!(drain(&mut a)[0]["State"]["playstate"]["setBy"], "bob");
    }

    #[test]
    fn vanilla_mode_keeps_managed_rooms() {
        let mut s = ServerState::new(SyncplayOptions {
            vanilla_mode: true,
            ..managed_opts()
        });
        let mut a = join(&mut s, 1, "alice", ROOM, 0.0);
        drain(&mut a);
        auth(&mut s, 1, None, PW, 1.0);
        assert_eq!(find(&drain(&mut a), "controllerAuth")[0]["success"], true);
    }

    #[test]
    fn heartbeat_extrapolates_and_times_out() {
        let mut s = ServerState::new(SyncplayOptions::default());
        let mut a = join(&mut s, 1, "alice", "movie", 0.0);
        drain(&mut a);
        s.handle_message(
            1,
            &msg(json!({ "Set": { "file": { "name": "a.mkv", "duration": 100.0, "size": 1 } } })),
            0.0,
        );
        s.handle_message(
            1,
            &msg(json!({ "State": { "playstate": { "position": 5.0, "paused": false } } })),
            0.0,
        );
        drain(&mut a);
        // Acknowledge the forced update so the next reports count.
        s.handle_message(1, &msg(json!({ "State": { "ignoringOnTheFly": { "server": 1 }, "playstate": { "position": 5.0, "paused": false } } })), 0.0);
        assert!(s.tick(3.0).is_empty());
        let st = drain(&mut a).last().unwrap()["State"].clone();
        assert!(
            (st["playstate"]["position"].as_f64().unwrap() - 8.0).abs() < 0.05,
            "{st}"
        );
        assert!(st["ping"]["latencyCalculation"].as_f64().unwrap() > 0.0);
        assert_eq!(s.tick(20.0), vec![1]);
        assert_eq!(s.user_count(), 0);
    }

    #[test]
    fn playlist_chat_list_and_leave() {
        let mut s = ServerState::new(SyncplayOptions {
            max_chat_message_length: 5,
            ..Default::default()
        });
        let mut a = join(&mut s, 1, "alice", "movie", 0.0);
        let mut b = join(&mut s, 2, "bob", "movie", 0.0);
        drain(&mut a);
        drain(&mut b);
        s.handle_message(
            1,
            &msg(json!({ "Set": { "playlistChange": { "files": ["a.mkv", "b.mkv"] } } })),
            0.0,
        );
        assert_eq!(
            drain(&mut b)[0]["Set"]["playlistChange"]["files"][1],
            "b.mkv"
        );
        s.handle_message(
            1,
            &msg(json!({ "Set": { "playlistIndex": { "index": 1 } } })),
            0.0,
        );
        assert_eq!(drain(&mut b)[0]["Set"]["playlistIndex"]["index"], 1);

        // A late joiner receives the room's playlist.
        let mut c = join(&mut s, 3, "carol", "movie", 0.0);
        let to_c = drain(&mut c);
        assert_eq!(to_c[1]["Set"]["playlistChange"]["files"][0], "a.mkv");
        assert_eq!(to_c[2]["Set"]["playlistIndex"]["index"], 1);
        drain(&mut a);
        drain(&mut b);

        s.handle_message(2, &msg(json!({ "Chat": "hello world" })), 0.0);
        assert_eq!(
            drain(&mut a)[0]["Chat"],
            json!({ "message": "hello", "username": "bob" })
        );

        s.handle_message(1, &msg(json!({ "List": null })), 0.0);
        let list = &drain(&mut a)[0]["List"]["movie"];
        assert!(list["bob"].is_object() && list["carol"].is_object());

        s.remove(2);
        assert!(drain(&mut a)[0]["Set"]["user"]["bob"]["event"]["left"]
            .as_bool()
            .unwrap());
        assert_eq!(s.rooms()[0].users, vec!["alice", "carol"]);
    }

    #[test]
    fn isolated_rooms_hide_other_rooms() {
        let mut s = ServerState::new(SyncplayOptions {
            isolate_rooms: true,
            ..Default::default()
        });
        let mut a = join(&mut s, 1, "alice", "one", 0.0);
        drain(&mut a);
        let _b = join(&mut s, 2, "bob", "two", 0.0);
        assert!(drain(&mut a).is_empty());
        s.handle_message(1, &msg(json!({ "List": null })), 0.0);
        let list = drain(&mut a)[0]["List"].clone();
        assert!(list.get("two").is_none());
    }

    fn join_ext(s: &mut ServerState, id: ConnId, name: &str, room: &str) -> UnboundedReceiver<Out> {
        let (tx, rx) = unbounded_channel();
        let mut h = hello(name, room);
        h["features"]["yarmiplay"] = json!({ "protocol": 3 });
        s.handle_hello(id, &h, tx, 0.0).unwrap();
        rx
    }

    #[test]
    fn tls_name_and_home_addresses_are_advertised() {
        let mut s = ServerState::new(SyncplayOptions::default());
        let tls = TlsInfo {
            name: "home.duckdns.org".into(),
            lan: vec!["192.168.1.20:8999".into()],
            lan_name: None,
        };
        s.set_tls(Some(tls.clone()));
        let mut a = join_ext(&mut s, 1, "alice", "movie");
        let out = drain(&mut a);
        let hello = out.iter().find(|m| m.get("Hello").is_some()).unwrap();
        let y = &hello["Hello"]["features"]["yarmiplay"];
        assert_eq!(y["tls"]["name"], "home.duckdns.org");
        assert_eq!(y["capabilities"]["https"], true);
        let ext = out
            .iter()
            .find(|m| m["Yarmiplay"].get("session").is_some())
            .unwrap();
        assert_eq!(ext["Yarmiplay"]["tls"]["lan"][0], "192.168.1.20:8999");
        assert_eq!(s.ext_info()["tls"]["name"], "home.duckdns.org");

        // Losing the certificate is broadcast.
        s.set_tls(None);
        let out = drain(&mut a);
        assert_eq!(out[0]["Yarmiplay"]["tls"], Value::Null);
        assert_eq!(out[0]["Yarmiplay"]["capabilities"]["https"], false);

        // Vanilla mode hides it.
        s.set_tls(Some(tls));
        s.set_options(SyncplayOptions {
            vanilla_mode: true,
            ..Default::default()
        });
        assert_eq!(s.ext_info()["tls"], Value::Null);
        let mut b = join(&mut s, 2, "bob", "movie", 0.0);
        let out = drain(&mut b);
        let hello = out.iter().find(|m| m.get("Hello").is_some()).unwrap();
        assert!(hello["Hello"]["features"].get("yarmiplay").is_none());
    }

    fn relay_opts() -> SyncplayOptions {
        SyncplayOptions {
            file_relay: true,
            ..Default::default()
        }
    }

    #[test]
    fn extension_handshake_only_for_clients_that_ask() {
        let mut s = ServerState::new(relay_opts());
        let mut v = join(&mut s, 1, "vanilla", "movie", 0.0);
        let out = drain(&mut v);
        assert!(out[3]["Hello"]["features"].get("yarmiplay").is_none());
        assert_eq!(out.len(), 4);

        let mut y = join_ext(&mut s, 2, "yarmi", "movie");
        let out = drain(&mut y);
        let f = &out[3]["Hello"]["features"]["yarmiplay"];
        assert_eq!(f["server"], "YarmiplayServerTV");
        assert_eq!(f["protocol"], 1, "the lower protocol wins");
        assert_eq!(f["capabilities"]["fileRelay"], true);
        assert_eq!(f["capabilities"]["jellyfin"], false);
        let session = &out[4]["Yarmiplay"]["session"];
        let token = session["token"].as_str().unwrap();
        assert_eq!(token.len(), 48);
        assert_eq!(out[4]["Yarmiplay"]["jellyfin"]["available"], false);
        assert_eq!(s.session_for_token(token), Some((2, "movie".into())));
        assert_eq!(s.session_for_token(""), None);
        assert_eq!(
            s.take_effects(),
            vec![Effect::Joined {
                conn: 2,
                room: "movie".into()
            }]
        );

        // The vanilla peer never sees the extension in features.
        let joined = &drain(&mut v)[0]["Set"]["user"]["yarmi"]["event"];
        assert_eq!(
            joined["features"],
            json!({ "sharedPlaylists": true, "chat": true })
        );
        s.handle_message(1, &msg(json!({ "List": null })), 0.0);
        assert!(drain(&mut v)[0]["List"]["movie"]["yarmi"]["features"]
            .get("yarmiplay")
            .is_none());
        s.handle_message(2, &msg(json!({ "List": null })), 0.0);
        assert_eq!(
            drain(&mut y)[0]["List"]["movie"]["yarmi"]["features"]["yarmiplay"],
            json!({ "protocol": 1 })
        );

        // A vanilla client's Yarmiplay commands do nothing.
        s.handle_message(
            1,
            &msg(json!({ "Yarmiplay": { "offer": { "files": [] } } })),
            0.0,
        );
        assert!(s.take_effects().is_empty());
        assert!(drain(&mut v).is_empty());
        s.handle_message(
            2,
            &msg(json!({ "Yarmiplay": { "offer": { "files": [] } } })),
            0.0,
        );
        assert_eq!(
            s.take_effects(),
            vec![Effect::Offer {
                conn: 2,
                room: "movie".into(),
                files: vec![]
            }]
        );
    }

    #[test]
    fn vanilla_mode_refuses_and_revokes_sessions() {
        let mut s = ServerState::new(relay_opts());
        let mut y = join_ext(&mut s, 1, "yarmi", "movie");
        let token = drain(&mut y)[4]["Yarmiplay"]["session"]["token"]
            .as_str()
            .unwrap()
            .to_string();
        s.take_effects();

        s.set_options(SyncplayOptions {
            vanilla_mode: true,
            ..relay_opts()
        });
        let out = drain(&mut y);
        assert_eq!(out[0]["Yarmiplay"]["capabilities"]["fileRelay"], false);
        assert_eq!(s.session_for_token(&token), None);
        assert_eq!(s.take_effects(), vec![Effect::Left { conn: 1 }]);
        s.handle_message(
            1,
            &msg(json!({ "Yarmiplay": { "offer": { "files": [] } } })),
            0.0,
        );
        assert!(s.take_effects().is_empty());

        let mut late = join_ext(&mut s, 2, "late", "movie");
        let out = drain(&mut late);
        assert!(out[3]["Hello"]["features"].get("yarmiplay").is_none());
        assert_eq!(out.len(), 4);
    }

    fn ext_messages<'a>(out: &'a [Value], sub: &str) -> Vec<&'a Value> {
        out.iter()
            .filter_map(|m| m.get("Yarmiplay").and_then(|y| y.get(sub)))
            .collect()
    }

    #[test]
    fn room_settings_are_shared_with_the_room_and_sent_on_joining() {
        let mut s = ServerState::new(SyncplayOptions::default());
        let mut a = join_ext(&mut s, 1, "alice", "movie");
        let first = drain(&mut a);
        // With the session: nobody set anything yet.
        assert_eq!(
            ext_messages(&first, "capabilities")[0]["roomSettings"],
            true
        );
        assert_eq!(
            *ext_messages(&first, "roomSettings")[0],
            json!({ "autoSkip": false, "setBy": null })
        );
        let mut b = join_ext(&mut s, 2, "bob", "movie");
        let mut v = join(&mut s, 3, "vanilla", "movie", 0.0);
        drain(&mut a);
        drain(&mut b);
        drain(&mut v);

        s.handle_message(
            2,
            &msg(json!({ "Yarmiplay": { "roomSettings": { "autoSkip": true } } })),
            1.0,
        );
        let changed = json!({ "autoSkip": true, "setBy": "bob" });
        assert_eq!(ext_messages(&drain(&mut a), "roomSettings"), vec![&changed]);
        assert_eq!(ext_messages(&drain(&mut b), "roomSettings"), vec![&changed]);
        assert!(
            drain(&mut v).is_empty(),
            "a client without the extension hears nothing"
        );

        // Switching to another room and back: each room has its own.
        s.handle_message(
            1,
            &msg(json!({ "Set": { "room": { "name": "anime" } } })),
            2.0,
        );
        assert_eq!(
            ext_messages(&drain(&mut a), "roomSettings"),
            vec![&json!({ "autoSkip": false, "setBy": null })]
        );
        s.handle_message(
            1,
            &msg(json!({ "Set": { "room": { "name": "movie" } } })),
            3.0,
        );
        assert_eq!(ext_messages(&drain(&mut a), "roomSettings"), vec![&changed]);
    }

    #[test]
    fn room_settings_outlast_an_empty_room() {
        let mut s = ServerState::new(SyncplayOptions::default());
        let mut a = join_ext(&mut s, 1, "alice", "movie");
        s.handle_message(
            1,
            &msg(json!({ "Yarmiplay": { "roomSettings": { "autoSkip": true } } })),
            1.0,
        );
        drain(&mut a);
        s.remove(1);
        let mut b = join_ext(&mut s, 2, "bob", "movie");
        assert_eq!(
            *ext_messages(&drain(&mut b), "roomSettings")[0],
            json!({ "autoSkip": true, "setBy": "alice" })
        );
    }

    #[test]
    fn only_operators_change_a_managed_room_s_settings() {
        let mut s = ServerState::new(managed_opts());
        let mut a = join_ext(&mut s, 1, "alice", ROOM);
        let mut b = join_ext(&mut s, 2, "bob", ROOM);
        auth(&mut s, 1, Some(ROOM), PW, 0.5);
        drain(&mut a);
        drain(&mut b);

        s.handle_message(
            2,
            &msg(json!({ "Yarmiplay": { "roomSettings": { "autoSkip": true } } })),
            1.0,
        );
        assert!(drain(&mut a).is_empty());
        assert_eq!(
            ext_messages(&drain(&mut b), "roomSettings"),
            vec![&json!({ "autoSkip": false, "setBy": null })]
        );

        s.handle_message(
            1,
            &msg(json!({ "Yarmiplay": { "roomSettings": { "autoSkip": true } } })),
            2.0,
        );
        let on = json!({ "autoSkip": true, "setBy": "alice" });
        assert_eq!(ext_messages(&drain(&mut a), "roomSettings"), vec![&on]);
        assert_eq!(ext_messages(&drain(&mut b), "roomSettings"), vec![&on]);
    }

    #[test]
    fn room_settings_need_a_session() {
        let mut s = ServerState::new(SyncplayOptions::default());
        let mut y = join_ext(&mut s, 1, "yarmi", "movie");
        let mut v = join(&mut s, 2, "vanilla", "movie", 0.0);
        drain(&mut y);
        drain(&mut v);
        s.handle_message(
            2,
            &msg(json!({ "Yarmiplay": { "roomSettings": { "autoSkip": true } } })),
            1.0,
        );
        assert!(drain(&mut y).is_empty());
        assert!(drain(&mut v).is_empty());
    }

    #[test]
    fn capability_changes_reach_sessions() {
        let mut s = ServerState::new(SyncplayOptions::default());
        let mut y = join_ext(&mut s, 1, "yarmi", "movie");
        let mut v = join(&mut s, 2, "vanilla", "movie", 0.0);
        drain(&mut y);
        drain(&mut v);
        s.set_jellyfin(Some(JellyfinShare {
            server_id: "abc".into(),
            server_name: "Home".into(),
            proxy: true,
            addresses: vec!["http://192.168.1.2:8096".into()],
        }));
        let out = drain(&mut y);
        assert_eq!(out[0]["Yarmiplay"]["capabilities"]["jellyfin"], true);
        assert_eq!(out[0]["Yarmiplay"]["jellyfin"]["serverId"], "abc");
        assert_eq!(out[0]["Yarmiplay"]["jellyfin"]["available"], true);
        assert!(drain(&mut v).is_empty());
        s.set_jellyfin(s.jellyfin.clone());
        assert!(drain(&mut y).is_empty(), "no news, no message");
    }

    #[test]
    fn jellyfin_authorize_is_gated_and_rate_limited() {
        let mut s = ServerState::new(SyncplayOptions::default());
        let mut y = join_ext(&mut s, 1, "yarmi", "movie");
        drain(&mut y);
        s.take_effects();
        let ask = msg(json!({ "Yarmiplay": { "jellyfinAuthorize": { "code": "123456" } } }));
        s.handle_message(1, &ask, 0.0);
        assert_eq!(
            drain(&mut y)[0]["Yarmiplay"]["jellyfinAuthorize"]["ok"],
            false
        );
        assert!(s.take_effects().is_empty());

        s.set_jellyfin(Some(JellyfinShare {
            server_id: "abc".into(),
            server_name: "Home".into(),
            proxy: false,
            addresses: vec![],
        }));
        drain(&mut y);
        for i in 0..5 {
            s.handle_message(1, &ask, i as f64);
        }
        assert_eq!(s.take_effects().len(), 5);
        s.handle_message(1, &ask, 10.0);
        assert!(s.take_effects().is_empty());
        assert!(drain(&mut y)[0]["Yarmiplay"]["jellyfinAuthorize"]["error"]
            .as_str()
            .unwrap()
            .contains("wait"));
    }

    fn plex_share() -> PlexShare {
        PlexShare {
            machine_id: "m1".into(),
            server_name: "Home Plex".into(),
            proxy: true,
            addresses: vec!["http://192.168.1.2:32400".into()],
        }
    }

    #[test]
    fn plex_share_reaches_sessions_and_vanilla_mode_hides_it() {
        let mut s = ServerState::new(SyncplayOptions::default());
        let mut y = join_ext(&mut s, 1, "yarmi", "movie");
        let hello = drain(&mut y);
        assert_eq!(hello[4]["Yarmiplay"]["plex"]["available"], false);
        assert_eq!(hello[4]["Yarmiplay"]["capabilities"]["plex"], false);

        s.set_plex(Some(plex_share()));
        let out = drain(&mut y);
        assert_eq!(out[0]["Yarmiplay"]["capabilities"]["plex"], true);
        assert_eq!(out[0]["Yarmiplay"]["plex"]["machineId"], "m1");
        assert_eq!(out[0]["Yarmiplay"]["plex"]["proxy"], true);
        assert_eq!(out[0]["Yarmiplay"]["plex"]["available"], true);
        s.set_plex(Some(plex_share()));
        assert!(drain(&mut y).is_empty(), "no news, no message");
        assert_eq!(s.ext_info()["capabilities"]["plex"], true);

        s.set_options(SyncplayOptions {
            vanilla_mode: true,
            ..SyncplayOptions::default()
        });
        assert_eq!(drain(&mut y)[0]["Yarmiplay"]["capabilities"]["plex"], false);
        assert_eq!(s.ext_info()["capabilities"]["plex"], false);
    }

    #[test]
    fn plex_invite_is_gated_and_rate_limited() {
        let mut s = ServerState::new(SyncplayOptions::default());
        let mut y = join_ext(&mut s, 1, "yarmi", "movie");
        drain(&mut y);
        s.take_effects();
        let ask = msg(json!({ "Yarmiplay": { "plexInvite": { "username": "alice" } } }));
        s.handle_message(1, &ask, 0.0);
        assert_eq!(
            drain(&mut y)[0]["Yarmiplay"]["plexInvite"]["error"],
            "Plex sharing is off on this server"
        );
        assert!(s.take_effects().is_empty());

        s.set_plex(Some(plex_share()));
        drain(&mut y);
        let bad = msg(json!({ "Yarmiplay": { "plexInvite": { "username": "a b" } } }));
        s.handle_message(1, &bad, 0.0);
        assert_eq!(
            drain(&mut y)[0]["Yarmiplay"]["plexInvite"]["error"],
            "No plex.tv username"
        );
        for i in 0..3 {
            s.handle_message(1, &ask, i as f64);
        }
        let effects = s.take_effects();
        assert_eq!(effects.len(), 3);
        assert_eq!(
            effects[0],
            Effect::InvitePlex {
                conn: 1,
                username: "alice".into(),
                syncplay_name: "yarmi".into(),
            }
        );
        s.handle_message(1, &ask, 10.0);
        assert!(s.take_effects().is_empty());
        assert!(drain(&mut y)[0]["Yarmiplay"]["plexInvite"]["error"]
            .as_str()
            .unwrap()
            .contains("wait"));
    }

    fn ext_hello(name: &str) -> Value {
        let mut h = hello(name, "movie");
        h["features"]["yarmiplay"] =
            json!({ "protocol": 1, "client": "YarmiplayTV", "version": "2.0.0" });
        h
    }

    fn access_opts(access: SyncplayAccess, vanilla_mode: bool) -> SyncplayOptions {
        SyncplayOptions {
            access,
            password: if access.uses_password() {
                "pw".into()
            } else {
                String::new()
            },
            vanilla_mode,
            ..Default::default()
        }
    }

    #[test]
    fn access_decisions() {
        use SyncplayAccess::*;
        let plain = hello("v", "movie");
        let mut plain_pw = plain.clone();
        plain_pw["password"] = json!(protocol::md5_hex("pw"));
        let ext = ext_hello("y");

        let s = ServerState::new(access_opts(Open, false));
        assert_eq!(s.check_access(&plain, true), Access::Admit);
        assert_eq!(s.check_access(&ext, true), Access::Admit);

        let s = ServerState::new(access_opts(Password, false));
        assert_eq!(s.check_access(&ext, true), Access::Challenge(Password));
        assert_eq!(
            s.check_access(&plain, true),
            Access::Reject("Password required".into())
        );
        assert_eq!(s.check_access(&plain_pw, true), Access::Admit);
        // Without a device store, YarmiplayTV uses the password like everyone else.
        assert_eq!(
            s.check_access(&ext, false),
            Access::Reject("Password required".into())
        );

        // Password only: no challenge, even for YarmiplayTV.
        let s = ServerState::new(access_opts(PasswordOnly, false));
        assert_eq!(
            s.check_access(&ext, true),
            Access::Reject("Password required".into())
        );
        let mut ext_wrong = ext.clone();
        ext_wrong["password"] = json!(protocol::md5_hex("nope"));
        assert_eq!(
            s.check_access(&ext_wrong, true),
            Access::Reject("Wrong password supplied".into())
        );
        let mut ext_pw = ext.clone();
        ext_pw["password"] = json!(protocol::md5_hex("pw"));
        assert_eq!(s.check_access(&ext_pw, true), Access::Admit);
        assert_eq!(s.check_access(&plain_pw, true), Access::Admit);
        assert_eq!(
            s.check_access(&plain, true),
            Access::Reject("Password required".into())
        );

        let s = ServerState::new(access_opts(Approved, false));
        assert_eq!(s.check_access(&ext, true), Access::Challenge(Approved));
        assert_eq!(
            s.check_access(&plain_pw, true),
            Access::Reject(APPROVED_ONLY.into())
        );
        assert_eq!(
            s.check_access(&ext, false),
            Access::Reject(APPROVED_ONLY.into())
        );

        // Vanilla mode: the official rules for everyone.
        let s = ServerState::new(access_opts(Password, true));
        assert_eq!(
            s.check_access(&ext, true),
            Access::Reject("Password required".into())
        );
        let mut ext_pw = ext.clone();
        ext_pw["password"] = json!(protocol::md5_hex("pw"));
        assert_eq!(s.check_access(&ext_pw, true), Access::Admit);

        // Missing arguments come first, as on the official server.
        let s = ServerState::new(access_opts(Approved, false));
        assert_eq!(
            s.check_access(&json!({ "username": "x" }), true),
            Access::Reject("Not enough Hello arguments".into())
        );
    }

    #[test]
    fn marker_reports_access_and_device() {
        let mut s = ServerState::new(access_opts(SyncplayAccess::Password, false));
        let (tx, mut d) = unbounded_channel();
        s.login(
            1,
            &ext_hello("dev"),
            tx,
            0.0,
            Some("AAAA-BBBB-CCCC-DDDD".into()),
        )
        .unwrap();
        let reply = |out: Vec<Value>| out.into_iter().find(|m| m.get("Hello").is_some()).unwrap();
        let marker = reply(drain(&mut d))["Hello"]["features"]["yarmiplay"].clone();
        assert_eq!(marker["access"], "password");
        assert_eq!(marker["device"], "approved");
        assert_eq!(marker["server"], "YarmiplayServerTV");

        let (tx, mut p) = unbounded_channel();
        s.login(2, &ext_hello("pw"), tx, 0.0, None).unwrap();
        assert_eq!(
            reply(drain(&mut p))["Hello"]["features"]["yarmiplay"]["device"],
            "none"
        );

        let (tx, mut v) = unbounded_channel();
        s.login(3, &hello("vanilla", "movie"), tx, 0.0, None)
            .unwrap();
        let out = drain(&mut v);
        assert!(out.iter().all(|m| m.get("Yarmiplay").is_none()));
        assert!(reply(out)["Hello"]["features"].get("yarmiplay").is_none());
    }

    #[test]
    fn approved_only_drops_other_logins_and_removed_devices_are_kicked() {
        let mut s = ServerState::new(access_opts(SyncplayAccess::Password, false));
        let fp = "AAAA-BBBB-CCCC-DDDD";
        let (tx, mut d) = unbounded_channel();
        s.login(1, &ext_hello("dev"), tx, 0.0, Some(fp.into()))
            .unwrap();
        let (tx, mut p) = unbounded_channel();
        s.login(2, &hello("pw", "movie"), tx, 0.0, None).unwrap();
        drain(&mut d);
        drain(&mut p);

        s.set_options(access_opts(SyncplayAccess::Approved, false));
        let mut out = Vec::new();
        let mut closed = false;
        while let Ok(o) = p.try_recv() {
            match o {
                Out::Line(l) => out.push(serde_json::from_str::<Value>(l.trim_end()).unwrap()),
                Out::Close => closed = true,
            }
        }
        assert_eq!(out[0]["Error"]["message"], APPROVED_ONLY);
        assert!(closed);
        assert!(s.is_logged(1) && !s.is_logged(2));
        assert!(drain(&mut d)[0]["Set"]["user"]["pw"]["event"]["left"] == true);

        assert_eq!(s.kick_device("FFFF-FFFF-FFFF-FFFF"), 0);
        assert_eq!(s.kick_device(fp), 1);
        let out = drain(&mut d);
        assert_eq!(out[0]["Yarmiplay"]["status"]["state"], "revoked");
        assert_eq!(out[0]["Yarmiplay"]["status"]["fingerprint"], fp);
        assert!(out[1]["Error"]["message"]
            .as_str()
            .unwrap()
            .contains("removed"));
        assert!(!s.is_logged(1));
    }

    #[test]
    fn password_only_drops_device_logins_and_marks_the_mode() {
        let mut s = ServerState::new(access_opts(SyncplayAccess::Password, false));
        let (tx, mut d) = unbounded_channel();
        s.login(
            1,
            &ext_hello("dev"),
            tx,
            0.0,
            Some("AAAA-BBBB-CCCC-DDDD".into()),
        )
        .unwrap();
        let (tx, mut p) = unbounded_channel();
        s.login(2, &ext_hello("pw"), tx, 0.0, None).unwrap();
        drain(&mut d);
        drain(&mut p);

        s.set_options(access_opts(SyncplayAccess::PasswordOnly, false));
        let mut out = Vec::new();
        let mut closed = false;
        while let Ok(o) = d.try_recv() {
            match o {
                Out::Line(l) => out.push(serde_json::from_str::<Value>(l.trim_end()).unwrap()),
                Out::Close => closed = true,
            }
        }
        assert_eq!(out[0]["Error"]["message"], "Password required");
        assert!(closed);
        assert!(!s.is_logged(1) && s.is_logged(2));

        let (tx, mut n) = unbounded_channel();
        s.login(3, &ext_hello("new"), tx, 0.0, None).unwrap();
        let hello = drain(&mut n)
            .into_iter()
            .find(|m| m.get("Hello").is_some())
            .unwrap();
        assert_eq!(
            hello["Hello"]["features"]["yarmiplay"]["access"],
            "passwordOnly"
        );
        assert_eq!(hello["Hello"]["features"]["yarmiplay"]["device"], "none");
    }

    #[test]
    fn long_welcome_messages_are_cut() {
        let mut s = ServerState::new(SyncplayOptions {
            motd: "é".repeat(protocol::MAX_MOTD_LENGTH + 10),
            ..Default::default()
        });
        let out = drain(&mut join(&mut s, 1, "alice", "movie", 0.0));
        let hello = out.iter().find(|m| m.get("Hello").is_some()).unwrap();
        let motd = hello["Hello"]["motd"].as_str().unwrap();
        assert_eq!(motd.chars().count(), protocol::MAX_MOTD_LENGTH);
    }
}
