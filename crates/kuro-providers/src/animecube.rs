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

/// The most recent season that actually has episodes, with its identifiers.
///
/// kuro models a series as one flat episode list, while the site groups by season
/// and restarts numbering in each. Flattening would collide — two "episode 1" —
/// so the newest populated season is used, which is what the site itself opens on.
fn latest_populated_season(tabs: &[Value]) -> Option<(String, String, Vec<Value>)> {
    let mut best: Option<(String, String, Vec<Value>)> = None;

    for tab in tabs {
        let Some(tab_id) = tab.get("id").and_then(Value::as_str) else {
            continue;
        };
        let Some(seasons) = tab.get("seasons").and_then(Value::as_array) else {
            continue;
        };
        for season in seasons {
            let Some(season_id) = season.get("id").and_then(Value::as_str) else {
                continue;
            };
            let episodes = season
                .get("episodes")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            if !episodes.is_empty() {
                best = Some((tab_id.to_string(), season_id.to_string(), episodes));
            }
        }
    }
    best
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
        let Some((tab_id, season_id, entries)) = latest_populated_season(&tabs) else {
            return Ok(Vec::new());
        };

        let mut out = Vec::with_capacity(entries.len());
        for entry in &entries {
            let (Some(id), Some(number)) = (
                entry.get("id").and_then(Value::as_str),
                entry.get("number").and_then(Value::as_f64),
            ) else {
                continue;
            };

            // The mirror lookup needs all three identifiers, so they ride along in
            // the episode's URL rather than being recomputed later.
            let path = format!(
                "/api/anime/{}/episode/{}/sources?primaryTabId={}&seasonId={}",
                series.id, id, tab_id, season_id
            );
            let Ok(url) = self.join(&path) else { continue };

            out.push(Episode {
                series_id: series.id.clone(),
                number: number as f32,
                title: entry
                    .get("title")
                    .and_then(Value::as_str)
                    .filter(|t| !t.is_empty())
                    .map(str::to_string),
                url,
            });
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

    #[test]
    fn the_newest_populated_season_wins() {
        let tabs = item(
            r#"[{"id":"primary-1","seasons":[
                 {"id":"tab-1","episodes":[]},
                 {"id":"tab-2","episodes":[{"id":"ep-1","number":1}]}
               ]}]"#,
        );
        let (tab, season, eps) = latest_populated_season(tabs.as_array().unwrap()).unwrap();
        assert_eq!(tab, "primary-1");
        assert_eq!(season, "tab-2");
        assert_eq!(eps.len(), 1);
    }

    #[test]
    fn a_series_with_no_episodes_yet_yields_nothing() {
        let tabs = item(r#"[{"id":"primary-1","seasons":[{"id":"tab-1","episodes":[]}]}]"#);
        assert!(latest_populated_season(tabs.as_array().unwrap()).is_none());
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
