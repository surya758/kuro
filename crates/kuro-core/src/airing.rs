//! Broadcast schedules: when an episode actually aired.
//!
//! Streaming providers publish no air date — an episode list is numbers and links,
//! nothing more. So "did something come out this week?" cannot be answered from a
//! provider at all, only inferred from watching its list change over time.
//!
//! AniList knows the real answer. It exposes an `airingSchedule` per series, giving
//! a wall-clock timestamp for every episode, which turns that inference into a fact.
//! The same API already resolves MyAnimeList ids for [`crate::skip`], so this is a
//! second use of a hop kuro is known to work with — and the reason it is preferred
//! over Jikan here is recorded there: Jikan returned `504` for multi-word queries.
//!
//! **Coverage is anime-shaped.** Japanese TV anime is scheduled thoroughly. Donghua
//! usually resolve to an AniList entry with an empty schedule — verified against
//! *Against the Gods*, which matches `Nitian Xie Shen` and carries zero nodes. Every
//! lookup here is therefore best-effort, and callers must keep a fallback.

use crate::fetch::FetchCtx;
use crate::skip::{clean_title, without_season};
use serde::Deserialize;
use tracing::debug;
use url::Url;

const ANILIST_API: &str = "https://graphql.anilist.co";

/// One episode's broadcast slot, as Unix seconds.
///
/// Seconds rather than a `DateTime` so this crate stays free of a date library;
/// the store and CLI layers convert when they need calendar arithmetic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AiredEpisode {
    pub episode: u32,
    pub airing_at: i64,
}

/// A series' full broadcast schedule, past and future.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Schedule {
    pub mal_id: Option<u32>,
    /// Every slot AniList lists, in episode order. Includes episodes that have not
    /// aired yet, so callers must compare against the clock.
    pub episodes: Vec<AiredEpisode>,
}

impl Schedule {
    pub fn is_empty(&self) -> bool {
        self.episodes.is_empty()
    }

    /// The most recent episode whose slot has passed.
    pub fn last_aired(&self, now: i64) -> Option<AiredEpisode> {
        self.episodes
            .iter()
            .filter(|e| e.airing_at <= now)
            .max_by_key(|e| e.airing_at)
            .copied()
    }

    /// Episodes broadcast within the last `window` seconds, oldest first.
    pub fn aired_within(&self, now: i64, window_secs: i64) -> Vec<AiredEpisode> {
        let cutoff = now.saturating_sub(window_secs.max(0));
        let mut recent: Vec<AiredEpisode> = self
            .episodes
            .iter()
            .filter(|e| e.airing_at <= now && e.airing_at > cutoff)
            .copied()
            .collect();
        recent.sort_by_key(|e| e.airing_at);
        recent
    }

    /// The next episode due, if one is scheduled.
    pub fn next_airing(&self, now: i64) -> Option<AiredEpisode> {
        self.episodes
            .iter()
            .filter(|e| e.airing_at > now)
            .min_by_key(|e| e.airing_at)
            .copied()
    }
}

#[derive(Debug, Deserialize)]
struct Response {
    data: Option<Data>,
}

#[derive(Debug, Deserialize)]
struct Data {
    #[serde(rename = "Media")]
    media: Option<Media>,
}

#[derive(Debug, Deserialize)]
struct Media {
    #[serde(rename = "idMal")]
    id_mal: Option<u32>,
    #[serde(rename = "airingSchedule")]
    airing_schedule: Option<Connection>,
}

#[derive(Debug, Deserialize)]
struct Connection {
    #[serde(default)]
    nodes: Vec<Node>,
}

#[derive(Debug, Deserialize)]
struct Node {
    episode: u32,
    #[serde(rename = "airingAt")]
    airing_at: i64,
}

/// `notYetAired` is deliberately not passed: AniList returns future slots either
/// way, so filtering is the caller's job and asking for it only invites a silent
/// change of meaning.
const QUERY_BY_TITLE: &str =
    "query($s:String){Media(search:$s,type:ANIME){idMal airingSchedule{nodes{episode airingAt}}}}";

const QUERY_BY_MAL: &str =
    "query($i:Int){Media(idMal:$i,type:ANIME){idMal airingSchedule{nodes{episode airingAt}}}}";

