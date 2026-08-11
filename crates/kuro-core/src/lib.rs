//! Core domain model and provider abstraction for `kuro`.
//!
//! This crate deliberately knows nothing about HTML, `yt-dlp`, or IINA. It defines
//! *what* a provider is and orchestrates calls across many of them; the scraping,
//! stream resolution, and playback layers depend on this crate, never the reverse.

pub mod cache;
pub mod error;
pub mod fetch;
pub mod orchestrator;
pub mod provider;
pub mod types;

pub use cache::HttpCache;
pub use error::{PlayerError, ProviderError, ResolveError};
pub use fetch::{FetchConfig, FetchCtx, DEFAULT_USER_AGENT};
pub use provider::Provider;
pub use types::{
    Episode, Mirror, ProviderId, QualityPref, Series, SeriesDetails, SeriesStatus, Stream,
    StreamKind,
};
