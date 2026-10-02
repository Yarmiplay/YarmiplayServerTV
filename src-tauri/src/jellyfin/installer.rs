//! Downloads the pinned Jellyfin server (and jellyfin-ffmpeg where the server
//! archive doesn't bundle it), verifies the SHA-256 from the embedded
//! manifest, and extracts it into the app-data folder.

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::info;

const MANIFEST: &str = include_str!("../../jellyfin-manifest.json");

#[derive(Debug, Clone, Deserialize)]
pub struct Artifact {
    pub url: String,
    pub sha256: String,
    pub size: u64,
    pub format: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PlatformEntry {
    pub server: Artifact,
    pub ffmpeg: Option<Artifact>,
    pub bundled_ffmpeg: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    pub jellyfin: String,
    pub ffmpeg: String,
    pub platforms: BTreeMap<String, PlatformEntry>,
}

pub fn manifest() -> Manifest {
    serde_json::from_str(MANIFEST).expect("embedded Jellyfin manifest is valid")
}

pub fn platform_key() -> String {
    let os = match std::env::consts::OS {
        "macos" => "macos",
        "windows" => "windows",
        _ => "linux",
    };
    format!("{os}-{}", std::env::consts::ARCH)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Installed {
    pub version: String,
    pub ffmpeg_version: String,
    pub exe: PathBuf,
    pub web: PathBuf,
    pub ffmpeg: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub downloaded: u64,
    pub total: u64,
    /// `download` or `extract`.
    pub stage: &'static str,
}

pub type ProgressFn = Arc<dyn Fn(Progress) + Send + Sync>;

fn installed_file(root: &Path) -> PathBuf {
    root.join("installed.json")
}

/// The current install, if it matches the pinned version and still exists.
pub fn current(root: &Path) -> Option<Installed> {
    let raw = std::fs::read_to_string(installed_file(root)).ok()?;
    let installed: Installed = serde_json::from_str(&raw).ok()?;
    let m = manifest();
    (installed.version == m.jellyfin && installed.exe.is_file() && installed.web.is_dir())
        .then_some(installed)
}

pub async fn ensure(root: &Path, progress: ProgressFn) -> Result<Installed, String> {
    if let Some(i) = current(root) {
        return Ok(i);
    }
    let m = manifest();
    let key = platform_key();
    let entry = m
        .platforms
        .get(&key)
        .cloned()
        .ok_or_else(|| format!("Jellyfin is not available for this platform ({key})"))?;
    std::fs::create_dir_all(root).map_err(|e| e.to_string())?;
    let downloads = root.join("downloads");
    std::fs::create_dir_all(&downloads).map_err(|e| e.to_string())?;

    let total = entry.server.size + entry.ffmpeg.as_ref().map_or(0, |f| f.size);
    let client = crate::net::client_builder()
        .user_agent(concat!("YarmiplayServerTV/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|e| e.to_string())?;

    info!(version = %m.jellyfin, platform = %key, "downloading Jellyfin");
    let server_archive = downloads.join(format!("server.{}", entry.server.format));
    download(&client, &entry.server, &server_archive, 0, total, &progress).await?;
    let ffmpeg_archive = match &entry.ffmpeg {
        Some(f) => {
            let path = downloads.join(format!("ffmpeg.{}", f.format));
            download(&client, f, &path, entry.server.size, total, &progress).await?;
            Some((path, f.format.clone()))
        }
        None => None,
    };

    progress(Progress {
        downloaded: total,
        total,
        stage: "extract",
    });
    let install_dir = root.join("install").join(&m.jellyfin);
    let staging = root
        .join("install")
        .join(format!(".staging-{}", uuid::Uuid::new_v4().simple()));
    let server_format = entry.server.format.clone();
    let bundled = entry.bundled_ffmpeg.clone();
    let staging2 = staging.clone();
    let install_dir2 = install_dir.clone();
    let server_archive2 = server_archive.clone();
    let installed = tokio::task::spawn_blocking(move || -> Result<Installed, String> {
        let _ = std::fs::remove_dir_all(&staging2);
        let server_dir = staging2.join("server");
        extract(&server_archive2, &server_format, &server_dir)?;
        let mut ffmpeg_rel = None;
        if let Some((archive, format)) = &ffmpeg_archive {
            let dir = staging2.join("ffmpeg");
            extract(archive, format, &dir)?;
            let found = find_file(&dir, &[exe_name("ffmpeg")])
                .ok_or("ffmpeg not found in the jellyfin-ffmpeg archive")?;
            ffmpeg_rel = Some(found.strip_prefix(&staging2).unwrap().to_path_buf());
        } else if let Some(name) = &bundled {
            if let Some(found) = find_file(&server_dir, &[name.as_str()]) {
                ffmpeg_rel = Some(found.strip_prefix(&staging2).unwrap().to_path_buf());
            }
        }
        let exe = find_file(&server_dir, &[exe_name("jellyfin").as_str()])
            .ok_or("jellyfin executable not found in the archive")?;
        let web = find_dir_with(&server_dir, "jellyfin-web", "index.html")
            .ok_or("jellyfin-web not found in the archive")?;
        let exe_rel = exe.strip_prefix(&staging2).unwrap().to_path_buf();
        let web_rel = web.strip_prefix(&staging2).unwrap().to_path_buf();

        let _ = std::fs::remove_dir_all(&install_dir2);
        std::fs::rename(&staging2, &install_dir2).map_err(|e| format!("install: {e}"))?;
        let installed = Installed {
            version: manifest().jellyfin,
            ffmpeg_version: manifest().ffmpeg,
            exe: install_dir2.join(exe_rel),
            web: install_dir2.join(web_rel),
            ffmpeg: ffmpeg_rel.map(|r| install_dir2.join(r)),
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for p in std::iter::once(&installed.exe).chain(installed.ffmpeg.iter()) {
                let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755));
            }
        }
        Ok(installed)
    })
    .await
    .map_err(|e| e.to_string())?;
    let installed = match installed {
        Ok(i) => i,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(e);
        }
    };

    let json = serde_json::to_vec_pretty(&installed).map_err(|e| e.to_string())?;
    crate::paths::write_atomic(&installed_file(root), &json)?;
    let _ = std::fs::remove_dir_all(&downloads);
    // Drop older versions.
    if let Ok(entries) = std::fs::read_dir(root.join("install")) {
        for e in entries.flatten() {
            if e.path() != install_dir {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    }
    info!(version = %installed.version, "Jellyfin installed");
    Ok(installed)
}

fn exe_name(base: &str) -> String {
    if cfg!(windows) {
        format!("{base}.exe")
    } else {
        base.to_string()
    }
}

async fn download(
    client: &reqwest::Client,
    artifact: &Artifact,
    dest: &Path,
    offset: u64,
    total: u64,
    progress: &ProgressFn,
) -> Result<(), String> {
    if let Ok(meta) = std::fs::metadata(dest) {
        if meta.len() == artifact.size
            && sha256_file(dest).as_deref() == Some(artifact.sha256.as_str())
        {
            return Ok(());
        }
    }
    let resp = client
        .get(&artifact.url)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| format!("download failed: {e}"))?;
    let part = dest.with_extension("part");
    let mut file = std::fs::File::create(&part).map_err(|e| e.to_string())?;
    let mut hasher = Sha256::new();
    let mut done: u64 = 0;
    let mut last_report = std::time::Instant::now();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("download interrupted: {e}"))?;
        file.write_all(&chunk).map_err(|e| e.to_string())?;
        hasher.update(&chunk);
        done += chunk.len() as u64;
        if done > artifact.size {
            return Err("download is larger than expected".into());
        }
        if last_report.elapsed() > std::time::Duration::from_millis(250) {
            last_report = std::time::Instant::now();
            progress(Progress {
                downloaded: offset + done,
                total,
                stage: "download",
            });
        }
    }
    file.flush().map_err(|e| e.to_string())?;
    drop(file);
    let digest = hex::encode(hasher.finalize());
    if done != artifact.size || digest != artifact.sha256 {
        let _ = std::fs::remove_file(&part);
        return Err(format!("checksum mismatch for {}", artifact.url));
    }
    std::fs::rename(&part, dest).map_err(|e| e.to_string())?;
    progress(Progress {
        downloaded: offset + done,
        total,
        stage: "download",
    });
    Ok(())
}

