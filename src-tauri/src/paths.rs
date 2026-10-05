//! Per-user app folders. Nothing here ever lives inside the repo or a synced
//! project folder: certificates, ACME keys and Jellyfin data stay in the OS
//! app-data location.

use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct AppPaths {
    /// Settings JSON.
    pub config: PathBuf,
    /// Persistent data: ACME account + certificates, Jellyfin install and data.
    pub data: PathBuf,
}

impl AppPaths {
    pub fn resolve() -> Self {
        if let Ok(root) = std::env::var("YARMIPLAYSERVERTV_HOME") {
            let root = PathBuf::from(root);
            return Self {
                config: root.join("config"),
                data: root.join("data"),
            };
        }
        // A packaged app's new AppData files land in a hidden per-package copy
        // that Explorer doesn't see, so the Store copy keeps everything in its
        // own LocalState folder, which Windows removes on uninstall.
        if let Some(family) = package_family() {
            let base = directories::BaseDirs::new().expect("no home directory");
            let root = base
                .data_local_dir()
                .join("Packages")
                .join(family)
                .join("LocalState");
            return Self {
                config: root.join("config"),
                data: root.join("data"),
            };
        }
        let dirs = directories::ProjectDirs::from("com", "Yarmiplay", "YarmiplayServerTV")
            .expect("no home directory");
        Self {
            config: dirs.config_dir().to_path_buf(),
            data: dirs.data_local_dir().to_path_buf(),
        }
    }

    pub fn settings_file(&self) -> PathBuf {
        self.config.join("settings.json")
    }

    pub fn acme_dir(&self) -> PathBuf {
        self.data.join("acme")
    }

    pub fn jellyfin_root(&self) -> PathBuf {
        self.data.join("jellyfin")
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.data.join("logs")
    }

    pub fn ensure(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.config)?;
        std::fs::create_dir_all(&self.data)?;
        restrict_dir(&self.config);
        restrict_dir(&self.data);
        Ok(())
    }
}

/// The package family name when running from the Microsoft Store package
/// (MSIX), None for every other install.
pub fn package_family() -> Option<&'static str> {
    static FAMILY: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    FAMILY.get_or_init(read_package_family).as_deref()
}

#[cfg(windows)]
fn read_package_family() -> Option<String> {
    use windows_sys::Win32::Foundation::ERROR_INSUFFICIENT_BUFFER;
    use windows_sys::Win32::Storage::Packaging::Appx::GetCurrentPackageFamilyName;
    let mut len = 0u32;
    // APPMODEL_ERROR_NO_PACKAGE when not packaged.
    let probe = unsafe { GetCurrentPackageFamilyName(&mut len, std::ptr::null_mut()) };
    if probe != ERROR_INSUFFICIENT_BUFFER || len == 0 {
        return None;
    }
    let mut buf = vec![0u16; len as usize];
    if unsafe { GetCurrentPackageFamilyName(&mut len, buf.as_mut_ptr()) } != 0 {
        return None;
    }
    buf.truncate(len.saturating_sub(1) as usize);
    Some(String::from_utf16_lossy(&buf))
}

#[cfg(not(windows))]
fn read_package_family() -> Option<String> {
    None
}

/// Written by packages whose package manager updates the app (the AUR package
/// puts `aur` in it). A file rather than an environment variable because the
/// login entry starts the binary directly, bypassing any wrapper script.
pub const MANAGED_BY_FILE: &str = "/usr/lib/yarmiplayservertv/managed-by";

/// Who updates this copy, when it isn't the app's own updater:
/// "microsoft-store", "flathub", "snap", or the contents of [`MANAGED_BY_FILE`].
pub fn managed_by() -> Option<&'static str> {
    static MANAGED: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    MANAGED
        .get_or_init(|| {
            managed_by_from(
                package_family(),
                std::env::var_os("FLATPAK_ID").is_some(),
                std::env::var_os("SNAP").is_some(),
                || std::fs::read_to_string(MANAGED_BY_FILE).ok(),
            )
        })
        .as_deref()
}

fn managed_by_from(
    package_family: Option<&str>,
    flatpak: bool,
    snap: bool,
    marker: impl FnOnce() -> Option<String>,
) -> Option<String> {
    if package_family.is_some() {
        return Some("microsoft-store".into());
    }
    if flatpak {
        return Some("flathub".into());
    }
    if snap {
        return Some("snap".into());
    }
    if !cfg!(target_os = "linux") {
        return None;
    }
    let name = marker()?.trim().to_ascii_lowercase();
    let valid = !name.is_empty()
        && name.len() <= 32
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    Some(if valid { name } else { "package-manager".into() })
}

/// The Flatpak and Snap packages carry the pinned Jellyfin themselves, so
/// nothing executable is downloaded into a sandbox at runtime.
pub fn sandboxed() -> bool {
    matches!(managed_by(), Some("flathub" | "snap"))
}

/// Owner-only permissions where the OS supports it (Windows app-data is already per-user).
pub fn restrict_dir(path: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// Write via a temp file + rename so a crash never leaves a half-written file.
pub fn write_atomic(path: &std::path::Path, contents: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    let tmp = path.with_extension(format!(
        "{}.tmp",
        path.extension().and_then(|e| e.to_str()).unwrap_or("")
    ));
    std::fs::write(&tmp, contents).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp, path).map_err(|e| format!("rename to {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::managed_by_from;

    fn none() -> Option<String> {
        None
    }

    #[test]
    fn managed_by_prefers_the_sandbox_over_the_marker_file() {
        let marker = || Some("aur\n".to_string());
        assert_eq!(
            managed_by_from(Some("Yarmiplay.YarmiplayServerTV_x"), false, false, marker).as_deref(),
            Some("microsoft-store")
        );
        assert_eq!(managed_by_from(None, true, true, marker).as_deref(), Some("flathub"));
        assert_eq!(managed_by_from(None, false, true, marker).as_deref(), Some("snap"));
        assert_eq!(managed_by_from(None, false, false, none), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn managed_by_reads_the_marker_file() {
        assert_eq!(
            managed_by_from(None, false, false, || Some("AUR\n".into())).as_deref(),
            Some("aur")
        );
        assert_eq!(
            managed_by_from(None, false, false, || Some("<script>".into())).as_deref(),
            Some("package-manager")
        );
        assert_eq!(
            managed_by_from(None, false, false, || Some("  ".into())).as_deref(),
            Some("package-manager")
        );
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn marker_file_is_linux_only() {
        assert_eq!(managed_by_from(None, false, false, || Some("aur".into())), None);
    }
}
