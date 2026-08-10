//! Watch history, resume positions, and bookmarks.

use crate::paths::{write_atomic, Paths, StoreError};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Fraction of an episode that must be watched before it counts as completed.
/// Anything past this is treated as "finished" so end credits don't leave every
/// episode looking half-watched.
const COMPLETION_THRESHOLD: f32 = 0.90;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub provider_id: String,
    pub series_id: String,
    pub series_title: String,
    /// Stored so `kuro next` / `kuro continue` can re-fetch the episode list
    /// without rebuilding a slug — provider slugs are too inconsistent to construct.
    #[serde(default)]
    pub series_url: String,
    pub episode: f32,
    pub position_secs: u64,
    pub duration_secs: Option<u64>,
    pub completed: bool,
    pub watched_at: DateTime<Utc>,
}

impl HistoryEntry {
    pub fn progress(&self) -> Option<f32> {
        let duration = self.duration_secs?;
        if duration == 0 {
            return None;
        }
        Some((self.position_secs as f32 / duration as f32).clamp(0.0, 1.0))
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct History {
    #[serde(default)]
    pub entries: Vec<HistoryEntry>,
}

impl History {
    pub fn load(paths: &Paths) -> Result<Self, StoreError> {
        let path = paths.history_file();
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                serde_json::from_str(&text).map_err(|source| StoreError::Json { path, source })
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(StoreError::Io { path, source }),
        }
    }

    pub fn save(&self, paths: &Paths) -> Result<(), StoreError> {
        let text =
            serde_json::to_string_pretty(self).expect("History is always serialisable");
        write_atomic(&paths.history_file(), &text)
    }

    /// Record progress, replacing any existing entry for the same episode.
    ///
    /// The list is kept sorted most-recent-first so `kuro continue` and `kuro list`
    /// can just read from the front.
    #[allow(clippy::too_many_arguments)]
    pub fn record(
        &mut self,
        provider_id: &str,
        series_id: &str,
        series_title: &str,
        series_url: &str,
        episode: f32,
        position_secs: u64,
        duration_secs: Option<u64>,
    ) {
        let completed = duration_secs
            .filter(|d| *d > 0)
            .map(|d| position_secs as f32 / d as f32 >= COMPLETION_THRESHOLD)
            .unwrap_or(false);

        self.entries.retain(|e| {
            !(e.provider_id == provider_id
                && e.series_id == series_id
                && (e.episode - episode).abs() < f32::EPSILON)
        });

        self.entries.insert(
            0,
            HistoryEntry {
                provider_id: provider_id.to_string(),
                series_id: series_id.to_string(),
                series_title: series_title.to_string(),
                series_url: series_url.to_string(),
                episode,
                position_secs,
                duration_secs,
                completed,
                watched_at: Utc::now(),
            },
        );
    }

    pub fn most_recent(&self) -> Option<&HistoryEntry> {
        self.entries.first()
    }

    pub fn find(&self, provider_id: &str, series_id: &str, episode: f32) -> Option<&HistoryEntry> {
        self.entries.iter().find(|e| {
            e.provider_id == provider_id
                && e.series_id == series_id
                && (e.episode - episode).abs() < f32::EPSILON
        })
    }

    /// Highest episode number of this series that has been marked completed.
    pub fn last_completed_episode(&self, provider_id: &str, series_id: &str) -> Option<f32> {
        self.entries
            .iter()
            .filter(|e| e.provider_id == provider_id && e.series_id == series_id && e.completed)
            .map(|e| e.episode)
            .max_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
    }

    /// Resume position for an episode, or `None` if it was never started or is
    /// already finished (replaying a completed episode should start from zero).
    pub fn resume_position(
        &self,
        provider_id: &str,
        series_id: &str,
        episode: f32,
    ) -> Option<u64> {
        let entry = self.find(provider_id, series_id, episode)?;
        if entry.completed || entry.position_secs == 0 {
            None
        } else {
            Some(entry.position_secs)
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bookmark {
    pub provider_id: String,
    pub series_id: String,
    pub series_title: String,
    pub url: String,
    pub added_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Bookmarks {
    #[serde(default)]
    pub entries: Vec<Bookmark>,
}

impl Bookmarks {
    pub fn load(paths: &Paths) -> Result<Self, StoreError> {
        let path = paths.bookmarks_file();
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                serde_json::from_str(&text).map_err(|source| StoreError::Json { path, source })
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(StoreError::Io { path, source }),
        }
    }

    pub fn save(&self, paths: &Paths) -> Result<(), StoreError> {
        let text =
            serde_json::to_string_pretty(self).expect("Bookmarks are always serialisable");
        write_atomic(&paths.bookmarks_file(), &text)
    }

    /// Returns `true` if the bookmark was added, `false` if it already existed.
    pub fn add(&mut self, bookmark: Bookmark) -> bool {
        if self
            .entries
            .iter()
            .any(|b| b.provider_id == bookmark.provider_id && b.series_id == bookmark.series_id)
        {
            return false;
        }
        self.entries.push(bookmark);
        true
    }

    /// Returns `true` if something was removed.
    pub fn remove(&mut self, series_id: &str) -> bool {
        let before = self.entries.len();
        self.entries.retain(|b| b.series_id != series_id);
        self.entries.len() != before
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn history_with(position: u64, duration: Option<u64>) -> History {
        let mut h = History::default();
        h.record("p", "s", "S", "https://x.tld/s", 1.0, position, duration);
        h
    }

    #[test]
    fn near_the_end_counts_as_completed() {
        let h = history_with(950, Some(1000));
        assert!(h.most_recent().expect("recorded").completed);
    }

    #[test]
    fn midway_is_not_completed_and_resumes() {
        let h = history_with(500, Some(1000));
        assert!(!h.most_recent().expect("recorded").completed);
        assert_eq!(h.resume_position("p", "s", 1.0), Some(500));
    }

    #[test]
    fn completed_episodes_restart_from_the_beginning() {
        let h = history_with(950, Some(1000));
        assert_eq!(h.resume_position("p", "s", 1.0), None);
    }

    #[test]
    fn unknown_duration_never_marks_completed() {
        let h = history_with(500, None);
        assert!(!h.most_recent().expect("recorded").completed);
    }

    #[test]
    fn rewatching_replaces_rather_than_appends() {
        let mut h = history_with(100, Some(1000));
        h.record("p", "s", "S", "https://x.tld/s", 1.0, 300, Some(1000));
        assert_eq!(h.entries.len(), 1);
        assert_eq!(h.entries[0].position_secs, 300);
    }

    #[test]
    fn last_completed_tracks_the_highest_episode() {
        let mut h = History::default();
        h.record("p", "s", "S", "https://x.tld/s", 1.0, 1000, Some(1000));
        h.record("p", "s", "S", "https://x.tld/s", 3.0, 1000, Some(1000));
        h.record("p", "s", "S", "https://x.tld/s", 2.0, 10, Some(1000));
        assert_eq!(h.last_completed_episode("p", "s"), Some(3.0));
    }

    #[test]
    fn bookmarks_are_deduplicated() {
        let mut b = Bookmarks::default();
        let mk = || Bookmark {
            provider_id: "p".into(),
            series_id: "s".into(),
            series_title: "S".into(),
            url: "https://example.com".into(),
            added_at: Utc::now(),
        };
        assert!(b.add(mk()));
        assert!(!b.add(mk()));
        assert_eq!(b.entries.len(), 1);
        assert!(b.remove("s"));
        assert!(!b.remove("s"));
    }
}