fn sha256_file(path: &Path) -> Option<String> {
    let mut f = std::fs::File::open(path).ok()?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut f, &mut hasher).ok()?;
    Some(hex::encode(hasher.finalize()))
}

fn extract(archive: &Path, format: &str, dest: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    let file = std::fs::File::open(archive).map_err(|e| e.to_string())?;
    match format {
        "zip" => {
            let mut zip = zip::ZipArchive::new(file).map_err(|e| format!("zip: {e}"))?;
            for i in 0..zip.len() {
                let mut entry = zip.by_index(i).map_err(|e| format!("zip: {e}"))?;
                let Some(rel) = entry.enclosed_name() else {
                    continue;
                };
                let out = dest.join(rel);
                if entry.is_dir() {
                    std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
                    continue;
                }
                if let Some(parent) = out.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
                let mut w = std::fs::File::create(&out).map_err(|e| e.to_string())?;
                std::io::copy(&mut entry, &mut w).map_err(|e| format!("zip: {e}"))?;
                #[cfg(unix)]
                if let Some(mode) = entry.unix_mode() {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = std::fs::set_permissions(
                        &out,
                        std::fs::Permissions::from_mode(mode & 0o777),
                    );
                }
            }
        }
        "tar.xz" => {
            let xz = liblzma::read::XzDecoder::new(std::io::BufReader::new(file));
            let mut tar = tar::Archive::new(xz);
            tar.set_preserve_permissions(true);
            for entry in tar.entries().map_err(|e| format!("tar: {e}"))? {
                let mut entry = entry.map_err(|e| format!("tar: {e}"))?;
                entry.unpack_in(dest).map_err(|e| format!("tar: {e}"))?;
            }
        }
        other => return Err(format!("unsupported archive format {other}")),
    }
    Ok(())
}

