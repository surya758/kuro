//! Shared HTTP context handed to every provider.
//!
//! Providers never construct their own client: routing all traffic through one
//! [`FetchCtx`] is what makes timeouts, retries, politeness delays and the shared
//! cookie jar uniform across scrapers.

use crate::cache::HttpCache;
use crate::error::ProviderError;
use crate::external::ExternalFetcher;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;
use tracing::{debug, warn};
use url::Url;

pub const DEFAULT_USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
     AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";

#[derive(Debug, Clone)]
pub struct FetchConfig {
    pub timeout: Duration,
    pub max_retries: u32,
    /// Maximum concurrent in-flight requests to a single provider host.
    pub per_host_concurrency: usize,
    /// Politeness delay applied between retries, doubled on each attempt.
    pub retry_backoff: Duration,
    /// Where to cache responses. `None` disables caching.
    pub cache_dir: Option<PathBuf>,
    /// Binary used for providers that ask to be fetched through a browser-shaped
    /// client. `None` leaves those providers unusable rather than failing others.
    pub impersonate_command: Option<String>,
}

/// Per-request knobs a provider can set.
///
/// A struct rather than more parameters: these travel together through several
/// layers, and three of the four call sites need to override only one of them.
#[derive(Debug, Clone, Copy, Default)]
pub struct FetchOpts<'a> {
    pub referer: Option<&'a str>,
    pub user_agent: Option<&'a str>,
    /// Route through the external client instead of the built-in one.
    pub impersonate: bool,
}

impl Default for FetchConfig {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(15),
            max_retries: 2,
            per_host_concurrency: 4,
            retry_backoff: Duration::from_millis(400),
            cache_dir: None,
            // Present by default so an installed binary just works; when it is
            // absent the error names it, which is more use than silence.
            impersonate_command: Some(crate::external::DEFAULT_IMPERSONATE_COMMAND.to_string()),
        }
    }
}

#[derive(Clone)]
pub struct FetchCtx {
    client: reqwest::Client,
    config: FetchConfig,
    permits: Arc<Semaphore>,
    cache: HttpCache,
    impersonator: Option<Arc<ExternalFetcher>>,
}

impl FetchCtx {
    pub fn new(config: FetchConfig) -> Result<Self, reqwest::Error> {
        let client = reqwest::Client::builder()
            .user_agent(DEFAULT_USER_AGENT)
            .timeout(config.timeout)
            .cookie_store(true)
            // Some providers redirect through interstitials before landing on the
            // real page; allow a generous but bounded chain.
            .redirect(reqwest::redirect::Policy::limited(10))
            .build()?;

        let permits = Arc::new(Semaphore::new(config.per_host_concurrency));
        let cache = HttpCache::new(config.cache_dir.clone());
        let impersonator = config
            .impersonate_command
            .as_ref()
            .map(|c| Arc::new(ExternalFetcher::new(c.clone())));

        Ok(Self {
            client,
            config,
            permits,
            cache,
            impersonator,
        })
    }

    /// The configured external client, if any.
    pub fn impersonator(&self) -> Option<&ExternalFetcher> {
        self.impersonator.as_deref()
    }

    pub fn config(&self) -> &FetchConfig {
        &self.config
    }

    pub fn cache(&self) -> &HttpCache {
        &self.cache
    }

    /// GET with caching. A fresh cached body short-circuits the request entirely.
    ///
    /// Only successful responses are cached — an error would otherwise be replayed
    /// for the whole TTL, turning a blip into a persistent outage.
    pub async fn get_cached(
        &self,
        url: &Url,
        referer: Option<&str>,
        user_agent: Option<&str>,
        ttl: Duration,
    ) -> Result<String, ProviderError> {
        self.get_cached_with(
            url,
            FetchOpts {
                referer,
                user_agent,
                impersonate: false,
            },
            ttl,
        )
        .await
    }

    /// As [`FetchCtx::get_cached`], honouring the full [`FetchOpts`].
    pub async fn get_cached_with(
        &self,
        url: &Url,
        opts: FetchOpts<'_>,
        ttl: Duration,
    ) -> Result<String, ProviderError> {
        let key = url.as_str();

        if let Some(body) = self.cache.get(key) {
            return Ok(body);
        }

        let body = self.get_with(url, opts).await?;
        self.cache.put(key, &body, ttl);
        Ok(body)
    }

