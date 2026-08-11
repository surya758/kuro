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
    app: &App,
    provider: &Arc<dyn Provider>,
    mirrors: Vec<Mirror>,
) -> Vec<ResolvedMirror> {
    let tasks = mirrors.into_iter().map(|mirror| {
        let provider = Arc::clone(provider);
        let ctx = app.ctx.clone();
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
fn order_by_preference(mut mirrors: Vec<ResolvedMirror>, preference: &[String]) -> Vec<ResolvedMirror> {
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
    pub start_secs: Option<u64>,
}

/// Resolve every mirror for an episode and return them best-first.
///
/// Shared by playback and downloading: both need the same "which host should we
/// try, in what order" answer.
pub async fn ordered_mirrors(
    app: &mut App,
    provider: &Arc<dyn Provider>,
    episode: &Episode,
    mirror_filter: Option<&str>,
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

    eprintln!("  found {} mirror(s), resolving…", mirrors.len());

    let resolved = resolve_embeds(app, provider, mirrors).await;
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
        ordered_mirrors(app, &req.provider, req.episode, req.mirror.as_deref()).await?;

    let mut failures = Vec::new();

    for mirror in &ordered {
        eprintln!("  trying {} …", mirror.label);

        let streams = match app.resolver.resolve(&mirror.embed, app.quality).await {
            Ok(s) => s,
            Err(e) => {
                warn!(mirror = %mirror.label, error = %e, "resolution failed");
                failures.push(format!("{}: {e}", mirror.label));
                continue;
            }
        };

        let Some(stream) = streams.into_iter().next() else {
            failures.push(format!("{}: no playable formats", mirror.label));
            continue;
        };

        return launch(app, req.series, req.episode, &stream, mirror, req.start_secs).await;
    }

    anyhow::bail!(
        "every mirror failed for this episode:\n  {}",
        failures.join("\n  ")
    );
}

async fn launch(
    app: &mut App,
    series: &Series,
    episode: &Episode,
    stream: &Stream,
    mirror: &ResolvedMirror,
    start_secs: Option<u64>,
) -> Result<()> {
    let title = format!(
        "{} · Episode {}",
        series.title,
        episode.number_label()
    );

    let socket = ipc_socket_path();
    let opts = PlaybackOpts {
        title: Some(title.clone()),
        start_secs: start_secs.filter(|_| app.config.player.resume),
        fullscreen: app.config.player.fullscreen,
        ipc_socket: Some(socket.clone()),
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

    // Track position alongside playback so the session can be resumed later.
    let socket_path = std::path::PathBuf::from(&socket);
    let tracker = tokio::spawn(async move {
        ipc::track_until_exit(
            &socket_path,
            Duration::from_secs(5),
            Duration::from_secs(30),
        )
        .await
    });

    handle.wait().await.ok();
    let progress = tracker.await.ok().flatten();

    // The socket is ours; leaving it behind would litter the temp directory.
    std::fs::remove_file(&socket).ok();

    if let Some(progress) = progress {
        let mut history = app.history()?;
        history.record(
            series.provider_id.as_str(),
            &series.id,
            &series.title,
            series.url.as_str(),
            episode.number,
            progress.position_secs,
            progress.duration_secs,
        );
        history.save(&app.paths).context("saving watch history")?;
        debug!(position = progress.position_secs, "recorded progress");
    }

    app.save_health().ok();
    Ok(())
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
