//! Offline scraper tests against real recorded HTML.
//!
//! These assert the shipped selector spec against pages captured from the live site,
//! so a refactor that breaks parsing fails in CI without touching the network. Script
//! and style bodies were stripped when recording — `kuro` never executes page JS, so
//! they are dead weight and carry the site's ad code.
//!
//! When the site redesigns, these tests are *expected* to fail. That failure is the
//! signal to re-record and update `providers.d/luciferdonghua.toml`.

use kuro_core::ProviderId;
use kuro_providers::parse;
use kuro_providers::spec::ProviderSpec;
use url::Url;

const SPEC_TOML: &str = include_str!("../../../providers.d/luciferdonghua.toml");

const SEARCH_HTML: &str = include_str!("../../../tests/fixtures/luciferdonghua/search.html");
const SERIES_HTML: &str = include_str!("../../../tests/fixtures/luciferdonghua/series.html");
const EPISODE_HTML: &str = include_str!("../../../tests/fixtures/luciferdonghua/episode.html");
const MIRROR2_HTML: &str = include_str!("../../../tests/fixtures/luciferdonghua/mirror-2.html");

fn spec() -> ProviderSpec {
    ProviderSpec::from_toml(SPEC_TOML).expect("shipped spec parses")
}

fn base() -> Url {
    Url::parse("https://luciferdonghua.in").expect("valid base url")
}

#[test]
fn search_page_yields_every_result() {
    let spec = spec();
    let series = parse::parse_search(
        SEARCH_HTML,
        &spec.selectors.search,
        &base(),
        &ProviderId::new("luciferdonghua"),
    )
    .expect("search page parses");

    assert_eq!(series.len(), 10, "recorded page has ten results");

    for s in &series {
        assert!(!s.title.trim().is_empty(), "every result has a title");
        assert!(!s.id.is_empty(), "every result has a slug id");
        assert_eq!(s.url.host_str(), Some("luciferdonghua.in"));
        assert!(
            s.url.path().starts_with("/anime/"),
            "series URLs live under /anime/, got {}",
            s.url
        );
    }

    assert!(
        series
            .iter()
            .all(|s| s.title.to_lowercase().contains("martial")),
        "the recorded query was `martial`"
    );
}

#[test]
fn search_results_carry_posters() {
    let spec = spec();
    let series = parse::parse_search(
        SEARCH_HTML,
        &spec.selectors.search,
        &base(),
        &ProviderId::new("luciferdonghua"),
    )
    .expect("search page parses");

    assert!(
        series.iter().filter(|s| s.poster.is_some()).count() >= 8,
        "most results should have a poster image"
    );
}

#[test]
fn series_page_yields_a_sorted_episode_list() {
    let spec = spec();
    let episodes = parse::parse_episodes(
        SERIES_HTML,
        &spec.selectors.episodes,
        "martial-god-asura-season-2",
        &base(),
    )
    .expect("series page parses");

    assert!(
        episodes.len() >= 15,
        "recorded series has at least 15 episodes, got {}",
        episodes.len()
    );

    let numbers: Vec<f32> = episodes.iter().map(|e| e.number).collect();
    let mut sorted = numbers.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).expect("no NaN episode numbers"));
    assert_eq!(numbers, sorted, "episodes are returned in ascending order");

    assert!(
        episodes
            .iter()
            .any(|e| (e.number - 15.0).abs() < f32::EPSILON),
        "episode 15 is present"
    );

    for e in &episodes {
        assert_eq!(e.series_id, "martial-god-asura-season-2");
        assert!(e.url.path().contains("episode"), "got {}", e.url);
    }
}

#[test]
fn episode_numbers_are_unique() {
    let spec = spec();
    let episodes = parse::parse_episodes(
        SERIES_HTML,
        &spec.selectors.episodes,
        "martial-god-asura-season-2",
        &base(),
    )
    .expect("series page parses");

    let mut seen = std::collections::HashSet::new();
    for e in &episodes {
        // Guards against the index-based fallback silently taking over and
        // renumbering every episode identically.
        assert!(
            seen.insert(e.number.to_bits()),
            "duplicate episode number {}",
            e.number
        );
    }
}