async fn query(ctx: &FetchCtx, body: serde_json::Value) -> Option<Schedule> {
    let url = Url::parse(ANILIST_API).ok()?;
    let text = ctx.post_json(&url, &body).await.ok()?;
    let parsed: Response = serde_json::from_str(&text).ok()?;
    let media = parsed.data?.media?;

    Some(Schedule {
        mal_id: media.id_mal,
        episodes: media
            .airing_schedule
            .map(|c| {
                c.nodes
                    .into_iter()
                    .map(|n| AiredEpisode {
                        episode: n.episode,
                        airing_at: n.airing_at,
                    })
                    .collect()
            })
            .unwrap_or_default(),
    })
}

/// Look up a schedule by MyAnimeList id.
///
/// Preferred once an id is known: a title search can drift onto a different season
/// as providers rename things, while an id cannot.
pub async fn lookup_by_mal_id(ctx: &FetchCtx, mal_id: u32) -> Option<Schedule> {
    let schedule = query(
        ctx,
        serde_json::json!({ "query": QUERY_BY_MAL, "variables": { "i": mal_id } }),
    )
    .await?;

    debug!(mal_id, slots = schedule.episodes.len(), "fetched schedule");
    Some(schedule)
}

/// Look up a schedule by series title.
///
/// The season marker is kept for the first attempt, because each season is its own
/// AniList entry with its own schedule — falling back to the franchise would report
/// season one's broadcast dates for a season four bookmark.
pub async fn lookup_by_title(ctx: &FetchCtx, title: &str) -> Option<Schedule> {
    let cleaned = clean_title(title);

    if let Some(schedule) = query(
        ctx,
        serde_json::json!({ "query": QUERY_BY_TITLE, "variables": { "s": cleaned } }),
    )
    .await
    .filter(|s| !s.is_empty())
    {
        debug!(
            title = cleaned,
            slots = schedule.episodes.len(),
            "fetched schedule"
        );
        return Some(schedule);
    }

    // Only worth a second call when there is actually a suffix to drop.
    let fallback = without_season(&cleaned)?;
    let schedule = query(
        ctx,
        serde_json::json!({ "query": QUERY_BY_TITLE, "variables": { "s": fallback } }),
    )
    .await
    .filter(|s| !s.is_empty())?;

    debug!(
        title = fallback,
        slots = schedule.episodes.len(),
        "fetched schedule without season suffix"
    );
    Some(schedule)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: i64 = 86_400;

    fn schedule() -> Schedule {
        Schedule {
            mal_id: Some(1),
            episodes: vec![
                AiredEpisode {
                    episode: 16,
                    airing_at: 100 * DAY,
                },
                AiredEpisode {
                    episode: 17,
                    airing_at: 107 * DAY,
                },
                AiredEpisode {
                    episode: 18,
                    airing_at: 114 * DAY,
                },
                // Scheduled, not yet broadcast.
                AiredEpisode {
                    episode: 19,
                    airing_at: 121 * DAY,
                },
            ],
        }
    }

    #[test]
    fn future_slots_are_not_treated_as_aired() {
        let now = 118 * DAY;
        assert_eq!(schedule().last_aired(now).map(|e| e.episode), Some(18));
        assert_eq!(schedule().next_airing(now).map(|e| e.episode), Some(19));
    }

    #[test]
    fn the_window_selects_only_recent_broadcasts() {
        let now = 118 * DAY;
        let recent: Vec<u32> = schedule()
            .aired_within(now, 7 * DAY)
            .iter()
            .map(|e| e.episode)
            .collect();
        assert_eq!(recent, vec![18]);
    }

    #[test]
    fn a_wider_window_reaches_further_back() {
        let now = 118 * DAY;
        let recent: Vec<u32> = schedule()
            .aired_within(now, 30 * DAY)
            .iter()
            .map(|e| e.episode)
            .collect();
        assert_eq!(recent, vec![16, 17, 18]);
    }

    #[test]
    fn nothing_aired_in_a_zero_length_window() {
        assert!(schedule().aired_within(118 * DAY, 0).is_empty());
    }

    #[test]
    fn a_series_with_no_schedule_reports_nothing() {
        let empty = Schedule::default();
        assert!(empty.is_empty());
        assert_eq!(empty.last_aired(118 * DAY), None);
        assert!(empty.aired_within(118 * DAY, 7 * DAY).is_empty());
    }
}
