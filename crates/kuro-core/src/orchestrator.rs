//! Cross-provider orchestration.
//!
//! The guarantee this module provides: **one misbehaving provider can never take
//! down a run**. Every provider call is wrapped in a timeout and a panic guard, so
//! a scraper that hangs, panics, or returns garbage degrades to "this provider
//! returned nothing" and the other providers still produce results.

use crate::error::ProviderError;
use crate::fetch::FetchCtx;
use crate::provider::Provider;
use crate::types::{ProviderId, Series};
use futures::future::join_all;
use futures::FutureExt;
use std::sync::Arc;
use std::time::Duration;
use tracing::{info, warn};

/// Outcome of querying a single provider.
pub struct ProviderOutcome<T> {
    pub provider_id: ProviderId,
    pub result: Result<T, ProviderError>,
    pub elapsed: Duration,
}

/// Results of a fan-out across every enabled provider, keeping successes and
/// failures side by side so the CLI can report both.
pub struct SearchResults {
    pub series: Vec<Series>,
    pub failures: Vec<(ProviderId, ProviderError)>,
}

/// Runs one provider call under a timeout and a panic guard.
///
/// `AssertUnwindSafe` is sound here because a poisoned scraper future is dropped
/// on panic and never observed again — nothing downstream reads its state.
pub async fn guarded<T, F>(provider_id: ProviderId, timeout: Duration, fut: F) -> ProviderOutcome<T>
where
    F: std::future::Future<Output = Result<T, ProviderError>>,
{
    let started = std::time::Instant::now();
    let guarded = std::panic::AssertUnwindSafe(fut).catch_unwind();

    let result = match tokio::time::timeout(timeout, guarded).await {
        Ok(Ok(inner)) => inner,
        Ok(Err(_panic)) => {
            warn!(provider = %provider_id, "scraper panicked — isolating");
            Err(ProviderError::Panic)
        }
        Err(_elapsed) => Err(ProviderError::Timeout),
    };

    ProviderOutcome {
        provider_id,
        result,
        elapsed: started.elapsed(),
    }
}

/// Search every supplied provider concurrently.
///
/// Providers are queried in parallel; `concurrency` bounds how many run at once so
/// a long provider list doesn't open a hundred sockets at the same time.
pub async fn search_all(
    providers: &[Arc<dyn Provider>],
    ctx: &FetchCtx,
    query: &str,
    timeout: Duration,
    concurrency: usize,
) -> SearchResults {
    let limiter = Arc::new(tokio::sync::Semaphore::new(concurrency.max(1)));

    let tasks = providers.iter().map(|p| {
        let provider = Arc::clone(p);
        let limiter = Arc::clone(&limiter);
        let ctx = ctx.clone();
        let query = query.to_string();

        async move {
            let _permit = limiter.acquire().await.expect("semaphore is never closed");
            let id = provider.id();
            let fut = async { provider.search(&ctx, &query).await };
            guarded(id, timeout, fut).await
        }
    });

    let outcomes = join_all(tasks).await;

    let mut series = Vec::new();
    let mut failures = Vec::new();

    for outcome in outcomes {
        match outcome.result {
            Ok(mut found) => {
                info!(
                    provider = %outcome.provider_id,
                    count = found.len(),
                    elapsed_ms = outcome.elapsed.as_millis(),
                    "search completed"
                );
                series.append(&mut found);
            }
            Err(err) => {
                warn!(provider = %outcome.provider_id, error = %err, "search failed");
                failures.push((outcome.provider_id, err));
            }
        }
    }

    SearchResults { series, failures }
}

/// Rank search results by how well the title matches the query, then by provider
/// priority. Higher `priority` wins ties so a preferred site floats to the top.
pub fn rank(series: &mut [Series], query: &str, priority_of: impl Fn(&ProviderId) -> i32) {
    let q = query.to_ascii_lowercase();

    series.sort_by(|a, b| {
        let sa = match_score(&a.title.to_ascii_lowercase(), &q);
        let sb = match_score(&b.title.to_ascii_lowercase(), &q);
        sb.partial_cmp(&sa)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| priority_of(&b.provider_id).cmp(&priority_of(&a.provider_id)))
            .then_with(|| a.title.cmp(&b.title))
    });
}

/// Cheap relevance score in `0.0..=1.0`.
///
/// An exact match beats a prefix match, which beats a substring match, which beats
/// partial token coverage. This is deliberately simple — search result sets here
/// are tens of items, not thousands.
fn match_score(title: &str, query: &str) -> f32 {
    if query.is_empty() {
        return 0.0;
    }
    if title == query {
        return 1.0;
    }
    if title.starts_with(query) {
        return 0.9;
    }
    if title.contains(query) {
        return 0.8;
    }

    let tokens: Vec<&str> = query.split_whitespace().collect();
    if tokens.is_empty() {
        return 0.0;
    }
    let hits = tokens.iter().filter(|t| title.contains(*t)).count();
    (hits as f32 / tokens.len() as f32) * 0.7
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_match_outranks_prefix_and_substring() {
        assert!(
            match_score("martial peak", "martial peak")
                > match_score("martial peak s2", "martial peak")
        );
        assert!(
            match_score("martial peak s2", "martial peak")
                > match_score("the martial peak", "martial peak")
        );
    }

    #[test]
    fn partial_token_coverage_scores_below_substring() {
        let partial = match_score("martial universe", "martial peak");
        assert!(partial > 0.0);
        assert!(partial < match_score("the martial peak", "martial peak"));
    }

    #[test]
    fn empty_query_scores_zero() {
        assert_eq!(match_score("anything", ""), 0.0);
    }
}
