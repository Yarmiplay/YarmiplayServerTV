//! Browser access to the control panel, for this PC only. Serves the UI and
//! bridges `/api` to the same commands the webview calls. It listens on
//! 127.0.0.1 while the `browser.enabled` setting is on; a session starts from
//! a one-time link (tray menu or the in-app button) and every API call needs
//! that session's cookie.

use crate::commands as cmd;
use crate::orchestrator::App;
use axum::body::Bytes;
use axum::extract::{Path, Query, Request, State};
use axum::http::{header, HeaderMap, StatusCode, Uri};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use parking_lot::Mutex;
use rand::RngCore;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;
use std::convert::Infallible;
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};
use tauri_plugin_opener::OpenerExt;
use tokio::sync::{broadcast, watch};
use tracing::{info, warn};

/// Fixed so the dev server's `/api` proxy in vite.config.ts can reach it.
pub const PORT: u16 = 8097;
const COOKIE: &str = "ystv_session";
/// Browsers only send a custom header cross-origin after a CORS preflight,
/// which this server never approves, so requiring it blocks forged requests.
const CSRF_HEADER: &str = "x-ystv";
const LINK_TTL: Duration = Duration::from_secs(120);
const CSP: &str = "default-src 'self'; img-src 'self' data:; style-src 'self' 'unsafe-inline'; \
                   script-src 'self'; connect-src 'self'; frame-ancestors 'none'";
const EXPIRED_PAGE: &str = "<!doctype html><meta charset=utf-8><title>YarmiplayServerTV</title>\
    <body style=\"font-family:system-ui;background:#0E1116;color:#E6E9EF;padding:3em\">\
    <h2>This link has expired</h2><p>Open the control panel in your browser again from the \
    YarmiplayServerTV tray icon (Open in Browser).</p>";

#[derive(Clone)]
struct Push {
    name: &'static str,
    data: String,
}

struct Web {
    /// 0 while the server is off.
    port: AtomicU16,
    /// Present while the server runs; sending `true` shuts it down.
    stop: Mutex<Option<watch::Sender<bool>>>,
    links: Mutex<Vec<(String, Instant)>>,
    sessions: Mutex<HashSet<String>>,
    events: broadcast::Sender<Push>,
}

static WEB: LazyLock<Web> = LazyLock::new(|| Web {
    port: AtomicU16::new(0),
    stop: Mutex::new(None),
    links: Mutex::new(Vec::new()),
    sessions: Mutex::new(HashSet::new()),
    events: broadcast::channel(512).0,
});

/// Send an event to connected browsers; the webview gets it through `emit`.
pub fn publish<T: Serialize>(name: &'static str, payload: &T) {
    if WEB.events.receiver_count() == 0 {
        return;
    }
    if let Ok(data) = serde_json::to_string(payload) {
        let _ = WEB.events.send(Push { name, data });
    }
}

/// Start or stop the server to match the `browser.enabled` setting. Stopping
/// also signs out every browser.
pub fn apply(handle: &AppHandle, enabled: bool) {
    let mut stop = WEB.stop.lock();
    if enabled && stop.is_none() {
        let (tx, rx) = watch::channel(false);
        *stop = Some(tx);
        tauri::async_runtime::spawn(serve(handle.clone(), rx));
    } else if !enabled {
        if let Some(tx) = stop.take() {
            let _ = tx.send(true);
            WEB.port.store(0, Ordering::SeqCst);
            WEB.sessions.lock().clear();
            WEB.links.lock().clear();
            info!("browser control panel turned off");
        }
    }
}

async fn stopped(mut stop: watch::Receiver<bool>) {
    let _ = stop.wait_for(|s| *s).await;
}

async fn serve(handle: AppHandle, stop: watch::Receiver<bool>) {
    let listener = match tokio::net::TcpListener::bind(("127.0.0.1", PORT)).await {
        Ok(l) => l,
        Err(e) => {
            warn!(error = %e, port = PORT, "browser control panel port is busy; using a random port");
            match tokio::net::TcpListener::bind(("127.0.0.1", 0)).await {
                Ok(l) => l,
                Err(e) => {
                    warn!(error = %e, "browser control panel unavailable");
                    return;
                }
            }
        }
    };
    let port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
    {
        // Under the lock, so a concurrent `apply(false)` can't be overwritten.
        let _guard = WEB.stop.lock();
        if *stop.borrow() {
            return;
        }
        WEB.port.store(port, Ordering::SeqCst);
    }
    info!(port, "browser control panel on 127.0.0.1");

    let router = Router::new()
        .route("/api/auth", get(auth))
        .route("/api/events", get(events))
        .route("/api/invoke/{command}", post(invoke))
        .fallback(get(asset))
        .layer(middleware::from_fn(check_host))
        .with_state(handle);
    if let Err(e) = axum::serve(listener, router)
        .with_graceful_shutdown(stopped(stop))
        .await
    {
        warn!(error = %e, "browser control panel stopped");
    }
}

