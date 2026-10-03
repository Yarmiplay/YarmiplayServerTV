//! Updates from the latest GitHub release. The release workflow publishes
//! `latest.json` with installers signed by the updater key; the public half is
//! in tauri.conf.json, and the plugin refuses anything it didn't sign.
//!
//! Nothing contacts GitHub unless the user clicks "Check for updates" or
//! turns on automatic updates (`updates.auto`). Then checks run at startup
//! and every few hours, a found update is downloaded right away, and it
//! installs when nobody is using Syncplay and the install needs no
//! administrator prompt; .msi and .deb installs wait for a click instead,
//! since an unanswered prompt would leave the servers down.

use crate::orchestrator::App;
use crate::tls::Notify;
use parking_lot::{Mutex, RwLock};
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::utils::config::BundleType;
use tauri::{AppHandle, Manager};
use tauri_plugin_updater::{Update, UpdaterExt};
use tracing::{info, warn};

const FIRST_CHECK: Duration = Duration::from_secs(60);
const CHECK_EVERY: Duration = Duration::from_secs(6 * 60 * 60);
/// How often a downloaded update retries installing while Syncplay is busy.
const INSTALL_RETRY: Duration = Duration::from_secs(10 * 60);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStatus {
    /// "idle", "checking", "upToDate", "downloading", "ready", "installing" or "error".
    pub phase: &'static str,
    /// The newer version, once one is found.
    pub version: Option<String>,
    pub notes: Option<String>,
    pub downloaded: u64,
    pub total: Option<u64>,
    pub error: Option<String>,
    /// Unix seconds.
    pub last_check: Option<i64>,
    /// Whether this kind of install can update without an administrator prompt.
    pub unattended: bool,
    /// False in development builds.
    pub supported: bool,
}

pub struct Updates {
    status: RwLock<UpdateStatus>,
    ready: Mutex<Option<(Update, Arc<Vec<u8>>)>>,
    busy: tokio::sync::Mutex<()>,
    /// Set by the Windows exit hook once it has stopped the services.
    services_stopped: AtomicBool,
    notify: Notify,
}

impl Updates {
    pub fn new(notify: Notify) -> Arc<Self> {
        Arc::new(Self {
            status: RwLock::new(UpdateStatus {
                phase: "idle",
                version: None,
                notes: None,
                downloaded: 0,
                total: None,
                error: None,
                last_check: None,
                unattended: unattended(),
                supported: !tauri::is_dev(),
            }),
            ready: Mutex::new(None),
            busy: tokio::sync::Mutex::new(()),
            services_stopped: AtomicBool::new(false),
            notify,
        })
    }

    pub fn status(&self) -> UpdateStatus {
        self.status.read().clone()
    }

    fn set(&self, f: impl FnOnce(&mut UpdateStatus)) {
        f(&mut self.status.write());
        (self.notify)();
    }
}

fn unattended() -> bool {
    !matches!(
        tauri::utils::platform::bundle_type(),
        Some(BundleType::Msi | BundleType::Deb | BundleType::Rpm)
    )
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Scheduled checks while automatic updates are on; nothing runs in
/// development builds.
pub fn start(handle: AppHandle) {
    if tauri::is_dev() {
        return;
    }
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(FIRST_CHECK).await;
        let mut last_check: Option<Instant> = None;
        loop {
            let auto = handle.state::<Arc<App>>().settings().updates.auto;
            if auto && last_check.map_or(true, |t| t.elapsed() >= CHECK_EVERY) {
                last_check = Some(Instant::now());
                let _ = check(&handle).await;
            }
            if auto {
                auto_install(&handle).await;
            }
            tokio::time::sleep(INSTALL_RETRY).await;
        }
    });
}

/// Called when the user turns automatic updates on, so they needn't wait
/// for the next scheduled round.
pub fn turned_on(handle: &AppHandle) {
    if tauri::is_dev() {
        return;
    }
    let handle = handle.clone();
    tauri::async_runtime::spawn(async move {
        if check(&handle).await.is_ok() {
            auto_install(&handle).await;
        }
    });
}

