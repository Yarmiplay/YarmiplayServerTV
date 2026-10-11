//! A Plex Media Server the user installed themselves from plex.tv. Plex's
//! terms don't allow bundling it and the app stores don't want apps that
//! install other software, so this app never installs, starts or stops Plex.
//! It watches Plex over HTTP, links the host's plex.tv account, manages
//! libraries and shares Plex with the people on the Syncplay server.

pub mod api;
pub mod share;
#[cfg(test)]
mod spike;
pub mod tv;

use crate::tls::Notify;
use parking_lot::{Mutex, RwLock};
use serde::Serialize;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;

/// How often to look for Plex while it isn't answering, and once it is.
const POLL_DOWN: Duration = Duration::from_secs(5);
const POLL_UP: Duration = Duration::from_secs(30);

/// Where to get Plex; the app only ever links here.
pub const DOWNLOAD_URL: &str = "https://www.plex.tv/media-server-downloads/";

#[derive(Debug, Clone, Serialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PlexStatus {
    /// off | notInstalled | notRunning | unclaimed | running
    pub phase: &'static str,
    /// Plex is installed in a usual place: None when that can't be known
    /// (the Snap can't see the system's programs).
    pub installed: Option<bool>,
    pub version: Option<String>,
    pub machine_id: Option<String>,
    pub server_name: Option<String>,
}

pub struct PlexManager {
    /// Plex's port while the integration is on.
    desired: watch::Sender<Option<u16>>,
    status: RwLock<PlexStatus>,
    /// Server access token, for the server's name.
    token: Mutex<Option<String>>,
    client_id: Mutex<String>,
    wake: tokio::sync::Notify,
    notify: Notify,
}

impl PlexManager {
    pub fn new(notify: Notify) -> Arc<Self> {
        let (desired, rx) = watch::channel(None);
        let me = Arc::new(Self {
            desired,
            status: RwLock::new(PlexStatus {
                phase: "off",
                ..Default::default()
            }),
            token: Mutex::new(None),
            client_id: Mutex::new(String::new()),
            wake: tokio::sync::Notify::new(),
            notify,
        });
        let runner = me.clone();
        tokio::spawn(async move { runner.run(rx).await });
        me
    }

    pub fn status(&self) -> PlexStatus {
        self.status.read().clone()
    }

    pub fn is_running(&self) -> bool {
        self.status.read().phase == "running"
    }

    /// Watch Plex on `port`, or stop watching (None).
    pub fn apply(&self, port: Option<u16>, client_id: &str, token: Option<String>) {
        *self.client_id.lock() = client_id.to_string();
        let token_changed = {
            let mut t = self.token.lock();
            let changed = *t != token;
            *t = token;
            changed
        };
        let port_changed = self.desired.send_if_modified(|cur| {
            if *cur == port {
                false
            } else {
                *cur = port;
                true
            }
        });
        if token_changed && !port_changed {
            self.refresh();
        }
    }

    /// Look again now (e.g. after the user finished Plex's own setup).
    pub fn refresh(&self) {
        self.wake.notify_one();
    }

    fn set(&self, f: impl FnOnce(&mut PlexStatus)) {
        let changed = {
            let mut s = self.status.write();
            let before = s.clone();
            f(&mut s);
            *s != before
        };
        if changed {
            (self.notify)();
        }
    }

    async fn run(self: Arc<Self>, mut rx: watch::Receiver<Option<u16>>) {
        loop {
            let port = *rx.borrow_and_update();
            let wait = match port {
                None => {
                    self.set(|s| {
                        *s = PlexStatus {
                            phase: "off",
                            ..Default::default()
                        }
                    });
                    None
                }
                Some(port) => Some(if self.check(port).await {
                    POLL_UP
                } else {
                    POLL_DOWN
                }),
            };
            match wait {
                None => {
                    if rx.changed().await.is_err() {
                        return;
                    }
                }
                Some(wait) => {
                    tokio::select! {
                        r = rx.changed() => if r.is_err() { return },
                        _ = tokio::time::sleep(wait) => {}
                        _ = self.wake.notified() => {}
                    }
                }
            }
        }
    }

    /// One look at Plex; true if it answered.
    async fn check(&self, port: u16) -> bool {
        let client_id = self.client_id.lock().clone();
        let token = self.token.lock().clone();
        let anon = api::PlexApi::new(port, &client_id, None);
        match anon.identity().await {
            Ok(id) => {
                let name = match token {
                    Some(t) if id.claimed => api::PlexApi::new(port, &client_id, Some(t))
                        .friendly_name()
                        .await
                        .ok()
                        .filter(|n| !n.is_empty()),
                    _ => None,
                };
                self.set(|s| {
                    s.phase = if id.claimed { "running" } else { "unclaimed" };
                    s.installed = Some(true);
                    s.version = Some(id.version).filter(|v| !v.is_empty());
                    s.machine_id = Some(id.machine_id).filter(|m| !m.is_empty());
                    if name.is_some() {
                        s.server_name = name;
                    }
                });
                true
            }
            Err(_) => {
                let installed = installed();
                self.set(|s| {
                    *s = PlexStatus {
                        phase: if installed == Some(false) {
                            "notInstalled"
                        } else {
                            "notRunning"
                        },
                        installed,
                        // Keep what we knew; it's the same server when it's back.
                        machine_id: s.machine_id.take(),
                        server_name: s.server_name.take(),
                        version: None,
                    }
                });
                false
            }
        }
    }
}

/// Whether Plex Media Server is installed where its installers put it.
fn installed() -> Option<bool> {
    if crate::paths::sandboxed() {
        return None;
    }
    let candidates: Vec<std::path::PathBuf> = if cfg!(windows) {
        ["ProgramFiles", "ProgramFiles(x86)"]
            .iter()
            .filter_map(std::env::var_os)
            .map(|dir| {
                std::path::Path::new(&dir)
                    .join("Plex")
                    .join("Plex Media Server")
                    .join("Plex Media Server.exe")
            })
            .collect()
    } else if cfg!(target_os = "macos") {
        vec!["/Applications/Plex Media Server.app".into()]
    } else {
        vec!["/usr/lib/plexmediaserver/Plex Media Server".into()]
    };
    Some(candidates.iter().any(|p| p.exists()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Against a real Plex on this computer: `cargo test plex_live -- --ignored`.
    #[tokio::test]
    #[ignore]
    async fn plex_live_is_detected() {
        let notify: Notify = Arc::new(|| {});
        let m = PlexManager::new(notify);
        m.apply(Some(32400), "live-test", None);
        for _ in 0..50 {
            if m.status().phase != "off" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let s = m.status();
        println!("{s:?}");
        assert!(matches!(s.phase, "running" | "unclaimed"), "{s:?}");
        assert!(s.machine_id.is_some());
        m.apply(None, "live-test", None);
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(m.status().phase, "off");
    }
}
