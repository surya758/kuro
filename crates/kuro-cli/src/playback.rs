//! The resolve-and-play pipeline.
//!
//! Mirrors are resolved to embed URLs concurrently, ordered by the user's host
//! preference, then tried in turn until one yields a playable stream. Only when
//! every mirror fails is the episode reported unplayable.

use crate::app::{require_player, App};
use anyhow::{Context, Result};
use futures::future::join_all;
use kuro_core::{Episode, Mirror, Provider, Series, Stream};
use kuro_player::{ipc, ipc_socket_path, PlaybackOpts, Player};
use kuro_providers::hosts;
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, warn};
use url::Url;

/// A mirror whose embed URL has been resolved and labelled by host.
pub struct ResolvedMirror {
    pub label: String,
    pub embed: Url,
}

/// Resolve every mirror's embed URL concurrently, discarding ones that fail.
async fn resolve_embeds(
    ctx: &kuro_core::FetchCtx,
    provider: &Arc<dyn Provider>,
    mirrors: Vec<Mirror>,
) -> Vec<ResolvedMirror> {
    let tasks = mirrors.into_iter().map(|mirror| {
        let provider = Arc::clone(provider);
        let ctx = ctx.clone();
        async move {
            match provider.embed_url(&ctx, &mirror).await {
                Ok(embed) => {
                    let label = embed
                        .host_str()
                        .map(hosts::host_label)
                        .unwrap_or_else(|| mirror.label.clone());
                    Some(ResolvedMirror { label, embed })
                }
                Err(e) => {
                    // A dead mirror is routine; the others still stand a chance.
                    debug!(mirror = mirror.index, error = %e, "mirror embed unavailable");
                    None
                }
            }
        }
    });

    join_all(tasks).await.into_iter().flatten().collect()
}

/// Order mirrors by the configured host preference, best first.
fn order_by_preference(
    mut mirrors: Vec<ResolvedMirror>,
    preference: &[String],
) -> Vec<ResolvedMirror> {
    mirrors.sort_by_key(|m| {
        let label = m.label.to_ascii_lowercase();
        preference
            .iter()
            .position(|p| label.contains(&p.to_ascii_lowercase()))
            .unwrap_or(usize::MAX)
    });
    mirrors
}

pub struct PlayRequest<'a> {
    pub provider: Arc<dyn Provider>,
    pub series: &'a Series,
    pub episode: &'a Episode,
    /// Explicit `--mirror` host filter.
    pub mirror: Option<String>,
    /// Episodes after this one, queued into the player playlist so its own
    /// next/previous controls work part-way through an episode.
    pub upcoming: &'a [Episode],
    pub start_secs: Option<u64>,
}

/// Resolve every mirror for an episode and return them best-first.
///
/// Shared by playback and downloading: both need the same "which host should we
/// try, in what order" answer.
/// `show_progress` draws a spinner for the embed-resolution step. Downloading
/// passes `false` because it already runs its own spinner and progress bars, and
/// two writers competing for the same line garbles both.
pub async fn ordered_mirrors(
    app: &mut App,
    provider: &Arc<dyn Provider>,
    episode: &Episode,
    mirror_filter: Option<&str>,
    show_progress: bool,
) -> Result<Vec<ResolvedMirror>> {
    let provider_id = provider.id();

    let mirrors = match provider.mirrors(&app.ctx, episode).await {
        Ok(m) => {
            app.note_success(&provider_id);
            m
        }
        Err(e) => {
            app.note_failure(&provider_id, &e.to_string());
            app.save_health().ok();
            let mut msg = format!("could not read mirrors for this episode: {e}");
            if let Some(hint) = e.hint() {
                msg.push_str(&format!("\n  hint: {hint}"));
            }
            anyhow::bail!(msg);
        }
    };

    // Each mirror costs a fetch, so this is the step that visibly stalls.
    let spinner = show_progress
        .then(|| crate::ui::Spinner::start(format!("Resolving {} mirror(s)…", mirrors.len())));

    let resolved = resolve_embeds(&app.ctx, provider, mirrors).await;

    if let Some(spinner) = spinner {
        spinner.clear().await;
    }

    if resolved.is_empty() {
        anyhow::bail!("no mirror on this episode exposed a usable video embed");
    }

    let preference = app.config.provider(provider_id.as_str()).mirrors;
    let mut ordered = order_by_preference(resolved, &preference);

    if let Some(wanted) = mirror_filter {
        let wanted_lower = wanted.to_ascii_lowercase();
        let before = ordered.len();
        ordered.retain(|m| m.label.to_ascii_lowercase().contains(&wanted_lower));
        if ordered.is_empty() {
            anyhow::bail!(
                "no mirror matching `{wanted}` on this episode ({before} mirror(s) available)"
            );
        }
    }

    Ok(ordered)
}

