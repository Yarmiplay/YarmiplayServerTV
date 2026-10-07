//! End to end: a seeder and a leecher talk to a real server over TCP (Syncplay
//! lines) and HTTP (the relay endpoints) on the same port.

use super::*;
use crate::syncplay::protocol::line;
use crate::syncplay::{Extensions, SyncplayOptions, SyncplayServer};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

struct Client {
    r: BufReader<TcpStream>,
    token: String,
}

impl Client {
    async fn connect(port: u16, name: &str, ext: bool) -> Self {
        let mut r = BufReader::new(TcpStream::connect(("127.0.0.1", port)).await.unwrap());
        let mut features = json!({ "sharedPlaylists": true });
        if ext {
            features["yarmiplay"] = json!({ "protocol": 1 });
        }
        let hello = json!({ "Hello": { "username": name, "room": { "name": "movie" }, "version": "1.2.255", "realversion": "1.7.4", "features": features } });
        r.get_mut()
            .write_all(line(&hello).as_bytes())
            .await
            .unwrap();
        let mut c = Self {
            r,
            token: String::new(),
        };
        if ext {
            let s = c.ext("session").await;
            c.token = s["token"].as_str().unwrap().to_string();
        } else {
            c.until(|m| m.get("Hello").is_some()).await;
        }
        c
    }

    async fn next(&mut self) -> Value {
        let mut buf = String::new();
        let n = tokio::time::timeout(Duration::from_secs(10), self.r.read_line(&mut buf))
            .await
            .expect("timed out waiting for the server")
            .unwrap();
        assert!(n > 0, "connection closed");
        serde_json::from_str(buf.trim()).unwrap()
    }

    async fn until(&mut self, f: impl Fn(&Value) -> bool) -> Value {
        loop {
            let m = self.next().await;
            if f(&m) {
                return m;
            }
        }
    }

    /// The next `{"Yarmiplay": {sub: ...}}`.
    async fn ext(&mut self, sub: &str) -> Value {
        self.until(|m| m["Yarmiplay"].get(sub).is_some()).await["Yarmiplay"][sub].clone()
    }

    async fn send(&mut self, v: Value) {
        self.r
            .get_mut()
            .write_all(line(&v).as_bytes())
            .await
            .unwrap();
    }
}

fn content(size: usize) -> Vec<u8> {
    (0..size).map(|i| (i * 7 + i / 4093) as u8).collect()
}

fn hash_of(data: &[u8]) -> String {
    let m = (1usize << 20).min(data.len());
    quick_hash(&data[..m], &data[data.len() - m..], data.len() as u64)
}

async fn start() -> (SyncplayServer, Relay, tempfile::TempDir) {
    start_with_limit(1 << 30).await
}

async fn start_with_limit(limit: u64) -> (SyncplayServer, Relay, tempfile::TempDir) {
    crate::net::install_crypto_provider();
    let dir = tempfile::tempdir().unwrap();
    let relay = Relay::new(dir.path().join("relay"), limit, true);
    let opts = SyncplayOptions {
        file_relay: true,
        ..Default::default()
    };
    let ext = Extensions {
        relay: Some(relay.clone()),
        ..Default::default()
    };
    let server = SyncplayServer::start(0, opts, Arc::default(), Arc::new(|| {}), ext)
        .await
        .unwrap();
    (server, relay, dir)
}

/// Answer upload requests from `data` until told to stop.
fn seed(
    mut c: Client,
    port: u16,
    data: Arc<Vec<u8>>,
    fail_first: bool,
) -> tokio::task::JoinHandle<Client> {
    tokio::spawn(async move {
        let http = reqwest::Client::new();
        let mut failed = !fail_first;
        loop {
            let m = c.next().await;
            let Some(u) = m["Yarmiplay"].get("upload").cloned() else {
                if m["Yarmiplay"]
                    .get("files")
                    .is_some_and(|f| f.as_array().is_some_and(|a| a.is_empty()))
                {
                    return c;
                }
                continue;
            };
            let id = u["id"].as_str().unwrap().to_string();
            if !failed {
                failed = true;
                c.send(
                    json!({ "Yarmiplay": { "uploadFailed": { "id": id, "error": "disk busy" } } }),
                )
                .await;
                continue;
            }
            let offset = u["offset"].as_u64().unwrap() as usize;
            let length = u["length"].as_u64().unwrap() as usize;
            let body = data[offset..offset + length].to_vec();
            let r = http
                .put(format!("http://127.0.0.1:{port}/yarmiplay/upload/{id}"))
                .bearer_auth(&c.token)
                .body(body)
                .send()
                .await
                .unwrap();
            assert_eq!(r.status(), 204, "{}", r.text().await.unwrap_or_default());
        }
    })
}

