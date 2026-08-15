//! Parsing for providers backed by a JSON API.
//!
//! The counterpart to [`crate::parse`], which handles markup. Sites that render
//! their episode list client-side expose it as JSON instead, and a CSS selector has
//! nothing to bite on. Field names take the place of selectors; everything else
//! about the provider — health, caching, mirror failover — is unchanged.

use crate::spec::{EpisodeSelectors, MirrorSelectors};
use kuro_core::{Episode, Mirror, ProviderError};
use serde_json::Value;
use url::Url;

/// Trailing digits of a slug like `solo-leveling-4883`.
///
/// JSON backends key on a numeric id while the human-facing URL carries a slug, so
/// the id has to be recovered from the end of the slug to address the API.
pub fn id_from_slug(slug: &str) -> Option<&str> {
    let tail = slug.rsplit('-').next()?;
    if !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit()) {
        Some(tail)
    } else {
        None
    }
}

/// Substitute `{slug}` and `{id}` into a URL template.
pub fn fill_template(template: &str, slug: &str) -> String {
    let id = id_from_slug(slug).unwrap_or(slug);
    template.replace("{slug}", slug).replace("{id}", id)
}

fn document(body: &str, context: &str) -> Result<Value, ProviderError> {
    serde_json::from_str(body)
        .map_err(|e| ProviderError::parse(format!("<json: {e}>"), context.to_string()))
}

/// The array at `key`, or at the document root when `key` is empty.
fn array<'a>(doc: &'a Value, key: &str, context: &str) -> Result<&'a Vec<Value>, ProviderError> {
    let node = if key.is_empty() { doc } else { &doc[key] };
    node.as_array()
        .ok_or_else(|| ProviderError::parse(key.to_string(), context.to_string()))
}

