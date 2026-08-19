//! Anime Cube.
//!
//! The one provider that does not fit a selector TOML. Everything about it is a
//! step removed from markup: the catalogue and episode list arrive as React Server
//! Component payloads rather than HTML, and the playable id sits behind a two-call
//! API where the first call returns a rotating token the second one requires.
//!
//! Worth the exception because of what it serves — 2160p60 and real subtitle
//! tracks, where the declarative providers top out at 1080p with subtitles burned
//! into the picture.
//!
//! Verified chain:
//!   /                                          RSC → `fallbackData` (whole catalogue)
//!   /anime/{slug}                              RSC → primaryTabs[].seasons[].episodes[]
//!   /api/anime-sources-versions                bySeason[slug][tab][season] → token
//!   /api/anime/{slug}/episode/{id}/sources?v=… → privateId
//!   dailymotion.com/video/{privateId}          → resolved by the player

use crate::rsc;
use async_trait::async_trait;
use kuro_core::cache::ttl;
use kuro_core::{
    Episode, FetchCtx, Mirror, Provider, ProviderError, ProviderId, Series, SeriesDetails,
    SeriesStatus,
};
use serde_json::Value;
use tracing::debug;
use url::Url;

const BASE: &str = "https://animecube.live";

pub struct AnimeCubeProvider {
    id: ProviderId,
    base_url: Url,
}

impl Default for AnimeCubeProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl AnimeCubeProvider {
    pub fn new() -> Self {
        Self {
            id: ProviderId::new("animecube".to_string()),
            base_url: Url::parse(BASE).expect("BASE is a valid URL"),
        }
    }

    async fn get(
        &self,
        ctx: &FetchCtx,
        url: &Url,
        ttl: std::time::Duration,
    ) -> Result<String, ProviderError> {
        ctx.get_cached(url, Some(BASE), None, ttl).await
    }

    fn join(&self, path: &str) -> Result<Url, ProviderError> {
        self.base_url
            .join(path)
            .map_err(|e| ProviderError::Config(format!("bad path `{path}`: {e}")))
    }

    /// The whole catalogue, which every page prefetches.
    ///
    /// The site ignores `q` server-side and filters in the browser, so there is no
    /// per-query request to make — one cached fetch answers every search.
    ///
    /// Read from the home page rather than `/search`: both carry the same records,
    /// and `/search` has already been removed once, mid-development.
    async fn catalogue(&self, ctx: &FetchCtx) -> Result<Vec<Value>, ProviderError> {
        let url = self.join("/")?;
        let body = self.get(ctx, &url, ttl::SEARCH).await?;
        let flight = rsc::reassemble(&body);

        let value = rsc::value_after_key(&flight, "fallbackData")
            .ok_or_else(|| ProviderError::parse("fallbackData", "catalogue"))?;

        match value {
            Value::Array(items) => Ok(items),
            _ => Err(ProviderError::parse("fallbackData", "catalogue")),
        }
    }

