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
        let text = serde_json::to_string_pretty(self).expect("History is always serialisable");
        write_atomic(&paths.history_file(), &text)
    }

    /// Record progress, replacing any existing entry for the same episode.
    ///
    /// The list is kept sorted most-recent-first so `kuro continue` and `kuro history`
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
    pub fn resume_position(&self, provider_id: &str, series_id: &str, episode: f32) -> Option<u64> {
        let entry = self.find(provider_id, series_id, episode)?;
        if entry.completed || entry.position_secs == 0 {
            None
        } else {
            Some(entry.position_secs)
        }
    }
}

/// How long a newly-spotted episode keeps its "new" badge by default.
pub const DEFAULT_NEW_WINDOW_DAYS: i64 = 7;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bookmark {
    pub provider_id: String,
    pub series_id: String,
    pub series_title: String,
    pub url: String,
    pub added_at: DateTime<Utc>,

    // Release tracking. Providers publish no per-episode air date, so "recent"
    // can only mean "kuro first saw it recently" — these fields are the snapshot
    // that makes that comparison possible. All optional so bookmarks written by
    // older versions keep loading.
    /// Highest episode number present at the last check.
    #[serde(default)]
    pub last_episode: Option<f32>,
    /// How many episodes the list held at the last check.
    #[serde(default)]
    pub episode_count: Option<usize>,
    #[serde(default)]
    pub last_checked_at: Option<DateTime<Utc>>,
    /// When the current [`Bookmark::last_episode`] was first observed. `None`
    /// until a check actually finds something newer than the baseline.
    #[serde(default)]
    pub new_since: Option<DateTime<Utc>>,
    /// Episodes that arrived in the check that set [`Bookmark::new_since`].
    #[serde(default)]
    pub new_episodes: u32,

    // Broadcast schedule, when one exists. This is the real answer to "did an
    // episode come out recently", so it outranks the seen-it-appear snapshot
    // above wherever it is available — see [`Bookmark::freshness`].
    /// Cached MyAnimeList id, so later lookups key on an id rather than a title
    /// the provider might rename.
    #[serde(default)]
    pub mal_id: Option<u32>,
    /// Broadcast time of the newest episode known to have aired.
    #[serde(default)]
    pub last_aired_at: Option<DateTime<Utc>>,
    /// That episode's number *in broadcast numbering*, which is per-season and so
    /// need not match the provider's — anidb numbers Slime season four 73–90 where
    /// the schedule numbers it 1–18. Kept for display only; never compared against
    /// [`Bookmark::last_episode`].
    #[serde(default)]
    pub last_aired_episode: Option<u32>,
}

/// Why a series counts as having something new.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Freshness {
    /// A broadcast date says so. Trustworthy the moment a series is followed,
    /// with no need to have watched its episode list change.
    Aired { episode: u32, at: DateTime<Utc> },
    /// No schedule exists, so this rests on kuro having watched the episode list
    /// grow between two checks.
    Seen { count: u32, at: DateTime<Utc> },
}

impl Freshness {
    pub fn at(&self) -> DateTime<Utc> {
        match self {
            Self::Aired { at, .. } | Self::Seen { at, .. } => *at,
        }
    }
}

impl Bookmark {
    pub fn new(
        provider_id: impl Into<String>,
        series_id: impl Into<String>,
        series_title: impl Into<String>,
        url: impl Into<String>,
    ) -> Self {
        Self {
            provider_id: provider_id.into(),
            series_id: series_id.into(),
            series_title: series_title.into(),
            url: url.into(),
            added_at: Utc::now(),
            last_episode: None,
            episode_count: None,
            last_checked_at: None,
            new_since: None,
            new_episodes: 0,
            mal_id: None,
            last_aired_at: None,
            last_aired_episode: None,
        }
    }

    /// How long ago the newest episode showed up, or `None` if nothing new has
    /// been seen since this series was first checked.
    pub fn new_age(&self, now: DateTime<Utc>) -> Option<chrono::Duration> {
        Some(now.signed_duration_since(self.new_since?))
    }