/// A field as a number, accepting the string form some APIs use.
fn number_at(item: &Value, field: &str) -> Option<f32> {
    match &item[field] {
        Value::Number(n) => n.as_f64().map(|v| v as f32),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// A field as a string, accepting a number so ids need not be quoted.
fn string_at(item: &Value, field: &str) -> Option<String> {
    match &item[field] {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

pub fn parse_episodes(
    body: &str,
    sel: &EpisodeSelectors,
    series_id: &str,
    base: &Url,
) -> Result<Vec<Episode>, ProviderError> {
    let doc = document(body, "episode list")?;
    let items = array(&doc, &sel.item, "episode list")?;

    let id_field = sel.id.as_deref().unwrap_or("id");
    let number_field = sel.number.as_deref().unwrap_or("number");
    let url_template = sel.url.as_deref().ok_or_else(|| {
        ProviderError::Config(
            "a JSON episode list needs `selectors.episodes.url` to build episode links".to_string(),
        )
    })?;

    let mut out = Vec::with_capacity(items.len());
    for item in items {
        // An entry missing either field is skipped rather than failing the list:
        // one malformed row should not cost the viewer the whole series.
        let (Some(number), Some(id)) = (number_at(item, number_field), string_at(item, id_field))
        else {
            continue;
        };

        let path = url_template.replace("{id}", &id);
        let Ok(url) = base.join(&path) else { continue };

        out.push(Episode {
            series_id: series_id.to_string(),
            number,
            title: sel.title.as_deref().and_then(|f| string_at(item, f)),
            url,
        });
    }

    // An empty array is a real state for a series with nothing aired yet, and the
    // array's presence already proves the shape is intact — so unlike a missing
    // key, it is not evidence the provider broke.
    out.sort_by(|a, b| {
        a.number
            .partial_cmp(&b.number)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    Ok(out)
}

pub fn parse_mirrors(
    body: &str,
    sel: &MirrorSelectors,
    base: &Url,
) -> Result<Vec<Mirror>, ProviderError> {
    let doc = document(body, "mirror list")?;
    let key = sel.item.as_deref().unwrap_or("");
    let items = array(&doc, key, "mirror list")?;

    let value_field = sel.value.as_deref().unwrap_or("embed_url");

    let mut out = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let Some(raw) = string_at(item, value_field) else {
            continue;
        };
        let Ok(page_url) = base.join(&raw) else {
            continue;
        };

        let label = sel
            .label
            .as_deref()
            .and_then(|f| string_at(item, f))
            .unwrap_or_else(|| format!("mirror {}", index + 1));

        out.push(Mirror {
            index: index as u8,
            label,
            page_url,
            // The value is a player page, not a stream: it still needs resolving,
            // which is what leaving this `None` asks the caller to do.
            embed_url: None,
        });
    }

    if out.is_empty() {
        return Err(ProviderError::parse(
            value_field.to_string(),
            "mirror list".to_string(),
        ));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::Format;

    fn base() -> Url {
        Url::parse("https://example.test/").unwrap()
    }

    fn episode_selectors() -> EpisodeSelectors {
        EpisodeSelectors {
            format: Format::Json,
            item: "episodes".to_string(),
            id: Some("id".to_string()),
            url: Some("/api/episode/{id}/languages".to_string()),
            container: None,
            fallback: None,
            number: Some("number".to_string()),
            title: None,
        }
    }

    fn mirror_selectors() -> MirrorSelectors {
        MirrorSelectors {
            format: Format::Json,
            option: String::new(),
            item: Some("languages".to_string()),
            value: Some("embed_url".to_string()),
            value_attr: "value".to_string(),
            index_attr: None,
            label: Some("name".to_string()),
            value_encoding: Default::default(),
        }
    }

    #[test]
    fn a_numeric_slug_suffix_is_the_api_id() {
        assert_eq!(id_from_slug("solo-leveling-4883"), Some("4883"));
        assert_eq!(id_from_slug("bleach-670"), Some("670"));
    }

    #[test]
    fn a_slug_without_a_numeric_suffix_has_no_id() {
        // Falling back to the whole slug is the caller's job; guessing a number
        // out of a title would address the wrong series.
        assert_eq!(id_from_slug("solo-leveling"), None);
        assert_eq!(id_from_slug(""), None);
    }

    #[test]
    fn templates_take_both_the_slug_and_its_id() {
        assert_eq!(
            fill_template("/api/anime/{id}/episodes", "solo-leveling-4883"),
            "/api/anime/4883/episodes"
        );
        assert_eq!(
            fill_template("/anime/{slug}", "solo-leveling-4883"),
            "/anime/solo-leveling-4883"
        );
    }

    #[test]
    fn episodes_come_back_numbered_and_linked() {
        let body = r#"{"episodes":[
            {"id":16704,"number":1,"filler":false},
            {"id":16705,"number":2,"filler":false}
        ]}"#;
        let eps =
            parse_episodes(body, &episode_selectors(), "solo-leveling-4883", &base()).unwrap();
        assert_eq!(eps.len(), 2);
        assert_eq!(eps[0].number, 1.0);
        assert_eq!(
            eps[0].url.as_str(),
            "https://example.test/api/episode/16704/languages"
        );
    }

    #[test]
    fn episodes_are_sorted_even_when_the_api_is_not() {
        let body = r#"{"episodes":[{"id":3,"number":3},{"id":1,"number":1},{"id":2,"number":2}]}"#;
        let eps = parse_episodes(body, &episode_selectors(), "s-1", &base()).unwrap();
        let numbers: Vec<f32> = eps.iter().map(|e| e.number).collect();
        assert_eq!(numbers, vec![1.0, 2.0, 3.0]);
    }

    #[test]
    fn a_series_with_nothing_aired_is_not_a_broken_scraper() {
        // Mirrors the HTML side: the array's presence proves the shape is intact.
        let eps =
            parse_episodes(r#"{"episodes":[]}"#, &episode_selectors(), "s-1", &base()).unwrap();
        assert!(eps.is_empty());
    }

    #[test]
    fn a_missing_episode_key_is_a_parse_failure() {
        let err = parse_episodes(r#"{"other":[]}"#, &episode_selectors(), "s-1", &base())
            .expect_err("missing key must not pass silently");
        assert!(matches!(err, ProviderError::ParseFailure { .. }));
    }

    #[test]
    fn malformed_json_is_a_parse_failure_not_a_panic() {
        let err = parse_episodes("<html>challenge</html>", &episode_selectors(), "s", &base())
            .expect_err("html body must not parse as json");
        assert!(matches!(err, ProviderError::ParseFailure { .. }));
    }

    #[test]
    fn each_language_becomes_a_labelled_mirror() {
        let body = r#"{"languages":[
            {"code":"eng","name":"English","embed_url":"https://example.test/embed/a"},
            {"code":"jpn","name":"Japanese","embed_url":"https://example.test/embed/b"}
        ]}"#;
        let mirrors = parse_mirrors(body, &mirror_selectors(), &base()).unwrap();
        assert_eq!(mirrors.len(), 2);
        assert_eq!(mirrors[0].label, "English");
        assert_eq!(mirrors[1].page_url.as_str(), "https://example.test/embed/b");
        // Still a player page, so resolution has to happen later.
        assert!(mirrors[0].embed_url.is_none());
    }

    #[test]
    fn no_usable_mirror_is_a_parse_failure() {
        let err = parse_mirrors(
            r#"{"languages":[{"code":"eng"}]}"#,
            &mirror_selectors(),
            &base(),
        )
        .expect_err("entries without an embed url are unusable");
        assert!(matches!(err, ProviderError::ParseFailure { .. }));
    }
}