pub async fn play(app: &mut App, req: PlayRequest<'_>) -> Result<()> {
    let ordered =
        ordered_mirrors(app, &req.provider, req.episode, req.mirror.as_deref(), true).await?;

    let mut failures = Vec::new();

    for mirror in &ordered {
        // yt-dlp can sit here for several seconds per host.
        let spinner = crate::ui::Spinner::start(format!("Trying {}…", mirror.label));
        let result = app.resolver.resolve(&mirror.embed, app.quality).await;
        spinner.clear().await;

        let streams = match result {
            Ok(s) => s,
            Err(e) => {
                warn!(mirror = %mirror.label, error = %e, "resolution failed");
                eprintln!("  \x1b[2m{} unavailable\x1b[0m", mirror.label);
                failures.push(format!("{}: {e}", mirror.label));
                continue;
            }
        };

        let Some(stream) = streams.into_iter().next() else {
            failures.push(format!("{}: no playable formats", mirror.label));
            continue;
        };

        return launch(
            app,
            req.series,
            req.episode,
            &req.provider,
            req.upcoming,
            &stream,
            mirror,
            req.start_secs,
        )
        .await;
    }

    anyhow::bail!(
        "every mirror failed for this episode:\n  {}",
        failures.join("\n  ")
    );
}

/// Look up opening/ending times for this episode.
///
/// Best-effort throughout: a missing MyAnimeList match or an episode AniSkip has
/// never seen just means no skipping. AniSkip is crowd-sourced and its donghua
/// coverage is thin, so "not found" is the common case, not an error.
async fn resolve_skip(
    app: &App,
    series: &Series,
    episode: &Episode,
) -> Option<kuro_core::SkipTimes> {
    let Some(mal_id) = kuro_core::skip::lookup_mal_id(&app.ctx, &series.title).await else {
        eprintln!("  skip: no MyAnimeList match for this series");
        return None;
    };

    match kuro_core::skip::fetch_skip_times(&app.ctx, mal_id, episode.number).await {
        Some(times) => {
            eprintln!("  skip: {}", times.describe());
            Some(times)
        }
        None => {
            eprintln!(
                "  skip: no AniSkip data for episode {}",
                episode.number_label()
            );
            None
        }
    }
}

/// How many following episodes to queue into the player's playlist.
///
/// Small on purpose: each costs a scrape plus a yt-dlp call, and the resulting CDN
/// URLs are signed and short-lived, so queueing far ahead would hand the player
/// links that expire before it reaches them.
const LOOKAHEAD: usize = 2;

