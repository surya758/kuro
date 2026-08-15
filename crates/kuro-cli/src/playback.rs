//! The resolve-and-play pipeline.
//!
//! Mirrors are resolved to embed URLs concurrently, ordered by the user's host
//! preference, then tried in turn until one yields a playable stream. Only when
//! every mirror fails is the episode reported unplayable.

use crate::app::{require_player_for, App};
use anyhow::{Context, Result};
use futures::future::join_all;
use kuro_core::{Episode, Mirror, Provider, QualityPref, Series, Stream};
use kuro_player::{ipc, ipc_socket_path, PlaybackOpts};
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

/// Play an episode, returning the episode number the session actually ended on.
///
/// That is not always the one passed in: the player's own next/previous controls
/// move through the queued playlist, so the viewer can finish several episodes
/// further along. `None` when nothing was tracked — a dry run, or a session too
/// short for the recorder to see anything.
pub async fn play(app: &mut App, req: PlayRequest<'_>) -> Result<Option<f32>> {
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

/// Consecutive idle polls before an episode counts as finished.
///
/// At one poll every two seconds this is a few seconds of the player holding no
/// file — long enough that queueing up the next episode cannot trip it, short
/// enough that closing the window does not feel like a hang.
const IDLE_POLLS_BEFORE_DONE: u8 = 3;

/// Resolve the next few episodes and append them to the running player's playlist.
///
/// This is what makes the player's own next/previous work part-way through an
/// episode. It runs on its own task so the current episode starts immediately;
/// each append lands whenever that episode finishes resolving.
///
/// Returns the episodes actually queued, in playlist order after the first.
struct LookaheadJob {
    ctx: kuro_core::FetchCtx,
    provider: Arc<dyn Provider>,
    episodes: Vec<Episode>,
    quality: kuro_core::QualityPref,
    preference: Vec<String>,
    socket: std::path::PathBuf,
    /// Playlist index -> (episode number, display title).
    playlist: Arc<std::sync::Mutex<Vec<(f32, String)>>>,
    series_title: String,
}

fn spawn_lookahead(job: LookaheadJob) -> tokio::task::JoinHandle<Vec<Episode>> {
    tokio::spawn(async move {
        let LookaheadJob {
            ctx,
            provider,
            episodes,
            quality,
            preference,
            socket,
            playlist,
            series_title,
        } = job;
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
                let label = format!("{series_title} · Episode {}", episode.number_label());
                appended = ipc::append_to_playlist(&socket, stream.url.as_str(), &label).await;
                break;
            }

            // A gap would make the playlist order lie about which episode is which,
            // so stop at the first failure rather than skipping one.
            if !appended {
                break;
            }
            debug!(episode = episode.number, "queued for playback");
            // Keep the index -> episode map in step with the player's playlist so
            // the recorder credits the right episode if the viewer skips ahead.
            if let Ok(mut p) = playlist.lock() {
                p.push((
                    episode.number,
                    format!("{series_title} · Episode {}", episode.number_label()),
                ));
            }
            queued.push(episode);
        }

        queued
    })
}

/// Series identity for the background recorder, which cannot borrow `App`.
#[derive(Clone)]
struct HistoryTarget {
    provider_id: String,
    series_id: String,
    series_title: String,
    series_url: String,
}

