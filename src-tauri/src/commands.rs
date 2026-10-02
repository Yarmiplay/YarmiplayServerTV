//! IPC commands for the control panel. The UI is only reachable through the
//! app's own webview; there is no HTTP control port.

use crate::config::Settings;
use crate::jellyfin::api::Library;
use crate::logs::{self, LogLine};
use crate::orchestrator::{App, Snapshot};
use std::sync::Arc;
use tauri::{AppHandle, State};
use tauri_plugin_opener::OpenerExt;

type AppState<'a> = State<'a, Arc<App>>;

#[tauri::command]
pub fn get_state(app: AppState<'_>) -> Snapshot {
    app.snapshot()
}

#[tauri::command]
pub async fn update_settings(app: AppState<'_>, settings: Settings) -> Result<Snapshot, String> {
    app.inner().update_settings(settings).await
}

#[tauri::command]
pub async fn set_duckdns_token(
    app: AppState<'_>,
    token: Option<String>,
) -> Result<Snapshot, String> {
    app.inner().set_duckdns_token(token).await
}

#[tauri::command]
pub fn renew_certificate(app: AppState<'_>) -> Result<(), String> {
    app.renew_certificate()
}

#[tauri::command]
pub fn jellyfin_retry(app: AppState<'_>) {
    app.jellyfin.retry();
}

#[tauri::command]
pub async fn jellyfin_setup(
    app: AppState<'_>,
    server_name: String,
    username: String,
    password: String,
) -> Result<Snapshot, String> {
    app.jellyfin_setup(server_name, username, password).await
}

#[tauri::command]
pub async fn jellyfin_login(
    app: AppState<'_>,
    username: String,
    password: String,
) -> Result<Snapshot, String> {
    app.jellyfin_login(username, password).await
}

#[tauri::command]
pub fn jellyfin_logout(app: AppState<'_>) -> Result<Snapshot, String> {
    app.jellyfin_logout()
}

#[tauri::command]
pub async fn jellyfin_libraries(app: AppState<'_>) -> Result<Vec<Library>, String> {
    app.jellyfin_libraries().await
}

#[tauri::command]
pub async fn jellyfin_add_library(
    app: AppState<'_>,
    name: String,
    collection_type: String,
    path: String,
) -> Result<(), String> {
    app.jellyfin_add_library(name, collection_type, path).await
}

#[tauri::command]
pub async fn jellyfin_remove_library(app: AppState<'_>, name: String) -> Result<(), String> {
    app.jellyfin_remove_library(name).await
}

#[tauri::command]
pub async fn jellyfin_add_path(
    app: AppState<'_>,
    library: String,
    path: String,
) -> Result<(), String> {
    app.jellyfin_add_path(library, path).await
}

#[tauri::command]
pub async fn jellyfin_remove_path(
    app: AppState<'_>,
    library: String,
    path: String,
) -> Result<(), String> {
    app.jellyfin_remove_path(library, path).await
}

#[tauri::command]
pub async fn jellyfin_rescan(app: AppState<'_>) -> Result<(), String> {
    app.jellyfin_rescan().await
}

#[tauri::command]
pub fn get_logs() -> Vec<LogLine> {
    logs::snapshot()
}

#[tauri::command]
pub fn clear_logs() {
    logs::clear();
}

#[tauri::command]
pub fn open_url(handle: AppHandle, url: String) -> Result<(), String> {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err("only web links can be opened".into());
    }
    handle
        .opener()
        .open_url(url, None::<&str>)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn open_folder(handle: AppHandle, app: AppState<'_>, which: String) -> Result<(), String> {
    let path = match which.as_str() {
        "data" => app.paths.data.clone(),
        "jellyfin-logs" => app.jellyfin.data_dirs().log,
        _ => return Err("unknown folder".into()),
    };
    std::fs::create_dir_all(&path).map_err(|e| e.to_string())?;
    handle
        .opener()
        .open_path(path.to_string_lossy(), None::<&str>)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn quit_app(handle: AppHandle) {
    crate::request_quit(&handle);
}