    /// GET a URL and return the body as text, with retry on transient failures.
    pub async fn get_text(
        &self,
        url: &Url,
        referer: Option<&str>,
    ) -> Result<String, ProviderError> {
        self.get_text_with_ua(url, referer, None).await
    }

    /// As [`FetchCtx::get_text`], but allows a provider to override the User-Agent.
    pub async fn get_text_with_ua(
        &self,
        url: &Url,
        referer: Option<&str>,
        user_agent: Option<&str>,
    ) -> Result<String, ProviderError> {
        self.get_with(
            url,
            FetchOpts {
                referer,
                user_agent,
                impersonate: false,
            },
        )
        .await
    }

    /// GET with retry, using whichever client [`FetchOpts`] selects.
    ///
    /// Retries, backoff and the per-host permit apply identically to both clients,
    /// so a provider that needs the external one is no less polite than the rest.
    pub async fn get_with(&self, url: &Url, opts: FetchOpts<'_>) -> Result<String, ProviderError> {
        let _permit = self
            .permits
            .acquire()
            .await
            .expect("semaphore is never closed");

        let mut attempt = 0u32;
        let mut backoff = self.config.retry_backoff;

        loop {
            attempt += 1;
            debug!(%url, attempt, "fetching");

            let outcome = if opts.impersonate {
                match self.impersonator.as_ref() {
                    Some(fetcher) => {
                        fetcher
                            .get(url, opts.referer, opts.user_agent, self.config.timeout)
                            .await
                    }
                    None => Err(ProviderError::Config(
                        "this provider needs an external fetcher, but none is configured"
                            .to_string(),
                    )),
                }
            } else {
                let mut req = self.client.get(url.clone());
                if let Some(r) = opts.referer {
                    req = req.header(reqwest::header::REFERER, r);
                }
                if let Some(ua) = opts.user_agent {
                    req = req.header(reqwest::header::USER_AGENT, ua);
                }

                match req.send().await {
                    Ok(resp) => Self::classify(resp).await,
                    Err(e) if e.is_timeout() => Err(ProviderError::Timeout),
                    Err(e) => Err(ProviderError::Network(e)),
                }
            };

            match outcome {
                Ok(body) => return Ok(body),
                Err(err) => {
                    if attempt > self.config.max_retries || !err.is_retryable() {
                        return Err(err);
                    }
                    // Honour an explicit Retry-After when the server sent one,
                    // otherwise fall back to exponential backoff.
                    let wait = match &err {
                        ProviderError::RateLimited {
                            retry_after: Some(d),
                        } => *d,
                        _ => backoff,
                    };
                    warn!(%url, attempt, ?wait, error = %err, "retrying");
                    tokio::time::sleep(wait).await;
                    backoff *= 2;
                }
            }
        }
    }

    /// POST a JSON body and return the response text.
    ///
    /// Used for GraphQL metadata lookups. Deliberately not retried or cached —
    /// callers treat failure as "no data" and carry on.
    pub async fn post_json(
        &self,
        url: &Url,
        body: &serde_json::Value,
    ) -> Result<String, ProviderError> {
        let _permit = self
            .permits
            .acquire()
            .await
            .expect("semaphore is never closed");

        let resp = self
            .client
            .post(url.clone())
            .json(body)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    ProviderError::Timeout
                } else {
                    ProviderError::Network(e)
                }
            })?;

        Self::classify(resp).await
    }

    async fn classify(resp: reqwest::Response) -> Result<String, ProviderError> {
        let status = resp.status();

        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            let retry_after = resp
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok())
                .map(Duration::from_secs);
            return Err(ProviderError::RateLimited { retry_after });
        }

        // Cloudflare and friends use these for challenge pages.
        if matches!(status.as_u16(), 403 | 503) {
            return Err(ProviderError::Blocked);
        }

        if status == reqwest::StatusCode::NOT_FOUND {
            return Err(ProviderError::NotFound);
        }

        if !status.is_success() {
            return Err(ProviderError::Upstream {
                status: status.as_u16(),
            });
        }

        resp.text().await.map_err(ProviderError::Network)
    }
}