/// Persist watch progress *while* the episode plays, not only once it ends.
///
/// Writing only on exit meant Ctrl-C — the natural way to get the prompt back while
/// the player keeps running — discarded the entire session. Checkpointing every few
/// seconds means at most one interval is ever lost.
///
/// `playlist` maps playlist index to episode number and grows as the lookahead
/// queues more, so skipping ahead in the player credits the right episode.
///
/// `last_played` receives the episode number behind each checkpoint, so the caller
/// can tell where the viewer actually ended up after using the player's own
/// next/previous controls.
fn spawn_progress_recorder(
    socket: std::path::PathBuf,
    target: HistoryTarget,
    playlist: Arc<std::sync::Mutex<Vec<(f32, String)>>>,
    last_played: Arc<std::sync::Mutex<Option<f32>>>,
    player_gone: Arc<tokio::sync::Notify>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        // Keep watching the socket even when history cannot be written: the caller
        // relies on this task to notice the player closing.
        let paths = kuro_store::Paths::discover().ok();
        if paths.is_none() {
            warn!("cannot locate data directory; progress will not be saved");
        }

        let started = std::time::Instant::now();
        let mut last: std::collections::BTreeMap<usize, u64> = std::collections::BTreeMap::new();
        let mut current_index: Option<usize> = None;
        let mut seen_player = false;
        let mut idle_polls = 0u8;
        let mut start_cleared = false;

        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;

            let (index, progress) = match ipc::poll_progress(&socket).await {
                ipc::Poll::Progress(index, progress) => {
                    seen_player = true;
                    idle_polls = 0;
                    // The launch-time `--start` has done its job by the time the
                    // player reports a position, and mpv would otherwise re-apply
                    // it to every episode the lookahead queues.
                    if !start_cleared {
                        ipc::clear_start(&socket).await;
                        start_cleared = true;
                    }
                    (index, progress)
                }
                // mpv has no position between playlist entries, which is exactly
                // what skipping to the next episode looks like. Keep polling: the
                // player is still there and the next episode is about to start.
                ipc::Poll::Unavailable => {
                    seen_player = true;
                    idle_polls = 0;
                    continue;
                }
                ipc::Poll::Idle => {
                    seen_player = true;
                    idle_polls += 1;
                    // A player holding no file has either been closed or run out
                    // of playlist. Requiring several in a row keeps a slow load
                    // between episodes from being mistaken for either.
                    if idle_polls >= IDLE_POLLS_BEFORE_DONE {
                        debug!("player idle; treating episode as finished");
                        player_gone.notify_one();
                        return;
                    }
                    continue;
                }
                ipc::Poll::Closed => {
                    // Until the player has answered once, a dead socket only means
                    // it has not started yet — hence the grace period. After that,
                    // or once it has answered at all, the episode is over.
                    if seen_player || started.elapsed() > Duration::from_secs(30) {
                        debug!("player IPC closed; progress recorder stopping");
                        // Closing IINA's window with ⌘W leaves the app — and so the
                        // launcher process — running, so waiting on that process
                        // alone would hang here forever. The socket dying is the
                        // signal that actually tracks the window.
                        player_gone.notify_one();
                        return;
                    }
                    continue;
                }
            };

            // Avoid rewriting the file while paused.
            if last.get(&index) == Some(&progress.position_secs) {
                continue;
            }
            last.insert(index, progress.position_secs);

            let entry = playlist.lock().ok().and_then(|p| p.get(index).cloned());
            let Some((number, entry_title)) = entry else {
                continue;
            };

            if let Ok(mut last_played) = last_played.lock() {
                *last_played = Some(number);
            }

            // `force-media-title` is global, so without this the window keeps
            // showing the first episode's name for every later entry.
            if current_index != Some(index) {
                ipc::set_media_title(&socket, &entry_title).await;
                current_index = Some(index);
            }

            let Some(paths) = paths.as_ref() else {
                continue;
            };
            let Ok(mut history) = kuro_store::History::load(paths) else {
                continue;
            };
            history.record(
                &target.provider_id,
                &target.series_id,
                &target.series_title,
                &target.series_url,
                number,
                progress.position_secs,
                progress.duration_secs,
            );
            // Keep polling on a write failure rather than stopping: this task is
            // also what tells the caller the player has closed.
            if let Err(e) = history.save(paths) {
                warn!(error = %e, "could not save watch history");
                continue;
            }
            debug!(
                episode = number,
                position = progress.position_secs,
                "checkpointed"
            );
        }
    })
}