/// Shallowest regular file with one of `names`.
fn find_file<S: AsRef<str>>(root: &Path, names: &[S]) -> Option<PathBuf> {
    let mut queue = std::collections::VecDeque::from([root.to_path_buf()]);
    while let Some(dir) = queue.pop_front() {
        let mut entries: Vec<_> = std::fs::read_dir(&dir).ok()?.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for e in &entries {
            let path = e.path();
            if path.is_file()
                && names
                    .iter()
                    .any(|n| e.file_name().to_string_lossy() == n.as_ref())
            {
                return Some(path);
            }
        }
        for e in entries {
            if e.path().is_dir() {
                queue.push_back(e.path());
            }
        }
    }
    None
}

fn find_dir_with(root: &Path, name: &str, marker: &str) -> Option<PathBuf> {
    let mut queue = std::collections::VecDeque::from([root.to_path_buf()]);
    while let Some(dir) = queue.pop_front() {
        for e in std::fs::read_dir(&dir).ok()?.flatten() {
            let path = e.path();
            if path.is_dir() {
                if e.file_name() == name && path.join(marker).is_file() {
                    return Some(path);
                }
                queue.push_back(path);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_covers_desktop_platforms() {
        let m = manifest();
        for key in [
            "windows-x86_64",
            "windows-aarch64",
            "linux-x86_64",
            "linux-aarch64",
            "macos-x86_64",
            "macos-aarch64",
        ] {
            let e = m
                .platforms
                .get(key)
                .unwrap_or_else(|| panic!("{key} missing"));
            assert!(e.server.url.starts_with("https://repo.jellyfin.org/"));
            assert_eq!(e.server.sha256.len(), 64);
            assert!(
                e.ffmpeg.is_some() || e.bundled_ffmpeg.is_some(),
                "{key} has no ffmpeg"
            );
        }
        assert!(m.platforms.contains_key(&platform_key()));
    }

    #[test]
    fn extract_zip_and_locate_files() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("a.zip");
        {
            let f = std::fs::File::create(&archive).unwrap();
            let mut z = zip::ZipWriter::new(f);
            let opts = zip::write::SimpleFileOptions::default();
            z.start_file(format!("jellyfin/{}", exe_name("jellyfin")), opts)
                .unwrap();
            z.write_all(b"bin").unwrap();
            z.start_file("jellyfin/jellyfin-web/index.html", opts)
                .unwrap();
            z.write_all(b"<html>").unwrap();
            z.start_file("../evil.txt", opts).unwrap();
            z.write_all(b"x").unwrap();
            z.finish().unwrap();
        }
        let out = dir.path().join("out");
        extract(&archive, "zip", &out).unwrap();
        assert!(!dir.path().join("evil.txt").exists());
        let exe = find_file(&out, &[exe_name("jellyfin")]).unwrap();
        assert!(exe.ends_with(Path::new("jellyfin").join(exe_name("jellyfin"))));
        assert!(find_dir_with(&out, "jellyfin-web", "index.html").is_some());
    }

    #[test]
    fn extract_tar_xz() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("a.tar.xz");
        {
            let f = std::fs::File::create(&archive).unwrap();
            let xz = liblzma::write::XzEncoder::new(f, 6);
            let mut b = tar::Builder::new(xz);
            let data = b"ffmpeg";
            let mut h = tar::Header::new_gnu();
            h.set_size(data.len() as u64);
            h.set_mode(0o755);
            h.set_cksum();
            b.append_data(&mut h, format!("pkg/{}", exe_name("ffmpeg")), &data[..])
                .unwrap();
            b.into_inner().unwrap().finish().unwrap();
        }
        let out = dir.path().join("out");
        extract(&archive, "tar.xz", &out).unwrap();
        assert!(find_file(&out, &[exe_name("ffmpeg")]).is_some());
    }
}
