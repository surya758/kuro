//! Command implementations.

use crate::app::App;
use crate::cli::{BookmarkAction, CacheAction, ConfigAction, EpisodeSpec, ProviderAction};
use crate::playback::{play, PlayRequest};
use anyhow::{Context, Result};
use kuro_core::{orchestrator, Episode, Provider, Series, SeriesStatus};
use kuro_store::{Bookmark, Bookmarks, History};
use std::sync::Arc;
use std::time::Instant;
use url::Url;

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Search every active provider, ranked, recording health along the way.
pub(crate) async fn search_ranked(app: &mut App, query: &str) -> Result<Vec<Series>> {
    let providers = app.active_providers();
    if providers.is_empty() {
        anyhow::bail!(
            "no providers are active — check `kuro provider list`{}",
            app.provider_filter
                .as_ref()
                .map(|f| format!(" (filtered to `{f}`)"))
                .unwrap_or_default()
        );
    }

    let results = orchestrator::search_all(
        &providers,
        &app.ctx,
        query,
        app.config.general.search_timeout,
        app.config.general.concurrency,
    )
    .await;

    // Any provider that returned results is healthy; the rest count against it.
    let succeeded: Vec<_> = providers
        .iter()
        .map(|p| p.id())
        .filter(|id| !results.failures.iter().any(|(fid, _)| fid == id))
        .collect();

    for id in succeeded {
        app.note_success(&id);
    }
    for (id, err) in &results.failures {
        app.note_failure(id, &err.to_string());
        eprintln!("⚠  {id}: {err}");
        if let Some(hint) = err.hint() {
            eprintln!("   hint: {hint}");
        }
    }
    app.save_health().ok();

    let mut series = results.series;
    let config = &app.config;
    orchestrator::rank(&mut series, query, |id| config.priority(id.as_str()));
    Ok(series)
}

/// Choose one result: the Nth when `--select-nth` was given, else the best match.
fn pick_result(app: &App, results: &[Series], query: &str) -> Result<Series> {
    if results.is_empty() {
        anyhow::bail!("no results for `{query}`");
    }

    match app.select_nth {
        None => Ok(results[0].clone()),
        Some(n) => {
            // 1-based, matching how results are printed.
            let index = n
                .checked_sub(1)
                .context("--select-nth is 1-based; 0 is not a result")?;
            results.get(index).cloned().with_context(|| {
                format!(
                    "--select-nth {n} is out of range ({} result(s))",
                    results.len()
                )
            })
        }
    }
}

/// Episodes selected by a spec, erroring with the available range when empty.
fn episodes_for_spec(episodes: &[Episode], spec: EpisodeSpec) -> Result<Vec<&Episode>> {
    let picked: Vec<&Episode> = episodes
        .iter()
        .filter(|e| !spec.select(&[e.number]).is_empty())
        .collect();

    if picked.is_empty() {
        let available = match (episodes.first(), episodes.last()) {
            (Some(f), Some(l)) => format!("{} – {}", f.number_label(), l.number_label()),
            _ => "none".to_string(),
        };
        anyhow::bail!("no episode matches `{spec}` (available: {available})");
    }
    Ok(picked)
}

pub(crate) fn provider_for(app: &App, series: &Series) -> Result<Arc<dyn Provider>> {
    app.provider(series.provider_id.as_str())
        .with_context(|| format!("provider `{}` is not loaded", series.provider_id))
}

fn print_series_list(series: &[Series]) {
    for (i, s) in series.iter().enumerate() {
        // Most titles already carry their year, so only append it when it's missing.
        let year = s
            .year
            .filter(|y| !s.title.contains(&y.to_string()))
            .map(|y| format!(" ({y})"))
            .unwrap_or_default();
        println!(
            "{:>3}. {}{}  {}",
            i + 1,
            s.title,
            year,
            crate::ui::dim_stdout(format!("[{}]", s.provider_id))
        );
    }
}

