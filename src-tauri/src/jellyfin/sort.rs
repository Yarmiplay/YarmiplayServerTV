//! Sorting loose episodes into show folders, so Jellyfin sees them as
//! episodes: `[SubsPlease] Frieren - 05 (1080p).mkv` at a library folder's
//! top level moves to `Frieren/`, and one that names a season to
//! `Frieren/Season 02/`. Files keep their names (Syncplay rooms match by
//! name), subtitles with the same name come along, existing folders are
//! reused, and nothing is ever overwritten. The last sort of each folder can
//! be undone.

use super::names::{self, Parsed};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Files changed more recently than this may still be downloading.
pub const SETTLE: Duration = Duration::from_secs(120);

const VIDEO: &[&str] = &["mkv", "mp4", "m4v", "avi", "webm", "mov", "wmv", "ts", "m2ts", "mpg", "mpeg", "flv", "ogv"];
const SIDECAR: &[&str] = &["srt", "ass", "ssa", "vtt", "sub", "idx", "sup", "nfo"];

fn ext_of(name: &str) -> String {
    name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default()
}

/// One episode to move, with its subtitles.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Move {
    /// File name at the folder's top level.
    pub file: String,
    /// Where it goes, relative to the folder (`Frieren/Season 02/…`).
    pub to: String,
    pub show: String,
    pub season: Option<u32>,
    pub episode: u32,
    /// Subtitle and info files that go with it.
    pub extras: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Skip {
    pub file: String,
    pub why: String,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SortPlan {
    pub folder: String,
    pub moves: Vec<Move>,
    pub skipped: Vec<Skip>,
}

/// What a sort did.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SortResult {
    pub moved: usize,
    pub shows: usize,
    pub skipped: Vec<Skip>,
}

/// The last sort of each folder, so it can be undone.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SortLog {
    /// Folder → (from, to) absolute paths, and the folders the sort created.
    pub folders: BTreeMap<String, LoggedSort>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LoggedSort {
    pub moves: Vec<(PathBuf, PathBuf)>,
    pub created: Vec<PathBuf>,
}

impl SortLog {
    pub fn load(file: &Path) -> Self {
        std::fs::read(file)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, file: &Path) -> Result<(), String> {
        let json = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        crate::paths::write_atomic(file, &json)
    }
}

/// The subfolder of `parent` that `wanted` names, matched loosely
/// (`frieren` for `Frieren`), else `wanted` as a new folder.
fn existing_dir(parent: &Path, wanted: &str, same: impl Fn(&str) -> bool) -> (String, bool) {
    if let Ok(entries) = std::fs::read_dir(parent) {
        for e in entries.flatten() {
            if e.file_type().is_ok_and(|t| t.is_dir()) {
                let name = e.file_name().to_string_lossy().to_string();
                if same(&name) {
                    return (name, true);
                }
            }
        }
    }
    (wanted.to_string(), false)
}