/// Open the control panel in the default browser with a fresh one-time link.
pub fn open_in_browser(handle: &AppHandle) -> Result<(), String> {
    let port = WEB.port.load(Ordering::SeqCst);
    if port == 0 {
        return Err("Browser access is turned off. Turn it on in the Dashboard.".into());
    }
    let token = random_token();
    {
        let mut links = WEB.links.lock();
        links.retain(|(_, made)| made.elapsed() < LINK_TTL);
        links.push((token.clone(), Instant::now()));
    }
    let base = match &handle.config().build.dev_url {
        Some(url) if tauri::is_dev() => url.as_str().trim_end_matches('/').to_string(),
        _ => format!("http://127.0.0.1:{port}"),
    };
    handle
        .opener()
        .open_url(format!("{base}/api/auth?token={token}"), None::<&str>)
        .map_err(|e| e.to_string())
}

fn random_token() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

fn has_session(headers: &HeaderMap) -> bool {
    let sessions = WEB.sessions.lock();
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|c| c.trim().strip_prefix(COOKIE)?.strip_prefix('='))
        .any(|s| sessions.contains(s))
}

/// Rejects requests addressed to any other host name, so a web page that
/// rebinds its own domain to 127.0.0.1 can't talk to this server.
async fn check_host(req: Request, next: Next) -> Response {
    let port = WEB.port.load(Ordering::SeqCst);
    let ok = req
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .is_some_and(|h| h == format!("127.0.0.1:{port}") || h == format!("localhost:{port}"));
    if !ok {
        return StatusCode::FORBIDDEN.into_response();
    }
    next.run(req).await
}

#[derive(Deserialize)]
struct AuthQuery {
    token: String,
}

async fn auth(headers: HeaderMap, Query(q): Query<AuthQuery>) -> Response {
    let redirect_home = |cookie: Option<String>| {
        let mut res = (StatusCode::SEE_OTHER, [(header::LOCATION, "/")]).into_response();
        if let Some(c) = cookie.and_then(|c| c.parse().ok()) {
            res.headers_mut().insert(header::SET_COOKIE, c);
        }
        res
    };
    let valid = {
        let mut links = WEB.links.lock();
        links.retain(|(_, made)| made.elapsed() < LINK_TTL);
        let before = links.len();
        links.retain(|(t, _)| *t != q.token);
        links.len() != before
    };
    if !valid {
        if has_session(&headers) {
            return redirect_home(None);
        }
        return (StatusCode::FORBIDDEN, Html(EXPIRED_PAGE)).into_response();
    }
    let session = random_token();
    WEB.sessions.lock().insert(session.clone());
    redirect_home(Some(format!(
        "{COOKIE}={session}; Path=/; HttpOnly; SameSite=Strict"
    )))
}