/// Fetch episodes, converting provider failure into an actionable message.
pub(crate) async fn episodes_of(
    app: &mut App,
    provider: &Arc<dyn Provider>,
    series: &Series,
) -> Result<Vec<Episode>> {
    match provider.episodes(&app.ctx, series).await {
        Ok(eps) => {
            app.note_success(&provider.id());
            app.save_health().ok();
            Ok(eps)
        }
        Err(e) => {
            app.note_failure(&provider.id(), &e.to_string());
            app.save_health().ok();
            let mut msg = format!("could not read the episode list: {e}");
            if let Some(hint) = e.hint() {
                msg.push_str(&format!("\n  hint: {hint}"));
            }
            anyhow::bail!(msg)
        }
    }
}

fn pick_episode(episodes: &[Episode], wanted: f32) -> Result<&Episode> {
    episodes
        .iter()
        .find(|e| (e.number - wanted).abs() < f32::EPSILON)
        .with_context(|| {
            let available = match (episodes.first(), episodes.last()) {
                (Some(f), Some(l)) => format!("{} – {}", f.number_label(), l.number_label()),
                _ => "none".to_string(),
            };
            format!("episode {wanted} not found (available: {available})")
        })
}

/// Rebuild a minimal [`Series`] from a history entry so its episodes can be refetched.
fn series_from_history(entry: &kuro_store::HistoryEntry) -> Result<Series> {
    let url = Url::parse(&entry.series_url).with_context(|| {
        format!(
            "history entry for `{}` has no usable series URL — play it once more from search",
            entry.series_title
        )
    })?;

    Ok(Series {
        provider_id: entry.provider_id.as_str().into(),
        id: entry.series_id.clone(),
        title: entry.series_title.clone(),
        url,
        poster: None,
        year: None,
        synopsis: None,
        genres: Vec::new(),
        status: SeriesStatus::Unknown,
        total_episodes: None,
    })
}

// ---------------------------------------------------------------------------
// search
// ---------------------------------------------------------------------------

pub async fn search(app: &mut App, query: &str) -> Result<()> {
    let query = query.trim().to_string();
    if query.is_empty() {
        anyhow::bail!("give me something to search for");
    }

    let series = search_ranked(app, &query).await?;

    if app.json {
        println!("{}", serde_json::to_string_pretty(&series)?);
        return Ok(());
    }

    if series.is_empty() {
        println!("No results for `{query}`.");
        return Ok(());
    }

    print_series_list(&series);
    Ok(())
}

// ---------------------------------------------------------------------------
// watch / play
// ---------------------------------------------------------------------------

pub async fn play_cmd(
    app: &mut App,
    query: &str,
    ep: Option<EpisodeSpec>,
    mirror: Option<String>,
) -> Result<()> {
    let query = query.trim().to_string();
    let results = search_ranked(app, &query).await?;
    let chosen = pick_result(app, &results, &query)?;

    eprintln!("→ {} [{}]", chosen.title, chosen.provider_id);

    let provider = provider_for(app, &chosen)?;
    let episodes = episodes_of(app, &provider, &chosen).await?;

    let queue: Vec<Episode> = match ep {
        Some(spec) => episodes_for_spec(&episodes, spec)?
            .into_iter()
            .cloned()
            .collect(),
        None => {
            // No episode given: continue where this series left off.
            let history = app.history()?;
            let next = history
                .last_completed_episode(chosen.provider_id.as_str(), &chosen.id)
                .and_then(|last| episodes.iter().find(|e| e.number > last).cloned());

            vec![match next {
                Some(e) => e,
                None => episodes
                    .first()
                    .context("this series has no episodes")?
                    .clone(),
            }]
        }
    };

    if queue.len() > 1 {
        eprintln!("  queued {} episodes", queue.len());
    }

    for (i, episode) in queue.iter().enumerate() {
        if queue.len() > 1 {
            eprintln!(
                "\n[{}/{}] Episode {}",
                i + 1,
                queue.len(),
                episode.number_label()
            );
        }

        let start =
            app.history()?
                .resume_position(chosen.provider_id.as_str(), &chosen.id, episode.number);

        play(
            app,
            PlayRequest {
                provider: Arc::clone(&provider),
                series: &chosen,
                episode,
                mirror: mirror.clone(),
                upcoming: crate::playback::upcoming_after(&episodes, episode),
                start_secs: start,
            },
        )
        .await?;
    }

    Ok(())
}