#[tokio::test]
async fn leecher_streams_a_file_from_a_seeder_through_the_cache() {
    let (server, relay, _dir) = start().await;
    let port = server.port;
    let data = Arc::new(content(2 * CHUNK as usize + 12_345));
    let hash = hash_of(&data);

    let mut seeder = Client::connect(port, "seeder", true).await;
    seeder.ext("files").await;
    let mut leecher = Client::connect(port, "leecher", true).await;
    assert_eq!(leecher.ext("files").await, json!([]));
    seeder
        .send(json!({ "Yarmiplay": { "offer": { "files": [
            { "name": "Movie.mkv", "size": data.len(), "duration": 60.0, "quickHash": hash }
        ] } } }))
        .await;
    let files = leecher.ext("files").await;
    assert_eq!(files[0]["name"], "Movie.mkv");
    assert_eq!(files[0]["sources"], 1);
    assert_eq!(files[0]["quickHash"], hash);
    let id = files[0]["id"].as_str().unwrap().to_string();
    let seeding = seed(seeder, port, data.clone(), false);

    let http = reqwest::Client::new();
    let url = format!("http://127.0.0.1:{port}/yarmiplay/files/{id}");
    assert_eq!(http.get(&url).send().await.unwrap().status(), 401);
    let head = http
        .head(&url)
        .bearer_auth(&leecher.token)
        .send()
        .await
        .unwrap();
    assert_eq!(head.status(), 200);
    assert_eq!(head.headers()["content-length"], data.len().to_string());
    assert_eq!(head.headers()["content-type"], "video/x-matroska");

    let got = http
        .get(format!("{url}?t={}", leecher.token))
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    assert_eq!(got.len(), data.len());
    assert!(got[..] == data[..], "relayed bytes differ");

    // A range read straddling a chunk boundary, from the cache.
    let start = CHUNK as usize - 10;
    let r = http
        .get(&url)
        .bearer_auth(&leecher.token)
        .header("Range", format!("bytes={start}-{}", start + 19))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 206);
    assert_eq!(
        r.headers()["content-range"],
        format!("bytes {start}-{}/{}", start + 19, data.len())
    );
    assert_eq!(&r.bytes().await.unwrap()[..], &data[start..start + 20]);

    let status = relay.status();
    assert_eq!(status.cache_bytes, data.len() as u64);
    assert_eq!(status.active[0].sources, 1);

    // The seeder leaves; the cached copy is still listed and readable.
    seeding.abort();
    let _ = seeding.await;
    leecher
        .until(|m| m["Yarmiplay"]["files"][0]["sources"] == 0)
        .await;
    let r = http
        .get(&url)
        .bearer_auth(&leecher.token)
        .header("Range", "bytes=-100")
        .send()
        .await
        .unwrap();
    assert_eq!(&r.bytes().await.unwrap()[..], &data[data.len() - 100..]);

    // Other rooms can't see it.
    let mut other = Client::connect(port, "other", true).await;
    other
        .send(json!({ "Set": { "room": { "name": "elsewhere" } } }))
        .await;
    other.until(|m| m["Yarmiplay"]["files"] == json!([])).await;
    let r = http
        .get(&url)
        .bearer_auth(&other.token)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);

    relay.clear_cache();
    assert_eq!(relay.status().cache_bytes, 0);
    server.stop();
}

