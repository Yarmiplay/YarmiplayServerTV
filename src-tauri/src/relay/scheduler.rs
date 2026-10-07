//! Which chunks to ask for, in which order, from whom. Pure helpers; the
//! relay's tick applies them.

use crate::syncplay::room::ConnId;
use std::collections::{BTreeSet, HashMap, VecDeque};
use std::time::{Duration, Instant};

/// Chunks kept ahead of a streaming reader (64 MiB).
pub const READAHEAD_CHUNKS: u64 = 16;
/// Longest run of chunks in one upload request.
pub const RUN_CHUNKS: u64 = 8;
/// Upload requests a seeder works on at once.
pub const MAX_INFLIGHT_PER_SEEDER: usize = 2;
/// An upload with no new bytes for this long goes to someone else.
pub const STALL: Duration = Duration::from_secs(10);
/// How long a seeder that stalled or failed is skipped for that file.
pub const PENALTY: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Playing: the next 64 MiB from the reader's position, first.
    Stream,
    /// Copying the whole file: everything from the position on, after streams.
    Download,
}

#[derive(Debug, Clone, Copy)]
pub struct Reader {
    pub chunk: u64,
    pub mode: Mode,
}

/// Wanted chunk indexes, most urgent first: every stream window (nearest
/// chunk first), then download ranges.
pub fn wanted(readers: &[Reader], chunks: u64) -> Vec<u64> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    let mut streams: Vec<&Reader> = readers.iter().filter(|r| r.mode == Mode::Stream).collect();
    streams.sort_by_key(|r| r.chunk);
    for ahead in 0..READAHEAD_CHUNKS {
        for r in &streams {
            let i = r.chunk + ahead;
            if i < chunks && seen.insert(i) {
                out.push(i);
            }
        }
    }
    let mut downloads: Vec<&Reader> = readers
        .iter()
        .filter(|r| r.mode == Mode::Download)
        .collect();
    downloads.sort_by_key(|r| r.chunk);
    for r in downloads {
        for i in r.chunk..chunks {
            if seen.insert(i) {
                out.push(i);
            }
        }
    }
    out
}

/// Split wanted chunks (in priority order) into runs of consecutive indexes,
/// at most [`RUN_CHUNKS`] long, keeping the order of each run's first chunk.
pub fn runs(missing: &[u64]) -> Vec<(u64, u64)> {
    let mut out: Vec<(u64, u64)> = Vec::new();
    let mut taken = BTreeSet::new();
    let set: BTreeSet<u64> = missing.iter().copied().collect();
    for &start in missing {
        if taken.contains(&start) {
            continue;
        }
        let mut count = 0;
        while count < RUN_CHUNKS
            && set.contains(&(start + count))
            && !taken.contains(&(start + count))
        {
            taken.insert(start + count);
            count += 1;
        }
        out.push((start, count));
    }
    out
}

/// The least busy seeder that has room for another upload and isn't penalized.
pub fn pick_seeder(
    seeders: &[ConnId],
    load: &HashMap<ConnId, usize>,
    penalties: &HashMap<ConnId, Instant>,
    now: Instant,
) -> Option<ConnId> {
    seeders
        .iter()
        .copied()
        .filter(|s| penalties.get(s).is_none_or(|until| *until <= now))
        .map(|s| (load.get(&s).copied().unwrap_or(0), s))
        .filter(|(l, _)| *l < MAX_INFLIGHT_PER_SEEDER)
        .min()
        .map(|(_, s)| s)
}

/// Bytes per second over the last few seconds.
#[derive(Debug, Default)]
pub struct RateMeter {
    samples: VecDeque<(Instant, u64)>,
}

const WINDOW: Duration = Duration::from_secs(5);

impl RateMeter {
    pub fn add(&mut self, now: Instant, bytes: u64) {
        self.samples.push_back((now, bytes));
        self.trim(now);
    }

    fn trim(&mut self, now: Instant) {
        while self
            .samples
            .front()
            .is_some_and(|(t, _)| now.duration_since(*t) > WINDOW)
        {
            self.samples.pop_front();
        }
    }

    pub fn rate(&mut self, now: Instant) -> u64 {
        self.trim(now);
        let total: u64 = self.samples.iter().map(|(_, b)| b).sum();
        total / WINDOW.as_secs()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streams_come_before_downloads() {
        let readers = [
            Reader {
                chunk: 100,
                mode: Mode::Download,
            },
            Reader {
                chunk: 2,
                mode: Mode::Stream,
            },
        ];
        let w = wanted(&readers, 105);
        assert_eq!(&w[..3], &[2, 3, 4]);
        assert_eq!(w[15], 17);
        assert_eq!(&w[16..], &[100, 101, 102, 103, 104]);
    }

    #[test]
    fn stream_window_stops_at_the_end() {
        let w = wanted(
            &[Reader {
                chunk: 3,
                mode: Mode::Stream,
            }],
            5,
        );
        assert_eq!(w, vec![3, 4]);
    }

    #[test]
    fn runs_are_consecutive_and_capped() {
        let missing: Vec<u64> = (0..20).chain([30, 31, 40]).collect();
        let r = runs(&missing);
        assert_eq!(r, vec![(0, 8), (8, 8), (16, 4), (30, 2), (40, 1)]);
        assert_eq!(runs(&[5, 3, 4, 6]), vec![(5, 2), (3, 2)]);
    }

    #[test]
    fn seeders_are_balanced_and_penalties_respected() {
        let now = Instant::now();
        let mut load = HashMap::new();
        let mut pen = HashMap::new();
        assert_eq!(pick_seeder(&[1, 2], &load, &pen, now), Some(1));
        load.insert(1, 1);
        assert_eq!(pick_seeder(&[1, 2], &load, &pen, now), Some(2));
        load.insert(2, 2);
        load.insert(1, 2);
        assert_eq!(pick_seeder(&[1, 2], &load, &pen, now), None);
        load.clear();
        pen.insert(1, now + PENALTY);
        assert_eq!(pick_seeder(&[1], &load, &pen, now), None);
        assert_eq!(pick_seeder(&[1], &load, &pen, now + PENALTY), Some(1));
    }

    #[test]
    fn rate_is_averaged_over_the_window() {
        let t = Instant::now();
        let mut m = RateMeter::default();
        m.add(t, 5_000_000);
        assert_eq!(m.rate(t), 1_000_000);
        assert_eq!(m.rate(t + Duration::from_secs(6)), 0);
    }
}
