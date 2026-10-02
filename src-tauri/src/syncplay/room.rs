//! Server-side Syncplay state: watchers, rooms, playstate and shared
//! playlists. Mirrors the behaviour of the official server
//! (`syncplay/server.py`, `SyncServerProtocol`) so the official clients and
//! YarmiplayTV see exactly what they expect, including message order.
//!
//! Pure and synchronous: callers pass the current time, and outgoing lines go
//! to each connection's channel.

use super::protocol::{
    self, line, truncate, PingService, MAX_FILENAME_LENGTH, MAX_ROOM_NAME_LENGTH,
};
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

#[derive(Debug, Clone, PartialEq)]
pub struct SyncplayOptions {
    pub password: String,
    pub motd: String,
    pub isolate_rooms: bool,
    pub disable_chat: bool,
    pub disable_ready: bool,
    pub max_chat_message_length: usize,
    pub max_username_length: usize,
}

impl Default for SyncplayOptions {
    fn default() -> Self {
        Self {
            password: String::new(),
            motd: String::new(),
            isolate_rooms: false,
            disable_chat: false,
            disable_ready: false,
            max_chat_message_length: 150,
            max_username_length: 150,
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
    tx: UnboundedSender<Out>,
}

impl Watcher {
    fn send(&self, value: Value) {
        let _ = self.tx.send(Out::Line(line(&value)));
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
}

impl Room {
    fn new(now: f64) -> Self {
        Self {
            members: Vec::new(),
            paused: true,
            set_by: None,
            position: Some(0.0),
            last_update: now,
            playlist: Vec::new(),
            playlist_index: Value::Null,
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RoomInfo {
    pub name: String,
    pub users: Vec<String>,
    pub paused: bool,
}

pub struct ServerState {
    opts: SyncplayOptions,
    password_md5: Option<String>,
    watchers: HashMap<ConnId, Watcher>,
    rooms: HashMap<String, Room>,
}

impl ServerState {
    pub fn new(opts: SyncplayOptions) -> Self {
        let mut s = Self {
            opts: SyncplayOptions::default(),
            password_md5: None,
            watchers: HashMap::new(),
            rooms: HashMap::new(),
        };
        s.set_options(opts);
        s
    }

    pub fn set_options(&mut self, opts: SyncplayOptions) {
        self.password_md5 = (!opts.password.is_empty()).then(|| protocol::md5_hex(&opts.password));
        self.opts = opts;
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
            })
            .collect();
        rooms.sort_by(|a, b| a.name.cmp(&b.name));
        rooms
    }

    fn features(&self) -> Value {
        json!({
            "isolateRooms": self.opts.isolate_rooms,
            "readiness": !self.opts.disable_ready,
            "managedRooms": false,
            "persistentRooms": false,
            "chat": !self.opts.disable_chat,
            "maxChatMessageLength": self.opts.max_chat_message_length,
            "maxUsernameLength": self.opts.max_username_length,
            "maxRoomNameLength": MAX_ROOM_NAME_LENGTH,
            "maxFilenameLength": MAX_FILENAME_LENGTH,
        })
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

    /// Validate a Hello and log the client in. On error the caller sends
    /// `{"Error": {"message": ...}}` and closes the connection.
    pub fn handle_hello(
        &mut self,
        id: ConnId,
        hello: &Value,
        tx: UnboundedSender<Out>,
        now: f64,
    ) -> Result<(), String> {
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
        let (Some(username), Some(room), Some(version)) = (username, room, version) else {
            return Err("Not enough Hello arguments".into());
        };
        if let Some(expected) = &self.password_md5 {
            match hello
                .get("password")
                .and_then(Value::as_str)
                .filter(|p| !p.is_empty())
            {
                None => return Err("Password required".into()),
                Some(p) if !p.eq_ignore_ascii_case(expected) => {
                    return Err("Wrong password supplied".into())
                }
                Some(_) => {}
            }
        }
        let name = self.free_username(username);
        let features = hello
            .get("features")
            .cloned()
            .filter(Value::is_object)
            .unwrap_or_else(|| json!({}));
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
                "motd": self.opts.motd,
                "features": self.features(),
            }
        }));
        tracing::info!(user = %name, room = %w.room, client = %version, "Syncplay user joined");
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
        let room = self
            .rooms
            .entry(room_name.to_string())
            .or_insert_with(|| Room::new(now));
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
            let event = json!({ "joined": true, "version": w.version, "features": w.features });
            let msg = self.user_setting(id, None, Some(event));
            let others: Vec<ConnId> = self.audience(id).into_iter().filter(|m| *m != id).collect();
            self.send_to(&others, &msg);
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
    }

    /// Room position, re-anchored on the furthest-behind watcher at most once per second.
    fn room_position(&mut self, room_name: &str, now: f64) -> Option<f64> {
        let room = self.rooms.get(room_name)?;
        let age = now - room.last_update;
        if !room.members.is_empty() && age > 1.0 {
            let paused = room.paused;
            let mut best: Option<ConnId> = None;
            for id in &room.members {
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
                    if other.is_some_and(|u| u != own_name) {
                        // Setting other users' readiness requires managed rooms, which this server does not offer.
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
                        self.watchers.get_mut(&id).unwrap().features = value.clone();
                    }
                }
                "controllerAuth" => {
                    let w = &self.watchers[&id];
                    let room = value
                        .get("room")
                        .and_then(Value::as_str)
                        .unwrap_or(&w.room)
                        .to_string();
                    w.send_set(json!({ "controllerAuth": { "user": w.name, "room": room, "success": false } }));
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
            && files.iter().all(Value::is_string);
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
        let mut list = Map::new();
        for wid in ids {
            let Some(w) = self.watchers.get(&wid) else {
                continue;
            };
            let room = list.entry(w.room.clone()).or_insert_with(|| json!({}));
            room[w.name.clone()] = json!({
                "file": w.file.clone().unwrap_or_else(|| json!({})),
                "controller": false,
                "isReady": w.ready,
                "features": w.features,
            });
        }
        self.watchers[&id].send(json!({ "List": list }));
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
        if pause_changed {
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
            self.force_position_update(id, do_seek == Some(true), now);
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
            tracing::info!(user = %w.name, room = %w.room, "Syncplay user left");
        }
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
}
