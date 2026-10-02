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
            return Self { config: root.join("config"), data: root.join("data") };
        }
        let dirs = directories::ProjectDirs::from("com", "Yarmiplay", "YarmiplayServerTV")
            .expect("no home directory");
        Self { config: dirs.config_dir().to_path_buf(), data: dirs.data_local_dir().to_path_buf() }
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
