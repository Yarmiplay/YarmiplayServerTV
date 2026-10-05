//! "Start with system". The autostart plugin's login entry (the HKCU Run key
//! on Windows) never reaches Windows from the Microsoft Store package, so that
//! copy switches the startup task declared in src-tauri/msix/AppxManifest.xml
//! instead. A Flatpak can't write the host's autostart folder and asks the
//! Background portal. The Snap keeps the plugin's entry: its autostart folder
//! is inside the snap's home, where snapd looks for the `autostart` file named
//! in snap/snapcraft.yaml. These calls block (the portal may ask the user);
//! keep them off the main thread.

use tauri::AppHandle;
use tauri_plugin_autostart::ManagerExt as _;

pub fn is_enabled(app: &AppHandle) -> bool {
    #[cfg(windows)]
    if crate::paths::package_family().is_some() {
        return startup_task::is_enabled();
    }
    #[cfg(target_os = "linux")]
    if portal::in_flatpak() {
        return portal::is_enabled();
    }
    app.autolaunch().is_enabled().unwrap_or(false)
}

/// Returns whether it is on afterwards.
pub fn set(app: &AppHandle, enabled: bool) -> Result<bool, String> {
    #[cfg(windows)]
    if crate::paths::package_family().is_some() {
        return startup_task::set(enabled);
    }
    #[cfg(target_os = "linux")]
    if portal::in_flatpak() {
        return tauri::async_runtime::block_on(portal::set(enabled));
    }
    let launcher = app.autolaunch();
    let result = if enabled {
        launcher.enable()
    } else {
        launcher.disable()
    };
    result.map_err(|e| e.to_string())?;
    Ok(launcher.is_enabled().unwrap_or(false))
}

/// Whether this run was started at login: the plugin's entry passes
/// `--minimized`, the startup task shows up as the activation kind.
pub fn launched_at_login() -> bool {
    if std::env::args().any(|a| a == "--minimized") {
        return true;
    }
    #[cfg(windows)]
    if crate::paths::package_family().is_some() {
        return startup_task::activated();
    }
    false
}

#[cfg(test)]
mod tests {
    /// The plugin names its login entry after the product name; snapd only
    /// starts it if snapcraft.yaml declares that file name.
    #[test]
    fn snap_autostart_matches_the_plugin_entry() {
        let conf: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        let entry = format!("{}.desktop", conf["productName"].as_str().unwrap());
        let snapcraft = include_str!("../../snap/snapcraft.yaml");
        let declared: Vec<&str> = snapcraft
            .lines()
            .filter_map(|l| l.trim().strip_prefix("autostart:"))
            .map(str::trim)
            .collect();
        assert_eq!(declared, [entry.as_str()]);
    }
}

#[cfg(target_os = "linux")]
mod portal {
    use ashpd::desktop::background::Background;
    use std::path::PathBuf;

    pub fn in_flatpak() -> bool {
        crate::paths::managed_by() == Some("flathub")
    }

    /// The portal writes the login entry into the host's autostart folder,
    /// which the sandbox can't read back, so the last answer is kept here.
    fn state_file() -> PathBuf {
        crate::paths::AppPaths::resolve()
            .config
            .join("autostart-portal")
    }

    pub fn is_enabled() -> bool {
        state_file().is_file()
    }

    pub async fn set(enabled: bool) -> Result<bool, String> {
        let response = Background::request()
            .reason("Start YarmiplayServerTV in the tray when you log in")
            .auto_start(enabled)
            .command(["yarmiplayservertv", "--minimized"])
            .dbus_activatable(false)
            .send()
            .await
            .and_then(|r| r.response())
            .map_err(|e| format!("background portal: {e}"))?;
        let on = response.auto_start();
        if enabled && !on {
            return Err("Start with system was refused. Allow YarmiplayServerTV to run in the background in your system settings".into());
        }
        if on {
            crate::paths::write_atomic(&state_file(), b"on\n")?;
        } else {
            let _ = std::fs::remove_file(state_file());
        }
        Ok(on)
    }
}

#[cfg(windows)]
mod startup_task {
    use windows::core::HSTRING;
    use windows::ApplicationModel::Activation::ActivationKind;
    use windows::ApplicationModel::{AppInstance, StartupTask, StartupTaskState};

    /// The TaskId in AppxManifest.xml.
    const TASK_ID: &str = "YarmiplayServerTV";

    fn task() -> windows::core::Result<StartupTask> {
        StartupTask::GetAsync(&HSTRING::from(TASK_ID))?.join()
    }

    pub fn is_enabled() -> bool {
        task()
            .and_then(|t| t.State())
            .map(|s| s == StartupTaskState::Enabled || s == StartupTaskState::EnabledByPolicy)
            .unwrap_or(false)
    }

    pub fn set(enabled: bool) -> Result<bool, String> {
        let task = task().map_err(|e| format!("startup task: {e}"))?;
        if enabled {
            let state = task
                .RequestEnableAsync()
                .and_then(|op| op.join())
                .map_err(|e| format!("startup task: {e}"))?;
            if state == StartupTaskState::DisabledByUser {
                return Err("Start with system was switched off in Task Manager. Switch YarmiplayServerTV on under Startup apps there".into());
            }
            if state == StartupTaskState::DisabledByPolicy {
                return Err(
                    "Start with system is switched off by your organization's policy".into(),
                );
            }
        } else {
            task.Disable().map_err(|e| format!("startup task: {e}"))?;
        }
        Ok(is_enabled())
    }

    pub fn activated() -> bool {
        AppInstance::GetActivatedEventArgs()
            .and_then(|args| args.Kind())
            .map(|kind| kind == ActivationKind::StartupTask)
            .unwrap_or(false)
    }
}
