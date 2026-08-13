//! The interactive progression: search → series → episode → action.
//!
//! Reached by `kuro <query>` or `kuro search <query>` at a terminal. Every step is
//! cancellable with `q`/Esc, which walks back one level rather than exiting, so
//! picking the wrong series is not a dead end.

use crate::app::App;
use crate::commands;
use crate::playback::{play, PlayRequest};
use crate::ui::{self, Choice, Item, Spinner};
use anyhow::{Context, Result};
use kuro_core::{Episode, Provider, QualityPref, Series};
use kuro_store::{Bookmark, Bookmarks, History};
use std::sync::Arc;

/// Where the flow should go after an inner step returns.
enum Step {
    /// Return to the previous level.
    Back,
    /// Leave the flow entirely.
    Quit,
}

pub async fn run(app: &mut App, query: &str) -> Result<()> {
    let query = query.trim();
    if query.is_empty() {
        anyhow::bail!("give me something to search for");
    }

    let spinner = Spinner::start(format!(
        "Searching {} provider(s)…",
        app.active_providers().len()
    ));
    let results = commands::search_ranked(app, query).await;
    match &results {
        Ok(found) => {
            spinner
                .finish(format!("Found {} result(s)", found.len()))
                .await
        }
        Err(_) => spinner.clear().await,
    }

    let results = results?;
    if results.is_empty() {
        eprintln!("No results for `{query}`.");
        return Ok(());
    }

    // A single hit needs no menu.
    let mut preselected = (results.len() == 1).then_some(0usize);

    loop {
        let index = match preselected.take() {
            Some(i) => i,
            None => {
                let items: Vec<Item> = results
                    .iter()
                    .map(|s| Item::with_hint(display_title(s), s.provider_id.to_string()))
                    .collect();

                match ui::select(&format!("Results for “{query}”"), &items)? {
                    Choice::Picked(i) => i,
                    Choice::Cancelled => return Ok(()),
                }
            }
        };

        let series = results[index].clone();
        match series_menu(app, &series).await? {
            Step::Quit => return Ok(()),
            // Back at the top level with only one result means there is nowhere
            // left to go.
            Step::Back if results.len() == 1 => return Ok(()),
            Step::Back => {}
        }
    }
}

fn display_title(series: &Series) -> String {
    match series
        .year
        .filter(|y| !series.title.contains(&y.to_string()))
    {
        Some(year) => format!("{} ({year})", series.title),
        None => series.title.clone(),
    }
}

/// Episode list for one series.
async fn series_menu(app: &mut App, series: &Series) -> Result<Step> {
    let provider = commands::provider_for(app, series)?;

    let spinner = Spinner::start("Loading episodes…");
    let episodes = commands::episodes_of(app, &provider, series).await;
    spinner.clear().await;

    let episodes = episodes?;
    if episodes.is_empty() {
        eprintln!("No episodes listed for {}.", series.title);
        return Ok(Step::Back);
    }

    // Start on the first unwatched episode rather than the top of the list.
    let history = app.history()?;
    let mut cursor = history
        .last_completed_episode(series.provider_id.as_str(), &series.id)
        .and_then(|last| episodes.iter().position(|e| e.number > last))
        .unwrap_or(0);

    loop {
        let history = app.history()?;
        let items: Vec<Item> = episodes
            .iter()
            .map(|e| episode_item(&history, series, e))
            .collect();

        // `select` always starts at the top, so surface the suggested episode in
        // the title instead of silently landing elsewhere.
        let title = match episodes.get(cursor) {
            Some(e) if cursor > 0 => {
                format!("{} · next up: Episode {}", series.title, e.number_label())
            }
            _ => series.title.clone(),
        };

        let index = match ui::select(&title, &items)? {
            Choice::Picked(i) => i,
            Choice::Cancelled => return Ok(Step::Back),
        };
        cursor = index;

        match action_menu(app, series, &provider, &episodes, index).await? {
            Step::Quit => return Ok(Step::Quit),
            Step::Back => {}
        }
    }
}

fn episode_item(history: &History, series: &Series, episode: &Episode) -> Item {
    let label = match &episode.title {
        Some(t) if !t.is_empty() => format!("Episode {}  {t}", episode.number_label()),
        _ => format!("Episode {}", episode.number_label()),
    };

    let entry = history.find(series.provider_id.as_str(), &series.id, episode.number);

    match entry {
        Some(e) if e.completed => Item::with_hint(label, "watched"),
        Some(e) if e.position_secs > 0 => {
            Item::with_hint(label, format!("resume {}", clock(e.position_secs)))
        }
        _ => Item::new(label),
    }
}

