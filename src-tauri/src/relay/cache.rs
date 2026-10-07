//! On-disk chunk cache: one file per 4 MiB chunk under
//! `<relay-cache>/<file id>/<index>`. Bookkeeping (which chunks exist, LRU,
//! pins) lives in [`super::State`]; this module only touches the disk.

use std::path::{Path, PathBuf};
use std::time::Instant;

pub const CHUNK: u64 = 4 << 20;
/// Disk space always left free for everything else.
pub const FREE_DISK_FLOOR: u64 = 2 << 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkState {
    Missing,
    /// Assigned to an upload; `have` bytes have arrived so far.
    Inflight,
    Done,
}

#[derive(Debug)]
pub struct Chunk {
    pub state: ChunkState,
    pub have: u64,
    pub last_used: Instant,
    /// Readers currently copying from the chunk's file.
    pub pins: u32,
}

impl Chunk {
    pub fn new(now: Instant) -> Self {
        Self {
            state: ChunkState::Missing,
            have: 0,
            last_used: now,
            pins: 0,
        }
    }
}

pub fn chunk_count(size: u64) -> u64 {
    size.div_ceil(CHUNK)
}

pub fn chunk_len(size: u64, index: u64) -> u64 {
    CHUNK.min(size.saturating_sub(index * CHUNK))
}

/// Bytes the cache may hold: the configured limit, but never eating into the
/// last [`FREE_DISK_FLOOR`] of the disk.
pub fn budget(limit: u64, used: u64, free: Option<u64>) -> u64 {
    match free {
        Some(free) => limit.min((free + used).saturating_sub(FREE_DISK_FLOOR)),
        None => limit,
    }
}

#[derive(Debug)]
pub struct Store {
    dir: PathBuf,
}

impl Store {
    /// Opens the cache folder, deleting whatever a previous run left there.
    pub fn new(dir: PathBuf) -> Self {
        let store = Self { dir };
        store.wipe();
        store
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn chunk_path(&self, file_id: &str, index: u64) -> PathBuf {
        self.dir.join(file_id).join(index.to_string())
    }

    pub fn ensure_file_dir(&self, file_id: &str) -> std::io::Result<()> {
        std::fs::create_dir_all(self.dir.join(file_id))
    }

    pub fn remove_chunk(&self, file_id: &str, index: u64) {
        let _ = std::fs::remove_file(self.chunk_path(file_id, index));
    }

    pub fn remove_file(&self, file_id: &str) {
        let _ = std::fs::remove_dir_all(self.dir.join(file_id));
    }

    pub fn wipe(&self) {
        let _ = std::fs::remove_dir_all(&self.dir);
        if std::fs::create_dir_all(&self.dir).is_ok() {
            crate::paths::restrict_dir(&self.dir);
        }
    }

    pub fn free_space(&self) -> Option<u64> {
        free_space(&self.dir)
    }
}

#[cfg(windows)]
pub fn free_space(path: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    let mut free = 0u64;
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut free,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    (ok != 0).then_some(free)
}

#[cfg(unix)]
pub fn free_space(path: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
        return None;
    }
    #[allow(clippy::unnecessary_cast)]
    Some(st.f_bavail as u64 * st.f_frsize as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_math() {
        assert_eq!(chunk_count(0), 0);
        assert_eq!(chunk_count(1), 1);
        assert_eq!(chunk_count(CHUNK), 1);
        assert_eq!(chunk_count(CHUNK + 1), 2);
        assert_eq!(chunk_len(CHUNK + 5, 1), 5);
        assert_eq!(chunk_len(CHUNK + 5, 0), CHUNK);
    }

    #[test]
    fn budget_keeps_a_free_disk_floor() {
        let gb = 1u64 << 30;
        assert_eq!(budget(10 * gb, 0, None), 10 * gb);
        assert_eq!(budget(10 * gb, 0, Some(100 * gb)), 10 * gb);
        assert_eq!(budget(10 * gb, gb, Some(3 * gb)), 2 * gb);
        assert_eq!(budget(10 * gb, 0, Some(gb)), 0);
    }

    #[test]
    fn store_wipes_at_start_and_free_space_is_known() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("relay");
        std::fs::create_dir_all(root.join("old")).unwrap();
        std::fs::write(root.join("old").join("0"), b"x").unwrap();
        let store = Store::new(root.clone());
        assert!(!root.join("old").exists());
        assert!(root.exists());
        assert!(store.free_space().unwrap() > 0);
    }
}
