//! Core domain types shared across every crate.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use url::Url;

/// Stable identifier for a provider, matching the `id` field of its selector TOML
/// and the key used under `[providers.*]` in the user config.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ProviderId(pub String);

impl ProviderId {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ProviderId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for ProviderId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SeriesStatus {
    Ongoing,
    Completed,
    #[default]
    Unknown,
}

impl fmt::Display for SeriesStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ongoing => f.write_str("Ongoing"),
            Self::Completed => f.write_str("Completed"),
            Self::Unknown => f.write_str("Unknown"),
        }
    }
}

/// A series as returned by a provider's search or catalogue listing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Series {
    pub provider_id: ProviderId,
    /// Provider-local slug. Always read from the page's `href`, never constructed —
    /// slugs carry inconsistent suffixes (`-2024`, `-new`, `-edit`).
    pub id: String,
    pub title: String,
    pub url: Url,
    pub poster: Option<Url>,
    pub year: Option<u16>,
    pub synopsis: Option<String>,
    pub genres: Vec<String>,
    pub status: SeriesStatus,
    pub total_episodes: Option<u32>,
}

/// Extended metadata fetched from a series detail page.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SeriesDetails {
    pub synopsis: Option<String>,
    pub genres: Vec<String>,
    pub status: SeriesStatus,
    pub total_episodes: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Episode {
    pub series_id: String,
    /// `f32` so half-episodes and specials (`12.5`) are representable.
    pub number: f32,
    pub title: Option<String>,
    pub url: Url,
}

impl Episode {
    /// Renders the episode number without a trailing `.0` for whole numbers.
    pub fn number_label(&self) -> String {
        if (self.number.fract()).abs() < f32::EPSILON {
            format!("{}", self.number as i64)
        } else {
            format!("{}", self.number)
        }
    }
}

/// A candidate playback source, prior to resolution into a real stream.
///
/// On most providers this corresponds to one entry of a mirror `<select>`; the
/// `embed_url` is filled in lazily because it costs an extra fetch per mirror.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mirror {
    pub index: u8,
    /// Derived from the embed host after resolution — provider markup frequently
    /// leaves the `<option>` label empty.
    pub label: String,
    pub page_url: Url,
    pub embed_url: Option<Url>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StreamKind {
    Hls,
    Dash,
    ProgressiveMp4,
}

/// Requested quality. `Best`/`Worst` are resolved against whatever ladder the host
/// actually offers; a specific height picks the closest available at or below it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum QualityPref {
    #[default]
    Best,
    Worst,
    #[serde(rename = "2160p")]
    P2160,
    #[serde(rename = "1440p")]
    P1440,
    #[serde(rename = "1080p")]
    P1080,
    #[serde(rename = "720p")]
    P720,
    #[serde(rename = "480p")]
    P480,
    #[serde(rename = "360p")]
    P360,
}

impl QualityPref {
    /// Target height in pixels, or `None` for the relative `Best`/`Worst` variants.
    pub fn target_height(self) -> Option<u32> {
        match self {
            Self::Best | Self::Worst => None,
            Self::P2160 => Some(2160),
            Self::P1440 => Some(1440),
            Self::P1080 => Some(1080),
            Self::P720 => Some(720),
            Self::P480 => Some(480),
            Self::P360 => Some(360),
        }
    }
}

impl std::str::FromStr for QualityPref {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "best" | "max" => Ok(Self::Best),
            "worst" | "min" => Ok(Self::Worst),
            "2160" | "2160p" | "4k" => Ok(Self::P2160),
            "1440" | "1440p" | "2k" => Ok(Self::P1440),
            "1080" | "1080p" | "fhd" => Ok(Self::P1080),
            "720" | "720p" | "hd" => Ok(Self::P720),
            "480" | "480p" => Ok(Self::P480),
            "360" | "360p" => Ok(Self::P360),
            other => Err(format!(
                "unknown quality `{other}` (expected best/worst/2160p/1440p/1080p/720p/480p/360p)"
            )),
        }
    }
}

/// A fully-resolved, directly playable stream.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Stream {
    pub url: Url,
    pub kind: StreamKind,
    pub height: Option<u32>,
    pub bitrate_kbps: Option<u32>,
    /// Headers the CDN requires. Rumble and Dailymotion both key on `Referer`;
    /// these are forwarded to the player rather than being used here.
    pub headers: HashMap<String, String>,
    /// Ask the player to resolve [`Stream::url`] itself, with this format selector.
    ///
    /// Set only when the host publishes no muxed rendition. Those CDNs reject the
    /// per-track URLs when a player fetches them directly — the picture never
    /// loads — so the page URL is handed over instead and the player's own
    /// extractor pairs video with audio. The selector carries the requested quality
    /// through, so capping still works.
    #[serde(default)]
    pub ytdl_format: Option<String>,
}

impl Stream {
    pub fn quality_label(&self) -> String {
        match self.height {
            Some(h) => format!("{h}p"),
            None => "unknown".to_string(),
        }
    }
}