/// Strip characters that are awkward or illegal in filenames.
fn safe_filename(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '-',
            // Control characters would produce unusable names.
            c if c.is_control() => ' ',
            c => c,
        })
        .collect();
    cleaned.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub async fn download(
    app: &mut App,
    query: &str,
    ep: Option<EpisodeSpec>,
    all: bool,
    mirror: Option<String>,
    out: &std::path::Path,
    jobs: usize,
) -> Result<()> {
    let ytdlp = kuro_resolver::ytdlp::YtDlpResolver::default();
    if !ytdlp.is_available().await {
        anyhow::bail!("yt-dlp is required for downloads — install it with: brew install yt-dlp");
    }

    let query = query.trim().to_string();
    let results = search_ranked(app, &query).await?;
    let chosen = pick_result(app, &results, &query)?;

    eprintln!("→ {} [{}]", chosen.title, chosen.provider_id);

    let provider = provider_for(app, &chosen)?;
    let episodes = episodes_of(app, &provider, &chosen).await?;

    let wanted: Vec<Episode> = if all {
        episodes.clone()
    } else {
        let spec = ep.context(
            "give an episode with --ep 15, a range with --ep 1-5, or --all for the series",
        )?;
        episodes_for_spec(&episodes, spec)?
            .into_iter()
            .cloned()
            .collect()
    };

    std::fs::create_dir_all(out)
        .with_context(|| format!("creating output directory {}", out.display()))?;

    download_episodes(
        app,
        &chosen,
        &provider,
        &wanted,
        mirror.as_deref(),
        out,
        jobs,
    )
    .await
}

/// Download a set of episodes, drawing a progress bar per episode.
///
/// Mirror resolution needs `&mut App` for health tracking, so it happens up front;
/// only the downloads themselves run in parallel. Resolution is cheap next to a
/// download, and pages are cached.
pub async fn download_episodes(
    app: &mut App,
    series: &Series,
    provider: &Arc<dyn Provider>,
    wanted: &[Episode],
    mirror: Option<&str>,
    out: &std::path::Path,
    jobs: usize,
) -> Result<()> {
    let ytdlp = kuro_resolver::ytdlp::YtDlpResolver::default();
    if !ytdlp.is_available().await {
        anyhow::bail!("yt-dlp is required for downloads — install it with: brew install yt-dlp");
    }

    std::fs::create_dir_all(out)
        .with_context(|| format!("creating output directory {}", out.display()))?;

    let mut failed = Vec::new();
    let mut ready = Vec::new();

    let spinner = crate::ui::Spinner::start(format!("Resolving {} episode(s)…", wanted.len()));
    for episode in wanted {
        match crate::playback::ordered_mirrors(app, provider, episode, mirror, false).await {
            Ok(mirrors) => ready.push((episode.clone(), mirrors)),
            Err(_) => failed.push(episode.number_label()),
        }
    }
    spinner.clear().await;

    if ready.is_empty() {
        anyhow::bail!("no episode could be resolved to a mirror");
    }

    eprintln!("Downloading {} episode(s) → {}", ready.len(), out.display());

    let labels: Vec<String> = ready
        .iter()
        .map(|(e, _)| format!("E{}", e.number_label()))
        .collect();
    let display = crate::ui::ProgressHandle::start(labels);

    let limiter = Arc::new(tokio::sync::Semaphore::new(jobs.max(1)));
    let ytdlp = Arc::new(ytdlp);
    let quality = app.quality;
    let series_title = series.title.clone();

    let tasks = ready
        .into_iter()
        .enumerate()
        .map(|(slot, (episode, mirrors))| {
            let limiter = Arc::clone(&limiter);
            let ytdlp = Arc::clone(&ytdlp);
            let out = out.to_path_buf();
            let series_title = series_title.clone();
            let progress = display.handle();

            async move {
                let _permit = limiter.acquire().await.expect("semaphore is never closed");
                let label = episode.number_label();

                // `%(ext)s` is a yt-dlp placeholder — it fills in the real container.
                let filename = format!("{} - E{label}.%(ext)s", safe_filename(&series_title));
                let template = out.join(filename).display().to_string();

                let mut errors = Vec::new();
                for m in &mirrors {
                    let result = ytdlp
                        .download(&m.embed, quality, &template, |line| {
                            progress.apply_ytdlp_line(slot, line);
                        })
                        .await;

                    match result {
                        Ok(()) => {
                            progress.finish(slot, true, None);
                            return None;
                        }
                        Err(e) => errors.push(format!("{}: {e}", m.label)),
                    }
                }

                progress.finish(slot, false, errors.last().cloned());
                Some(label)
            }
        });

    failed.extend(futures::future::join_all(tasks).await.into_iter().flatten());
    display.finish().await;

    if failed.is_empty() {
        eprintln!("\n\x1b[32mDone.\x1b[0m Saved to {}", out.display());
        Ok(())
    } else {
        anyhow::bail!("could not download episode(s): {}", failed.join(", "))
    }
}