/// Explains a rendition that came back below what was asked for.
///
/// A specific quality is a ceiling, not a demand: the resolver caps the format
/// selector at the requested height and then takes the closest rung at or below it,
/// so asking for 2160p on a host whose ladder stops at 1080p plays rather than
/// fails. That is the right behaviour, but silently handing back something lower
/// reads as a bug — most of these embed hosts never exceed 1080p. Say so once here.
fn clamp_note(requested: QualityPref, actual: Option<u32>) -> String {
    match (requested.target_height(), actual) {
        (Some(target), Some(actual)) if actual < target => {
            format!(" — {target}p requested, host's best is {actual}p")
        }
        _ => String::new(),
    }
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
) -> Result<Option<f32>> {
    let title = format!("{} · Episode {}", series.title, episode.number_label());
    let requested = app.quality;

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

    kuro_player::sweep_stale_sockets();
    let socket = ipc_socket_path();
    let opts = PlaybackOpts {
        title: Some(title.clone()),
        start_secs: start_secs.filter(|_| app.config.player.resume),
        fullscreen: app.config.player.fullscreen,
        ipc_socket: Some(socket.clone()),
        skip,
        skip_script,
    };

    let player = require_player_for(app, stream).await?;

    if app.dry_run {
        let preview = player.command_preview(stream, &opts);
        println!("{}", preview.join(" "));
        return Ok(None);
    }

    eprintln!(
        "{}  {title}  {}",
        crate::ui::style::accent("▶"),
        crate::ui::style::dim(format!(
            "[{} · {}{}]",
            mirror.label,
            stream.quality_label(),
            clamp_note(requested, stream.height),
        )),
    );

    // The ⏪/⏩ buttons on IINA's on-screen controls are seek/speed, not playlist
    // navigation — worth saying, because reaching for them is the obvious move.
    if !upcoming.is_empty() {
        eprintln!("   \x1b[2m⌘→ / ⌘← in IINA switch episode · ⇧⌘P for the playlist\x1b[0m");
    }
    eprintln!("   \x1b[2mkuro stays open to save your progress\x1b[0m");

    let mut handle = player
        .play(stream, &opts)
        .await
        .context("launching the player")?;

    let socket_path = std::path::PathBuf::from(&socket);

    // Playlist index -> episode number. Seeded with what we just launched; the
    // lookahead appends as it queues more.
    let playlist = Arc::new(std::sync::Mutex::new(vec![(episode.number, title.clone())]));

    // Queue the next episodes so the player's own next/previous work mid-episode.
    let lookahead = spawn_lookahead(LookaheadJob {
        ctx: app.ctx.clone(),
        provider: Arc::clone(provider),
        episodes: upcoming.iter().take(LOOKAHEAD).cloned().collect(),
        quality: app.quality,
        preference: app.config.provider(series.provider_id.as_str()).mirrors,
        socket: socket_path.clone(),
        playlist: Arc::clone(&playlist),
        series_title: series.title.clone(),
    });

    // Tracks which episode the viewer actually finished on, which is not the one
    // launched if they used the player's own next/previous controls.
    let last_played = Arc::new(std::sync::Mutex::new(None));

    // Raised by the recorder when the IPC socket dies.
    let player_gone = Arc::new(tokio::sync::Notify::new());

    // Save progress as it happens, so quitting kuro does not discard the session.
    let recorder = spawn_progress_recorder(
        socket_path,
        HistoryTarget {
            provider_id: series.provider_id.to_string(),
            series_id: series.id.clone(),
            series_title: series.title.clone(),
            series_url: series.url.to_string(),
        },
        playlist,
        Arc::clone(&last_played),
        Arc::clone(&player_gone),
    );

    // Two different things end an episode, and neither implies the other. ⌘Q quits
    // IINA, so the launcher process exits. ⌘W closes only the window: the app keeps
    // running, the launcher never returns, and waiting on it alone hangs forever.
    // Whichever happens first means this episode is done.
    tokio::select! {
        _ = handle.wait() => {}
        _ = player_gone.notified() => debug!("player window closed"),
    }

    // Progress was already checkpointed as it played; these just stop cleanly.
    recorder.abort();
    lookahead.abort();

    // The socket is ours; leaving it behind would litter the temp directory.
    std::fs::remove_file(&socket).ok();

    app.save_health().ok();
    Ok(last_played.lock().ok().and_then(|last| *last))
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

    #[test]
    fn asking_above_the_host_ladder_is_explained() {
        // The reference case: Dailymotion tops out at 1080p, so 2160p cannot be
        // honoured and the gap has to be visible rather than silent.
        let note = clamp_note(QualityPref::P2160, Some(1080));
        assert!(note.contains("2160p"), "{note}");
        assert!(note.contains("1080p"), "{note}");
    }

    #[test]
    fn an_honoured_request_says_nothing() {
        assert_eq!(clamp_note(QualityPref::P1080, Some(1080)), "");
        // Over-delivery is not worth a remark either.
        assert_eq!(clamp_note(QualityPref::P720, Some(1080)), "");
    }

    #[test]
    fn relative_preferences_never_clamp() {
        // `best`/`worst` are defined by the host's ladder, so they cannot fall short.
        assert_eq!(clamp_note(QualityPref::Best, Some(480)), "");
        assert_eq!(clamp_note(QualityPref::Worst, Some(288)), "");
    }

    #[test]
    fn an_unknown_height_stays_quiet() {
        // Some extractors omit `height`; guessing a shortfall would be worse than
        // saying nothing.
        assert_eq!(clamp_note(QualityPref::P2160, None), "");
    }
}
