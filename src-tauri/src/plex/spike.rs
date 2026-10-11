//! Live checks against a real Plex Media Server and plex.tv account, for the
//! parts unit tests can't prove: linking, ownership, `customConnections`,
//! libraries, whether Plex counts relayed streams as remote, and the host side
//! of friend invites. Opt-in and interactive (it prints a plex.tv/link code):
//!
//! ```text
//! PLEX_SPIKE_DIR=<scratch folder> PLEX_SPIKE_EMAIL=<an address you own> \
//!   cargo test plex_spike -- --ignored --nocapture --test-threads=1
//! ```
//!
//! Everything it creates (a test library, a relay URL in `customConnections`,
//! invites) is removed again; it never touches anything it didn't create.

use super::api::{LibraryKind, PlexApi};
use super::share::with_relay;
use super::tv::PlexTv;
use crate::net::proxy::LocalProxy;
use axum::body::Body;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::Duration;

const PORT: u16 = 32400;
const CLIENT_ID: &str = "ystv-spike";
const RELAY_URL: &str = "https://ystv-spike.example.com:8999";
const LIBRARY: &str = "YarmiplayServerTV spike";
/// A public (documentation) address the proxy claims the viewer came from.
const FAKE_PEER: &str = "203.0.113.5";

fn report(step: &str, ok: bool, detail: impl std::fmt::Display) {
    println!("{} {step}: {detail}", if ok { "PASS" } else { "FAIL" });
}

async fn link(dir: &Path) -> String {
    let file = dir.join("plex-token");
    if let Ok(t) = std::fs::read_to_string(&file) {
        if !t.trim().is_empty() {
            return t.trim().to_string();
        }
    }
    let anon = PlexTv::new(CLIENT_ID, None);
    let pin = anon.create_pin().await.expect("link code");
    println!(
        "LINK CODE: {}  (enter it at https://plex.tv/link)",
        pin.code
    );
    for _ in 0..300 {
        tokio::time::sleep(Duration::from_secs(2)).await;
        if let Ok(Some(token)) = anon.check_pin(pin.id).await {
            crate::paths::write_atomic(&file, token.as_bytes()).unwrap();
            return token;
        }
    }
    panic!("the link code expired");
}

async fn get_json(token: &str, path: &str) -> Value {
    let client = crate::net::client_builder().build().unwrap();
    super::tv::plex_headers(
        client.get(format!("http://127.0.0.1:{PORT}{path}")),
        CLIENT_ID,
    )
    .header("X-Plex-Token", token)
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap_or(Value::Null)
}

