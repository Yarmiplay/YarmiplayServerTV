//! Reading show and episode out of video file names, for sorting loose
//! episodes into show folders: fansub names such as
//! `[SubsPlease] Sousou no Frieren S2 - 05 (1080p) [ABCD1234].mkv`, scene
//! names such as `Show.Name.S01E05.1080p.WEB.mkv`, and `Show Ep 5.mp4`.

/// What a file name says about the episode in it.
#[derive(Debug, Clone, PartialEq)]
pub struct Parsed {
    /// The show as the name spells it, without season, part or tags.
    pub show: String,
    /// The season the name gives (`S2`, `2nd Season`, `S02E05`), if any.
    pub season: Option<u32>,
    pub episode: u32,
}

/// A name without its extension (`.mkv`, `.en.ass` keeps `.en`).
pub fn stem(name: &str) -> &str {
    match name.rsplit_once('.') {
        Some((s, ext)) if !s.is_empty() && (1..=5).contains(&ext.len()) && ext.chars().all(|c| c.is_ascii_alphanumeric()) => s,
        _ => name,
    }
}

/// `05`, `05v2` (the version is ignored); `None` for `12.5`, ranges and specials.
fn episode_number(word: &str) -> Option<u32> {
    let digits = word.split(['v', 'V']).next()?;
    let rest = &word[digits.len()..];
    let version_ok = rest.is_empty() || (rest.len() > 1 && rest[1..].chars().all(|c| c.is_ascii_digit()));
    (!digits.is_empty() && digits.len() <= 4 && digits.chars().all(|c| c.is_ascii_digit()) && version_ok)
        .then(|| digits.parse().ok())
        .flatten()
}

/// `S01E05`, `s1e5`, `S01E05v2`: (season, episode).
fn season_episode(word: &str) -> Option<(u32, u32)> {
    let lower = word.to_ascii_lowercase();
    let rest = lower.strip_prefix('s')?;
    let (s, e) = rest.split_once('e')?;
    let season = (!s.is_empty() && s.len() <= 2 && s.chars().all(|c| c.is_ascii_digit())).then(|| s.parse().ok())??;
    Some((season, episode_number(e)?))
}

/// `1x05`: (season, episode).
fn cross(word: &str) -> Option<(u32, u32)> {
    let (s, e) = word.to_ascii_lowercase().split_once('x').map(|(a, b)| (a.to_string(), b.to_string()))?;
    let season = (!s.is_empty() && s.len() <= 2 && s.chars().all(|c| c.is_ascii_digit())).then(|| s.parse().ok())??;
    (e.len() >= 2).then_some(())?;
    Some((season, episode_number(&e)?))
}

/// `E05`, `Ep05`, `EP.05`: the episode.
fn e_word(word: &str) -> Option<u32> {
    let lower = word.to_ascii_lowercase();
    let n = lower.strip_prefix("ep").or_else(|| lower.strip_prefix('e'))?;
    episode_number(n.trim_start_matches('.'))
}

/// Read a video file name; `None` when it names no single episode.
pub fn parse(name: &str) -> Option<Parsed> {
    let mut s = stem(name.trim()).to_string();
    // `[Group] ` in front.
    if let Some(rest) = s.strip_prefix('[') {
        if let Some((_, rest)) = rest.split_once(']') {
            s = rest.trim_start().to_string();
        }
    }
    // Scene names use dots or underscores for spaces.
    let spaces = s.matches(' ').count();
    if s.contains('_') || s.matches('.').count() > spaces {
        s = s.replace('_', " ");
        if s.matches('.').count() > spaces {
            s = s.replace('.', " ");
        }
    }
    // Trailing tags: "(1080p)", "[1080p][MultiSub]".
    let body = s.find(['[', '(']).map_or(s.as_str(), |i| &s[..i]).trim().to_string();
    let words: Vec<&str> = body.split_whitespace().collect();

    // S01E05 / 1x05 anywhere: the show is what comes before.
    for (i, w) in words.iter().enumerate() {
        if let Some((season, episode)) = season_episode(w).or_else(|| cross(w)) {
            return finish(&words[..i].join(" "), Some(season), episode);
        }
    }
    // Fansub style: the number after the last " - ".
    if let Some((show, label)) = body.rsplit_once(" - ") {
        if let Some(episode) = label.split_whitespace().next().and_then(episode_number) {
            return finish(show, None, episode);
        }
    }
    // "Ep 05", "Episode 05", "E05".
    for (i, w) in words.iter().enumerate().rev() {
        let lower = w.to_ascii_lowercase();
        let lower = lower.trim_end_matches('.');
        if (lower == "ep" || lower == "episode") && i > 0 {
            if let Some(episode) = words.get(i + 1).and_then(|n| episode_number(n)) {
                return finish(&words[..i].join(" "), None, episode);
            }
        }
        if i > 0 {
            if let Some(episode) = e_word(w) {
                return finish(&words[..i].join(" "), None, episode);
            }
        }
    }
    None
}

