//! Pure HTML parsing.
//!
//! Every function here is synchronous and takes `&str`, returning owned data.
//! That is deliberate: `scraper::Html` is not `Send`, so keeping it out of async
//! functions entirely is what lets provider futures stay `Send`.

use crate::hosts;
use crate::spec::{
    EmbedSelectors, EpisodeSelectors, MirrorSelectors, SearchSelectors, SeriesSelectors,
};
use kuro_core::{Episode, Mirror, ProviderError, ProviderId, Series, SeriesDetails, SeriesStatus};
use scraper::{ElementRef, Html, Selector};
use url::Url;

fn compile(selector: &str) -> Result<Selector, ProviderError> {
    Selector::parse(selector)
        .map_err(|e| ProviderError::Config(format!("invalid selector `{selector}`: {e}")))
}

fn text_of(el: ElementRef<'_>) -> String {
    el.text().collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ")
}

fn select_text(root: ElementRef<'_>, selector: &str) -> Result<Option<String>, ProviderError> {
    let sel = compile(selector)?;
    Ok(root.select(&sel).next().map(text_of).filter(|s| !s.is_empty()))
}

/// First `src`-like attribute, checking lazy-load attributes before `src` since
/// lazily-loaded images carry a placeholder in `src`.
fn image_url(el: ElementRef<'_>, base: &Url) -> Option<Url> {
    ["data-src", "data-lazy-src", "src"]
        .iter()
        .find_map(|attr| el.value().attr(attr))
        .and_then(|raw| base.join(raw.trim()).ok())
}

/// Last non-empty path segment, used as the provider-local series id.
pub fn slug_from_url(url: &Url) -> String {
    url.path_segments()
        .and_then(|segs| segs.filter(|s| !s.is_empty()).next_back())
        .unwrap_or("")
        .to_string()
}

/// First number in a string, tolerating decimals. `"Episode 12.5"` → `12.5`.
pub fn first_number(s: &str) -> Option<f32> {
    let bytes = s.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            // Consume a decimal part only when a digit actually follows the dot,
            // so a trailing "15." doesn't swallow the period.
            if i + 1 < bytes.len() && bytes[i] == b'.' && bytes[i + 1].is_ascii_digit() {
                i += 1;
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
            }
            return s[start..i].parse().ok();
        }
        i += 1;
    }
    None
}

/// Four-digit year in a plausible range, e.g. from a title like `"Martial Peak (2024)"`.
fn year_from(s: &str) -> Option<u16> {
    let bytes = s.as_bytes();
    for start in 0..bytes.len().saturating_sub(3) {
        let window = &s[start..start + 4];
        if window.bytes().all(|b| b.is_ascii_digit()) {
            if let Ok(year) = window.parse::<u16>() {
                if (1950..=2100).contains(&year) {
                    return Some(year);
                }
            }
        }
    }
    None
}

pub fn parse_search(
    html: &str,
    sel: &SearchSelectors,
    base: &Url,
    provider_id: &ProviderId,
) -> Result<Vec<Series>, ProviderError> {
    let doc = Html::parse_document(html);
    let item_sel = compile(&sel.item)?;
    let url_sel = compile(&sel.url)?;

    let mut out = Vec::new();

    for item in doc.select(&item_sel) {
        let Some(href) = item
            .select(&url_sel)
            .next()
            .and_then(|a| a.value().attr("href"))
        else {
            continue;
        };

        let Ok(url) = base.join(href.trim()) else {
            continue;
        };

        let title = match select_text(item, &sel.title)? {
            Some(t) => t,
            None => continue,
        };

        let poster = sel
            .poster
            .as_deref()
            .and_then(|p| compile(p).ok())
            .and_then(|p| item.select(&p).next())
            .and_then(|img| image_url(img, base));

        out.push(Series {
            provider_id: provider_id.clone(),
            id: slug_from_url(&url),
            year: year_from(&title),
            title,
            url,
            poster,
            synopsis: None,
            genres: Vec::new(),
            status: SeriesStatus::Unknown,
            total_episodes: None,
        });
    }

    // An empty result set is a legitimate "no matches"; the page failing to contain
    // the container at all is what signals the markup changed.
    if out.is_empty() && doc.select(&item_sel).next().is_none() {
        let body_looks_real = html.len() > 512;
        if body_looks_real {
            return Err(ProviderError::parse(&sel.item, "search results"));
        }
    }

    Ok(out)
}

