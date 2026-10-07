//! YarmiplayServerTV: a tray app that hosts a Syncplay server and a Jellyfin
//! server, with optional UPnP forwarding and Let's Encrypt certificates for a
//! DuckDNS name.

pub mod autostart;
pub mod commands;
pub mod config;
pub mod jellyfin;
pub mod logs;
pub mod net;
pub mod orchestrator;
pub mod paths;
pub mod relay;
pub mod secrets;
pub mod syncplay;
pub mod tls;
pub mod tray;
pub mod updates;
pub mod web;

use orchestrator::App;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tauri::{Emitter, Manager, RunEvent, WindowEvent};

static QUITTING: AtomicBool = AtomicBool::new(false);
static SHUTDOWN_DONE: AtomicBool = AtomicBool::new(false);
static RESTART: AtomicBool = AtomicBool::new(false);

/// Like [`request_quit`], but starts the app again afterwards.
pub fn request_restart(app: &tauri::AppHandle) {
    RESTART.store(true, Ordering::SeqCst);
    request_quit(app);
}

/// Stop every service (and remove our UPnP mappings), then exit.
pub fn request_quit(app: &tauri::AppHandle) {
    if QUITTING.swap(true, Ordering::SeqCst) {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        if let Some(w) = app.get_webview_window("main") {
            let _ = w.hide();
        }
        if let Some(state) = app.try_state::<Arc<App>>() {
            state.inner().clone().shutdown().await;
        }
        SHUTDOWN_DONE.store(true, Ordering::SeqCst);
        if RESTART.load(Ordering::SeqCst) {
            app.request_restart();
        } else {
            app.exit(0);
        }
    });
}

pub fn run() {
    logs::init();
    net::install_crypto_provider();

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("yarmiplay")
        .build()
        .expect("tokio runtime");
    tauri::async_runtime::set(runtime.handle().clone());
    let _guard = runtime.enter();

    let minimized = autostart::launched_at_login();

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            tray::show_main(app)
        }))
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--minimized"]),
        ))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(move |app| {
            let handle = app.handle().clone();
            let wake = Arc::new(tokio::sync::Notify::new());
            let notify: tls::Notify = {
                let wake = wake.clone();
                Arc::new(move || wake.notify_one())
            };
            let state = App::new(paths::AppPaths::resolve(), notify);
            app.manage(state.clone());

            let log_handle = handle.clone();
            logs::set_sink(Box::new(move |line| {
                let _ = log_handle.emit("log", line);
                web::publish("log", line);
            }));

            tray::create(&handle)?;

            let status_state = state.clone();
            let status_handle = handle.clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    wake.notified().await;
                    tokio::time::sleep(Duration::from_millis(150)).await;
                    status_state.after_change().await;
                    let snap = status_state.snapshot();
                    let _ = status_handle.emit("status", &snap);
                    web::publish("status", &snap);
                    tray::update(&status_handle, &snap);
                }
            });

            state.start();
            web::apply(&handle, state.settings().browser.enabled);
            updates::start(handle.clone());
            if !minimized {
                tray::show_main(&handle);
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_state,
            commands::update_settings,
            commands::set_duckdns_token,
            commands::renew_certificate,
            commands::jellyfin_retry,
            commands::jellyfin_setup,
            commands::jellyfin_login,
            commands::jellyfin_logout,
            commands::jellyfin_libraries,
            commands::jellyfin_add_library,
            commands::jellyfin_remove_library,
            commands::jellyfin_add_path,
            commands::jellyfin_remove_path,
            commands::jellyfin_rescan,
            commands::clear_relay_cache,
            commands::syncplay_device_approve,
            commands::syncplay_device_deny,
            commands::syncplay_device_remove,
            commands::syncplay_device_rename,
            commands::get_logs,
            commands::clear_logs,
            commands::open_url,
            commands::open_folder,
            commands::pick_folder,
            commands::get_autostart,
            commands::set_autostart,
            commands::open_in_browser,
            commands::check_for_update,
            commands::install_update,
            commands::quit_app,
        ])
        .build(tauri::generate_context!())
        .expect("error while building YarmiplayServerTV");

    app.run(|app, event| {
        if let RunEvent::ExitRequested { api, code, .. } = event {
            // Closing the window only hides it; real exits clean up first.
            if code.is_none() || !SHUTDOWN_DONE.load(Ordering::SeqCst) {
                api.prevent_exit();
                if code.is_some() {
                    request_quit(app);
                }
            }
        }
    });
}
