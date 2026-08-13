//! The provider extension point.
//!
//! Every site implements [`Provider`]. Adding a site means implementing this trait
//! (or, for sites that fit the common WordPress-anime-theme shape, writing a
//! selector TOML and letting the declarative implementation do it).

use crate::error::ProviderError;
use crate::fetch::FetchCtx;
use crate::types::{Episode, Mirror, ProviderId, Series, SeriesDetails};
use async_trait::async_trait;
use url::Url;

#[async_trait]
pub trait Provider: Send + Sync {
    fn id(&self) -> ProviderId;

    fn display_name(&self) -> &str;

    fn base_url(&self) -> &Url;

    /// Cheap liveness probe used by the health system and `kuro doctor`.
    async fn health_check(&self, ctx: &FetchCtx) -> Result<(), ProviderError>;

    async fn search(&self, ctx: &FetchCtx, query: &str) -> Result<Vec<Series>, ProviderError>;

    async fn series_details(
        &self,
        ctx: &FetchCtx,
        series: &Series,
    ) -> Result<SeriesDetails, ProviderError>;

    async fn episodes(
        &self,
        ctx: &FetchCtx,
        series: &Series,
    ) -> Result<Vec<Episode>, ProviderError>;

    async fn mirrors(
        &self,
        ctx: &FetchCtx,
        episode: &Episode,
    ) -> Result<Vec<Mirror>, ProviderError>;

    /// Resolve one mirror to the third-party embed URL (e.g. a Rumble embed page).
    ///
    /// The provider's responsibility ends here: turning an embed URL into a real
    /// stream is the resolver's job, not the scraper's.
    async fn embed_url(&self, ctx: &FetchCtx, mirror: &Mirror) -> Result<Url, ProviderError>;

    /// Catalogue browsing, where the site supports it. Providers that don't may
    /// leave this returning an empty list.
    async fn latest(&self, _ctx: &FetchCtx, _page: u32) -> Result<Vec<Series>, ProviderError> {
        Ok(Vec::new())
    }
}