pub async fn next(app: &mut App) -> Result<()> {
    let history = app.history()?;
    let entry = history
        .most_recent()
        .context("no watch history yet — try `kuro watch <query>`")?
        .clone();

    let series = series_from_history(&entry)?;
    let provider = provider_for(app, &series)?;
    let episodes = episodes_of(app, &provider, &series).await?;

    let next_episode = episodes
        .iter()
        .find(|e| e.number > entry.episode)
        .cloned()
        .with_context(|| {
            format!(
                "no episode after {} of {}",
                entry.episode, entry.series_title
            )
        })?;

    eprintln!(
        "→ {} · Episode {}",
        series.title,
        next_episode.number_label()
    );

    play(
        app,
        PlayRequest {
            provider,
            series: &series,
            episode: &next_episode,
            mirror: None,
            upcoming: crate::playback::upcoming_after(&episodes, &next_episode),
            start_secs: None,
        },
    )
    .await?;

    // This command exits once playback ends, so where the viewer skipped to only
    // matters for history — which the recorder has already written.
    Ok(())
}

pub async fn continue_watching(app: &mut App) -> Result<()> {
    let history = app.history()?;
    let entry = history
        .most_recent()
        .context("no watch history yet — try `kuro watch <query>`")?
        .clone();

    let series = series_from_history(&entry)?;
    let provider = provider_for(app, &series)?;
    let episodes = episodes_of(app, &provider, &series).await?;
    let episode = pick_episode(&episodes, entry.episode)?.clone();

    let start = history.resume_position(&entry.provider_id, &entry.series_id, entry.episode);
    if let Some(pos) = start {
        eprintln!(
            "→ resuming {} · Episode {} at {}",
            series.title,
            episode.number_label(),
            format_hms(pos)
        );
    }

    play(
        app,
        PlayRequest {
            provider,
            series: &series,
            episode: &episode,
            mirror: None,
            upcoming: crate::playback::upcoming_after(&episodes, &episode),
            start_secs: start,
        },
    )
    .await?;

    Ok(())
}

// ---------------------------------------------------------------------------
// history / bookmarks
// ---------------------------------------------------------------------------

