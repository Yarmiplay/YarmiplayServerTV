//! System tray icon and menu.

use crate::orchestrator::{App, Snapshot};
use std::sync::Arc;
use tauri::menu::{CheckMenuItem, IsMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, Wry};
use tauri_plugin_clipboard_manager::ClipboardExt as _;
use tauri_plugin_opener::OpenerExt as _;
use tracing::warn;

pub struct TrayItems {
    status: MenuItem<Wry>,
    open_browser: MenuItem<Wry>,
    syncplay: CheckMenuItem<Wry>,
    jellyfin: CheckMenuItem<Wry>,
    copy_syncplay: MenuItem<Wry>,
    open_jellyfin: MenuItem<Wry>,
    autostart: CheckMenuItem<Wry>,
    update: MenuItem<Wry>,
}

pub fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}

pub fn create(app: &AppHandle) -> tauri::Result<()> {
    let status = MenuItem::with_id(app, "status", "Starting…", false, None::<&str>)?;
    let open = MenuItem::with_id(app, "open", "Open Control Panel", true, None::<&str>)?;
    let browser_on = app.state::<Arc<App>>().settings().browser.enabled;
    let open_browser = MenuItem::with_id(
        app,
        "open-browser",
        "Open in Browser",
        browser_on,
        None::<&str>,
    )?;
    let syncplay = CheckMenuItem::with_id(
        app,
        "syncplay",
        "Syncplay server",
        true,
        false,
        None::<&str>,
    )?;
    let jellyfin = CheckMenuItem::with_id(
        app,
        "jellyfin",
        "Jellyfin server",
        true,
        false,
        None::<&str>,
    )?;
    let copy_syncplay = MenuItem::with_id(
        app,
        "copy-syncplay",
        "Copy Syncplay address",
        false,
        None::<&str>,
    )?;
    let open_jellyfin =
        MenuItem::with_id(app, "open-jellyfin", "Open Jellyfin", false, None::<&str>)?;
    let autostart = CheckMenuItem::with_id(
        app,
        "autostart",
        "Start with system",
        true,
        false,
        None::<&str>,
    )?;
    let update = MenuItem::with_id(
        app,
        "update",
        "Check for updates",
        crate::updates::supported(),
        None::<&str>,
    )?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let sep = || PredefinedMenuItem::separator(app);
    let (sep1, sep2, sep3, sep4) = (sep()?, sep()?, sep()?, sep()?);
    let mut items: Vec<&dyn IsMenuItem<Wry>> = vec![
        &status,
        &sep1,
        &open,
        &open_browser,
        &sep2,
        &syncplay,
        &jellyfin,
        &sep3,
        &copy_syncplay,
        &open_jellyfin,
        &sep4,
        &autostart,
    ];
    // The Store updates its copy.
    if crate::paths::package_family().is_none() {
        items.push(&update);
    }
    items.push(&quit);
    let menu = Menu::with_items(app, &items)?;

    let icon = tauri::image::Image::from_bytes(include_bytes!("../icons/tray.png"))?;
    TrayIconBuilder::with_id("main")
        .icon(icon)
        .tooltip("YarmiplayServerTV")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| on_menu(app, event.id().as_ref()))
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main(tray.app_handle());
            }
        })
        .build(app)?;

    app.manage(TrayItems {
        status,
        open_browser,
        syncplay,
        jellyfin,
        copy_syncplay,
        open_jellyfin,
        autostart,
        update,
    });
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || set_autostart_checked(&app));
    Ok(())
}

/// Ticks "Start with system" to match the system; blocks briefly.
pub fn set_autostart_checked(app: &AppHandle) {
    let on = crate::autostart::is_enabled(app);
    if let Some(items) = app.try_state::<TrayItems>() {
        let _ = items.autostart.set_checked(on);
    }
}

