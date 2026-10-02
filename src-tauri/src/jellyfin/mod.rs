//! Jellyfin: on-demand install, configuration and a supervised child process.

pub mod api;
pub mod installer;
pub mod netconfig;
pub mod process;

use crate::tls::Notify;
use installer::Progress;
use netconfig::NetworkSettings;
use parking_lot::{Mutex, RwLock};
use process::{DataDirs, JellyfinProcess};
use serde::Serialize;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;
use tracing::{info, warn};

#[derive(Debug, Clone, PartialEq)]
pub struct JellyfinDesired {
    pub http_port: u16,
    pub https_port: u16,
    /// PFX path and password when HTTPS is on.
    pub https: Option<(String, String)>,
}

#[derive(Debug, Clone, Serialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct JellyfinStatus {
    /// `off`, `downloading`, `installing`, `starting`, `running`, `stopping` or `error`.
    pub phase: &'static str,
    pub installed_version: Option<String>,
    pub pinned_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<ProgressStatus>,
    pub error: Option<String>,
    pub pid: Option<u32>,
    pub https: bool,
    pub wizard_completed: Option<bool>,
    pub server_name: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ProgressStatus {
    pub downloaded: u64,
    pub total: u64,
    pub stage: &'static str,
}

impl From<Progress> for ProgressStatus {
    fn from(p: Progress) -> Self {
        Self {
            downloaded: p.downloaded,
            total: p.total,
            stage: p.stage,
        }
    }
}

pub struct JellyfinManager {
    root: PathBuf,
    desired: watch::Sender<Option<JellyfinDesired>>,
    status: RwLock<JellyfinStatus>,
    phase_tx: watch::Sender<&'static str>,
    api: Mutex<Option<api::JellyfinApi>>,
    notify: Notify,
}

const STARTUP_TIMEOUT: Duration = Duration::from_secs(240);
const INSTALL_RETRY: Duration = Duration::from_secs(300);

impl JellyfinManager {
    pub fn new(root: PathBuf, notify: Notify) -> Arc<Self> {
        let (desired, rx) = watch::channel(None);
        let installed = installer::current(&root).map(|i| i.version);
        let me = Arc::new(Self {
            root,
            desired,
            status: RwLock::new(JellyfinStatus {
                phase: "off",
                installed_version: installed,
                pinned_version: installer::manifest().jellyfin,
                ..Default::default()
            }),
            phase_tx: watch::channel("off").0,
            api: Mutex::new(None),
            notify,
        });
        let runner = me.clone();
        tokio::spawn(async move { runner.run(rx).await });
        me
    }

    pub fn status(&self) -> JellyfinStatus {
        self.status.read().clone()
    }

    pub fn data_dirs(&self) -> DataDirs {
        DataDirs::under(&self.root)
    }

    pub fn pfx_path(&self) -> PathBuf {
        self.root.join("https.pfx")
    }

    /// Start, reconfigure (restart) or stop Jellyfin.
    pub fn apply(&self, desired: Option<JellyfinDesired>) {
        self.desired.send_if_modified(|cur| {
            if *cur == desired {
                false
            } else {
                *cur = desired;
                true
            }
        });
    }

    /// Retry after an install or startup error.
    pub fn retry(&self) {
        self.desired.send_modify(|_| {});
    }

    /// Admin API used for a graceful shutdown (Windows has no SIGTERM).
    pub fn set_api(&self, api: Option<api::JellyfinApi>) {
        *self.api.lock() = api;
    }

    pub fn is_running(&self) -> bool {
        self.status.read().phase == "running"
    }

    pub async fn refresh_info(&self, port: u16) {
        let api = api::JellyfinApi::new(port, "", None);
        if let Ok(info) = api.public_info().await {
            self.set(|s| {
                s.wizard_completed = Some(info.startup_wizard_completed);
                s.server_name = Some(info.server_name);
            });
        }
    }

    /// Stop Jellyfin and wait (bounded) until it is down.
    pub async fn shutdown(&self) {
        self.apply(None);
        let mut rx = self.phase_tx.subscribe();
        let _ = tokio::time::timeout(
            Duration::from_secs(20),
            rx.wait_for(|p| *p == "off" || *p == "error"),
        )
        .await;
    }

    fn set(&self, f: impl FnOnce(&mut JellyfinStatus)) {
        let changed = {
            let mut s = self.status.write();
            let before = s.clone();
            f(&mut s);
            self.phase_tx.send_replace(s.phase);
            *s != before
        };
        if changed {
            (self.notify)();
        }
    }

    fn fail(&self, error: String) {
        warn!(error = %error, "Jellyfin");
        self.set(|s| {
            s.phase = "error";
            s.error = Some(error);
            s.progress = None;
            s.pid = None;
        });
    }

    async fn stop_process(&self, proc: JellyfinProcess) {
        self.set(|s| s.phase = "stopping");
        let api = self.api.lock().clone();
        let mut grace = if cfg!(windows) {
            Duration::ZERO
        } else {
            Duration::from_secs(15)
        };
        if let Some(api) = api {
            if api.shutdown().await.is_ok() {
                grace = Duration::from_secs(15);
            }
        }
        proc.stop(grace).await;
    }