    fn to_series(&self, item: &Value) -> Option<Series> {
        let slug = item.get("slug")?.as_str()?;
        let title = item.get("title")?.as_str()?;

        Some(Series {
            provider_id: self.id.clone(),
            id: slug.to_string(),
            title: title.to_string(),
            url: self.join(&format!("/anime/{slug}")).ok()?,
            poster: item
                .get("coverImage")
                .and_then(Value::as_str)
                .and_then(|p| Url::parse(p).ok()),
            year: item.get("year").and_then(Value::as_u64).map(|y| y as u16),
            synopsis: None,
            genres: item
                .get("genres")
                .and_then(Value::as_array)
                .map(|g| {
                    g.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
            status: match item.get("status").and_then(Value::as_str) {
                Some("ongoing") => SeriesStatus::Ongoing,
                Some(s) if s.contains("completed") => SeriesStatus::Completed,
                _ => SeriesStatus::Unknown,
            },
            total_episodes: item
                .get("totalEpisodes")
                .and_then(Value::as_u64)
                .map(|n| n as u32),
        })
    }
}

/// Whether a catalogue entry answers the query.
///
/// Aliases carry the romanised and alternate titles viewers actually type, so a
/// search for "ni tian xie shen" has to reach "Against the Gods".
fn matches(item: &Value, needle: &str) -> bool {
    let hit = |v: Option<&Value>| {
        v.and_then(Value::as_str)
            .is_some_and(|s| s.to_lowercase().contains(needle))
    };

    if hit(item.get("title")) {
        return true;
    }
    item.get("aliases")
        .and_then(Value::as_array)
        .is_some_and(|a| a.iter().any(|v| hit(Some(v))))
}

/// One season's episodes plus the identifiers a source lookup needs.
pub(crate) struct SeasonRef {
    tab_id: String,
    season_id: String,
    /// The site's own season number, used for labelling.
    number: u32,
    episodes: Vec<Value>,
}

/// Every season that actually has episodes, in the site's own order.
///
/// A season with an empty list is skipped rather than kept as a gap: the site
/// announces seasons before filling them in, and an empty one has nothing to play.
pub(crate) fn populated_seasons(tabs: &[Value]) -> Vec<SeasonRef> {
    let mut out = Vec::new();

    for tab in tabs {
        let Some(tab_id) = tab.get("id").and_then(Value::as_str) else {
            continue;
        };
        let Some(seasons) = tab.get("seasons").and_then(Value::as_array) else {
            continue;
        };
        for (index, season) in seasons.iter().enumerate() {
            let Some(season_id) = season.get("id").and_then(Value::as_str) else {
                continue;
            };
            let episodes = season
                .get("episodes")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            if episodes.is_empty() {
                continue;
            }
            out.push(SeasonRef {
                tab_id: tab_id.to_string(),
                season_id: season_id.to_string(),
                number: season
                    .get("number")
                    .and_then(Value::as_u64)
                    .unwrap_or(index as u64 + 1) as u32,
                episodes,
            });
        }
    }

    out.sort_by_key(|s| s.number);
    out
}

/// Flatten the site's per-season numbering into one continuous run.
///
/// kuro models a series as a single flat episode list, while this site restarts
/// numbering in every season — two "episode 1" cannot both live in that list. Each
/// season is therefore offset past the one before it, so *Ling Cage* season two
/// episode one becomes episode 17 rather than shadowing season one's opener.
///
/// A single-season series is offset by zero and keeps the site's numbers exactly,
/// which is the overwhelmingly common case.
///
/// The offset walks the highest **raw** number in each season, not the episode
/// count: season one carries a `6.5` and an `SP` numbered 16 across 16 entries, and
/// counting entries instead would overlap the next season onto it.
pub(crate) fn season_offsets(seasons: &[SeasonRef]) -> Vec<f32> {
    let mut offsets = Vec::with_capacity(seasons.len());
    let mut running = 0.0f32;

    for season in seasons {
        offsets.push(running);
        let highest = season
            .episodes
            .iter()
            .filter_map(|e| e.get("number").and_then(Value::as_f64))
            .fold(0.0f32, |acc, n| acc.max(n as f32));
        running += highest;
    }
    offsets
}

/// Player URL for a source entry.
///
/// The canonical page rather than the site's own `geo.dailymotion.com/player.html`
/// embed. Both resolve for the extractor, but these sources publish no muxed
/// rendition, so the player has to re-resolve the URL itself — and its extractor
/// hook only recognises the canonical form. Handing over the embed leaves it
/// fetching the page and finding an image.
fn embed_for(platform: &str, video: &str) -> Option<Url> {
    let raw = match platform {
        "dailymotion" => format!("https://www.dailymotion.com/video/{video}"),
        "rumble" => format!("https://rumble.com/embed/{video}/"),
        "youtube" => format!("https://www.youtube.com/watch?v={video}"),
        _ => return None,
    };
    Url::parse(&raw).ok()
}

#[async_trait]
impl Provider for AnimeCubeProvider {
    fn id(&self) -> ProviderId {
        self.id.clone()
    }

    fn display_name(&self) -> &str {
        "Anime Cube"
    }

    fn base_url(&self) -> &Url {
        &self.base_url
    }

    async fn health_check(&self, ctx: &FetchCtx) -> Result<(), ProviderError> {
        ctx.get_text(&self.base_url, Some(BASE)).await.map(|_| ())
    }

    async fn search(&self, ctx: &FetchCtx, query: &str) -> Result<Vec<Series>, ProviderError> {
        let needle = query.trim().to_lowercase();
        let items = self.catalogue(ctx).await?;

        // An empty catalogue means the payload shape moved; no matches within a
        // populated one is just a miss.
        if items.is_empty() {
            return Err(ProviderError::parse("fallbackData", "catalogue"));
        }

        Ok(items
            .iter()
            .filter(|item| matches(item, &needle))
            .filter_map(|item| self.to_series(item))
            .collect())
    }

    async fn series_details(
        &self,
        ctx: &FetchCtx,
        series: &Series,
    ) -> Result<SeriesDetails, ProviderError> {
        let items = self.catalogue(ctx).await?;
        let found = items
            .iter()
            .find(|i| i.get("slug").and_then(Value::as_str) == Some(series.id.as_str()));

        Ok(match found.and_then(|i| self.to_series(i)) {
            Some(s) => SeriesDetails {
                synopsis: None,
                genres: s.genres,
                status: s.status,
                total_episodes: s.total_episodes,
            },
            None => SeriesDetails::default(),
        })
    }

    async fn episodes(
        &self,
        ctx: &FetchCtx,
        series: &Series,
    ) -> Result<Vec<Episode>, ProviderError> {
        let body = self.get(ctx, &series.url, ttl::EPISODES).await?;
        let flight = rsc::reassemble(&body);

        let tabs = rsc::value_after_key(&flight, "primaryTabs")
            .and_then(|v| v.as_array().cloned())
            .ok_or_else(|| ProviderError::parse("primaryTabs", "episode list"))?;

        // A series the site has announced but not filled in yet is a normal state,
        // not a broken scraper — the tabs themselves parsed fine.
        let seasons = populated_seasons(&tabs);
        if seasons.is_empty() {
            return Ok(Vec::new());
        }
        let offsets = season_offsets(&seasons);

        let mut out = Vec::new();
        for (season, offset) in seasons.iter().zip(offsets) {
            for entry in &season.episodes {
                let (Some(id), Some(number)) = (
                    entry.get("id").and_then(Value::as_str),
                    entry.get("number").and_then(Value::as_f64),
                ) else {
                    continue;
                };

                // The source lookup needs all three identifiers, and each season
                // has its own, so they ride along in the episode's URL rather than
                // being recomputed from a series-wide guess later.
                let path = format!(
                    "/api/anime/{}/episode/{}/sources?primaryTabId={}&seasonId={}",
                    series.id, id, season.tab_id, season.season_id
                );
                let Ok(url) = self.join(&path) else { continue };

                // The site labels its own episodes "Season 1 Episode 1", which is
                // what keeps a renumbered list readable — kuro's episode 17 says
                // plainly that it is season two's first. Synthesised only when the
                // site left the label empty.
                let title = entry
                    .get("title")
                    .and_then(Value::as_str)
                    .filter(|t| !t.is_empty())
                    .map(str::to_string)
                    .or_else(|| {
                        let display = entry
                            .get("numberDisplay")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                            .unwrap_or_else(|| number.to_string());
                        Some(format!("Season {} Episode {display}", season.number))
                    });

                out.push(Episode {
                    series_id: series.id.clone(),
                    number: number as f32 + offset,
                    title,
                    url,
                });
            }
        }

        out.sort_by(|a, b| {
            a.number
                .partial_cmp(&b.number)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        Ok(out)
    }

    async fn mirrors(
        &self,
        ctx: &FetchCtx,
        episode: &Episode,
    ) -> Result<Vec<Mirror>, ProviderError> {
        let mut query = episode.url.query_pairs();
        let tab_id = query
            .find(|(k, _)| k == "primaryTabId")
            .map(|(_, v)| v.to_string())
            .ok_or_else(|| ProviderError::Config("episode URL lost primaryTabId".into()))?;
        let season_id = episode
            .url
            .query_pairs()
            .find(|(k, _)| k == "seasonId")
            .map(|(_, v)| v.to_string())
            .ok_or_else(|| ProviderError::Config("episode URL lost seasonId".into()))?;

        // The sources call rejects a stale or missing token, so the version map is
        // fetched live rather than cached for long.
        let versions_url = self.join("/api/anime-sources-versions")?;
        let versions: Value =
            serde_json::from_str(&self.get(ctx, &versions_url, ttl::MIRRORS).await?)
                .map_err(|e| ProviderError::parse(format!("<json: {e}>"), "source versions"))?;

        let token = versions
            .get("bySeason")
            .and_then(|b| b.get(&episode.series_id))
            .and_then(|t| t.get(&tab_id))
            .and_then(|s| s.get(&season_id))
            .and_then(Value::as_str)
            .ok_or_else(|| ProviderError::parse("bySeason", "source versions"))?;

        let mut url = episode.url.clone();
        url.query_pairs_mut().append_pair("v", token);
        debug!(episode = episode.number, "resolving sources");

        let body = self.get(ctx, &url, ttl::MIRRORS).await?;
        let payload: Value = serde_json::from_str(&body)
            .map_err(|e| ProviderError::parse(format!("<json: {e}>"), "episode sources"))?;

        let sources = payload
            .get("sources")
            .and_then(Value::as_array)
            .ok_or_else(|| ProviderError::parse("sources", "episode sources"))?;

        let mut out = Vec::new();
        for (index, source) in sources.iter().enumerate() {
            let platform = source
                .get("platform")
                .and_then(Value::as_str)
                .unwrap_or_default();

            // `privateId` is the id the extractor accepts; the site's own
            // `videoId` addresses the same video but resolves without the 4K rungs.
            let Some(video) = source
                .get("privateId")
                .or_else(|| source.get("videoId"))
                .and_then(Value::as_str)
            else {
                continue;
            };
            let Some(embed) = embed_for(platform, video) else {
                continue;
            };

            let quality = source.get("quality").and_then(Value::as_str).unwrap_or("");
            let good_sub = source
                .get("goodSub")
                .and_then(Value::as_bool)
                .unwrap_or(false);

            let mut label = platform.to_string();
            if !quality.is_empty() {
                label.push_str(&format!(" {quality}"));
            }
            if good_sub {
                label.push_str(" · good sub");
            }

            out.push(Mirror {
                index: index as u8,
                label,
                page_url: embed.clone(),
                // Already a player URL, so there is nothing further to resolve.
                embed_url: Some(embed),
            });
        }

        if out.is_empty() {
            return Err(ProviderError::parse("sources", "episode sources"));
        }
        Ok(out)
    }

    async fn embed_url(&self, _ctx: &FetchCtx, mirror: &Mirror) -> Result<Url, ProviderError> {
        mirror
            .embed_url
            .clone()
            .ok_or_else(|| ProviderError::parse("embed_url", "video embed"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(json: &str) -> Value {
        serde_json::from_str(json).expect("test json")
    }

    #[test]
    fn a_title_match_is_found() {
        let v = item(r#"{"title":"Against the Gods","aliases":[]}"#);
        assert!(matches(&v, "against"));
        assert!(!matches(&v, "perfect world"));
    }

    #[test]
    fn an_alias_match_is_found() {
        // The romanised name is what a viewer is likely to type.
        let v = item(r#"{"title":"Against the Gods","aliases":["ni tian xie shen"]}"#);
        assert!(matches(&v, "ni tian"));
    }

    /// The real shape of `ling-cage`: season one runs 1–14 with a `6.5` and an
    /// `SP` numbered 16, and season two restarts at 1.
    fn two_seasons() -> Value {
        item(
            r#"[{"id":"primary-1","seasons":[
                 {"id":"tab-1","number":1,"episodes":[
                   {"id":"s1e1","number":1},{"id":"s1e6","number":6},
                   {"id":"s1e6.5","number":6.5,"numberDisplay":"6.5"},
                   {"id":"s1e14","number":14},
                   {"id":"s1sp","number":16,"numberDisplay":"SP"}]},
                 {"id":"tab-2","number":2,"episodes":[
                   {"id":"s2e1","number":1},{"id":"s2e12","number":12}]}
               ]}]"#,
        )
    }

    #[test]
    fn every_populated_season_is_kept() {
        let seasons = populated_seasons(two_seasons().as_array().unwrap());
        assert_eq!(seasons.len(), 2);
        assert_eq!(seasons[0].season_id, "tab-1");
        assert_eq!(seasons[1].season_id, "tab-2");
    }

    #[test]
    fn a_later_season_is_offset_past_the_one_before_it() {
        let seasons = populated_seasons(two_seasons().as_array().unwrap());
        let offsets = season_offsets(&seasons);

        // Season one's highest number is the SP's 16, not its entry count of 5.
        assert_eq!(offsets, vec![0.0, 16.0]);
    }

    #[test]
    fn a_single_season_series_keeps_the_sites_own_numbers() {
        let tabs = item(
            r#"[{"id":"primary-1","seasons":[
             {"id":"tab-1","number":1,"episodes":[{"id":"e1","number":1}]}]}]"#,
        );
        let seasons = populated_seasons(tabs.as_array().unwrap());
        assert_eq!(season_offsets(&seasons), vec![0.0]);
    }

    #[test]
    fn an_empty_season_is_skipped_rather_than_leaving_a_gap() {
        let tabs = item(
            r#"[{"id":"primary-1","seasons":[
                 {"id":"tab-1","number":1,"episodes":[]},
                 {"id":"tab-2","number":2,"episodes":[{"id":"ep-1","number":1}]}
               ]}]"#,
        );
        let seasons = populated_seasons(tabs.as_array().unwrap());
        assert_eq!(seasons.len(), 1);
        assert_eq!(seasons[0].season_id, "tab-2");
        // Nothing precedes it in the flat list, so it must not be pushed along.
        assert_eq!(season_offsets(&seasons), vec![0.0]);
    }

    #[test]
    fn seasons_are_ordered_by_the_sites_own_number() {
        let tabs = item(
            r#"[{"id":"primary-1","seasons":[
                 {"id":"tab-b","number":2,"episodes":[{"id":"b","number":1}]},
                 {"id":"tab-a","number":1,"episodes":[{"id":"a","number":1}]}
               ]}]"#,
        );
        let seasons = populated_seasons(tabs.as_array().unwrap());
        assert_eq!(seasons[0].season_id, "tab-a");
    }

    #[test]
    fn a_series_with_no_episodes_yet_yields_nothing() {
        let tabs = item(r#"[{"id":"primary-1","seasons":[{"id":"tab-1","episodes":[]}]}]"#);
        assert!(populated_seasons(tabs.as_array().unwrap()).is_empty());
    }

    #[test]
    fn embeds_are_built_per_platform() {
        assert_eq!(
            embed_for("dailymotion", "kAbC").unwrap().as_str(),
            "https://www.dailymotion.com/video/kAbC"
        );
        assert!(embed_for("rumble", "v1x").is_some());
        assert!(embed_for("vimeo", "123").is_none());
    }
}