/// A 5-second clip made with Plex's own ffmpeg build, which has no lavfi
/// input, so it encodes raw grey frames piped in.
fn make_clip(folder: &Path) -> Result<PathBuf, String> {
    std::fs::create_dir_all(folder).map_err(|e| e.to_string())?;
    let out = folder.join("Spike Clip.mkv");
    if out.is_file() {
        return Ok(out);
    }
    let ffmpeg = ["ProgramFiles", "ProgramFiles(x86)"]
        .iter()
        .filter_map(std::env::var_os)
        .map(|d| PathBuf::from(d).join("Plex/Plex Media Server/Plex Transcoder.exe"))
        .find(|p| p.is_file())
        .ok_or("Plex Transcoder not found")?;
    let mut child = std::process::Command::new(ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(["-f", "rawvideo", "-pix_fmt", "yuv420p", "-s", "64x64"])
        .args([
            "-r", "25", "-i", "-", "-c:v", "mjpeg", "-pix_fmt", "yuvj420p",
        ])
        .arg(&out)
        .stdin(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    {
        use std::io::Write;
        let mut stdin = child.stdin.take().ok_or("no stdin")?;
        let frame = vec![128u8; 64 * 64 * 3 / 2];
        for _ in 0..125 {
            stdin.write_all(&frame).map_err(|e| e.to_string())?;
        }
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    if !status.success() {
        return Err(format!("ffmpeg failed: {status}"));
    }
    Ok(out)
}

#[tokio::test]
#[ignore]
async fn plex_spike() {
    let dir = PathBuf::from(std::env::var("PLEX_SPIKE_DIR").expect("set PLEX_SPIKE_DIR"));
    std::fs::create_dir_all(&dir).unwrap();

    // Identity and linking.
    let identity = PlexApi::new(PORT, CLIENT_ID, None)
        .identity()
        .await
        .expect("Plex running");
    report("identity", identity.claimed, format!("{identity:?}"));
    let machine = identity.machine_id.clone();
    let token = link(&dir).await;
    let tv = PlexTv::new(CLIENT_ID, Some(token.clone()));
    match tv.account().await {
        Ok(a) => report("account", true, format!("linked as {}", a.username)),
        Err(e) => report("account", false, e),
    }
    let resources = tv.resources().await.unwrap_or_default();
    let owned = resources
        .iter()
        .any(|r| r.client_identifier == machine && r.owned && r.provides.contains("server"));
    report("ownership", owned, format!("{} resources", resources.len()));
    let api = PlexApi::new(PORT, CLIENT_ID, Some(token.clone()));

    // customConnections reaches plex.tv, and is restored.
    let original = api
        .pref("customConnections")
        .await
        .unwrap_or(None)
        .unwrap_or_default();
    let ours = with_relay(&original, &[RELAY_URL.to_string()]);
    let set = api.set_pref("customConnections", &ours).await;
    report(
        "customConnections set",
        set.is_ok(),
        format!("{set:?} (was {original:?})"),
    );
    let mut seen = false;
    for _ in 0..24 {
        let client = crate::net::client_builder().build().unwrap();
        let raw: Value = super::tv::plex_headers(
            client.get("https://plex.tv/api/v2/resources?includeHttps=1"),
            CLIENT_ID,
        )
        .header("X-Plex-Token", &token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap_or(Value::Null);
        seen = raw.as_array().into_iter().flatten().any(|r| {
            r["clientIdentifier"] == machine.as_str()
                && r["connections"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|c| c["uri"].as_str().unwrap_or("").contains("ystv-spike"))
        });
        if seen {
            break;
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
    report(
        "relay URL listed by plex.tv",
        seen,
        "checked for up to 2 minutes",
    );
    let restored = api.set_pref("customConnections", &original).await;
    let now = api
        .pref("customConnections")
        .await
        .unwrap_or(None)
        .unwrap_or_default();
    report(
        "customConnections restored",
        restored.is_ok() && now == original,
        format!("now {now:?}"),
    );

    // Libraries, on a test folder.
    let clip_dir = dir.join("spike-library");
    let extra_dir = dir.join("spike-library-2");
    std::fs::create_dir_all(&extra_dir).unwrap();
    match make_clip(&clip_dir) {
        Ok(p) => report("test clip", true, p.display()),
        Err(e) => report("test clip", false, e),
    }
    let clip_path = clip_dir.to_string_lossy().into_owned();
    let extra_path = extra_dir.to_string_lossy().into_owned();
    let added = api
        .add_library(LIBRARY, LibraryKind::OtherVideos, &clip_path)
        .await;
    let lib = api
        .libraries()
        .await
        .unwrap_or_default()
        .into_iter()
        .find(|l| l.name == LIBRARY);
    report(
        "add library",
        added.is_ok() && lib.is_some(),
        format!("{added:?}"),
    );
    let Some(lib) = lib else { return };
    let key = lib.key.clone();
    let r = api.add_path(&key, &extra_path).await;
    let has = |libs: Vec<super::api::Library>, p: &str| {
        libs.into_iter()
            .any(|l| l.key == key && l.locations.iter().any(|x| x == p))
    };
    report(
        "add folder",
        r.is_ok() && has(api.libraries().await.unwrap_or_default(), &extra_path),
        format!("{r:?}"),
    );
    let r = api.remove_path(&key, &extra_path).await;
    report(
        "remove folder",
        r.is_ok() && !has(api.libraries().await.unwrap_or_default(), &extra_path),
        format!("{r:?}"),
    );
    report("scan", api.rescan().await.is_ok(), "all libraries");

    // Compliance gate: does Plex see a relayed viewer as remote?
    let mut item = None;
    for _ in 0..30 {
        let v = get_json(&token, &format!("/library/sections/{key}/all")).await;
        if let Some(m) = v.pointer("/MediaContainer/Metadata/0") {
            item = m["ratingKey"].as_str().map(str::to_string);
            break;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    match &item {
        None => report(
            "compliance gate",
            false,
            "the test clip was never scanned in",
        ),
        Some(rating_key) => {
            let proxy = LocalProxy::new("Plex");
            proxy.set_target(Some(PORT));
            let timeline = |state: &str| {
                axum::http::Request::builder()
                    .uri(format!(
                        "/:/timeline?ratingKey={rating_key}&key=%2Flibrary%2Fmetadata%2F{rating_key}&state={state}&time=1000&duration=5000"
                    ))
                    .header("X-Plex-Token", &token)
                    .header("X-Plex-Client-Identifier", "ystv-spike-relayed")
                    .header("X-Plex-Product", "YarmiplayTV")
                    .header("X-Plex-Device-Name", "Relayed viewer")
                    .header("X-Plex-Platform", "Android")
                    .body(Body::empty())
                    .unwrap()
            };
            let peer = FAKE_PEER.parse().unwrap();
            let resp = proxy.forward(timeline("playing"), peer, true).await;
            println!("  timeline through the proxy: {}", resp.status());
            tokio::time::sleep(Duration::from_secs(2)).await;
            let sessions = get_json(&token, "/status/sessions").await;
            let session = sessions
                .pointer("/MediaContainer/Metadata")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .find(|m| m["Player"]["machineIdentifier"] == "ystv-spike-relayed")
                .cloned();
            match session {
                Some(s) => {
                    let location = s["Session"]["location"].as_str().unwrap_or("?").to_string();
                    report(
                        "compliance gate (relayed viewer counted as remote)",
                        location == "wan",
                        format!(
                            "Session.location={location} Player.address={} Player.local={}",
                            s["Player"]["address"], s["Player"]["local"]
                        ),
                    );
                }
                None => report(
                    "compliance gate",
                    false,
                    format!("no session showed up: {sessions}"),
                ),
            }
            let _ = proxy.forward(timeline("stopped"), peer, true).await;
        }
    }
    let r = api.remove_library(&key).await;
    report("remove library", r.is_ok(), format!("{r:?}"));

    // Host side of invites.
    let Ok(email) = std::env::var("PLEX_SPIKE_EMAIL") else {
        println!("SKIP invites: PLEX_SPIKE_EMAIL not set");
        return;
    };
    let ids = tv.section_ids(&machine).await;
    report(
        "plex.tv section ids",
        ids.as_ref().is_ok_and(|m| !m.is_empty()),
        format!("{ids:?}"),
    );
    let ids: Vec<u64> = ids.unwrap_or_default().into_values().collect();
    let first = ids.iter().take(1).copied().collect::<Vec<_>>();
    match tv.invite(&machine, &first, &email).await {
        Err(e) => report("invite", false, e),
        Ok(share) => {
            report("invite", !share.share_id.is_empty(), format!("{share:?}"));
            let listed = tv.share_exists(&share.share_id).await;
            report("invite exists", listed == Ok(true), format!("{listed:?}"));
            let r = tv.update_share(&machine, &share.share_id, &ids).await;
            report("change shared libraries", r.is_ok(), format!("{r:?}"));
            let r = tv.unshare(&share.share_id).await;
            let gone = tv.share_exists(&share.share_id).await == Ok(false);
            report("remove invite", r.is_ok() && gone, format!("{r:?}"));
        }
    }
}
