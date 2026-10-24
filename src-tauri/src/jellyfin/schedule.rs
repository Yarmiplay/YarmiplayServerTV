//! Refresh Jellyfin libraries on the same interval as update checks.
//!
//! The interval is `updates.interval_minutes` (30 minutes unless the user
//! picked another). Changing it wakes this loop immediately. The refresh is
//! the same one as the Jellyfin page's Refresh libraries button: empty
//! libraries are removed, then Jellyfin scans what's left. Media files are
//! not deleted.

use crate::orchestrator::App;
use crate::updates::{library_refresh_is_due, FIRST_CHECK};
use std::sync::Arc;
use std::time::Instant;
use tauri::{AppHandle, Manager};
use tracing::{info, warn};

pub fn start(handle: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let mut last: Option<Instant> = None;
        let mut started = false;
        loop {
            let interval = handle.state::<Arc<App>>().settings().updates.interval();
            let wait = if !started {
                FIRST_CHECK
            } else {
                last.map(|t| interval.saturating_sub(t.elapsed()))
                    .unwrap_or_default()
                    .max(std::time::Duration::from_secs(1))
            };
            let schedule = handle.state::<Arc<App>>().updates.clone();
            tokio::select! {
                _ = tokio::time::sleep(wait) => {}
                _ = schedule.library_schedule_listener() => {}
            }
            started = true;
            let app = handle.state::<Arc<App>>().inner().clone();
            let interval = app.settings().updates.interval();
            if !library_refresh_is_due(last.map(|t| t.elapsed()), interval) {
                continue;
            }
            // Record the slot even when Jellyfin is off, so a stopped server
            // doesn't refresh in a tight loop. The next one is one interval later.
            last = Some(Instant::now());
            if !app.jellyfin_refresh_allowed() {
                continue;
            }
            match app.jellyfin_rescan().await {
                Ok(()) => info!("scheduled Jellyfin library refresh"),
                Err(e) => warn!(error = %e, "scheduled Jellyfin library refresh"),
            }
        }
    });
}
