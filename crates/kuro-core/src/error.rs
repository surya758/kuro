//! Typed provider failures.
//!
//! The variants exist so the orchestrator can react correctly instead of treating
//! every failure as "site down". In particular [`ProviderError::ParseFailure`] means
//! *the site changed its markup* and carries the selector that stopped matching, so
//! the fix (edit a selector TOML) can be named precisely in the error message.

use std::time::Duration;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),

    #[error("request timed out")]
    Timeout,

    #[error("rate limited by provider")]
    RateLimited { retry_after: Option<Duration> },

    #[error("blocked by anti-bot protection (Cloudflare/WAF challenge)")]
    Blocked,

    #[error("selector `{selector}` matched nothing while parsing {context}")]
    ParseFailure { selector: String, context: String },

    #[error("not found")]
    NotFound,

    #[error("provider returned HTTP {status}")]
    Upstream { status: u16 },

    #[error("provider misconfigured: {0}")]
    Config(String),

    #[error("provider panicked while scraping (this is a bug in the scraper)")]
    Panic,
}

impl ProviderError {
    pub fn parse(selector: impl Into<String>, context: impl Into<String>) -> Self {
        Self::ParseFailure {
            selector: selector.into(),
            context: context.into(),
        }
    }

    /// Whether retrying could plausibly succeed.
    ///
    /// Retrying a selector that no longer matches only wastes time and hammers the
    /// site, so parse and lookup failures are deliberately not retryable.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::Network(_) | Self::Timeout | Self::RateLimited { .. } | Self::Upstream { .. }
        )
    }

    /// Whether this failure should count toward auto-disabling the provider.
    ///
    /// A missing series is a normal outcome of a search, not evidence the site is
    /// unhealthy, so `NotFound` is excluded.
    pub fn counts_against_health(&self) -> bool {
        !matches!(self, Self::NotFound)
    }

    /// A short actionable hint shown alongside the error in CLI output.
    pub fn hint(&self) -> Option<&'static str> {
        match self {
            Self::ParseFailure { .. } => {
                Some("the site's markup changed — update this provider's selector TOML")
            }
            Self::Blocked => Some("try again later, or disable this provider"),
            Self::RateLimited { .. } => Some("lower `general.concurrency` in your config"),
            Self::Panic => Some("please report this scraper bug"),
            _ => None,
        }
    }
}

#[derive(Debug, Error)]
pub enum ResolveError {
    #[error("no resolver registered for host `{host}`")]
    UnsupportedHost { host: String },

    #[error("yt-dlp is not installed or not on PATH")]
    YtDlpMissing,

    #[error("yt-dlp failed: {0}")]
    YtDlp(String),

    #[error("no playable formats found for {url}")]
    NoFormats { url: String },

    #[error("failed to parse resolver output: {0}")]
    BadOutput(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Error)]
pub enum PlayerError {
    #[error("player `{0}` was not found — install IINA from https://iina.io")]
    NotFound(String),

    #[error("failed to launch player: {0}")]
    Launch(#[from] std::io::Error),

    #[error("player exited with status {0}")]
    Exited(i32),
}