pub fn parse_series_details(
    html: &str,
    sel: &SeriesSelectors,
) -> Result<SeriesDetails, ProviderError> {
    let doc = Html::parse_document(html);
    let root = doc.root_element();

    let synopsis = match &sel.synopsis {
        Some(s) => select_text(root, s)?,
        None => None,
    };

    let genres = match &sel.genres {
        Some(s) => {
            let gs = compile(s)?;
            root.select(&gs).map(text_of).filter(|t| !t.is_empty()).collect()
        }
        None => Vec::new(),
    };

    let mut status = SeriesStatus::Unknown;
    let mut total_episodes = None;

    if let Some(row_sel) = &sel.info_row {
        let rows = compile(row_sel)?;
        for row in root.select(&rows) {
            let text = text_of(row);
            let lower = text.to_ascii_lowercase();

            if lower.contains("status") {
                status = if lower.contains("ongoing") || lower.contains("airing") {
                    SeriesStatus::Ongoing
                } else if lower.contains("completed") || lower.contains("finished") {
                    SeriesStatus::Completed
                } else {
                    SeriesStatus::Unknown
                };
            }

            if lower.contains("episode") {
                total_episodes = first_number(&text).map(|n| n as u32);
            }
        }
    }

    Ok(SeriesDetails {
        synopsis,
        genres,
        status,
        total_episodes,
    })
}

pub fn parse_episodes(
    html: &str,
    sel: &EpisodeSelectors,
    series_id: &str,
    base: &Url,
) -> Result<Vec<Episode>, ProviderError> {
    let doc = Html::parse_document(html);
    let item_sel = compile(&sel.item)?;

    let mut out = Vec::new();

    for (idx, item) in doc.select(&item_sel).enumerate() {
        let Some(href) = item.value().attr("href") else {
            continue;
        };
        let Ok(url) = base.join(href.trim()) else {
            continue;
        };

        // Prefer the explicit number element; fall back to the URL slug, which is
        // where the episode number reliably lives even when the markup shifts.
        let number = sel
            .number
            .as_deref()
            .and_then(|s| select_text(item, s).ok().flatten())
            .and_then(|t| first_number(&t))
            .or_else(|| episode_number_from_url(&url))
            .unwrap_or((idx + 1) as f32);

        let title = sel
            .title
            .as_deref()
            .and_then(|s| select_text(item, s).ok().flatten());

        out.push(Episode {
            series_id: series_id.to_string(),
            number,
            title,
            url,
        });
    }

    if out.is_empty() {
        return Err(ProviderError::parse(&sel.item, "episode list"));
    }

    out.sort_by(|a, b| a.number.partial_cmp(&b.number).unwrap_or(std::cmp::Ordering::Equal));
    Ok(out)
}

/// Pull the episode number out of a slug like `…-episode-15-lucifer-donghua`.
fn episode_number_from_url(url: &Url) -> Option<f32> {
    let slug = slug_from_url(url);
    let idx = slug.find("episode-")?;
    first_number(&slug[idx + "episode-".len()..])
}

pub fn parse_mirrors(
    html: &str,
    sel: &MirrorSelectors,
    base: &Url,
) -> Result<Vec<Mirror>, ProviderError> {
    let doc = Html::parse_document(html);
    let opt_sel = compile(&sel.option)?;

    let mut out = Vec::new();

    for (i, opt) in doc.select(&opt_sel).enumerate() {
        let Some(raw) = opt.value().attr(&sel.value_attr) else {
            continue;
        };
        let raw = raw.trim();
        if raw.is_empty() {
            continue;
        }
        let Ok(page_url) = base.join(raw) else {
            continue;
        };

        let index = sel
            .index_attr
            .as_deref()
            .and_then(|a| opt.value().attr(a))
            .and_then(|v| v.parse::<u8>().ok())
            .unwrap_or((i + 1) as u8);

        // Labels are usually blank in these themes and get filled in from the
        // embed host once the mirror is resolved.
        let label = text_of(opt);
        let label = if label.is_empty() {
            format!("Mirror {index}")
        } else {
            label
        };

        out.push(Mirror {
            index,
            label,
            page_url,
            embed_url: None,
        });
    }

    if out.is_empty() {
        return Err(ProviderError::parse(&sel.option, "mirror list"));
    }

    Ok(out)
}

