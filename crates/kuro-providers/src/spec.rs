//! The declarative provider format.
//!
//! Roughly 80% of real scraper breakage is "a CSS class changed". Keeping selectors
//! in TOML rather than Rust means that class of failure is a config edit and a
//! `kuro provider reload` — no recompile, no release.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderSpec {
    pub id: String,
    pub display_name: String,
    pub base_url: String,

    pub endpoints: Endpoints,
    pub selectors: Selectors,

    #[serde(default)]
    pub request: RequestSpec,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Endpoints {
    /// Path template containing `{query}`, which is replaced with the
    /// percent-encoded search term.
    pub search: String,
    /// Path template containing `{slug}`. Informational — series URLs are always
    /// taken from the page's `href`, never built from this.
    #[serde(default)]
    pub series: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Selectors {
    pub search: SearchSelectors,
    pub series: SeriesSelectors,
    pub episodes: EpisodeSelectors,
    pub mirrors: MirrorSelectors,
    pub embed: EmbedSelectors,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchSelectors {
    /// Repeating element, one per result.
    pub item: String,
    pub title: String,
    pub url: String,
    #[serde(default)]
    pub poster: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SeriesSelectors {
    pub title: Option<String>,
    pub synopsis: Option<String>,
    pub poster: Option<String>,
    pub genres: Option<String>,
    /// Rows of a key/value metadata block, e.g. `Status: Ongoing`.
    pub info_row: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EpisodeSelectors {
    pub item: String,
    #[serde(default)]
    pub number: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MirrorSelectors {
    pub option: String,
    #[serde(default = "default_value_attr")]
    pub value_attr: String,
    #[serde(default)]
    pub index_attr: Option<String>,
    /// Optional selector for the mirror's human label. Many themes leave the
    /// `<option>` text blank, in which case labels are derived from the embed host.
    #[serde(default)]
    pub label: Option<String>,
}

fn default_value_attr() -> String {
    "value".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmbedSelectors {
    /// Iframe-based players. Checked first — this is the actual player element.
    pub iframe: String,
    /// Optional fallback for players injected by JavaScript rather than an iframe.
    /// Dailymotion mirrors expose only `<meta itemprop="embedUrl" content="…">`,
    /// so without this those mirrors look dead to the scraper.
    #[serde(default)]
    pub meta: Option<String>,
    /// Attribute holding the URL on `meta` matches.
    #[serde(default = "default_meta_attr")]
    pub meta_attr: String,
}

fn default_meta_attr() -> String {
    "content".to_string()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RequestSpec {
    pub user_agent: Option<String>,
    pub referer: Option<String>,
}

impl ProviderSpec {
    pub fn from_toml(text: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(text)
    }
}