#[test]
fn series_page_yields_metadata() {
    let spec = spec();
    let details =
        parse::parse_series_details(SERIES_HTML, &spec.selectors.series).expect("details parse");

    assert!(
        details.synopsis.as_deref().map(str::len).unwrap_or(0) > 40,
        "synopsis should be substantive, got {:?}",
        details.synopsis
    );
    assert!(!details.genres.is_empty(), "series lists genres");
}

#[test]
fn episode_page_yields_all_mirrors() {
    let spec = spec();
    let episode_url = Url::parse(
        "https://luciferdonghua.in/martial-god-asura-season-2-episode-15-lucifer-donghua/",
    )
    .expect("valid episode url");
    let mirrors = parse::parse_mirrors(
        EPISODE_HTML,
        &spec.selectors.mirrors,
        &spec.selectors.embed,
        &base(),
        &episode_url,
    )
    .expect("mirrors parse");

    assert_eq!(mirrors.len(), 5, "recorded episode offers five mirrors");

    let indices: Vec<u8> = mirrors.iter().map(|m| m.index).collect();
    assert_eq!(indices, vec![1, 2, 3, 4, 5]);

    for m in &mirrors {
        assert!(
            m.page_url.path().contains("/v/"),
            "mirrors point at /v/N/ sub-pages, got {}",
            m.page_url
        );
        assert!(m.embed_url.is_none(), "embeds are resolved lazily");
    }
}

#[test]
fn iframe_mirror_resolves_to_its_video_host() {
    let spec = spec();
    let embed =
        parse::parse_embed(EPISODE_HTML, &spec.selectors.embed, &base()).expect("embed resolves");

    assert_eq!(
        embed.host_str(),
        Some("rumble.com"),
        "the episode page embeds Rumble in an iframe"
    );
}

#[test]
fn javascript_mounted_mirror_resolves_via_metadata() {
    // Regression: this mirror has no video iframe at all. Before the metadata
    // fallback existed it parsed as "no embed", silently halving mirror failover.
    let spec = spec();
    let embed =
        parse::parse_embed(MIRROR2_HTML, &spec.selectors.embed, &base()).expect("embed resolves");

    assert_eq!(embed.host_str(), Some("geo.dailymotion.com"));
    assert!(
        embed.query().map(|q| q.contains("video=")).unwrap_or(false),
        "Dailymotion embed carries its video id, got {embed}"
    );
}

/// The second provider, which shares the theme but encodes its mirrors differently.
/// These tests exist mainly to prove the declarative layer really did absorb a new
/// site without provider-specific Rust.
mod donghuastream {
    use super::*;

    const SPEC_TOML: &str = include_str!("../../../providers.d/donghuastream.toml");
    const SEARCH_HTML: &str = include_str!("../../../tests/fixtures/donghuastream/search.html");
    const SERIES_HTML: &str = include_str!("../../../tests/fixtures/donghuastream/series.html");
    const EPISODE_HTML: &str = include_str!("../../../tests/fixtures/donghuastream/episode.html");

    fn spec() -> ProviderSpec {
        ProviderSpec::from_toml(SPEC_TOML).expect("shipped spec parses")
    }

    fn base() -> Url {
        Url::parse("https://donghuastream.org").expect("valid base url")
    }

    #[test]
    fn search_page_parses_with_the_shared_selectors() {
        let series = parse::parse_search(
            SEARCH_HTML,
            &spec().selectors.search,
            &base(),
            &ProviderId::new("donghuastream"),
        )
        .expect("search page parses");

        assert_eq!(series.len(), 10);
        assert!(series
            .iter()
            .all(|s| s.url.path().starts_with("/anime/") && !s.title.trim().is_empty()));
    }

    const EMPTY_SEARCH_HTML: &str =
        include_str!("../../../tests/fixtures/donghuastream/search-empty.html");

