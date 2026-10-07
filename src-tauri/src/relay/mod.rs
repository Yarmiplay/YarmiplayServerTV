//! File relay: YarmiplayTV clients in a room offer their local files, and a
//! client without the file streams it from the others through this server.
//!
//! Files are identified per room by `(size, quickHash)`. Bytes arrive from
//! seeders in runs of 4 MiB chunks (`PUT /yarmiplay/upload/{id}`) and are
//! cached on disk, so readers (`GET /yarmiplay/files/{id}`) can read a chunk
//! while it is still arriving and later readers don't cost the seeder again.
//! The cache is wiped at startup, files unused for a day are dropped, and the
//! total stays under the configured size and a free-disk floor (oldest chunks
//! go first).

pub mod cache;
pub mod http;
pub mod scheduler;

use crate::syncplay::ext::{Effect, OfferedFile};
use crate::syncplay::room::{ConnId, ServerState};
use cache::{chunk_count, chunk_len, Chunk, ChunkState, Store, CHUNK};
use parking_lot::Mutex;
use scheduler::{Mode, RateMeter, Reader};
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};
use tokio::sync::{watch, Notify};
use tracing::{debug, info};

/// Unused files are dropped from the cache after this long.
const UNUSED_TTL: Duration = Duration::from_secs(24 * 3600);
/// A reader waiting this long for bytes that don't come gives up.
const READ_STALL: Duration = Duration::from_secs(60);
const LIST_EVERY: Duration = Duration::from_secs(2);
const SWEEP_EVERY: Duration = Duration::from_secs(30);

/// SHA-256 over the first and last MiB of the file and its size (u64 little
/// endian), hex. The same function every client implements.
pub fn quick_hash(first: &[u8], last: &[u8], size: u64) -> String {
    let mut h = Sha256::new();
    h.update(first);
    h.update(last);
    h.update(size.to_le_bytes());
    hex::encode(h.finalize())
}

