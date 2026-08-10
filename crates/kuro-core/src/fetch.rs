//! Shared HTTP context handed to every provider.
//!
//! Providers never construct their own client: routing all traffic through one
//! [`FetchCtx`] is what makes timeouts, retries, politeness delays and the shared
//! cookie jar uniform across scrapers.

use crate::error::ProviderError;
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
}

impl Default for FetchConfig {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(15),
            max_retries: 2,
            per_host_concurrency: 4,
            retry_backoff: Duration::from_millis(400),
        }
    }
}

#[derive(Clone)]
pub struct FetchCtx {
    client: reqwest::Client,
    config: FetchConfig,
    permits: Arc<Semaphore>,
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
        Ok(Self {
            client,
            config,
            permits,
        })
    }

    pub fn config(&self) -> &FetchConfig {
        &self.config
    }

    /// GET a URL and return the body as text, with retry on transient failures.
    pub async fn get_text(&self, url: &Url, referer: Option<&str>) -> Result<String, ProviderError> {
        self.get_text_with_ua(url, referer, None).await
    }

    /// As [`FetchCtx::get_text`], but allows a provider to override the User-Agent.
    pub async fn get_text_with_ua(
        &self,
        url: &Url,
        referer: Option<&str>,
        user_agent: Option<&str>,
    ) -> Result<String, ProviderError> {
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

            let mut req = self.client.get(url.clone());
            if let Some(r) = referer {
                req = req.header(reqwest::header::REFERER, r);
            }
            if let Some(ua) = user_agent {
                req = req.header(reqwest::header::USER_AGENT, ua);
            }

            let outcome = match req.send().await {
                Ok(resp) => Self::classify(resp).await,
                Err(e) if e.is_timeout() => Err(ProviderError::Timeout),
                Err(e) => Err(ProviderError::Network(e)),
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
