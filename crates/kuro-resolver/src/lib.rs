//! Turning an embed URL into a directly playable stream.
//!
//! Providers stop at "here is the embed URL"; everything past that point lives
//! here. Most hosts are handled by [`ytdlp::YtDlpResolver`]; hosts it does not
//! cover get a native resolver implementing [`StreamResolver`].

pub mod ytdlp;

use async_trait::async_trait;
use kuro_core::{QualityPref, ResolveError, Stream};
use std::sync::Arc;
use tracing::debug;
use url::Url;

#[async_trait]
pub trait StreamResolver: Send + Sync {
    fn name(&self) -> &str;

    fn can_handle(&self, url: &Url) -> bool;

    /// Resolve to candidate streams, ranked best-first for `pref`.
    async fn resolve(&self, url: &Url, pref: QualityPref) -> Result<Vec<Stream>, ResolveError>;
}

/// Tries native resolvers in order, then falls back to the general one.
pub struct ResolverChain {
    native: Vec<Arc<dyn StreamResolver>>,
    fallback: Arc<dyn StreamResolver>,
}

impl ResolverChain {
    pub fn new(fallback: Arc<dyn StreamResolver>) -> Self {
        Self {
            native: Vec::new(),
            fallback,
        }
    }

    /// Register a host-specific resolver, consulted before the fallback.
    pub fn with_native(mut self, resolver: Arc<dyn StreamResolver>) -> Self {
        self.native.push(resolver);
        self
    }

    pub async fn resolve(
        &self,
        url: &Url,
        pref: QualityPref,
    ) -> Result<Vec<Stream>, ResolveError> {
        for resolver in &self.native {
            if resolver.can_handle(url) {
                debug!(resolver = resolver.name(), %url, "resolving with native extractor");
                return resolver.resolve(url, pref).await;
            }
        }

        debug!(resolver = self.fallback.name(), %url, "resolving with fallback");
        self.fallback.resolve(url, pref).await
    }
}

impl Default for ResolverChain {
    fn default() -> Self {
        Self::new(Arc::new(ytdlp::YtDlpResolver::default()))
    }
}