fn file_id(room: &str, size: u64, quick_hash: &str) -> String {
    let mut h = Sha256::new();
    h.update(room.as_bytes());
    h.update([0]);
    h.update(size.to_le_bytes());
    h.update(quick_hash.as_bytes());
    hex::encode(&h.finalize()[..16])
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RelayActivity {
    pub room: String,
    pub name: String,
    pub size: u64,
    pub cached_bytes: u64,
    pub sources: usize,
    pub readers: usize,
    pub rate: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct RelayStatus {
    pub enabled: bool,
    pub cache_bytes: u64,
    pub cache_limit_bytes: u64,
    pub active: Vec<RelayActivity>,
}

struct RelayFile {
    room: String,
    name: String,
    size: u64,
    duration: f64,
    quick_hash: String,
    seeders: Vec<ConnId>,
    chunks: Vec<Chunk>,
    readers: HashMap<u64, Reader>,
    penalties: HashMap<ConnId, Instant>,
    rate: RateMeter,
    last_used: Instant,
    progress: watch::Sender<u64>,
}

impl RelayFile {
    fn cached_bytes(&self) -> u64 {
        self.chunks
            .iter()
            .enumerate()
            .filter(|(_, c)| c.state == ChunkState::Done)
            .map(|(i, _)| chunk_len(self.size, i as u64))
            .sum()
    }

    fn bump(&self) {
        self.progress.send_modify(|g| *g = g.wrapping_add(1));
    }
}

struct Upload {
    file: String,
    seeder: ConnId,
    first: u64,
    count: u64,
    last_progress: Instant,
}

impl Upload {
    fn offset(&self) -> u64 {
        self.first * CHUNK
    }
}

enum Msg {
    Conn(ConnId, &'static str, Value),
    Room(String, &'static str, Value),
}

struct State {
    enabled: bool,
    limit: u64,
    budget: u64,
    conns: HashMap<ConnId, String>,
    files: HashMap<String, RelayFile>,
    uploads: HashMap<String, Upload>,
    cache_bytes: u64,
    next_reader: u64,
    sent: HashMap<String, Value>,
    last_list: Instant,
    last_sweep: Instant,
    out: Vec<Msg>,
}

struct Inner {
    store: Store,
    st: Mutex<State>,
    wake: Notify,
    server: Mutex<Option<Weak<Mutex<ServerState>>>>,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

#[derive(Clone)]
pub struct Relay {
    inner: Arc<Inner>,
}

impl Relay {
    /// `dir` is wiped: the cache never outlives the process.
    pub fn new(dir: PathBuf, limit_bytes: u64, enabled: bool) -> Self {
        let store = Store::new(dir);
        let now = Instant::now();
        let budget = cache::budget(limit_bytes, 0, store.free_space());
        Self {
            inner: Arc::new(Inner {
                store,
                st: Mutex::new(State {
                    enabled,
                    limit: limit_bytes,
                    budget,
                    conns: HashMap::new(),
                    files: HashMap::new(),
                    uploads: HashMap::new(),
                    cache_bytes: 0,
                    next_reader: 1,
                    sent: HashMap::new(),
                    last_list: now,
                    last_sweep: now,
                    out: Vec::new(),
                }),
                wake: Notify::new(),
                server: Mutex::new(None),
                task: Mutex::new(None),
            }),
        }
    }

    /// Connect to a running Syncplay server and start the scheduler.
    pub fn attach(&self, server: Weak<Mutex<ServerState>>) {
        self.reset();
        *self.inner.server.lock() = Some(server);
        let weak = Arc::downgrade(&self.inner);
        let task = tokio::spawn(async move {
            loop {
                let Some(inner) = weak.upgrade() else { return };
                let relay = Relay { inner };
                let wake = relay.inner.wake.notified();
                relay.tick(Instant::now());
                tokio::select! {
                    _ = wake => {}
                    _ = tokio::time::sleep(Duration::from_secs(1)) => {}
                }
            }
        });
        if let Some(old) = self.inner.task.lock().replace(task) {
            old.abort();
        }
    }

    /// The Syncplay server stopped: every session is gone.
    pub fn detach(&self) {
        if let Some(t) = self.inner.task.lock().take() {
            t.abort();
        }
        *self.inner.server.lock() = None;
        self.reset();
    }

    fn reset(&self) {
        let mut st = self.inner.st.lock();
        st.conns.clear();
        st.uploads.clear();
        st.sent.clear();
        st.out.clear();
        for f in st.files.values_mut() {
            f.seeders.clear();
            for c in &mut f.chunks {
                if c.state == ChunkState::Inflight {
                    c.state = ChunkState::Missing;
                    c.have = 0;
                }
            }
            f.bump();
        }
        recount(&mut st);
    }

    pub fn enabled(&self) -> bool {
        self.inner.st.lock().enabled
    }

    pub fn set_enabled(&self, enabled: bool) {
        {
            let mut st = self.inner.st.lock();
            if st.enabled == enabled {
                return;
            }
            st.enabled = enabled;
            if !enabled {
                let uploads: Vec<(String, ConnId)> = st
                    .uploads
                    .iter()
                    .map(|(id, u)| (id.clone(), u.seeder))
                    .collect();
                for (id, seeder) in uploads {
                    st.out
                        .push(Msg::Conn(seeder, "uploadCancel", json!({ "id": id })));
                }
                let rooms: Vec<String> = st.files.values().map(|f| f.room.clone()).collect();
                for room in rooms {
                    st.out.push(Msg::Room(room, "files", json!([])));
                }
                st.uploads.clear();
                for f in st.files.values() {
                    f.bump();
                }
                st.files.clear();
                st.sent.clear();
                st.cache_bytes = 0;
                self.inner.store.wipe();
                info!("file relay off; cache cleared");
            }
        }
        self.flush();
    }

    pub fn set_limit(&self, limit_bytes: u64) {
        let mut st = self.inner.st.lock();
        st.limit = limit_bytes;
        st.budget = cache::budget(limit_bytes, st.cache_bytes, self.inner.store.free_space());
        self.evict(&mut st, 0);
    }

    /// Drop every cached chunk nobody is reading right now.
    pub fn clear_cache(&self) {
        let mut st = self.inner.st.lock();
        let ids: Vec<String> = st.files.keys().cloned().collect();
        for id in ids {
            let f = st.files.get_mut(&id).unwrap();
            for (i, c) in f.chunks.iter_mut().enumerate() {
                if c.state == ChunkState::Done && c.pins == 0 {
                    c.state = ChunkState::Missing;
                    c.have = 0;
                    self.inner.store.remove_chunk(&id, i as u64);
                }
            }
            f.bump();
        }
        recount(&mut st);
        drop(st);
        self.inner.wake.notify_one();
    }

    pub fn status(&self) -> RelayStatus {
        let mut st = self.inner.st.lock();
        let now = Instant::now();
        let mut active: Vec<RelayActivity> = st
            .files
            .values_mut()
            .filter(|f| !f.seeders.is_empty() || !f.readers.is_empty())
            .map(|f| RelayActivity {
                room: f.room.clone(),
                name: f.name.clone(),
                size: f.size,
                cached_bytes: f.cached_bytes(),
                sources: f.seeders.len(),
                readers: f.readers.len(),
                rate: f.rate.rate(now),
            })
            .collect();
        active.sort_by(|a, b| (&a.room, &a.name).cmp(&(&b.room, &b.name)));
        RelayStatus {
            enabled: st.enabled,
            cache_bytes: st.cache_bytes,
            cache_limit_bytes: st.limit,
            active,
        }
    }

    /// Apply a Syncplay server [`Effect`].
    pub fn handle(&self, effect: &Effect) {
        {
            let mut st = self.inner.st.lock();
            match effect {
                Effect::Joined { conn, room } => {
                    if st.conns.get(conn).is_some_and(|r| r != room) {
                        self.withdraw(&mut st, *conn);
                    }
                    st.conns.insert(*conn, room.clone());
                    if st.enabled {
                        let list = room_list(&mut st, room);
                        st.out.push(Msg::Conn(*conn, "files", list));
                    }
                }
                Effect::Left { conn } => {
                    self.withdraw(&mut st, *conn);
                    st.conns.remove(conn);
                }
                Effect::Offer { conn, room, files } => {
                    if st.enabled {
                        self.offer(&mut st, *conn, room, files);
                    }
                }
                Effect::UploadFailed {
                    conn,
                    upload,
                    error,
                } => {
                    if st.uploads.get(upload).is_some_and(|u| u.seeder == *conn) {
                        debug!(upload, error, "seeder could not upload");
                        self.fail_upload(&mut st, upload, true);
                    }
                }
                Effect::AuthorizeJellyfin { .. } => return,
            }
        }
        self.flush();
        self.inner.wake.notify_one();
    }

    fn offer(&self, st: &mut State, conn: ConnId, room: &str, files: &[OfferedFile]) {
        st.conns.insert(conn, room.to_string());
        let now = Instant::now();
        let wanted: Vec<String> = files
            .iter()
            .map(|f| file_id(room, f.size, &f.quick_hash))
            .collect();
        let dropped: Vec<String> = st
            .files
            .iter()
            .filter(|(id, f)| f.room == room && f.seeders.contains(&conn) && !wanted.contains(id))
            .map(|(id, _)| id.clone())
            .collect();
        for id in dropped {
            self.drop_seeder(st, &id, conn);
        }
        for (f, id) in files.iter().zip(wanted) {
            let entry = st.files.entry(id).or_insert_with(|| RelayFile {
                room: room.to_string(),
                name: f.name.clone(),
                size: f.size,
                duration: f.duration,
                quick_hash: f.quick_hash.clone(),
                seeders: Vec::new(),
                chunks: (0..chunk_count(f.size)).map(|_| Chunk::new(now)).collect(),
                readers: HashMap::new(),
                penalties: HashMap::new(),
                rate: RateMeter::default(),
                last_used: now,
                progress: watch::channel(0).0,
            });
            if !entry.seeders.contains(&conn) {
                entry.seeders.push(conn);
            }
            if entry.duration <= 0.0 {
                entry.duration = f.duration;
            }
            entry.bump();
        }
        let list = room_list(st, room);
        st.sent.insert(room.to_string(), list.clone());
        st.out.push(Msg::Room(room.to_string(), "files", list));
    }

    /// `conn` no longer serves `file`: cancel its uploads for it.
    fn drop_seeder(&self, st: &mut State, file: &str, conn: ConnId) {
        let uploads: Vec<String> = st
            .uploads
            .iter()
            .filter(|(_, u)| u.seeder == conn && u.file == file)
            .map(|(id, _)| id.clone())
            .collect();
        for id in uploads {
            st.out
                .push(Msg::Conn(conn, "uploadCancel", json!({ "id": id })));
            self.fail_upload(st, &id, false);
        }
        if let Some(f) = st.files.get_mut(file) {
            f.seeders.retain(|s| *s != conn);
            f.bump();
        }
    }

    /// Everything `conn` offered or was uploading goes.
    fn withdraw(&self, st: &mut State, conn: ConnId) {
        let files: Vec<String> = st
            .files
            .iter()
            .filter(|(_, f)| f.seeders.contains(&conn))
            .map(|(id, _)| id.clone())
            .collect();
        for id in files {
            self.drop_seeder(st, &id, conn);
        }
        let uploads: Vec<String> = st
            .uploads
            .iter()
            .filter(|(_, u)| u.seeder == conn)
            .map(|(id, _)| id.clone())
            .collect();
        for id in uploads {
            self.fail_upload(st, &id, false);
        }
    }

    /// Forget an upload; chunks it didn't finish go back to missing.
    fn fail_upload(&self, st: &mut State, upload: &str, penalize: bool) {
        let Some(u) = st.uploads.remove(upload) else {
            return;
        };
        if let Some(f) = st.files.get_mut(&u.file) {
            for i in u.first..u.first + u.count {
                let c = &mut f.chunks[i as usize];
                if c.state == ChunkState::Inflight {
                    c.state = ChunkState::Missing;
                    c.have = 0;
                }
            }
            if penalize {
                f.penalties
                    .insert(u.seeder, Instant::now() + scheduler::PENALTY);
            }
            f.bump();
        }
        recount(st);
    }

    /// Free space for `need` more bytes by dropping the least recently used
    /// finished chunks that nobody is reading and no reader is about to.
    fn evict(&self, st: &mut State, need: u64) -> bool {
        while st.cache_bytes + need > st.budget {
            let victim =
                st.files
                    .iter()
                    .flat_map(|(id, f)| {
                        f.chunks
                            .iter()
                            .enumerate()
                            .filter(|(i, c)| {
                                let i = *i as u64;
                                let ahead = f.readers.values().any(|r| {
                                    i >= r.chunk && i < r.chunk + scheduler::READAHEAD_CHUNKS
                                });
                                c.state == ChunkState::Done && c.pins == 0 && !ahead
                            })
                            .map(move |(i, c)| (c.last_used, id.clone(), i))
                    })
                    .min();
            let Some((_, id, i)) = victim else {
                return false;
            };
            let f = st.files.get_mut(&id).unwrap();
            let c = &mut f.chunks[i];
            c.state = ChunkState::Missing;
            c.have = 0;
            st.cache_bytes -= chunk_len(f.size, i as u64);
            self.inner.store.remove_chunk(&id, i as u64);
        }
        true
    }

    fn tick(&self, now: Instant) {
        {
            let mut st = self.inner.st.lock();
            if st.enabled {
                let stalled: Vec<(String, ConnId)> = st
                    .uploads
                    .iter()
                    .filter(|(_, u)| now.duration_since(u.last_progress) > scheduler::STALL)
                    .map(|(id, u)| (id.clone(), u.seeder))
                    .collect();
                for (id, seeder) in stalled {
                    debug!(upload = %id, "upload stalled; asking someone else");
                    st.out
                        .push(Msg::Conn(seeder, "uploadCancel", json!({ "id": id })));
                    self.fail_upload(&mut st, &id, true);
                }
                self.schedule(&mut st, now);
                if now.duration_since(st.last_sweep) >= SWEEP_EVERY {
                    st.last_sweep = now;
                    self.sweep(&mut st, now);
                }
                if now.duration_since(st.last_list) >= LIST_EVERY {
                    st.last_list = now;
                    let rooms: Vec<String> = {
                        let mut r: Vec<String> = st.conns.values().cloned().collect();
                        r.sort();
                        r.dedup();
                        r
                    };
                    for room in rooms {
                        let list = room_list(&mut st, &room);
                        if st.sent.get(&room) != Some(&list) {
                            st.sent.insert(room.clone(), list.clone());
                            st.out.push(Msg::Room(room, "files", list));
                        }
                    }
                }
            }
        }
        self.flush();
    }

    fn schedule(&self, st: &mut State, now: Instant) {
        let mut load: HashMap<ConnId, usize> = HashMap::new();
        for u in st.uploads.values() {
            *load.entry(u.seeder).or_default() += 1;
        }
        let ids: Vec<String> = st
            .files
            .iter()
            .filter(|(_, f)| !f.readers.is_empty() && !f.seeders.is_empty())
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            let (runs, size) = {
                let f = &st.files[&id];
                let readers: Vec<Reader> = f.readers.values().copied().collect();
                let missing: Vec<u64> = scheduler::wanted(&readers, f.chunks.len() as u64)
                    .into_iter()
                    .filter(|i| f.chunks[*i as usize].state == ChunkState::Missing)
                    .collect();
                (scheduler::runs(&missing), f.size)
            };
            for (first, mut count) in runs {
                let seeder = {
                    let f = &st.files[&id];
                    scheduler::pick_seeder(&f.seeders, &load, &f.penalties, now)
                };
                let Some(seeder) = seeder else { break };
                // Shorten the run until it fits the cache.
                let run_len = |n: u64| (first..first + n).map(|i| chunk_len(size, i)).sum::<u64>();
                while count > 0 && !self.evict(st, run_len(count)) {
                    count -= 1;
                }
                if count == 0 {
                    break;
                }
                let length = run_len(count);
                if self.inner.store.ensure_file_dir(&id).is_err() {
                    break;
                }
                let upload = crate::syncplay::ext::new_token()[..24].to_string();
                let f = st.files.get_mut(&id).unwrap();
                for i in first..first + count {
                    let c = &mut f.chunks[i as usize];
                    c.state = ChunkState::Inflight;
                    c.have = 0;
                }
                st.cache_bytes += length;
                *load.entry(seeder).or_default() += 1;
                let msg = json!({
                    "id": upload,
                    "file": id,
                    "size": f.size,
                    "quickHash": f.quick_hash,
                    "offset": first * CHUNK,
                    "length": length,
                });
                st.uploads.insert(
                    upload,
                    Upload {
                        file: id.clone(),
                        seeder,
                        first,
                        count,
                        last_progress: now,
                    },
                );
                st.out.push(Msg::Conn(seeder, "upload", msg));
            }
        }
    }

    fn sweep(&self, st: &mut State, now: Instant) {
        st.budget = cache::budget(st.limit, st.cache_bytes, self.inner.store.free_space());
        let ids: Vec<String> = st.files.keys().cloned().collect();
        for id in ids {
            let f = st.files.get_mut(&id).unwrap();
            if !f.readers.is_empty() {
                continue;
            }
            let busy = f
                .chunks
                .iter()
                .any(|c| c.pins > 0 || c.state == ChunkState::Inflight);
            if !busy && now.duration_since(f.last_used) > UNUSED_TTL {
                for c in &mut f.chunks {
                    c.state = ChunkState::Missing;
                    c.have = 0;
                }
                self.inner.store.remove_file(&id);
                f.bump();
            }
            let empty = f.chunks.iter().all(|c| c.state == ChunkState::Missing);
            if empty && f.seeders.is_empty() && !busy {
                st.files.remove(&id);
                self.inner.store.remove_file(&id);
            }
        }
        recount(st);
        self.evict(st, 0);
    }

    /// Deliver queued messages (outside the relay lock).
    fn flush(&self) {
        let out = std::mem::take(&mut self.inner.st.lock().out);
        if out.is_empty() {
            return;
        }
        let Some(server) = self.inner.server.lock().as_ref().and_then(Weak::upgrade) else {
            return;
        };
        let state = server.lock();
        for m in out {
            match m {
                Msg::Conn(id, sub, v) => {
                    state.send_ext(id, sub, v);
                }
                Msg::Room(room, sub, v) => state.send_ext_room(&room, sub, &v),
            }
        }
    }
}

/// Recompute the cache total from chunk states.
fn recount(st: &mut State) {
    st.cache_bytes = st
        .files
        .values()
        .map(|f| {
            f.chunks
                .iter()
                .enumerate()
                .filter(|(_, c)| c.state != ChunkState::Missing)
                .map(|(i, _)| chunk_len(f.size, i as u64))
                .sum::<u64>()
        })
        .sum();
}

fn room_list(st: &mut State, room: &str) -> Value {
    let now = Instant::now();
    let mut files: Vec<(String, Value)> = st
        .files
        .iter_mut()
        .filter(|(_, f)| f.room == room)
        .filter_map(|(id, f)| {
            let cached = f.cached_bytes();
            (!f.seeders.is_empty() || cached > 0).then(|| {
                (
                    f.name.clone(),
                    json!({
                        "id": id,
                        "name": f.name,
                        "size": f.size,
                        "duration": f.duration,
                        "quickHash": f.quick_hash,
                        "sources": f.seeders.len(),
                        "cachedBytes": cached,
                        "rate": f.rate.rate(now),
                    }),
                )
            })
        })
        .collect();
    files.sort_by(|a, b| a.0.cmp(&b.0));
    Value::Array(files.into_iter().map(|(_, v)| v).collect())
}

/// A file as a reader sees it.
#[derive(Debug, Clone)]
pub struct FileInfo {
    pub name: String,
    pub size: u64,
}

impl Relay {
    pub fn file_info(&self, room: &str, id: &str) -> Option<FileInfo> {
        let st = self.inner.st.lock();
        if !st.enabled {
            return None;
        }
        st.files
            .get(id)
            .filter(|f| f.room == room)
            .map(|f| FileInfo {
                name: f.name.clone(),
                size: f.size,
            })
    }

    /// Start reading `id` from byte `start`; the returned guard keeps the
    /// reader's demand registered until dropped.
    fn open_reader(&self, id: &str, start: u64, mode: Mode) -> Option<(u64, watch::Receiver<u64>)> {
        let mut st = self.inner.st.lock();
        let reader = st.next_reader;
        st.next_reader += 1;
        let f = st.files.get_mut(id)?;
        f.readers.insert(
            reader,
            Reader {
                chunk: start / CHUNK,
                mode,
            },
        );
        f.last_used = Instant::now();
        let rx = f.progress.subscribe();
        drop(st);
        self.inner.wake.notify_one();
        Some((reader, rx))
    }

    fn close_reader(&self, id: &str, reader: u64) {
        if let Some(f) = self.inner.st.lock().files.get_mut(id) {
            f.readers.remove(&reader);
        }
        self.inner.wake.notify_one();
    }

    /// Up to `max` bytes at `offset`, once they are cached. `None` when the
    /// file is gone or nothing arrived for [`READ_STALL`].
    async fn read_at(
        &self,
        id: &str,
        reader: u64,
        rx: &mut watch::Receiver<u64>,
        offset: u64,
        max: u64,
    ) -> Option<Vec<u8>> {
        let index = offset / CHUNK;
        let within = offset % CHUNK;
        loop {
            let ready = {
                let mut st = self.inner.st.lock();
                rx.borrow_and_update();
                let f = st.files.get_mut(id)?;
                if let Some(r) = f.readers.get_mut(&reader) {
                    if r.chunk != index {
                        r.chunk = index;
                        self.inner.wake.notify_one();
                    }
                }
                let now = Instant::now();
                f.last_used = now;
                let c = &mut f.chunks[index as usize];
                if c.state != ChunkState::Missing && c.have > within {
                    c.pins += 1;
                    c.last_used = now;
                    Some((c.have - within).min(max))
                } else {
                    None
                }
            };
            if let Some(n) = ready {
                let path = self.inner.store.chunk_path(id, index);
                let data = read_part(path, within, n).await;
                if let Some(f) = self.inner.st.lock().files.get_mut(id) {
                    f.chunks[index as usize].pins -= 1;
                }
                match data {
                    Some(d) if !d.is_empty() => return Some(d),
                    _ => continue,
                }
            }
            tokio::time::timeout(READ_STALL, rx.changed())
                .await
                .ok()?
                .ok()?;
        }
    }

    /// Check that `conn` may upload `upload`; returns where its bytes go.
    fn upload_target(&self, conn: ConnId, upload: &str) -> Option<(String, u64, u64, u64)> {
        let st = self.inner.st.lock();
        let u = st.uploads.get(upload).filter(|u| u.seeder == conn)?;
        let size = st.files.get(&u.file)?.size;
        let length = (u.first..u.first + u.count)
            .map(|i| chunk_len(size, i))
            .sum();
        Some((u.file.clone(), size, u.offset(), length))
    }

    /// Record `n` bytes written at `pos` for `upload`. False once the upload
    /// was cancelled.
    fn upload_progress(&self, upload: &str, pos: u64, n: u64) -> bool {
        let mut st = self.inner.st.lock();
        let now = Instant::now();
        let Some(u) = st.uploads.get_mut(upload) else {
            return false;
        };
        u.last_progress = now;
        let file = u.file.clone();
        let Some(f) = st.files.get_mut(&file) else {
            return false;
        };
        let index = pos / CHUNK;
        let len = chunk_len(f.size, index);
        let c = &mut f.chunks[index as usize];
        c.have = (pos % CHUNK + n).min(len);
        c.last_used = now;
        if c.have == len {
            c.state = ChunkState::Done;
        }
        f.rate.add(now, n);
        f.last_used = now;
        f.bump();
        true
    }

    fn upload_still_on(&self, upload: &str) -> bool {
        self.inner.st.lock().uploads.contains_key(upload)
    }

    fn finish_upload(&self, upload: &str, complete: bool) {
        {
            let mut st = self.inner.st.lock();
            if complete {
                st.uploads.remove(upload);
            } else {
                self.fail_upload(&mut st, upload, true);
            }
        }
        self.flush();
        self.inner.wake.notify_one();
    }
}

async fn read_part(path: PathBuf, at: u64, n: u64) -> Option<Vec<u8>> {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};
    let mut f = tokio::fs::File::open(path).await.ok()?;
    f.seek(std::io::SeekFrom::Start(at)).await.ok()?;
    let mut buf = vec![0u8; n as usize];
    let mut got = 0;
    while got < buf.len() {
        match f.read(&mut buf[got..]).await.ok()? {
            0 => break,
            k => got += k,
        }
    }
    buf.truncate(got);
    Some(buf)
}

#[cfg(test)]
mod tests;