async fn auto_install(handle: &AppHandle) {
    let app = handle.state::<Arc<App>>().inner().clone();
    let snap = app.snapshot();
    let ready = app.updates.ready.lock().is_some();
    if ready && snap.update.unattended {
        if snap.syncplay.users > 0 {
            info!("update ready; waiting until nobody is using Syncplay");
            return;
        }
        let _ = install(handle).await;
    }
}

/// Look for a newer release and download it. Errors also land in the status.
pub async fn check(handle: &AppHandle) -> Result<(), String> {
    let app = handle.state::<Arc<App>>().inner().clone();
    let updates = app.updates.clone();
    let Ok(_busy) = updates.busy.try_lock() else {
        return Err("An update is already being checked or installed".into());
    };
    if updates.ready.lock().is_some() {
        return Ok(());
    }
    updates.set(|s| {
        s.phase = "checking";
        s.error = None;
    });

    let shutdown_app = app.clone();
    let found = async {
        handle
            .updater_builder()
            // Windows: the plugin starts the installer, then exits the process.
            .on_before_exit(move || {
                tauri::async_runtime::block_on(shutdown_app.shutdown());
                shutdown_app
                    .updates
                    .services_stopped
                    .store(true, Ordering::SeqCst);
            })
            .build()
            .map_err(|e| e.to_string())?
            .check()
            .await
            .map_err(|e| e.to_string())
    }
    .await;
    let now = unix_now();
    let update = match found {
        Ok(Some(update)) => update,
        Ok(None) => {
            updates.set(|s| {
                s.phase = "upToDate";
                s.version = None;
                s.notes = None;
                s.last_check = Some(now);
            });
            return Ok(());
        }
        Err(e) => {
            warn!(error = %e, "update check failed");
            updates.set(|s| {
                s.phase = "error";
                s.error = Some(format!("Could not check for updates: {e}"));
                s.last_check = Some(now);
            });
            return Err(e);
        }
    };

    info!(version = %update.version, "update available, downloading");
    updates.set(|s| {
        s.phase = "downloading";
        s.version = Some(update.version.clone());
        s.notes = update.body.clone();
        s.downloaded = 0;
        s.total = None;
        s.last_check = Some(now);
    });
    let progress = updates.clone();
    let bytes = update
        .download(
            move |chunk, total| {
                progress.set(|s| {
                    s.downloaded += chunk as u64;
                    s.total = total;
                })
            },
            || {},
        )
        .await;
    match bytes {
        Ok(bytes) => {
            info!(version = %update.version, "update downloaded and verified");
            *updates.ready.lock() = Some((update, Arc::new(bytes)));
            updates.set(|s| s.phase = "ready");
            Ok(())
        }
        Err(e) => {
            warn!(error = %e, "update download failed");
            let e = e.to_string();
            updates.set(|s| {
                s.phase = "error";
                s.error = Some(format!("Could not download the update: {e}"));
            });
            Err(e)
        }
    }
}

/// Install the downloaded update and restart into it.
pub async fn install(handle: &AppHandle) -> Result<(), String> {
    if tauri::is_dev() {
        return Err("Updates can't be installed over a development build".into());
    }
    let app = handle.state::<Arc<App>>().inner().clone();
    let updates = app.updates.clone();
    let Ok(_busy) = updates.busy.try_lock() else {
        return Err("An update is already being checked or installed".into());
    };
    let Some((update, bytes)) = updates.ready.lock().clone() else {
        return Err("No update is ready to install".into());
    };
    info!(version = %update.version, "installing update");
    updates.set(|s| {
        s.phase = "installing";
        s.error = None;
    });

    let result = tokio::task::spawn_blocking(move || update.install(&*bytes))
        .await
        .map_err(|e| e.to_string())
        .and_then(|r| r.map_err(|e| e.to_string()));

    match result {
        Ok(()) => {
            crate::request_restart(handle);
            Ok(())
        }
        Err(e) => {
            warn!(error = %e, "update install failed");
            if updates.services_stopped.load(Ordering::SeqCst) {
                // The services are already down; start over cleanly.
                crate::request_restart(handle);
            }
            updates.set(|s| {
                s.phase = "ready";
                s.error = Some(format!("Could not install the update: {e}"));
            });
            Err(e)
        }
    }
}