/// What sorting `folder` would do. Only its top level is looked at.
pub fn plan(folder: &Path, settle: Duration) -> Result<SortPlan, String> {
    let entries = std::fs::read_dir(folder).map_err(|e| format!("Couldn't open {}: {e}", folder.display()))?;
    let mut files: Vec<(String, Option<SystemTime>)> = Vec::new();
    for e in entries.flatten() {
        if e.file_type().is_ok_and(|t| t.is_file()) {
            let modified = e.metadata().ok().and_then(|m| m.modified().ok());
            files.push((e.file_name().to_string_lossy().to_string(), modified));
        }
    }
    files.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
    let names: Vec<&str> = files.iter().map(|f| f.0.as_str()).collect();
    let now = SystemTime::now();
    let mut out = SortPlan { folder: folder.display().to_string(), ..Default::default() };

    // Episodes by show (base title), each show's most common spelling for its folder.
    let mut episodes: Vec<(String, Parsed)> = Vec::new();
    for (name, modified) in &files {
        if !VIDEO.contains(&ext_of(name).as_str()) {
            continue;
        }
        let Some(parsed) = names::parse(name) else {
            out.skipped.push(Skip { file: name.clone(), why: "no episode number in its name".into() });
            continue;
        };
        let recent = modified.and_then(|m| now.duration_since(m).ok()).is_some_and(|age| age < settle);
        if recent {
            out.skipped.push(Skip { file: name.clone(), why: "changed in the last 2 minutes, so it may still be downloading".into() });
            continue;
        }
        episodes.push((name.clone(), parsed));
    }
    let mut spellings: HashMap<String, BTreeMap<String, usize>> = HashMap::new();
    for (_, p) in &episodes {
        *spellings.entry(names::base_title(&p.show)).or_default().entry(p.show.clone()).or_default() += 1;
    }
    let folder_for = |base: &str| -> String {
        let best = spellings[base].iter().max_by_key(|(_, n)| **n).map(|(s, _)| s.clone()).unwrap_or_default();
        let wanted = names::safe_file_name(&best);
        existing_dir(folder, &wanted, |d| names::base_title(d) == base && !names::base_title(d).is_empty()).0
    };

    let mut taken: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (name, p) in episodes {
        let show_dir = folder_for(&names::base_title(&p.show));
        let mut rel = PathBuf::from(&show_dir);
        if let Some(season) = p.season {
            let wanted = format!("Season {season:02}");
            let season_dir = existing_dir(&folder.join(&show_dir), &wanted, |d| {
                let lower = d.to_lowercase();
                lower.strip_prefix("season").and_then(|n| n.trim().parse::<u32>().ok()) == Some(season)
            })
            .0;
            rel.push(season_dir);
        }
        let stem = names::stem(&name).to_string();
        let extras: Vec<String> = names
            .iter()
            .filter(|n| **n != name && SIDECAR.contains(&ext_of(n).as_str()) && n.starts_with(&stem))
            .filter(|n| matches!(n.as_bytes().get(stem.len()), Some(b'.')))
            .map(|n| n.to_string())
            .collect();
        let target = rel.join(&name);
        let clash = std::iter::once(&name)
            .chain(extras.iter())
            .any(|f| folder.join(rel.join(f)).exists() || !taken.insert(rel.join(f).display().to_string()));
        if clash {
            out.skipped.push(Skip { file: name, why: "a file with that name is already in the show's folder".into() });
            continue;
        }
        out.moves.push(Move {
            file: name,
            to: target.to_string_lossy().replace('\\', "/"),
            show: show_dir,
            season: p.season,
            episode: p.episode,
            extras,
        });
    }
    Ok(out)
}

/// Sort `folder`: move what [`plan`] says. Files that can't be moved (in use)
/// are skipped; the moves made are logged in `log` for [`undo`].
pub fn apply(folder: &Path, settle: Duration, log: &mut SortLog) -> Result<SortResult, String> {
    let plan = plan(folder, settle)?;
    let mut result = SortResult { skipped: plan.skipped.clone(), ..Default::default() };
    let mut logged = LoggedSort::default();
    let mut shows = std::collections::HashSet::new();
    for m in &plan.moves {
        let target = folder.join(&m.to);
        let dir = target.parent().unwrap_or(folder).to_path_buf();
        // Note each folder this creates, outermost first, for undo.
        let mut missing = Vec::new();
        let mut d = dir.as_path();
        while d != folder && !d.exists() {
            missing.push(d.to_path_buf());
            d = match d.parent() {
                Some(p) => p,
                None => break,
            };
        }
        if let Err(e) = std::fs::create_dir_all(&dir) {
            result.skipped.push(Skip { file: m.file.clone(), why: format!("couldn't make its folder: {e}") });
            continue;
        }
        logged.created.extend(missing.into_iter().rev());
        let from = folder.join(&m.file);
        if let Err(e) = std::fs::rename(&from, &target) {
            result.skipped.push(Skip { file: m.file.clone(), why: format!("couldn't move it (is it playing?): {e}") });
            continue;
        }
        logged.moves.push((from, target));
        for extra in &m.extras {
            let (from, to) = (folder.join(extra), dir.join(extra));
            if std::fs::rename(&from, &to).is_ok() {
                logged.moves.push((from, to));
            }
        }
        result.moved += 1;
        shows.insert(m.show.clone());
    }
    result.shows = shows.len();
    if !logged.moves.is_empty() {
        log.folders.insert(folder.display().to_string(), logged);
    }
    Ok(result)
}