async fn events(headers: HeaderMap) -> Response {
    if !has_session(&headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let Some(stop) = WEB.stop.lock().as_ref().map(|s| s.subscribe()) else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let rx = WEB.events.subscribe();
    let stream = futures_util::stream::unfold((rx, stop), |(mut rx, stop)| async move {
        // Ends on shutdown (open streams would otherwise hold it up forever),
        // but only after events already queued, such as the final status.
        let event = tokio::select! {
            biased;
            msg = rx.recv() => match msg {
                Ok(p) => Event::default().event(p.name).data(p.data),
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    Event::default().event("resync").data("")
                }
                Err(broadcast::error::RecvError::Closed) => return None,
            },
            _ = stopped(stop.clone()) => return None,
        };
        Some((Ok::<_, Infallible>(event), (rx, stop)))
    });
    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

async fn invoke(
    State(handle): State<AppHandle>,
    Path(command): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !has_session(&headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if !headers.contains_key(CSRF_HEADER) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let args: Value = if body.is_empty() {
        Value::Null
    } else {
        match serde_json::from_slice(&body) {
            Ok(v) => v,
            Err(e) => return (StatusCode::BAD_REQUEST, Json(e.to_string())).into_response(),
        }
    };
    match dispatch(&handle, &command, &args).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => (StatusCode::UNPROCESSABLE_ENTITY, Json(e)).into_response(),
    }
}

fn arg<T: DeserializeOwned>(args: &Value, key: &str) -> Result<T, String> {
    serde_json::from_value(args.get(key).cloned().unwrap_or(Value::Null))
        .map_err(|e| format!("invalid `{key}`: {e}"))
}

fn ok<T: Serialize>(v: T) -> Result<Value, String> {
    serde_json::to_value(v).map_err(|e| e.to_string())
}

/// Argument names match what the UI passes to `invoke` (camelCase).
async fn dispatch(h: &AppHandle, command: &str, a: &Value) -> Result<Value, String> {
    let app = || h.state::<Arc<App>>();
    match command {
        "get_state" => ok(cmd::get_state(app())),
        "update_settings" => ok(cmd::update_settings(h.clone(), app(), arg(a, "settings")?).await?),
        "set_duckdns_token" => ok(cmd::set_duckdns_token(app(), arg(a, "token")?).await?),
        "renew_certificate" => ok(cmd::renew_certificate(app())?),
        "jellyfin_retry" => {
            cmd::jellyfin_retry(app());
            Ok(Value::Null)
        }
        "jellyfin_setup" => ok(cmd::jellyfin_setup(
            app(),
            arg(a, "serverName")?,
            arg(a, "username")?,
            arg(a, "password")?,
        )
        .await?),
        "jellyfin_login" => {
            ok(cmd::jellyfin_login(app(), arg(a, "username")?, arg(a, "password")?).await?)
        }
        "jellyfin_logout" => ok(cmd::jellyfin_logout(app())?),
        "jellyfin_libraries" => ok(cmd::jellyfin_libraries(app()).await?),
        "jellyfin_add_library" => ok(cmd::jellyfin_add_library(
            app(),
            arg(a, "name")?,
            arg(a, "collectionType")?,
            arg(a, "path")?,
        )
        .await?),
        "jellyfin_remove_library" => {
            ok(cmd::jellyfin_remove_library(app(), arg(a, "name")?).await?)
        }
        "jellyfin_add_path" => {
            ok(cmd::jellyfin_add_path(app(), arg(a, "library")?, arg(a, "path")?).await?)
        }
        "jellyfin_remove_path" => {
            ok(cmd::jellyfin_remove_path(app(), arg(a, "library")?, arg(a, "path")?).await?)
        }
        "jellyfin_rescan" => ok(cmd::jellyfin_rescan(app()).await?),
        "clear_relay_cache" => ok(cmd::clear_relay_cache(app())),
        "syncplay_device_approve" => {
            ok(cmd::syncplay_device_approve(app(), arg(a, "fingerprint")?)?)
        }
        "syncplay_device_deny" => ok(cmd::syncplay_device_deny(app(), arg(a, "fingerprint")?)?),
        "syncplay_device_remove" => ok(cmd::syncplay_device_remove(app(), arg(a, "fingerprint")?)?),
        "syncplay_device_rename" => ok(cmd::syncplay_device_rename(
            app(),
            arg(a, "fingerprint")?,
            arg(a, "name")?,
        )?),
        "get_logs" => ok(cmd::get_logs()),
        "clear_logs" => {
            cmd::clear_logs();
            Ok(Value::Null)
        }
        "open_folder" => ok(cmd::open_folder(h.clone(), app(), arg(a, "which")?)?),
        "pick_folder" => ok(cmd::pick_folder(h.clone()).await),
        "get_autostart" => ok(cmd::get_autostart(h.clone()).await),
        "set_autostart" => ok(cmd::set_autostart(h.clone(), arg(a, "enabled")?).await?),
        "check_for_update" => ok(cmd::check_for_update(h.clone()).await?),
        "install_update" => ok(cmd::install_update(h.clone()).await?),
        "quit_app" => {
            cmd::quit_app(h.clone());
            Ok(Value::Null)
        }
        _ => Err(format!("unknown command `{command}`")),
    }
}

async fn asset(State(handle): State<AppHandle>, uri: Uri) -> Response {
    let path = match uri.path() {
        "/" => "/index.html",
        p => p,
    };
    let Some(asset) = handle.asset_resolver().get(path.to_string()) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    (
        [
            (header::CONTENT_TYPE, asset.mime_type().to_string()),
            (header::CONTENT_SECURITY_POLICY, CSP.to_string()),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_string()),
            (header::CACHE_CONTROL, "no-cache".to_string()),
        ],
        asset.bytes().to_vec(),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn event_stream_delivers_queued_events_then_closes_on_stop() {
        let (tx, _rx) = watch::channel(false);
        *WEB.stop.lock() = Some(tx);
        WEB.sessions.lock().insert("test-session".into());
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            "a=b; ystv_session=test-session".parse().unwrap(),
        );

        let res = events(headers).await;
        assert_eq!(res.status(), StatusCode::OK);
        publish(
            "status",
            &serde_json::json!({ "browser": { "enabled": false } }),
        );
        WEB.stop.lock().take().unwrap().send(true).unwrap();

        let body = axum::body::to_bytes(res.into_body(), 1 << 20)
            .await
            .unwrap();
        let text = String::from_utf8(body.to_vec()).unwrap();
        assert!(text.contains("event: status"), "{text}");
        assert!(text.contains(r#""enabled":false"#), "{text}");

        assert_eq!(
            events(HeaderMap::new()).await.status(),
            StatusCode::UNAUTHORIZED
        );
    }
}