fn on_menu(app: &AppHandle, id: &str) {
    let state = app.state::<Arc<App>>().inner().clone();
    match id {
        "open" => show_main(app),
        "open-browser" => {
            if let Err(e) = crate::web::open_in_browser(app) {
                warn!(error = %e, "could not open the control panel in a browser");
            }
        }
        "syncplay" | "jellyfin" => {
            let which = id.to_string();
            tauri::async_runtime::spawn(async move {
                let mut s = state.settings();
                if which == "syncplay" {
                    s.syncplay.enabled = !s.syncplay.enabled;
                } else {
                    s.jellyfin.enabled = !s.jellyfin.enabled;
                }
                if let Err(e) = state.update_settings(s).await {
                    warn!(error = %e, "tray toggle failed");
                }
            });
        }
        "copy-syncplay" => {
            let snap = state.snapshot();
            if let Some(addr) = snap
                .addresses
                .syncplay_public
                .or(snap.addresses.syncplay_lan)
            {
                let _ = app.clipboard().write_text(addr);
            }
        }
        "open-jellyfin" => {
            if let Some(url) = state.snapshot().addresses.jellyfin_local {
                let _ = app.opener().open_url(url, None::<&str>);
            }
        }
        "autostart" => {
            let app = app.clone();
            tauri::async_runtime::spawn_blocking(move || {
                let on = crate::autostart::is_enabled(&app);
                if let Err(e) = crate::autostart::set(&app, !on) {
                    warn!(error = %e, "could not change start-with-system");
                }
                set_autostart_checked(&app);
            });
        }
        "update" => {
            let app = app.clone();
            let ready = state.snapshot().update.phase == "ready";
            tauri::async_runtime::spawn(async move {
                let result = if ready {
                    crate::updates::install(&app).await
                } else {
                    crate::updates::check(&app).await
                };
                if let Err(e) = result {
                    warn!(error = %e, "update from the tray failed");
                }
            });
        }
        "quit" => crate::request_quit(app),
        _ => {}
    }
}

fn status_line(snap: &Snapshot) -> String {
    let syncplay = if snap.syncplay.running {
        match snap.syncplay.users {
            0 => "Syncplay on".to_string(),
            1 => "Syncplay: 1 user".to_string(),
            n => format!("Syncplay: {n} users"),
        }
    } else if snap.syncplay.error.is_some() {
        "Syncplay error".to_string()
    } else {
        "Syncplay off".to_string()
    };
    let jellyfin = match snap.jellyfin.phase {
        "running" => "Jellyfin on",
        "off" => "Jellyfin off",
        "downloading" | "installing" => "Jellyfin installing",
        "starting" => "Jellyfin starting",
        "stopping" => "Jellyfin stopping",
        _ => "Jellyfin error",
    };
    format!("{syncplay} · {jellyfin}")
}

pub fn update(app: &AppHandle, snap: &Snapshot) {
    let Some(items) = app.try_state::<TrayItems>() else {
        return;
    };
    let line = status_line(snap);
    let _ = items.status.set_text(&line);
    let _ = items
        .open_browser
        .set_enabled(snap.settings.browser.enabled);
    let _ = items.syncplay.set_checked(snap.settings.syncplay.enabled);
    let _ = items.jellyfin.set_checked(snap.settings.jellyfin.enabled);
    let _ = items.copy_syncplay.set_enabled(snap.syncplay.running);
    let _ = items
        .open_jellyfin
        .set_enabled(snap.jellyfin.phase == "running");
    let (text, enabled) = match (snap.update.phase, &snap.update.version) {
        ("ready", Some(v)) => (format!("Install update {v}"), true),
        ("downloading", _) => ("Downloading update…".to_string(), false),
        ("checking", _) => ("Checking for updates…".to_string(), false),
        ("installing", _) => ("Installing update…".to_string(), false),
        _ => ("Check for updates".to_string(), snap.update.supported),
    };
    let _ = items.update.set_text(text);
    let _ = items.update.set_enabled(enabled);
    if let Some(tray) = app.tray_by_id("main") {
        let _ = tray.set_tooltip(Some(format!("YarmiplayServerTV — {line}")));
    }
}