/// The show name cleaned of season and part words, with the season they gave.
fn finish(show: &str, season: Option<u32>, episode: u32) -> Option<Parsed> {
    let named_season = season_number(show);
    let show = display_title(show);
    (!show.is_empty()).then(|| Parsed {
        show,
        season: season.or(named_season),
        episode,
    })
}

/// Roman numerals that mark a season (`Overlord IV`). `v` and `x` are left
/// alone: too many titles use them as words (`Hunter x Hunter`).
fn roman(w: &str) -> Option<u32> {
    Some(match w {
        "ii" => 2,
        "iii" => 3,
        "iv" => 4,
        "vi" => 6,
        "vii" => 7,
        "viii" => 8,
        "ix" => 9,
        _ => return None,
    })
}

fn ordinal(w: &str) -> Option<u32> {
    let n = w.trim_end_matches(|c: char| c.is_ascii_alphabetic());
    let suffix = &w[n.len()..];
    matches!(suffix, "st" | "nd" | "rd" | "th")
        .then(|| n.parse().ok())
        .flatten()
}

fn is_marker(w: &str) -> bool {
    w.len() >= 2 && (w.starts_with('s') || w.starts_with('p')) && w[1..].chars().all(|c| c.is_ascii_digit())
}

/// Lowercase words of letters and digits, with season and part spellings
/// made one: `2nd Season`, `Season 2`, `S2` and `II` become `s2`; `Part 2`,
/// `Cour 2` and `2nd Part` become `p2`. Years, `TV` and a leading `The` go.
pub fn normalize_title(title: &str) -> String {
    let cleaned: String = title
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect();
    let words: Vec<&str> = cleaned.split_whitespace().collect();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < words.len() {
        let w = words[i];
        let next = words.get(i + 1).copied();
        if w == "season" {
            if let Some(n) = next.and_then(|n| n.parse::<u32>().ok().or_else(|| roman(n))) {
                out.push(format!("s{n}"));
                i += 2;
                continue;
            }
        }
        if w == "part" || w == "cour" {
            if let Some(n) = next.and_then(|n| n.parse::<u32>().ok().or_else(|| roman(n))) {
                out.push(format!("p{n}"));
                i += 2;
                continue;
            }
        }
        if let Some(n) = ordinal(w) {
            match next {
                Some("season") => {
                    out.push(format!("s{n}"));
                    i += 2;
                    continue;
                }
                Some("part") | Some("cour") => {
                    out.push(format!("p{n}"));
                    i += 2;
                    continue;
                }
                _ => {}
            }
        }
        if let Some(n) = roman(w) {
            if next.is_none() || matches!(next, Some("part") | Some("cour")) {
                out.push(format!("s{n}"));
                i += 1;
                continue;
            }
        }
        let year = w.len() == 4 && (w.starts_with("19") || w.starts_with("20")) && w.chars().all(|c| c.is_ascii_digit());
        let filler = (w == "tv" && i > 0) || (w == "the" && out.is_empty() && words.len() > 1);
        if !(year && i > 0) && !filler {
            out.push(w.to_string());
        }
        i += 1;
    }
    out.retain(|w| w != "s1" && w != "p1");
    out.join(" ")
}