/// Extract the video embed URL, skipping ad and tracker iframes.
pub fn parse_embed(html: &str, sel: &EmbedSelectors, base: &Url) -> Result<Url, ProviderError> {
    let doc = Html::parse_document(html);
    let iframe_sel = compile(&sel.iframe)?;

    let mut seen_any = false;

    for iframe in doc.select(&iframe_sel) {
        let Some(src) = ["src", "data-src", "data-litespeed-src"]
            .iter()
            .find_map(|a| iframe.value().attr(a))
        else {
            continue;
        };
        seen_any = true;

        let Ok(url) = base.join(src.trim()) else {
            continue;
        };

        if url.host_str().map(hosts::is_video_host).unwrap_or(false) {
            return Ok(url);
        }
    }

    if seen_any {
        // Iframes exist, but none was a known video host — either a new host needs
        // adding to the allowlist, or this mirror is dead and serving only ads.
        Err(ProviderError::parse(
            &sel.iframe,
            "video embed (only non-video iframes found)",
        ))
    } else {
        Err(ProviderError::parse(&sel.iframe, "video embed"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_numbers_including_decimals() {
        assert_eq!(first_number("Episode 15"), Some(15.0));
        assert_eq!(first_number("12.5"), Some(12.5));
        assert_eq!(first_number("Eps 7 - Title"), Some(7.0));
        assert_eq!(first_number("no digits"), None);
        // A trailing dot is not part of the number.
        assert_eq!(first_number("15."), Some(15.0));
    }

    #[test]
    fn reads_episode_number_from_slug() {
        let url = Url::parse("https://x.tld/martial-god-asura-season-2-episode-15-lucifer-donghua/")
            .expect("valid url");
        assert_eq!(episode_number_from_url(&url), Some(15.0));
    }

    #[test]
    fn season_number_does_not_shadow_episode_number() {
        // "season-2" appears before "episode-15"; parsing must anchor on "episode-".
        let url = Url::parse("https://x.tld/show-season-2-episode-15-suffix/").expect("valid url");
        assert_eq!(episode_number_from_url(&url), Some(15.0));
    }

    #[test]
    fn slug_ignores_trailing_slash() {
        let url = Url::parse("https://x.tld/anime/martial-peak-2024/").expect("valid url");
        assert_eq!(slug_from_url(&url), "martial-peak-2024");
    }

    #[test]
    fn year_is_only_taken_from_plausible_range() {
        assert_eq!(year_from("Martial Peak (2024)"), Some(2024));
        assert_eq!(year_from("Episode 1080"), None);
        assert_eq!(year_from("no year"), None);
    }

    #[test]
    fn embed_extraction_skips_ad_iframes() {
        let html = r#"
            <div>
              <iframe src="https://sads.adsboosters.xyz/ad.html"></iframe>
              <iframe src="https://rumble.com/embed/v75qb3o/?pub=4p006u"></iframe>
            </div>"#;
        let sel = EmbedSelectors {
            iframe: "iframe".to_string(),
        };
        let base = Url::parse("https://x.tld/").expect("valid url");
        let got = parse_embed(html, &sel, &base).expect("finds the video iframe");
        assert_eq!(got.host_str(), Some("rumble.com"));
    }

    #[test]
    fn embed_extraction_reports_parse_failure_when_only_ads_present() {
        let html = r#"<iframe src="https://sads.adsboosters.xyz/ad.html"></iframe>"#;
        let sel = EmbedSelectors {
            iframe: "iframe".to_string(),
        };
        let base = Url::parse("https://x.tld/").expect("valid url");
        assert!(matches!(
            parse_embed(html, &sel, &base),
            Err(ProviderError::ParseFailure { .. })
        ));
    }
}