fn clock(secs: u64) -> String {
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// What to do with the chosen episode.
async fn action_menu(
    app: &mut App,
    series: &Series,
    provider: &Arc<dyn Provider>,
    episodes: &[Episode],
    mut index: usize,
) -> Result<Step> {
    loop {
        let episode = &episodes[index];
        let items = vec![
            Item::new("▶  Play"),
            Item::new("⬇  Download this episode"),
            Item::new("⬇  Download a range…"),
            Item::with_hint("⚙  Quality", quality_label(app.quality).to_string()),
            Item::new("☆  Bookmark series"),
            Item::new("←  Back to episodes"),
        ];

        let title = format!("{} · Episode {}", series.title, episode.number_label());
        let picked = match ui::select(&title, &items)? {
            Choice::Picked(i) => i,
            Choice::Cancelled => return Ok(Step::Back),
        };

        match picked {
            0 => match play_and_continue(app, series, provider, episodes, index).await? {
                PlayNext::Continue(next) => index = next,
                PlayNext::Back => return Ok(Step::Back),
                PlayNext::Quit => return Ok(Step::Quit),
            },
            1 => {
                commands::download_episodes(
                    app,
                    series,
                    provider,
                    std::slice::from_ref(episode),
                    None,
                    std::path::Path::new("."),
                    3,
                )
                .await?;
            }
            2 => {
                if let Some(range) = prompt_range(episodes)? {
                    commands::download_episodes(
                        app,
                        series,
                        provider,
                        &range,
                        None,
                        std::path::Path::new("."),
                        3,
                    )
                    .await?;
                }
            }
            3 => app.quality = pick_quality(app.quality)?,
            4 => bookmark(app, series)?,
            _ => return Ok(Step::Back),
        }
    }
}

fn quality_label(q: QualityPref) -> &'static str {
    match q {
        QualityPref::Best => "best",
        QualityPref::Worst => "worst",
        QualityPref::P2160 => "2160p",
        QualityPref::P1440 => "1440p",
        QualityPref::P1080 => "1080p",
        QualityPref::P720 => "720p",
        QualityPref::P480 => "480p",
        QualityPref::P360 => "360p",
    }
}

fn pick_quality(current: QualityPref) -> Result<QualityPref> {
    const CHOICES: [QualityPref; 8] = [
        QualityPref::Best,
        QualityPref::P2160,
        QualityPref::P1440,
        QualityPref::P1080,
        QualityPref::P720,
        QualityPref::P480,
        QualityPref::P360,
        QualityPref::Worst,
    ];

    let items: Vec<Item> = CHOICES
        .iter()
        .map(|q| {
            if *q == current {
                Item::with_hint(quality_label(*q), "current")
            } else {
                Item::new(quality_label(*q))
            }
        })
        .collect();

    Ok(match ui::select("Quality", &items)? {
        Choice::Picked(i) => CHOICES[i],
        // Cancelling a submenu keeps the existing setting.
        Choice::Cancelled => current,
    })
}

fn prompt_range(episodes: &[Episode]) -> Result<Option<Vec<Episode>>> {
    let first = episodes
        .first()
        .map(|e| e.number_label())
        .unwrap_or_default();
    let last = episodes
        .last()
        .map(|e| e.number_label())
        .unwrap_or_default();

    let Some(input) = ui::prompt_line(&format!("Range to download [{first}-{last}]: "))? else {
        return Ok(None);
    };

    let spec: crate::cli::EpisodeSpec =
        input.parse().map_err(|e: String| anyhow::anyhow!("{e}"))?;

    let picked: Vec<Episode> = episodes
        .iter()
        .filter(|e| !spec.select(&[e.number]).is_empty())
        .cloned()
        .collect();

    if picked.is_empty() {
        eprintln!("No episodes match `{spec}`.");
        return Ok(None);
    }
    Ok(Some(picked))
}

fn bookmark(app: &App, series: &Series) -> Result<()> {
    let mut bookmarks = Bookmarks::load(&app.paths).context("loading bookmarks")?;
    let added = bookmarks.add(Bookmark {
        provider_id: series.provider_id.to_string(),
        series_id: series.id.clone(),
        series_title: series.title.clone(),
        url: series.url.to_string(),
        added_at: chrono::Utc::now(),
    });

    if added {
        bookmarks.save(&app.paths)?;
        eprintln!("☆ Bookmarked {}.", series.title);
    } else {
        eprintln!("{} is already bookmarked.", series.title);
    }
    Ok(())
}

/// Outcome of the post-playback menu.
enum PlayNext {
    /// Reopen the action menu on this episode.
    Continue(usize),
    Back,
    Quit,
}

/// Play an episode, then offer to continue with the next one.
async fn play_and_continue(
    app: &mut App,
    series: &Series,
    provider: &Arc<dyn Provider>,
    episodes: &[Episode],
    index: usize,
) -> Result<PlayNext> {
    let mut current = index;

    loop {
        let episode = &episodes[current];
        let start =
            app.history()?
                .resume_position(series.provider_id.as_str(), &series.id, episode.number);

        play(
            app,
            PlayRequest {
                provider: Arc::clone(provider),
                series,
                episode,
                mirror: None,
                start_secs: start,
            },
        )
        .await?;

        let next = episodes.get(current + 1);
        let mut items = Vec::new();
        if let Some(n) = next {
            items.push(Item::new(format!("▶  Next: Episode {}", n.number_label())));
        }
        items.push(Item::new("↻  Replay this episode"));
        items.push(Item::new("☰  Back to episodes"));
        items.push(Item::new("✕  Quit"));

        let picked = match ui::select("Finished", &items)? {
            Choice::Picked(i) => i,
            Choice::Cancelled => return Ok(PlayNext::Back),
        };

        // The "next" row only exists when there is a next episode, so the
        // remaining indices shift accordingly.
        let offset = usize::from(next.is_some());
        match picked {
            0 if next.is_some() => current += 1,
            // Replay: fall through the loop and play the same episode again.
            i if i == offset => continue,
            i if i == offset + 1 => return Ok(PlayNext::Continue(current)),
            _ => return Ok(PlayNext::Quit),
        }
    }
}
