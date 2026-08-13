//! Opening/ending skip times.
//!
//! Two hops: a title is resolved to a MyAnimeList id via AniList, then that id is
//! looked up in [AniSkip], which is crowd-sourced.
//!
//! AniList rather than Jikan for the first hop — Jikan proxies MyAnimeList and
//! returned `504` for every multi-word query during testing, while AniList answered
//! reliably and exposes `idMal` directly.
//!
//! **Coverage is thin for donghua.** AniSkip is contributed by viewers and skews
//! heavily toward mainstream Japanese anime; most donghua episodes have no entry.
//! Every failure here is non-fatal — playback proceeds unskipped.
//!
//! [AniSkip]: https://api.aniskip.com/

use crate::fetch::FetchCtx;
use serde::Deserialize;
use tracing::debug;
use url::Url;

const ANILIST_API: &str = "https://graphql.anilist.co";
const ANISKIP_API: &str = "https://api.aniskip.com/v2/skip-times";

/// A half-open time range to jump over, in seconds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Interval {
    pub start: f64,
    pub end: f64,
}

impl Interval {
    /// Guards against malformed API data producing a backwards or zero-length seek.
    fn is_usable(&self) -> bool {
        self.end > self.start && self.end > 0.0
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct SkipTimes {
    pub op: Option<Interval>,
    pub ed: Option<Interval>,
}

impl SkipTimes {
    pub fn is_empty(&self) -> bool {
        self.op.is_none() && self.ed.is_none()
    }

    /// Human-readable summary, e.g. `"opening 0:00–2:08"`.
    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        if let Some(op) = self.op {
            parts.push(format!("opening {}", range_label(op)));
        }
        if let Some(ed) = self.ed {
            parts.push(format!("ending {}", range_label(ed)));
        }
        if parts.is_empty() {
            "nothing to skip".to_string()
        } else {
            parts.join(", ")
        }
    }
}

fn range_label(i: Interval) -> String {
    format!("{}–{}", clock(i.start), clock(i.end))
}

fn clock(secs: f64) -> String {
    let secs = secs.max(0.0) as u64;
    format!("{}:{:02}", secs / 60, secs % 60)
}

/// Strip decorations that hurt a title search: years and bracketed alternate names.
///
/// The season marker is deliberately **kept**. Each season is a separate AniList
/// entry with its own MyAnimeList id (`Swallowed Star Season 4` is 56524, while
/// `Swallowed Star` is 44218), and skip times differ per season — dropping it would
/// silently apply season one's opening to every later season.
pub fn clean_title(title: &str) -> String {
    let mut out = String::with_capacity(title.len());
    let mut depth = 0usize;

    for c in title.chars() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }

    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Drop a trailing season marker, for use as a fallback search only.
///
/// Returns `None` when there is nothing to strip, so the caller can skip a second
/// identical query.
pub fn without_season(title: &str) -> Option<String> {
    let lowered = title.to_ascii_lowercase();

    for marker in [" season ", " part ", " cour "] {
        if let Some(idx) = lowered.rfind(marker) {
            let tail = lowered[idx + marker.len()..].trim();
            // Only "<marker> <digits>" at the end is a season suffix.
            if !tail.is_empty() && tail.chars().all(|c| c.is_ascii_digit()) {
                let stripped = title[..idx].trim().to_string();
                return (!stripped.is_empty()).then_some(stripped);
            }
        }
    }
    None
}

#[derive(Debug, Deserialize)]
struct AniListResponse {
    data: Option<AniListData>,
}

#[derive(Debug, Deserialize)]
struct AniListData {
    #[serde(rename = "Media")]
    media: Option<AniListMedia>,
}

#[derive(Debug, Deserialize)]
struct AniListMedia {
    #[serde(rename = "idMal")]
    id_mal: Option<u32>,
}

async fn search_anilist(ctx: &FetchCtx, search: &str) -> Option<u32> {
    let url = Url::parse(ANILIST_API).ok()?;

    let body = serde_json::json!({
        "query": "query($s:String){Media(search:$s,type:ANIME){idMal}}",
        "variables": { "s": search },
    });

    let text = ctx.post_json(&url, &body).await.ok()?;
    let parsed: AniListResponse = serde_json::from_str(&text).ok()?;
    parsed.data?.media?.id_mal
}

/// Resolve a series title to a MyAnimeList id, or `None` if there is no match.
///
/// The full title is tried first so a specific season resolves to its own entry;
/// only if that finds nothing is the season suffix dropped, which at least lands on
/// the right franchise.
pub async fn lookup_mal_id(ctx: &FetchCtx, title: &str) -> Option<u32> {
    let cleaned = clean_title(title);

    if let Some(id) = search_anilist(ctx, &cleaned).await {
        debug!(title = cleaned, mal_id = id, "resolved MyAnimeList id");
        return Some(id);
    }

    let fallback = without_season(&cleaned)?;
    let id = search_anilist(ctx, &fallback).await?;
    debug!(
        title = fallback,
        mal_id = id,
        "resolved MyAnimeList id without season suffix"
    );
    Some(id)
}

#[derive(Debug, Deserialize)]
struct AniSkipResponse {
    found: bool,
    #[serde(default)]
    results: Vec<AniSkipResult>,
}

#[derive(Debug, Deserialize)]
struct AniSkipResult {
    interval: AniSkipInterval,
    #[serde(rename = "skipType")]
    skip_type: String,
}

#[derive(Debug, Deserialize)]
struct AniSkipInterval {
    #[serde(rename = "startTime")]
    start_time: f64,
    #[serde(rename = "endTime")]
    end_time: f64,
}

/// Fetch skip intervals for one episode. `None` means AniSkip has no entry.
pub async fn fetch_skip_times(ctx: &FetchCtx, mal_id: u32, episode: f32) -> Option<SkipTimes> {
    // AniSkip keys on whole episode numbers; a `.5` special has no entry.
    let episode = episode.round() as u32;

    let url = Url::parse(&format!(
        "{ANISKIP_API}/{mal_id}/{episode}?types=op&types=ed&episodeLength=0"
    ))
    .ok()?;

    let text = ctx.get_text(&url, None).await.ok()?;
    let parsed: AniSkipResponse = serde_json::from_str(&text).ok()?;

    if !parsed.found {
        debug!(mal_id, episode, "no skip data");
        return None;
    }

    let mut times = SkipTimes::default();
    for result in parsed.results {
        let interval = Interval {
            start: result.interval.start_time,
            end: result.interval.end_time,
        };
        if !interval.is_usable() {
            continue;
        }
        match result.skip_type.as_str() {
            "op" => times.op = Some(interval),
            "ed" => times.ed = Some(interval),
            _ => {}
        }
    }

    (!times.is_empty()).then_some(times)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_lose_years_and_bracketed_alternates() {
        assert_eq!(clean_title("Martial Peak (2024)"), "Martial Peak");
        assert_eq!(
            clean_title("Martial Universe [Wu Dong Qian Kun]"),
            "Martial Universe"
        );
        assert_eq!(clean_title("Wu Ni [Martial Inverse] Anime (2024)"), "Wu Ni Anime");
    }

    #[test]
    fn cleaning_keeps_the_season_marker() {
        // Each season is its own AniList entry with its own MyAnimeList id, so the
        // season must survive into the primary search.
        assert_eq!(
            clean_title("Swallowed Star Season 4 [2025]"),
            "Swallowed Star Season 4"
        );
    }

    #[test]
    fn season_suffix_is_strippable_as_a_fallback() {
        assert_eq!(
            without_season("Swallowed Star Season 4").as_deref(),
            Some("Swallowed Star")
        );
        assert_eq!(
            without_season("Perfect World Part 3").as_deref(),
            Some("Perfect World")
        );
    }

    #[test]
    fn a_season_word_inside_the_name_is_not_stripped() {
        // Only a trailing "<marker> <digits>" is a season suffix.
        assert_eq!(without_season("Season of Blossom"), None);
        assert_eq!(without_season("Martial Season Two"), None);
        // Nothing to strip means no pointless second query.
        assert_eq!(without_season("Martial God Asura"), None);
    }

    #[test]
    fn cleaning_collapses_leftover_whitespace() {
        assert_eq!(clean_title("  A   B  (2025) "), "A B");
    }

    #[test]
    fn non_ascii_titles_survive_cleaning() {
        assert_eq!(clean_title("斗罗大陆 (2023)"), "斗罗大陆");
    }

    #[test]
    fn backwards_or_empty_intervals_are_rejected() {
        assert!(!Interval { start: 100.0, end: 50.0 }.is_usable());
        assert!(!Interval { start: 10.0, end: 10.0 }.is_usable());
        assert!(Interval { start: 0.0, end: 90.0 }.is_usable());
    }

    #[test]
    fn describe_reads_as_clock_times() {
        let times = SkipTimes {
            op: Some(Interval { start: 0.0, end: 128.0 }),
            ed: None,
        };
        assert_eq!(times.describe(), "opening 0:00–2:08");
        assert!(!times.is_empty());
        assert_eq!(SkipTimes::default().describe(), "nothing to skip");
    }
}