#[tokio::test]
async fn failed_uploads_move_to_another_seeder() {
    let (server, _relay, _dir) = start().await;
    let port = server.port;
    let data = Arc::new(content(CHUNK as usize + 1));
    let offer = json!({ "Yarmiplay": { "offer": { "files": [
        { "name": "a.mkv", "size": data.len(), "duration": 1.0, "quickHash": hash_of(&data) }
    ] } } });
    let mut a = Client::connect(port, "a", true).await;
    a.send(offer.clone()).await;
    a.ext("files").await;
    let mut b = Client::connect(port, "b", true).await;
    b.send(offer).await;
    let files = b
        .until(|m| m["Yarmiplay"]["files"][0]["sources"] == 2)
        .await;
    let id = files["Yarmiplay"]["files"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    // `a` is asked first (lowest connection id) and refuses; `b` takes over.
    let a = seed(a, port, data.clone(), true);
    let b = seed(b, port, data.clone(), false);

    let reader = Client::connect(port, "reader", true).await;
    let got = reqwest::Client::new()
        .get(format!(
            "http://127.0.0.1:{port}/yarmiplay/files/{id}?mode=download"
        ))
        .bearer_auth(&reader.token)
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    assert!(got[..] == data[..]);
    a.abort();
    b.abort();
    server.stop();
}

/// A file bigger than the cache streams through a sliding window of chunks.
#[tokio::test]
async fn files_bigger_than_the_cache_stream_through() {
    let (server, relay, _dir) = start_with_limit(2 * CHUNK).await;
    let port = server.port;
    let data = Arc::new(content(5 * CHUNK as usize + 99));
    let mut seeder = Client::connect(port, "seeder", true).await;
    seeder
        .send(json!({ "Yarmiplay": { "offer": { "files": [
            { "name": "big.mkv", "size": data.len(), "duration": 1.0, "quickHash": hash_of(&data) }
        ] } } }))
        .await;
    let files = seeder
        .until(|m| m["Yarmiplay"]["files"][0]["id"].is_string())
        .await;
    let id = files["Yarmiplay"]["files"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let seeding = seed(seeder, port, data.clone(), false);

    let reader = Client::connect(port, "reader", true).await;
    let mut resp = reqwest::Client::new()
        .get(format!("http://127.0.0.1:{port}/yarmiplay/files/{id}"))
        .bearer_auth(&reader.token)
        .send()
        .await
        .unwrap();
    let mut got = Vec::new();
    let mut peak = 0;
    while let Some(part) = resp.chunk().await.unwrap() {
        got.extend_from_slice(&part);
        peak = peak.max(relay.status().cache_bytes);
    }
    assert!(got[..] == data[..]);
    assert!(peak <= 2 * CHUNK, "cache grew to {peak}");
    seeding.abort();
    server.stop();
}

/// When the only seeder working on a request leaves, another seeder finishes it.
#[tokio::test]
async fn work_moves_on_when_a_seeder_leaves() {
    let (server, _relay, _dir) = start().await;
    let port = server.port;
    let data = Arc::new(content(CHUNK as usize * 2));
    let offer = json!({ "Yarmiplay": { "offer": { "files": [
        { "name": "a.mkv", "size": data.len(), "duration": 1.0, "quickHash": hash_of(&data) }
    ] } } });
    let mut quitter = Client::connect(port, "quitter", true).await;
    quitter.send(offer.clone()).await;
    quitter.ext("files").await;
    let mut stayer = Client::connect(port, "stayer", true).await;
    stayer.send(offer).await;
    let files = stayer
        .until(|m| m["Yarmiplay"]["files"][0]["sources"] == 2)
        .await;
    let id = files["Yarmiplay"]["files"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let stayer = seed(stayer, port, data.clone(), false);

    let reader = Client::connect(port, "reader", true).await;
    let fetch = tokio::spawn(async move {
        reqwest::Client::new()
            .get(format!("http://127.0.0.1:{port}/yarmiplay/files/{id}"))
            .bearer_auth(&reader.token)
            .send()
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap()
    });
    // The quitter gets the first request (lowest id) and disconnects instead of answering.
    quitter
        .until(|m| m["Yarmiplay"].get("upload").is_some())
        .await;
    drop(quitter);
    let got = tokio::time::timeout(Duration::from_secs(20), fetch)
        .await
        .expect("the stayer never took over")
        .unwrap();
    assert!(got[..] == data[..]);
    stayer.abort();
    server.stop();
}

#[tokio::test]
async fn vanilla_clients_and_vanilla_mode_get_no_relay() {
    let (server, relay, _dir) = start().await;
    let port = server.port;
    let mut v = Client::connect(port, "vanilla", false).await;
    v.send(json!({ "Yarmiplay": { "offer": { "files": [
        { "name": "a.mkv", "size": 10, "quickHash": "a".repeat(64) }
    ] } } }))
    .await;
    let info: Value = reqwest::get(format!("http://127.0.0.1:{port}/yarmiplay/info"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(info["capabilities"]["fileRelay"], true);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(relay.status().active.is_empty());

    let mut y = Client::connect(port, "yarmi", true).await;
    server.set_options(SyncplayOptions {
        vanilla_mode: true,
        file_relay: true,
        ..Default::default()
    });
    let caps = y.ext("capabilities").await;
    assert_eq!(caps["fileRelay"], false);
    assert!(!relay.enabled());
    // HTTP is no longer sniffed: the request is read as a broken Syncplay line.
    let r = reqwest::get(format!("http://127.0.0.1:{port}/yarmiplay/info")).await;
    assert!(r.is_err() || !r.unwrap().status().is_success());
    server.stop();
}