/// The normalized title without season and part markers: what files of one
/// show have in common.
pub fn base_title(title: &str) -> String {
    normalize_title(title)
        .split(' ')
        .filter(|w| !is_marker(w))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The season number a title says, if any (`S3`, `3rd Season` → 3).
pub fn season_number(title: &str) -> Option<u32> {
    normalize_title(title)
        .split(' ')
        .find(|w| w.starts_with('s') && is_marker(w))
        .and_then(|w| w[1..].parse().ok())
}

/// The title as written, without its season and part words and the
/// separators they leave: `Sousou no Frieren 2nd Season` → `Sousou no Frieren`.
pub fn display_title(title: &str) -> String {
    let words: Vec<&str> = title.split_whitespace().collect();
    let lower: Vec<String> = words
        .iter()
        .map(|w| w.to_lowercase().trim_matches(|c: char| !c.is_alphanumeric()).to_string())
        .collect();
    let number = |w: &str| w.parse::<u32>().is_ok() || roman(w).is_some();
    let mut keep = vec![true; words.len()];
    let mut i = 0;
    while i < words.len() {
        let w = lower[i].as_str();
        let next = lower.get(i + 1).map(String::as_str);
        let two = ((w == "season" || w == "part" || w == "cour") && next.is_some_and(number))
            || (ordinal(w).is_some() && matches!(next, Some("season") | Some("part") | Some("cour")));
        if two {
            keep[i] = false;
            keep[i + 1] = false;
            i += 2;
            continue;
        }
        let last = i + 1 == words.len();
        if (i > 0 && is_marker(w) && w.starts_with('s')) || (i > 0 && last && roman(w).is_some()) {
            keep[i] = false;
        }
        i += 1;
    }
    let kept: Vec<&str> = words.iter().zip(keep).filter(|(_, k)| *k).map(|(w, _)| *w).collect();
    kept.join(" ")
        .trim_matches(|c: char| c.is_whitespace() || matches!(c, '-' | ':' | '_' | ',' | '–'))
        .to_string()
}

/// A safe folder name: no path separators or characters Windows refuses, at
/// most 200 characters.
pub fn safe_file_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => ' ',
            c if c.is_control() => ' ',
            c => c,
        })
        .collect();
    let cleaned = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    let cleaned = cleaned.trim_matches('.').trim();
    let mut out: String = cleaned.chars().take(200).collect();
    if out.is_empty() {
        out = "Unknown".into();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(name: &str) -> Option<(String, Option<u32>, u32)> {
        parse(name).map(|x| (x.show, x.season, x.episode))
    }

    fn some(show: &str, season: Option<u32>, ep: u32) -> Option<(String, Option<u32>, u32)> {
        Some((show.to_string(), season, ep))
    }

    #[test]
    fn fansub_names() {
        assert_eq!(p("[SubsPlease] Sousou no Frieren - 05 (1080p) [ABCD1234].mkv"), some("Sousou no Frieren", None, 5));
        assert_eq!(p("[SubsPlease] Sousou no Frieren S2 - 05 (1080p) [ABCD1234].mkv"), some("Sousou no Frieren", Some(2), 5));
        assert_eq!(p("[Erai-raws] Kaiju No. 8 - 03 [1080p CR WEB-DL AVC AAC][MultiSub][ABC].mkv"), some("Kaiju No. 8", None, 3));
        assert_eq!(p("[Erai-raws] Kaiju No. 8 - 03v2 [1080p].mkv"), some("Kaiju No. 8", None, 3));
        assert_eq!(p("[SubsPlease] Dragon Ball Super - Beerus - 01 (720p).mkv"), some("Dragon Ball Super - Beerus", None, 1));
        assert_eq!(p("[Group] Re Zero kara Hajimeru Isekai Seikatsu 3rd Season - 04 [1080p].mkv"), some("Re Zero kara Hajimeru Isekai Seikatsu", Some(3), 4));
        assert_eq!(p("[Group] Overlord IV - 02 [1080p].mkv"), some("Overlord", Some(4), 2));
        assert_eq!(p("Mushoku Tensei - 12.mp4"), some("Mushoku Tensei", None, 12));
    }

    #[test]
    fn scene_and_plain_names() {
        assert_eq!(p("Show.Name.S01E05.1080p.WEB.h264-GRP.mkv"), some("Show Name", Some(1), 5));
        assert_eq!(p("The Office S03E10.mp4"), some("The Office", Some(3), 10));
        assert_eq!(p("show_name_2x07_hdtv.avi"), some("show name", Some(2), 7));
        assert_eq!(p("Show_-_05_[720p].mkv"), some("Show", None, 5));
        assert_eq!(p("Show Ep 5.mp4"), some("Show", None, 5));
        assert_eq!(p("Show Episode 12.mkv"), some("Show", None, 12));
        assert_eq!(p("Show E07 [1080p].mkv"), some("Show", None, 7));
    }

    #[test]
    fn not_single_episodes() {
        assert_eq!(p("[SubsPlease] Sousou no Frieren (01-28) (1080p) [Batch].mkv"), None);
        assert_eq!(p("[Group] Show - 01-12 [1080p].mkv"), None, "a range is a batch");
        assert_eq!(p("Spirited Away (2001).mkv"), None);
        assert_eq!(p("[Group] Show - 12.5 [1080p].mkv"), None);
        assert_eq!(p("Kaiju No. 8.mkv"), None, "a name ending in a number is no episode");
    }

    #[test]
    fn seasons_and_titles() {
        assert_eq!(normalize_title("Sousou no Frieren 2nd Season"), "sousou no frieren s2");
        assert_eq!(base_title("Sousou no Frieren S2"), base_title("Sousou no Frieren"));
        assert_ne!(base_title("Frieren"), base_title("Sousou no Frieren"));
        assert_eq!(season_number("Dr. Stone S4"), Some(4));
        assert_eq!(display_title("Sousou no Frieren 2nd Season"), "Sousou no Frieren");
        assert_eq!(display_title("Dr. Stone: Science Future Part 2"), "Dr. Stone: Science Future");
        assert_eq!(display_title("Shingeki no Kyojin Season 3 Part 2"), "Shingeki no Kyojin");
        assert_eq!(display_title("Mob Psycho 100 II"), "Mob Psycho 100");
        assert_eq!(display_title("Hunter x Hunter"), "Hunter x Hunter");
        assert_eq!(display_title("S"), "S");
        assert_eq!(stem("a.en.ass"), "a.en");
        assert_eq!(safe_file_name("Re:Zero? / x"), "Re Zero x");
    }
}