    #[test]
    fn a_search_with_no_matches_is_not_a_scraper_failure() {
        // Regression: a query with zero results renders the container with nothing
        // in it. That used to be reported as "the site's markup changed", which also
        // counted against provider health — enough of them would auto-disable a
        // perfectly healthy site.
        let series = parse::parse_search(
            EMPTY_SEARCH_HTML,
            &spec().selectors.search,
            &base(),
            &ProviderId::new("donghuastream"),
        )
        .expect("zero results must not be an error");

        assert!(series.is_empty());
    }

    #[test]
    fn a_page_without_the_container_is_still_a_failure() {
        // The distinction has to hold in both directions, or a real redesign would
        // pass silently as "no results".
        let mut selectors = spec().selectors.search;
        selectors.container = Some("div.this-container-is-gone".to_string());

        let result = parse::parse_search(
            EMPTY_SEARCH_HTML,
            &selectors,
            &base(),
            &ProviderId::new("donghuastream"),
        );

        assert!(matches!(
            result,
            Err(kuro_core::ProviderError::ParseFailure { .. })
        ));
    }

    #[test]
    fn series_page_parses_episodes_and_synopsis() {
        let episodes = parse::parse_episodes(
            SERIES_HTML,
            &spec().selectors.episodes,
            "martial-god-asura-season-2",
            &base(),
        )
        .expect("series page parses");

        assert!(episodes.len() >= 15, "got {}", episodes.len());
        // The site lists newest-first; the parser must normalise to ascending.
        assert!(episodes[0].number < episodes[episodes.len() - 1].number);

        let details = parse::parse_series_details(SERIES_HTML, &spec().selectors.series)
            .expect("details parse");
        assert!(details.synopsis.as_deref().map(str::len).unwrap_or(0) > 40);
    }

    #[test]
    fn base64_mirrors_resolve_inline_without_a_second_fetch() {
        let spec = spec();
        let episode_url = Url::parse(
            "https://donghuastream.org/martial-god-asura-season-2-episode-15-multiple-subtitles/",
        )
        .expect("valid episode url");

        let mirrors = parse::parse_mirrors(
            EPISODE_HTML,
            &spec.selectors.mirrors,
            &spec.selectors.embed,
            &base(),
            &episode_url,
        )
        .expect("mirrors parse");

        assert_eq!(mirrors.len(), 4, "recorded episode offers four mirrors");

        // The whole point of `base64_html`: every embed is already known, so
        // playback needs no extra round-trip per mirror.
        assert!(
            mirrors.iter().all(|m| m.embed_url.is_some()),
            "every base64 mirror should resolve inline"
        );

        let hosts: Vec<String> = mirrors
            .iter()
            .filter_map(|m| m.embed_url.as_ref()?.host_str().map(str::to_string))
            .collect();

        for expected in ["dailymotion", "rumble", "streamplay", "ok.ru"] {
            assert!(
                hosts.iter().any(|h| h.contains(expected)),
                "expected a {expected} mirror, got {hosts:?}"
            );
        }
    }

    #[test]
    fn base64_mirrors_are_labelled_by_host_not_by_index() {
        let spec = spec();
        let episode_url = Url::parse("https://donghuastream.org/ep/").expect("valid url");
        let mirrors = parse::parse_mirrors(
            EPISODE_HTML,
            &spec.selectors.mirrors,
            &spec.selectors.embed,
            &base(),
            &episode_url,
        )
        .expect("mirrors parse");

        assert!(
            mirrors.iter().all(|m| !m.label.starts_with("Mirror ")),
            "inline embeds should be named from their host: {:?}",
            mirrors.iter().map(|m| &m.label).collect::<Vec<_>>()
        );
    }
}

#[test]
fn ad_iframes_are_never_mistaken_for_the_player() {
    // The recorded pages carry real ad iframes, several of them before the player.
    let spec = spec();
    for (name, html) in [("episode", EPISODE_HTML), ("mirror-2", MIRROR2_HTML)] {
        let embed = parse::parse_embed(html, &spec.selectors.embed, &base())
            .unwrap_or_else(|e| panic!("{name} should resolve: {e}"));
        let host = embed.host_str().unwrap_or_default();
        assert!(
            !host.contains("adsboosters") && !host.contains("dmca"),
            "{name} resolved to a non-video host: {host}"
        );
    }
}