/// Put back the last sort of `folder`, where nothing is in the way, and
/// remove the folders it made if they're empty. How many files went back.
pub fn undo(folder: &Path, log: &mut SortLog) -> Result<usize, String> {
    let Some(last) = log.folders.remove(&folder.display().to_string()) else {
        return Err("There's no sort of this folder to undo".into());
    };
    let mut back = 0;
    for (from, to) in last.moves.iter().rev() {
        if to.exists() && !from.exists() && std::fs::rename(to, from).is_ok() {
            back += 1;
        }
    }
    for dir in last.created.iter().rev() {
        let _ = std::fs::remove_dir(dir);
    }
    Ok(back)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(dir: &Path, name: &str) {
        std::fs::write(dir.join(name), name.as_bytes()).unwrap();
    }

    #[test]
    fn sorts_episodes_into_show_and_season_folders() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path();
        touch(d, "[SubsPlease] Sousou no Frieren - 01 (1080p) [A].mkv");
        touch(d, "[SubsPlease] Sousou no Frieren - 01 (1080p) [A].en.ass");
        touch(d, "[SubsPlease] Sousou no Frieren - 02 (1080p) [B].mkv");
        touch(d, "[SubsPlease] Sousou no Frieren S2 - 03 (1080p) [C].mkv");
        touch(d, "Show.Name.S01E05.1080p.WEB.mkv");
        touch(d, "Spirited Away (2001).mkv");
        touch(d, "notes.txt");
        std::fs::create_dir(d.join("Already Sorted")).unwrap();
        touch(&d.join("Already Sorted"), "Already Sorted - 01.mkv");

        let p = plan(d, Duration::ZERO).unwrap();
        let tos: Vec<&str> = p.moves.iter().map(|m| m.to.as_str()).collect();
        assert!(tos.contains(&"Sousou no Frieren/[SubsPlease] Sousou no Frieren - 01 (1080p) [A].mkv"), "{tos:?}");
        assert!(tos.contains(&"Sousou no Frieren/Season 02/[SubsPlease] Sousou no Frieren S2 - 03 (1080p) [C].mkv"));
        assert!(tos.contains(&"Show Name/Season 01/Show.Name.S01E05.1080p.WEB.mkv"));
        assert_eq!(p.moves.len(), 4);
        assert_eq!(p.skipped, vec![Skip { file: "Spirited Away (2001).mkv".into(), why: "no episode number in its name".into() }]);
        let first = p.moves.iter().find(|m| m.episode == 1).unwrap();
        assert_eq!(first.extras, vec!["[SubsPlease] Sousou no Frieren - 01 (1080p) [A].en.ass".to_string()]);

        let mut log = SortLog::default();
        let r = apply(d, Duration::ZERO, &mut log).unwrap();
        assert_eq!((r.moved, r.shows), (4, 2));
        assert!(d.join("Sousou no Frieren/[SubsPlease] Sousou no Frieren - 01 (1080p) [A].en.ass").exists());
        assert!(d.join("Spirited Away (2001).mkv").exists() && d.join("notes.txt").exists());
        assert!(d.join("Already Sorted/Already Sorted - 01.mkv").exists(), "subfolders are left alone");

        assert_eq!(undo(d, &mut log).unwrap(), 5, "4 episodes and a subtitle");
        assert!(d.join("[SubsPlease] Sousou no Frieren S2 - 03 (1080p) [C].mkv").exists());
        assert!(!d.join("Sousou no Frieren").exists() && !d.join("Show Name").exists(), "the folders it made are gone");
        assert!(undo(d, &mut log).is_err());
    }

    #[test]
    fn reuses_folders_and_never_overwrites() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path();
        std::fs::create_dir_all(d.join("sousou no frieren/Season 2")).unwrap();
        touch(&d.join("sousou no frieren"), "[G] Sousou no Frieren - 01.mkv");
        touch(d, "[G] Sousou no Frieren - 01.mkv");
        touch(d, "[G] Sousou no Frieren - 02.mkv");
        touch(d, "[G] Sousou no Frieren 2nd Season - 01.mkv");
        let p = plan(d, Duration::ZERO).unwrap();
        let tos: Vec<&str> = p.moves.iter().map(|m| m.to.as_str()).collect();
        assert_eq!(tos, vec!["sousou no frieren/[G] Sousou no Frieren - 02.mkv", "sousou no frieren/Season 2/[G] Sousou no Frieren 2nd Season - 01.mkv"]);
        assert_eq!(p.skipped.len(), 1, "episode 1 is already there: {:?}", p.skipped);
    }

    #[test]
    fn leaves_files_that_may_still_be_downloading() {
        let tmp = tempfile::tempdir().unwrap();
        touch(tmp.path(), "[G] Show - 01.mkv");
        let p = plan(tmp.path(), SETTLE).unwrap();
        assert!(p.moves.is_empty());
        assert!(p.skipped[0].why.contains("still be downloading"));
    }
}
