//! A [`Provider`] driven entirely by a selector TOML.
//!
//! Sites built on the common WordPress anime themes all share the same shape:
//! a search page, a series page with an episode list, and an episode page with a
//! mirror `<select>`. Those need no Rust at all — only a spec.

use crate::json;
use crate::parse;
use crate::spec::{Format, ProviderSpec};
use async_trait::async_trait;
use kuro_core::cache::ttl;
use kuro_core::{
    Episode, FetchCtx, FetchOpts, Mirror, Provider, ProviderError, ProviderId, Series,
    SeriesDetails,
};
use std::time::Duration;
use tracing::debug;
use url::Url;

pub struct DeclarativeProvider {
    spec: ProviderSpec,
    id: ProviderId,
    base_url: Url,
}

impl DeclarativeProvider {
    pub fn new(spec: ProviderSpec) -> Result<Self, ProviderError> {
        let base_url = Url::parse(&spec.base_url).map_err(|e| {
            ProviderError::Config(format!("invalid base_url `{}`: {e}", spec.base_url))
        })?;
        let id = ProviderId::new(spec.id.clone());
        Ok(Self { spec, id, base_url })
    }

    pub fn from_toml(text: &str) -> Result<Self, ProviderError> {
        let spec = ProviderSpec::from_toml(text)
            .map_err(|e| ProviderError::Config(format!("invalid provider spec: {e}")))?;
        Self::new(spec)
    }

    pub fn spec(&self) -> &ProviderSpec {
        &self.spec
    }

    fn referer(&self) -> Option<&str> {
        self.spec.request.referer.as_deref()
    }

    fn user_agent(&self) -> Option<&str> {
        self.spec.request.user_agent.as_deref()
    }

    fn opts(&self) -> FetchOpts<'_> {
        FetchOpts {
            referer: self.referer(),
            user_agent: self.user_agent(),
            impersonate: self.spec.request.impersonate,
        }
    }

    /// Uncached fetch, for liveness probes that must reflect the site right now.
    async fn get(&self, ctx: &FetchCtx, url: &Url) -> Result<String, ProviderError> {
        ctx.get_with(url, self.opts()).await
    }

    async fn get_cached(
        &self,
        ctx: &FetchCtx,
        url: &Url,
        ttl: Duration,
    ) -> Result<String, ProviderError> {
        ctx.get_cached_with(url, self.opts(), ttl).await
    }

    fn search_url(&self, query: &str) -> Result<Url, ProviderError> {
        let encoded: String = url::form_urlencoded::byte_serialize(query.as_bytes()).collect();
        let path = self.spec.endpoints.search.replace("{query}", &encoded);
        self.base_url
            .join(&path)
            .map_err(|e| ProviderError::Config(format!("bad search endpoint `{path}`: {e}")))
    }
}

#[async_trait]
impl Provider for DeclarativeProvider {
    fn id(&self) -> ProviderId {
        self.id.clone()
    }

    fn display_name(&self) -> &str {
        &self.spec.display_name
    }

    fn base_url(&self) -> &Url {
        &self.base_url
    }

    async fn health_check(&self, ctx: &FetchCtx) -> Result<(), ProviderError> {
        self.get(ctx, &self.base_url).await.map(|_| ())
    }

    async fn search(&self, ctx: &FetchCtx, query: &str) -> Result<Vec<Series>, ProviderError> {
        let url = self.search_url(query)?;
        let html = self.get_cached(ctx, &url, ttl::SEARCH).await?;
        parse::parse_search(&html, &self.spec.selectors.search, &self.base_url, &self.id)
    }

    async fn series_details(
        &self,
        ctx: &FetchCtx,
        series: &Series,
    ) -> Result<SeriesDetails, ProviderError> {
        let html = self.get_cached(ctx, &series.url, ttl::EPISODES).await?;
        parse::parse_series_details(&html, &self.spec.selectors.series)
    }

    async fn episodes(
        &self,
        ctx: &FetchCtx,
        series: &Series,
    ) -> Result<Vec<Episode>, ProviderError> {
        // A JSON backend serves episodes from its own endpoint, keyed by id, rather
        // than rendering them into the series page.
        let url = match &self.spec.endpoints.episodes {
            Some(template) => {
                let path = json::fill_template(template, &series.id);
                self.base_url.join(&path).map_err(|e| {
                    ProviderError::Config(format!("bad episodes endpoint `{path}`: {e}"))
                })?
            }
            None => series.url.clone(),
        };

        let body = self.get_cached(ctx, &url, ttl::EPISODES).await?;
        let sel = &self.spec.selectors.episodes;
        match sel.format {
            Format::Json => json::parse_episodes(&body, sel, &series.id, &self.base_url),
            Format::Html => parse::parse_episodes(&body, sel, &series.id, &self.base_url),
        }
    }

    async fn mirrors(
        &self,
        ctx: &FetchCtx,
        episode: &Episode,
    ) -> Result<Vec<Mirror>, ProviderError> {
        let sel = &self.spec.selectors.mirrors;

        // Sites with one embed per episode have no mirror list to parse — the
        // episode page *is* the mirror. Declaring no `option` selector says so, and
        // answering before the fetch keeps it to a single request.
        if sel.format == Format::Html && sel.option.is_empty() {
            return Ok(vec![Mirror {
                index: 0,
                label: self.spec.display_name.clone(),
                page_url: episode.url.clone(),
                embed_url: None,
            }]);
        }

        let body = self.get_cached(ctx, &episode.url, ttl::MIRRORS).await?;
        match sel.format {
            Format::Json => json::parse_mirrors(&body, sel, &self.base_url),
            Format::Html => parse::parse_mirrors(
                &body,
                sel,
                &self.spec.selectors.embed,
                &self.base_url,
                &episode.url,
            ),
        }
    }

    async fn embed_url(&self, ctx: &FetchCtx, mirror: &Mirror) -> Result<Url, ProviderError> {
        if let Some(cached) = &mirror.embed_url {
            return Ok(cached.clone());
        }

        debug!(provider = %self.id, mirror = mirror.index, url = %mirror.page_url, "resolving embed");
        let html = self.get_cached(ctx, &mirror.page_url, ttl::MIRRORS).await?;
        parse::parse_embed(&html, &self.spec.selectors.embed, &self.base_url)
    }
}