/// Resolve the next few episodes and append them to the running player's playlist.
///
/// This is what makes the player's own next/previous work part-way through an
/// episode. It runs on its own task so the current episode starts immediately;
/// each append lands whenever that episode finishes resolving.
///
/// Returns the episodes actually queued, in playlist order after the first.
fn spawn_lookahead(
    ctx: kuro_core::FetchCtx,
    provider: Arc<dyn Provider>,
    episodes: Vec<Episode>,
    quality: kuro_core::QualityPref,
    preference: Vec<String>,
    socket: std::path::PathBuf,
) -> tokio::task::JoinHandle<Vec<Episode>> {
    tokio::spawn(async move {
        let mut queued = Vec::new();
        if episodes.is_empty() {
            return queued;
        }

        // The socket only exists once the player has started mpv.
        for _ in 0..60 {
            if socket.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        if !socket.exists() {
            debug!("player IPC socket never appeared; playlist not extended");
            return queued;
        }

        let resolver = kuro_resolver::ResolverChain::default();

        for episode in episodes {
            let Ok(mirrors) = provider.mirrors(&ctx, &episode).await else {
                break;
            };
            let resolved = resolve_embeds(&ctx, &provider, mirrors).await;
            if resolved.is_empty() {
                break;
            }

            let ordered = order_by_preference(resolved, &preference);
            let mut appended = false;

            for m in &ordered {
                let Ok(streams) = resolver.resolve(&m.embed, quality).await else {
                    continue;
                };
                let Some(stream) = streams.into_iter().next() else {
                    continue;
                };
                appended = ipc::append_to_playlist(&socket, stream.url.as_str()).await;
                break;
            }

            // A gap would make the playlist order lie about which episode is which,
            // so stop at the first failure rather than skipping one.
            if !appended {
                break;
            }
            debug!(episode = episode.number, "queued for playback");
            queued.push(episode);
        }

        queued
    })
}

#[allow(clippy::too_many_arguments)]
async fn launch(
    app: &mut App,
    series: &Series,
    episode: &Episode,
    provider: &Arc<dyn Provider>,
    upcoming: &[Episode],
    stream: &Stream,
    mirror: &ResolvedMirror,
    start_secs: Option<u64>,
) -> Result<()> {
    let title = format!("{} · Episode {}", series.title, episode.number_label());

    let skip = if app.skip {
        resolve_skip(app, series, episode).await
    } else {
        None
    };

    // Only materialise the mpv script when there is actually something to skip.
    let skip_script = match skip {
        Some(_) => kuro_player::write_skip_script()
            .map_err(|e| warn!(error = %e, "could not write skip script"))
            .ok(),
        None => None,
    };

    let socket = ipc_socket_path();
    let opts = PlaybackOpts {
        title: Some(title.clone()),
        start_secs: start_secs.filter(|_| app.config.player.resume),
        fullscreen: app.config.player.fullscreen,
        ipc_socket: Some(socket.clone()),
        skip,
        skip_script,
    };

    let player = require_player(app).await?;

    if app.dry_run {
        let preview = player.command_preview(stream, &opts);
        println!("{}", preview.join(" "));
        return Ok(());
    }

    eprintln!(
        "▶  {title}  [{} · {}]",
        mirror.label,
        stream.quality_label()
    );

    let mut handle = player
        .play(stream, &opts)
        .await
        .context("launching the player")?;

    let socket_path = std::path::PathBuf::from(&socket);

    // Queue the next episodes so the player's own next/previous work mid-episode.
    let lookahead = spawn_lookahead(
        app.ctx.clone(),
        Arc::clone(provider),
        upcoming.iter().take(LOOKAHEAD).cloned().collect(),
        app.quality,
        app.config.provider(series.provider_id.as_str()).mirrors,
        socket_path.clone(),
    );

    // Track position alongside playback so the session can be resumed later.
    let tracker = tokio::spawn(async move {
        ipc::track_until_exit(&socket_path, Duration::from_secs(5), Duration::from_secs(30)).await
    });

    handle.wait().await.ok();
    let progress = tracker.await.unwrap_or_default();
    let queued = lookahead.await.unwrap_or_default();

    // The socket is ours; leaving it behind would litter the temp directory.
    std::fs::remove_file(&socket).ok();

    // Playlist index 0 is the episode we launched; the rest are what the lookahead
    // appended, in order. Recording per index means skipping ahead in the player
    // still credits the right episode.
    let mut history = app.history()?;
    let mut recorded = 0usize;

    for (index, progress) in progress {
        let Some(watched) = (if index == 0 {
            Some(episode)
        } else {
            queued.get(index - 1)
        }) else {
            continue;
        };

        history.record(
            series.provider_id.as_str(),
            &series.id,
            &series.title,
            series.url.as_str(),
            watched.number,
            progress.position_secs,
            progress.duration_secs,
        );
        recorded += 1;
        debug!(
            episode = watched.number,
            position = progress.position_secs,
            "recorded progress"
        );
    }

    if recorded > 0 {
        history.save(&app.paths).context("saving watch history")?;
    }

    app.save_health().ok();
    Ok(())
}

/// Episodes following `episode`, for playlist lookahead.
///
/// Compared by number rather than index so it behaves the same whether the caller
/// holds the full episode list or a filtered range.
pub fn upcoming_after<'a>(episodes: &'a [Episode], episode: &Episode) -> &'a [Episode] {
    match episodes.iter().position(|e| e.number > episode.number) {
        Some(i) => &episodes[i..],
        None => &[],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mirror(label: &str) -> ResolvedMirror {
        ResolvedMirror {
            label: label.to_string(),
            embed: Url::parse("https://example.com/embed").expect("valid url"),
        }
    }

    fn labels(mirrors: &[ResolvedMirror]) -> Vec<&str> {
        mirrors.iter().map(|m| m.label.as_str()).collect()
    }

    #[test]
    fn preferred_hosts_come_first() {
        let ordered = order_by_preference(
            vec![mirror("Rumble"), mirror("Dailymotion")],
            &["dailymotion".to_string()],
        );
        assert_eq!(labels(&ordered), vec!["Dailymotion", "Rumble"]);
    }

    #[test]
    fn unlisted_hosts_keep_their_relative_order_after_preferred_ones() {
        let ordered = order_by_preference(
            vec![mirror("Voe"), mirror("Rumble"), mirror("Dailymotion")],
            &["dailymotion".to_string(), "rumble".to_string()],
        );
        assert_eq!(labels(&ordered), vec!["Dailymotion", "Rumble", "Voe"]);
    }

    #[test]
    fn empty_preference_preserves_page_order() {
        let ordered = order_by_preference(vec![mirror("Rumble"), mirror("Dailymotion")], &[]);
        assert_eq!(labels(&ordered), vec!["Rumble", "Dailymotion"]);
    }
}