    /// Whether the newest episode arrived within the last `days` days.
    pub fn has_new_within(&self, days: i64, now: DateTime<Utc>) -> bool {
        self.freshness(days, now).is_some()
    }

    /// What counts as new for this series, and on what evidence.
    ///
    /// A broadcast date is preferred wherever one exists: it is a fact about the
    /// series rather than an observation about kuro's own history with it, so it
    /// works on the very first check. The snapshot is the fallback for everything
    /// with no schedule — which in practice means donghua.
    pub fn freshness(&self, days: i64, now: DateTime<Utc>) -> Option<Freshness> {
        let window = chrono::Duration::days(days.max(0));

        if let (Some(at), Some(episode)) = (self.last_aired_at, self.last_aired_episode) {
            // A clock that jumped backwards must not hide a fresh episode, so a
            // negative age still counts as recent.
            return (now.signed_duration_since(at) < window)
                .then_some(Freshness::Aired { episode, at });
        }

        let at = self.new_since?;
        (now.signed_duration_since(at) < window).then_some(Freshness::Seen {
            count: self.new_episodes,
            at,
        })
    }
}

/// What a check against a freshly-fetched episode list turned up.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CheckOutcome {
    /// First check for this series: the list becomes the baseline. Nothing is
    /// reported as new, because "new" would only mean "never looked before".
    Baseline {
        latest: Option<f32>,
    },
    /// Episodes numbered above the previous high-water mark.
    New {
        count: u32,
        latest: f32,
    },
    Unchanged,
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
        let text = serde_json::to_string_pretty(self).expect("Bookmarks are always serialisable");
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

    pub fn find_mut(&mut self, provider_id: &str, series_id: &str) -> Option<&mut Bookmark> {
        self.entries
            .iter_mut()
            .find(|b| b.provider_id == provider_id && b.series_id == series_id)
    }

    /// Fold a freshly-fetched episode list into the stored snapshot.
    ///
    /// Returns `None` when no such bookmark exists. The clock is passed in rather
    /// than read here so a check that fetches several series stamps them all with
    /// the same instant.
    pub fn record_check(
        &mut self,
        provider_id: &str,
        series_id: &str,
        episode_numbers: &[f32],
        now: DateTime<Utc>,
    ) -> Option<CheckOutcome> {
        let bookmark = self.find_mut(provider_id, series_id)?;

        let latest = episode_numbers
            .iter()
            .copied()
            .max_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

        let previous = bookmark.last_episode;
        bookmark.last_checked_at = Some(now);
        bookmark.episode_count = Some(episode_numbers.len());

        let outcome = match (previous, latest) {
            // Nothing to compare against yet: adopt the list as the baseline.
            (None, latest) => {
                bookmark.last_episode = latest;
                CheckOutcome::Baseline { latest }
            }
            (Some(prev), Some(latest)) if latest > prev => {
                let count = episode_numbers.iter().filter(|n| **n > prev).count() as u32;
                bookmark.last_episode = Some(latest);
                bookmark.new_since = Some(now);
                bookmark.new_episodes = count;
                CheckOutcome::New { count, latest }
            }
            // A shrinking list means the provider re-numbered or dropped episodes,
            // not that anything was released — keep the high-water mark.
            _ => CheckOutcome::Unchanged,
        };

        Some(outcome)
    }

    /// Store what a broadcast-schedule lookup found.
    ///
    /// `last_aired` of `None` means the series has no schedule — common for
    /// donghua — and leaves the snapshot fields as the only evidence of newness.
    pub fn record_schedule(
        &mut self,
        provider_id: &str,
        series_id: &str,
        mal_id: Option<u32>,
        last_aired: Option<(u32, DateTime<Utc>)>,
    ) -> bool {
        let Some(bookmark) = self.find_mut(provider_id, series_id) else {
            return false;
        };

        // Never clear a known id: a lookup that fails this run should not undo a
        // successful one from an earlier run.
        if mal_id.is_some() {
            bookmark.mal_id = mal_id;
        }

        if let Some((episode, at)) = last_aired {
            bookmark.last_aired_episode = Some(episode);
            bookmark.last_aired_at = Some(at);
        }
        true
    }

    /// Bookmarks whose newest episode arrived within the last `days` days,
    /// freshest first.
    pub fn recently_updated(&self, days: i64, now: DateTime<Utc>) -> Vec<&Bookmark> {
        let mut fresh: Vec<&Bookmark> = self
            .entries
            .iter()
            .filter(|b| b.has_new_within(days, now))
            .collect();
        fresh.sort_by_key(|b| std::cmp::Reverse(b.new_since));
        fresh
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
        let mk = || Bookmark::new("p", "s", "S", "https://example.com");
        assert!(b.add(mk()));
        assert!(!b.add(mk()));
        assert_eq!(b.entries.len(), 1);
        assert!(b.remove("s"));
        assert!(!b.remove("s"));
    }

    fn bookmarked() -> Bookmarks {
        let mut b = Bookmarks::default();
        b.add(Bookmark::new("p", "s", "S", "https://example.com"));
        b
    }

    #[test]
    fn the_first_check_is_a_baseline_not_a_release() {
        let mut b = bookmarked();
        let outcome = b.record_check("p", "s", &[1.0, 2.0, 3.0], Utc::now());

        assert_eq!(outcome, Some(CheckOutcome::Baseline { latest: Some(3.0) }));
        let entry = &b.entries[0];
        assert_eq!(entry.last_episode, Some(3.0));
        assert_eq!(entry.episode_count, Some(3));
        assert!(entry.new_since.is_none());
        assert!(!entry.has_new_within(DEFAULT_NEW_WINDOW_DAYS, Utc::now()));
    }

    #[test]
    fn episodes_past_the_high_water_mark_count_as_new() {
        let mut b = bookmarked();
        b.record_check("p", "s", &[1.0, 2.0], Utc::now());
        let now = Utc::now();
        let outcome = b.record_check("p", "s", &[1.0, 2.0, 3.0, 4.0], now);

        assert_eq!(
            outcome,
            Some(CheckOutcome::New {
                count: 2,
                latest: 4.0
            })
        );
        let entry = &b.entries[0];
        assert_eq!(entry.new_episodes, 2);
        assert!(entry.has_new_within(DEFAULT_NEW_WINDOW_DAYS, now));
    }

    #[test]
    fn an_unchanged_list_leaves_the_snapshot_alone() {
        let mut b = bookmarked();
        b.record_check("p", "s", &[1.0, 2.0], Utc::now());
        let stamped = b.entries[0].new_since;
        assert_eq!(
            b.record_check("p", "s", &[1.0, 2.0], Utc::now()),
            Some(CheckOutcome::Unchanged)
        );
        assert_eq!(b.entries[0].last_episode, Some(2.0));
        assert_eq!(b.entries[0].new_since, stamped);
    }

    #[test]
    fn a_shrinking_list_is_not_a_release() {
        let mut b = bookmarked();
        b.record_check("p", "s", &[1.0, 2.0, 3.0], Utc::now());
        assert_eq!(
            b.record_check("p", "s", &[1.0], Utc::now()),
            Some(CheckOutcome::Unchanged)
        );
        assert_eq!(b.entries[0].last_episode, Some(3.0));
    }

    #[test]
    fn half_episodes_register_as_new() {
        let mut b = bookmarked();
        b.record_check("p", "s", &[11.0, 12.0], Utc::now());
        assert_eq!(
            b.record_check("p", "s", &[11.0, 12.0, 12.5], Utc::now()),
            Some(CheckOutcome::New {
                count: 1,
                latest: 12.5
            })
        );
    }

    #[test]
    fn checking_an_unknown_series_reports_nothing() {
        let mut b = bookmarked();
        assert_eq!(b.record_check("p", "other", &[1.0], Utc::now()), None);
    }

    #[test]
    fn news_expires_once_the_window_passes() {
        let mut b = bookmarked();
        b.record_check("p", "s", &[1.0], Utc::now());
        let seen_at = Utc::now();
        b.record_check("p", "s", &[1.0, 2.0], seen_at);

        let within = seen_at + chrono::Duration::days(6);
        let past = seen_at + chrono::Duration::days(8);
        assert_eq!(b.recently_updated(DEFAULT_NEW_WINDOW_DAYS, within).len(), 1);
        assert!(b.recently_updated(DEFAULT_NEW_WINDOW_DAYS, past).is_empty());
    }

    #[test]
    fn a_series_with_no_episodes_yet_still_baselines() {
        let mut b = bookmarked();
        assert_eq!(
            b.record_check("p", "s", &[], Utc::now()),
            Some(CheckOutcome::Baseline { latest: None })
        );
        // The next check is the one that can find a first episode.
        assert_eq!(
            b.record_check("p", "s", &[1.0], Utc::now()),
            Some(CheckOutcome::Baseline { latest: Some(1.0) })
        );
    }

    #[test]
    fn a_broadcast_date_makes_a_series_fresh_without_any_snapshot_history() {
        let mut b = bookmarked();
        // Followed today, checked once: the snapshot alone knows nothing yet.
        b.record_check("p", "s", &[73.0, 90.0], Utc::now());
        assert!(b.entries[0].new_since.is_none());

        let aired = Utc::now() - chrono::Duration::days(4);
        assert!(b.record_schedule("p", "s", Some(59970), Some((18, aired))));

        // This is the case that used to report nothing at all.
        assert_eq!(
            b.entries[0].freshness(DEFAULT_NEW_WINDOW_DAYS, Utc::now()),
            Some(Freshness::Aired {
                episode: 18,
                at: aired
            })
        );
    }

    #[test]
    fn a_broadcast_date_outranks_the_snapshot() {
        let mut b = bookmarked();
        b.record_check("p", "s", &[1.0], Utc::now());
        // kuro saw an episode appear just now...
        b.record_check("p", "s", &[1.0, 2.0], Utc::now());
        // ...but the schedule says it actually aired a month ago.
        let aired = Utc::now() - chrono::Duration::days(30);
        b.record_schedule("p", "s", None, Some((2, aired)));

        assert_eq!(
            b.entries[0].freshness(DEFAULT_NEW_WINDOW_DAYS, Utc::now()),
            None
        );
    }

    #[test]
    fn a_series_with_no_schedule_still_falls_back_to_the_snapshot() {
        let mut b = bookmarked();
        b.record_check("p", "s", &[1.0], Utc::now());
        b.record_check("p", "s", &[1.0, 2.0], Utc::now());
        // A donghua: an id may resolve, but no broadcast dates exist.
        b.record_schedule("p", "s", Some(123), None);

        assert!(matches!(
            b.entries[0].freshness(DEFAULT_NEW_WINDOW_DAYS, Utc::now()),
            Some(Freshness::Seen { count: 1, .. })
        ));
    }

    #[test]
    fn a_failed_lookup_never_clears_a_known_id() {
        let mut b = bookmarked();
        b.record_schedule("p", "s", Some(59970), None);
        b.record_schedule("p", "s", None, None);
        assert_eq!(b.entries[0].mal_id, Some(59970));
    }

    #[test]
    fn bookmarks_written_before_release_tracking_still_load() {
        let json = r#"{"entries":[{"provider_id":"p","series_id":"s","series_title":"S",
            "url":"https://example.com","added_at":"2024-01-01T00:00:00Z"}]}"#;
        let b: Bookmarks = serde_json::from_str(json).expect("legacy bookmarks parse");
        assert_eq!(b.entries.len(), 1);
        assert!(b.entries[0].last_episode.is_none());
        assert_eq!(b.entries[0].new_episodes, 0);
    }
}