    async fn run(self: Arc<Self>, mut rx: watch::Receiver<Option<JellyfinDesired>>) {
        let mut failures: u32 = 0;
        loop {
            let desired = rx.borrow_and_update().clone();
            let Some(d) = desired else {
                self.set(|s| {
                    s.phase = "off";
                    s.progress = None;
                    s.pid = None;
                    s.error = None;
                });
                if rx.changed().await.is_err() {
                    return;
                }
                continue;
            };

            let installed = match installer::current(&self.root) {
                Some(i) => i,
                None => {
                    self.set(|s| {
                        s.phase = "downloading";
                        s.error = None;
                    });
                    let me = self.clone();
                    let progress: installer::ProgressFn = Arc::new(move |p: Progress| {
                        me.set(|s| {
                            s.progress = Some(p.into());
                            if p.stage == "extract" {
                                s.phase = "installing";
                            }
                        })
                    });
                    let root = self.root.clone();
                    let fut = installer::ensure(&root, progress);
                    tokio::pin!(fut);
                    let result = loop {
                        tokio::select! {
                            r = &mut fut => break Some(r),
                            c = rx.changed() => {
                                if c.is_err() { return; }
                                if rx.borrow().is_none() { break None; }
                            }
                        }
                    };
                    match result {
                        None => continue,
                        Some(Ok(i)) => {
                            self.set(|s| {
                                s.installed_version = Some(i.version.clone());
                                s.progress = None;
                            });
                            i
                        }
                        Some(Err(e)) => {
                            self.fail(format!("Jellyfin download failed: {e}"));
                            tokio::select! {
                                c = rx.changed() => if c.is_err() { return },
                                _ = tokio::time::sleep(INSTALL_RETRY) => {}
                            }
                            continue;
                        }
                    }
                }
            };

            let dirs = DataDirs::under(&self.root);
            if let Err(e) = dirs.ensure() {
                self.fail(format!("Jellyfin folders: {e}"));
                if rx.changed().await.is_err() {
                    return;
                }
                continue;
            }
            let net = NetworkSettings {
                http_port: d.http_port,
                https_port: d.https_port,
                https: d.https.clone(),
            };
            if let Err(e) = netconfig::write_network_xml(&dirs.config, &net) {
                self.fail(format!("Jellyfin network settings: {e}"));
                if rx.changed().await.is_err() {
                    return;
                }
                continue;
            }

            self.set(|s| {
                s.phase = "starting";
                s.error = None;
                s.https = d.https.is_some();
            });
            let mut proc = match JellyfinProcess::spawn(&installed, &dirs) {
                Ok(p) => p,
                Err(e) => {
                    self.fail(e);
                    if rx.changed().await.is_err() {
                        return;
                    }
                    continue;
                }
            };
            self.set(|s| s.pid = proc.pid());

            let ready = tokio::select! {
                r = process::wait_ready(d.http_port, &mut proc, STARTUP_TIMEOUT) => Some(r),
                alive = wait_for_change(&mut rx, &d) => { if !alive { proc.stop(Duration::ZERO).await; return; } None }
            };
            match ready {
                None => {
                    self.stop_process(proc).await;
                    continue;
                }
                Some(Err(e)) => {
                    proc.stop(Duration::ZERO).await;
                    failures += 1;
                    if !self.backoff(&mut rx, &d, failures, e).await {
                        return;
                    }
                    continue;
                }
                Some(Ok(())) => {
                    info!(
                        port = d.http_port,
                        https = d.https.is_some(),
                        "Jellyfin is ready"
                    );
                    self.set(|s| s.phase = "running");
                    self.refresh_info(d.http_port).await;
                }
            }

            let started = tokio::time::Instant::now();
            tokio::select! {
                status = proc.wait() => {
                    if started.elapsed() > Duration::from_secs(300) {
                        failures = 0;
                    }
                    failures += 1;
                    if !self.backoff(&mut rx, &d, failures, format!("Jellyfin stopped unexpectedly ({status})")).await {
                        return;
                    }
                }
                alive = wait_for_change(&mut rx, &d) => {
                    self.stop_process(proc).await;
                    if !alive { return; }
                }
            }
        }
    }

    /// Show the error and wait before restarting. Returns false when the
    /// manager is being dropped.
    async fn backoff(
        &self,
        rx: &mut watch::Receiver<Option<JellyfinDesired>>,
        d: &JellyfinDesired,
        failures: u32,
        error: String,
    ) -> bool {
        let delay = Duration::from_secs((1u64 << failures.min(6)).min(60));
        self.fail(format!("{error}; retrying in {}s", delay.as_secs()));
        tokio::select! {
            _ = tokio::time::sleep(delay) => true,
            alive = wait_for_change(rx, d) => alive,
        }
    }
}

/// Resolves when the desired config differs from `current` (true) or the
/// sender is gone (false).
async fn wait_for_change(
    rx: &mut watch::Receiver<Option<JellyfinDesired>>,
    current: &JellyfinDesired,
) -> bool {
    loop {
        if rx.changed().await.is_err() {
            return false;
        }
        if rx.borrow().as_ref() != Some(current) {
            return true;
        }
    }
}
