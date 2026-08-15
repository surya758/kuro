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
    /// Where the episode list lives, when it is not the series page.
    ///
    /// Sites with a JSON backend serve episodes from an API keyed by a numeric id
    /// rather than rendering them into the series HTML. Supports `{slug}` for the
    /// full series id and `{id}` for its trailing digits — `solo-leveling-4883`
    /// gives `4883`.
    #[serde(default)]
    pub episodes: Option<String>,
}

/// How a response should be read.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Format {
    /// CSS selectors against markup.
    #[default]
    Html,
    /// Field names against a JSON document.
    Json,
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
    /// The element that *holds* the results.
    ///
    /// Lets a genuine "no matches" be told apart from a broken scraper: a search
    /// with no results still renders this container, whereas a redesign removes it.
    /// Without it, an unmatched query looks like breakage and counts against the
    /// provider's health.
    #[serde(default)]
    pub container: Option<String>,
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
    /// Whether the fields below are CSS selectors or JSON keys.
    #[serde(default)]
    pub format: Format,
    /// CSS selector for each row, or the JSON key holding the episode array.
    pub item: String,
    /// JSON only: field whose value builds each episode's URL via [`Self::url`].
    #[serde(default)]
    pub id: Option<String>,
    /// JSON only: template for an episode's URL, with `{id}` substituted.
    ///
    /// JSON episode lists carry ids rather than links, so the URL that later
    /// yields mirrors has to be constructed rather than read.
    #[serde(default)]
    pub url: Option<String>,
    /// The element that *holds* the episode list.
    ///
    /// Same role as [`SearchSelectors::container`]: a series whose list has not been
    /// populated still renders this, so its presence means "no episodes yet" rather
    /// than "the markup changed".
    #[serde(default)]
    pub container: Option<String>,
    /// Where to look when the list is empty.
    ///
    /// Some series render an unpopulated list but still link the latest episode
    /// elsewhere on the page; without this they would be unwatchable.
    #[serde(default)]
    pub fallback: Option<String>,
    #[serde(default)]
    pub number: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MirrorSelectors {
    /// Whether the fields below are CSS selectors or JSON keys.
    #[serde(default)]
    pub format: Format,
    /// CSS selector for each mirror `<option>`. Unused when `format` is JSON.
    #[serde(default)]
    pub option: String,
    /// JSON only: key holding the array of mirrors.
    #[serde(default)]
    pub item: Option<String>,
    /// JSON only: field holding the embed URL.
    #[serde(default)]
    pub value: Option<String>,
    #[serde(default = "default_value_attr")]
    pub value_attr: String,
    #[serde(default)]
    pub index_attr: Option<String>,
    /// Optional selector for the mirror's human label. Many themes leave the
    /// `<option>` text blank, in which case labels are derived from the embed host.
    #[serde(default)]
    pub label: Option<String>,
    /// How to interpret the option's value.
    #[serde(default)]
    pub value_encoding: MirrorValueEncoding,
}

/// What a mirror `<option>`'s value actually holds.
///
/// Sites on this theme family split into two camps, and the difference is not
/// cosmetic: `Url` costs one extra fetch per mirror, `Base64Html` costs none.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MirrorValueEncoding {
    /// A link to a sub-page that must be fetched to find the player.
    #[default]
    Url,
    /// Base64-encoded HTML containing the `<iframe>` itself — decode and read it
    /// inline, no network round-trip.
    Base64Html,
}

fn default_value_attr() -> String {
    "value".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmbedSelectors {
    /// Iframe-based players. Checked first — this is the actual player element.
    #[serde(default)]
    pub iframe: String,
    /// Key of a script-configured player's stream URL, e.g. `file` in
    /// `file: 'https://…/master.m3u8'`.
    ///
    /// For players built by JavaScript rather than an iframe, where the stream URL
    /// appears in a config object and no element holds it.
    #[serde(default)]
    pub script_key: Option<String>,
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
    /// Fetch this provider through the external browser-shaped client.
    ///
    /// Only for sites whose challenge inspects the TLS handshake, where no header
    /// combination gets through. It costs a process per request, so it stays off
    /// unless a provider actually needs it.
    pub impersonate: bool,
}

impl ProviderSpec {
    pub fn from_toml(text: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(text)
    }
}