fn format_hms(secs: u64) -> String {
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

pub fn history(app: &App, limit: usize, clear: bool) -> Result<()> {
    let mut history = History::load(&app.paths).context("loading watch history")?;

    if clear {
        let count = history.entries.len();
        history.entries.clear();
        history.save(&app.paths).context("clearing watch history")?;
        println!(
            "Cleared {count} history entr{}.",
            if count == 1 { "y" } else { "ies" }
        );
        return Ok(());
    }

    if app.json {
        println!("{}", serde_json::to_string_pretty(&history.entries)?);
        return Ok(());
    }

    if history.entries.is_empty() {
        println!("Nothing watched yet.");
        return Ok(());
    }

    for entry in history.entries.iter().take(limit) {
        let mark = if entry.completed { "✓" } else { "·" };
        let progress = match entry.progress() {
            Some(p) => format!(" {:.0}%", p * 100.0),
            None => String::new(),
        };
        println!(
            "{mark} {}  Episode {}{}  {}",
            entry.series_title,
            entry.episode,
            progress,
            crate::ui::dim_stdout(entry.watched_at.format("%Y-%m-%d %H:%M").to_string())
        );
    }
    Ok(())
}

pub async fn bookmark(app: &mut App, action: &BookmarkAction) -> Result<()> {
    let mut bookmarks = Bookmarks::load(&app.paths).context("loading bookmarks")?;

    match action {
        BookmarkAction::List => {
            if app.json {
                println!("{}", serde_json::to_string_pretty(&bookmarks.entries)?);
            } else if bookmarks.entries.is_empty() {
                println!("No bookmarks.");
            } else {
                for b in &bookmarks.entries {
                    println!(
                        "{}  \x1b[2m[{}] {}\x1b[0m",
                        b.series_title, b.provider_id, b.series_id
                    );
                }
            }
        }

        BookmarkAction::Rm { series_id } => {
            if bookmarks.remove(series_id) {
                bookmarks.save(&app.paths)?;
                println!("Removed {series_id}.");
            } else {
                println!("No bookmark with id `{series_id}`.");
            }
        }

        BookmarkAction::Add { query } => {
            let query = query.trim().to_string();
            let series = search_ranked(app, &query).await?;
            let chosen = series
                .first()
                .with_context(|| format!("no results for `{query}`"))?;

            let added = bookmarks.add(Bookmark {
                provider_id: chosen.provider_id.to_string(),
                series_id: chosen.id.clone(),
                series_title: chosen.title.clone(),
                url: chosen.url.to_string(),
                added_at: chrono::Utc::now(),
            });

            if added {
                bookmarks.save(&app.paths)?;
                println!("Bookmarked {}.", chosen.title);
            } else {
                println!("{} is already bookmarked.", chosen.title);
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// providers
// ---------------------------------------------------------------------------

pub async fn provider_cmd(app: &mut App, action: &ProviderAction) -> Result<()> {
    match action {
        ProviderAction::List => {
            let recheck = app.config.health.recheck_interval;

            for id in app.registry.ids().map(str::to_string).collect::<Vec<_>>() {
                let enabled = app.config.is_enabled(&id);
                let health = app.health.get(&id);
                let usable = app.health.is_usable(&id, recheck);

                let state = if !enabled {
                    "\x1b[2mdisabled\x1b[0m"
                } else if health.auto_disabled && !usable {
                    "\x1b[33mauto-disabled\x1b[0m"
                } else {
                    "\x1b[32menabled\x1b[0m"
                };

                let source = if app.registry.is_user_override(&id) {
                    " (user override)"
                } else {
                    ""
                };

                println!(
                    "{:<20} {:<24} priority {}{}",
                    id,
                    state,
                    app.config.priority(&id),
                    source
                );

                if let Some(err) = &health.last_error {
                    println!("  \x1b[2mlast error: {err}\x1b[0m");
                }
            }

            for (name, err) in &app.registry.errors {
                println!("{name:<20} \x1b[31mfailed to load\x1b[0m — {err}");
            }
        }

        ProviderAction::Enable { id } => {
            ensure_known(app, id)?;
            app.config.provider_mut(id).enabled = true;
            app.save_config()?;
            // Clear the failure record so a manual re-enable starts from a clean slate.
            app.health.record_success(id);
            app.save_health()?;
            println!("Enabled {id}.");
        }

        ProviderAction::Disable { id } => {
            ensure_known(app, id)?;
            app.config.provider_mut(id).enabled = false;
            app.save_config()?;
            println!("Disabled {id}.");
        }

        ProviderAction::Only { id } => {
            ensure_known(app, id)?;
            for known in app.registry.ids().map(str::to_string).collect::<Vec<_>>() {
                app.config.provider_mut(&known).enabled = &known == id;
            }
            app.save_config()?;
            println!("Enabled {id} only.");
        }

        ProviderAction::Reload => {
            let registry = kuro_providers::Registry::load(Some(&app.paths.user_providers_dir()));
            println!("Loaded {} provider(s).", registry.len());
            for id in registry.ids() {
                let marker = if registry.is_user_override(id) {
                    " (user override)"
                } else {
                    ""
                };
                println!("  {id}{marker}");
            }
            for (name, err) in &registry.errors {
                println!("  \x1b[31m{name}\x1b[0m — {err}");
            }
            app.registry = registry;
        }

        ProviderAction::Test { id } => {
            test_provider(app, id).await?;
        }
    }
    Ok(())
}

fn ensure_known(app: &App, id: &str) -> Result<()> {
    if app.registry.get(id).is_none() {
        let known: Vec<&str> = app.registry.ids().collect();
        anyhow::bail!("unknown provider `{id}` (known: {})", known.join(", "));
    }
    Ok(())
}

/// Exercise the full scrape chain and report where it breaks.
async fn test_provider(app: &mut App, id: &str) -> Result<()> {
    ensure_known(app, id)?;
    let provider = app.provider(id).expect("checked above");

    println!("Testing {id} ({})…\n", provider.base_url());

    let step = |label: &str, elapsed: std::time::Duration, detail: String| {
        println!(
            "  \x1b[32m✓\x1b[0m {label:<12} {:>6} ms  {detail}",
            elapsed.as_millis()
        );
    };

    let t = Instant::now();
    provider
        .health_check(&app.ctx)
        .await
        .map_err(|e| anyhow::anyhow!("health check failed: {e}"))?;
    step("reachable", t.elapsed(), String::new());

    let t = Instant::now();
    let results = provider
        .search(&app.ctx, "a")
        .await
        .map_err(|e| anyhow::anyhow!("search failed: {e}"))?;
    step(
        "search",
        t.elapsed(),
        format!("{} result(s)", results.len()),
    );

    let Some(series) = results.first() else {
        println!("\n  search returned nothing — cannot test deeper.");
        return Ok(());
    };

    let t = Instant::now();
    let episodes = provider
        .episodes(&app.ctx, series)
        .await
        .map_err(|e| anyhow::anyhow!("episode list failed: {e}"))?;
    step(
        "episodes",
        t.elapsed(),
        format!("{} episode(s) of {}", episodes.len(), series.title),
    );

    let Some(episode) = episodes.first() else {
        println!("\n  no episodes — cannot test deeper.");
        return Ok(());
    };

    let t = Instant::now();
    let mirrors = provider
        .mirrors(&app.ctx, episode)
        .await
        .map_err(|e| anyhow::anyhow!("mirror list failed: {e}"))?;
    step(
        "mirrors",
        t.elapsed(),
        format!("{} mirror(s)", mirrors.len()),
    );

    let t = Instant::now();
    let mut embeds = Vec::new();
    for mirror in mirrors.iter().take(3) {
        if let Ok(url) = provider.embed_url(&app.ctx, mirror).await {
            if let Some(host) = url.host_str() {
                embeds.push(kuro_providers::hosts::host_label(host));
            }
        }
    }
    if embeds.is_empty() {
        anyhow::bail!("no mirror produced a usable embed URL");
    }
    step("embeds", t.elapsed(), embeds.join(", "));

    println!("\n\x1b[32mAll checks passed.\x1b[0m");
    app.note_success(&provider.id());
    app.save_health().ok();
    Ok(())
}

// ---------------------------------------------------------------------------
// config / doctor
// ---------------------------------------------------------------------------

pub fn config_cmd(app: &App, action: &ConfigAction) -> Result<()> {
    match action {
        ConfigAction::Path => println!("{}", app.paths.config_file().display()),

        ConfigAction::Show => {
            if app.json {
                println!("{}", serde_json::to_string_pretty(&app.config)?);
            } else {
                print!("{}", toml::to_string_pretty(&app.config)?);
            }
        }

        ConfigAction::Init => {
            let path = app.paths.config_file();
            if path.exists() {
                println!("Config already exists at {}.", path.display());
                return Ok(());
            }
            // Materialise every provider so the file documents what can be toggled.
            let mut config = app.config.clone();
            for id in app.registry.ids().map(str::to_string).collect::<Vec<_>>() {
                config.provider_mut(&id);
            }
            config.save(&app.paths)?;
            println!("Wrote {}.", path.display());
        }
    }
    Ok(())
}

pub fn cache_cmd(app: &App, action: &CacheAction) -> Result<()> {
    let cache = app.ctx.cache();

    match action {
        CacheAction::Status => {
            println!("path     {}", app.paths.cache_dir().display());
            println!(
                "state    {}",
                if cache.is_enabled() {
                    "enabled"
                } else {
                    "disabled for this run"
                }
            );
            println!("entries  {}", cache.len());
        }

        CacheAction::Clear => {
            // Clear works even when caching is off for this run, so `--no-cache`
            // doesn't make the cache impossible to empty.
            let dir = app.paths.cache_dir();
            let removed = kuro_core::HttpCache::new(Some(dir)).clear()?;
            println!("Removed {removed} cached page(s).");
        }
    }
    Ok(())
}

pub async fn doctor(app: &mut App) -> Result<()> {
    println!("kuro doctor\n");

    println!("config     {}", app.paths.config_file().display());
    println!("providers  {}", app.paths.user_providers_dir().display());
    println!("history    {}", app.paths.history_file().display());
    println!();

    match app.player() {
        Ok(player) => println!(
            "\x1b[32m✓\x1b[0m player     {} at {}",
            player.name(),
            player.binary().display()
        ),
        Err(e) => println!("\x1b[31m✗\x1b[0m player     {e}"),
    }

    let ytdlp = kuro_resolver::ytdlp::YtDlpResolver::default();
    match ytdlp.version().await {
        Some(v) => println!("\x1b[32m✓\x1b[0m yt-dlp     {v}"),
        None => println!(
            "\x1b[33m!\x1b[0m yt-dlp     not found — most mirrors will be unresolvable\n\
             \x20            install with: brew install yt-dlp"
        ),
    }

    // Only worth reporting when a provider actually asks for it, so the common
    // two-provider setup is not nagged about a dependency it never uses.
    let needs_impersonation: Vec<String> = app
        .registry
        .impersonating_ids()
        .map(str::to_string)
        .collect();

    if !needs_impersonation.is_empty() {
        match app.ctx.impersonator() {
            Some(fetcher) if fetcher.is_available().await => {
                println!("\x1b[32m✓\x1b[0m impersonate {}", fetcher.command())
            }
            Some(fetcher) => println!(
                "\x1b[33m!\x1b[0m impersonate {} not found — {} unusable\n\
                 \x20            these providers sit behind a TLS-level challenge; install\n\
                 \x20            curl-impersonate from https://github.com/lexiforest/curl-impersonate\n\
                 \x20            and put it on PATH, or set `general.impersonate_command`",
                fetcher.command(),
                needs_impersonation.join(", ")
            ),
            None => println!(
                "\x1b[33m!\x1b[0m impersonate disabled — {} unusable",
                needs_impersonation.join(", ")
            ),
        }
    }

    println!("\nProviders:");
    let recheck = app.config.health.recheck_interval;
    for id in app.registry.ids().map(str::to_string).collect::<Vec<_>>() {
        let enabled = app.config.is_enabled(&id);
        let usable = app.health.is_usable(&id, recheck);
        let mark = if enabled && usable {
            "\x1b[32m✓\x1b[0m"
        } else {
            "\x1b[33m!\x1b[0m"
        };
        println!("{mark} {id}");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filenames_drop_path_separators_and_reserved_characters() {
        // A title containing `/` would otherwise write outside the output directory.
        assert_eq!(safe_filename("Wu Dong / Qian Kun"), "Wu Dong - Qian Kun");
        assert_eq!(safe_filename("What: A Title?"), "What- A Title-");
        assert_eq!(safe_filename("a\\b*c\"d<e>f|g"), "a-b-c-d-e-f-g");
    }

    #[test]
    fn filenames_collapse_whitespace() {
        assert_eq!(safe_filename("  Spaced   Out  "), "Spaced Out");
        assert_eq!(safe_filename("tab\there"), "tab here");
    }

    #[test]
    fn non_ascii_titles_are_preserved() {
        // Donghua titles are routinely CJK; mangling them would be worse than useless.
        assert_eq!(safe_filename("斗罗大陆"), "斗罗大陆");
    }

    #[test]
    fn durations_render_as_clock_time() {
        assert_eq!(format_hms(0), "0:00");
        assert_eq!(format_hms(612), "10:12");
        assert_eq!(format_hms(3661), "1:01:01");
    }
}
