//! The interactive progression: search → series → episode → action.
//!
//! Reached by `kuro search "<query>"` at a terminal. Every step is
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
    index: usize,
) -> Result<Step> {
    loop {
        let episode = &episodes[index];
        let items = vec![
            Item::new("▶  Play"),
            Item::new("⬇  Download this episode"),
            Item::new("⬇  Download a range…"),
            Item::with_hint("⚙  Max quality", quality_label(app.quality).to_string()),
            Item::new("☆  Bookmark series"),
            Item::new("←  Back to episodes"),
        ];

        let title = format!("{} · Episode {}", series.title, episode.number_label());
        let picked = match ui::select(&title, &items)? {
            Choice::Picked(i) => i,
            Choice::Cancelled => return Ok(Step::Back),
        };

        match picked {
            // Playback runs its own next/replay loop and only returns once the
            // viewer wants out of this episode entirely.
            0 => return play_and_continue(app, series, provider, episodes, index).await,
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
            3 => app.quality = pick_quality(app, provider, episode, app.quality).await?,
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

/// Why a rung may not deliver what its name suggests.
///
/// The list is fixed rather than probed from the host: the ladder is only known
/// after a mirror has been resolved, which costs a `yt-dlp` round-trip and happens
/// after this menu. Flagging the rungs that these embed hosts realistically never
/// serve keeps the menu honest without that cost, and without hardcoding a ceiling
/// that a future provider might exceed.
fn quality_caveat(q: QualityPref) -> Option<&'static str> {
    match q {
        QualityPref::P2160 | QualityPref::P1440 => Some("rarely available"),
        _ => None,
    }
}

/// What `pref` would actually deliver from a host offering `heights`.
///
/// Mirrors the resolver's rule — the closest rendition at or below the cap, or the
/// smallest available when everything overshoots — so the menu promises exactly
/// what playback will do.
fn delivered(heights: &[u32], pref: QualityPref) -> Option<u32> {
    if heights.is_empty() {
        return None;
    }
    match pref.target_height() {
        None => match pref {
            QualityPref::Worst => heights.iter().min().copied(),
            _ => heights.iter().max().copied(),
        },
        Some(cap) => heights
            .iter()
            .filter(|h| **h <= cap)
            .max()
            .copied()
            .or_else(|| heights.iter().min().copied()),
    }
}

/// The host's real ladder for this episode, or `None` if it cannot be determined.
///
/// Costs a mirror resolution plus an extractor call, which is why it happens only
/// when the menu is opened rather than on every episode.
async fn probe_heights(
    app: &mut App,
    provider: &Arc<dyn Provider>,
    episode: &Episode,
) -> Option<Vec<u32>> {
    let ordered = crate::playback::ordered_mirrors(app, provider, episode, None, false)
        .await
        .ok()?;
    let first = ordered.first()?;
    let heights = app.resolver.available_heights(&first.embed).await.ok()?;
    (!heights.is_empty()).then_some(heights)
}

async fn pick_quality(
    app: &mut App,
    provider: &Arc<dyn Provider>,
    episode: &Episode,
    current: QualityPref,
) -> Result<QualityPref> {
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

    let spinner = Spinner::start("Checking what this episode offers…");
    let heights = probe_heights(app, provider, episode).await;
    spinner.clear().await;

    // Rungs that resolve to the same rendition are the same choice wearing two
    // names; showing both invites picking "2160p" on a source that stops at 1080p.
    let mut items = Vec::new();
    let mut choices = Vec::new();
    let mut previous: Option<u32> = None;

    for quality in CHOICES {
        let got = heights.as_deref().and_then(|h| delivered(h, quality));
        if let (Some(got), Some(prev)) = (got, previous) {
            if got == prev {
                continue;
            }
        }
        if got.is_some() {
            previous = got;
        }

        // A probed height is the truth; without one, fall back to flagging the
        // rungs these hosts rarely serve.
        let detail = match got {
            Some(h) => Some(format!("{h}p")),
            None => quality_caveat(quality).map(str::to_string),
        };
        let hint = match (detail, quality == current) {
            (Some(d), true) => Some(format!("{d} · current")),
            (Some(d), false) => Some(d),
            (None, true) => Some("current".to_string()),
            (None, false) => None,
        };

        items.push(match hint {
            Some(h) => Item::with_hint(quality_label(quality), h),
            None => Item::new(quality_label(quality)),
        });
        choices.push(quality);
    }

    let title = if heights.is_some() {
        "Max quality — heights this episode actually offers"
    } else {
        "Max quality — you get the best the host has at or below this"
    };

    Ok(match ui::select(title, &items)? {
        Choice::Picked(i) => choices[i],
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

/// Which episode to continue from once the player exits.
///
/// The player queues the next episodes, so its own next/previous controls can carry
/// the viewer well past what kuro launched. Preferring the episode the recorder last
/// saw stops the "Finished" menu offering one they just watched. Falls back to the
/// launched position when nothing was tracked, or when the number is not in this
/// list — a filtered range, say.
fn resume_index(episodes: &[Episode], launched: usize, finished: Option<f32>) -> usize {
    finished
        .and_then(|number| {
            episodes
                .iter()
                .position(|e| (e.number - number).abs() < f32::EPSILON)
        })
        .unwrap_or(launched)
}

/// Play an episode, then offer to continue with the next one.
///
/// Returns only when the viewer is done with this episode: "next" and "replay" are
/// handled by the loop here, so a return means back to the episode list or out.
async fn play_and_continue(
    app: &mut App,
    series: &Series,
    provider: &Arc<dyn Provider>,
    episodes: &[Episode],
    index: usize,
) -> Result<Step> {
    let mut current = index;

    loop {
        let episode = &episodes[current];
        let start =
            app.history()?
                .resume_position(series.provider_id.as_str(), &series.id, episode.number);

        let finished = play(
            app,
            PlayRequest {
                provider: Arc::clone(provider),
                series,
                episode,
                mirror: None,
                upcoming: crate::playback::upcoming_after(episodes, episode),
                start_secs: start,
            },
        )
        .await?;

        current = resume_index(episodes, current, finished);

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
            Choice::Cancelled => return Ok(Step::Back),
        };

        // The "next" row only exists when there is a next episode, so the
        // remaining indices shift accordingly.
        let offset = usize::from(next.is_some());
        match picked {
            0 if next.is_some() => current += 1,
            // Replay: fall through the loop and play the same episode again.
            i if i == offset => continue,
            // "Back to episodes" means the episode list, not this episode's own
            // action menu — returning to the latter made the label a lie.
            i if i == offset + 1 => return Ok(Step::Back),
            _ => return Ok(Step::Quit),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn episodes(numbers: &[f32]) -> Vec<Episode> {
        numbers
            .iter()
            .map(|n| Episode {
                series_id: "s".to_string(),
                number: *n,
                title: None,
                url: "https://example.test/e".parse().expect("test url"),
            })
            .collect()
    }

    #[test]
    fn a_cap_selects_the_closest_rendition_at_or_below_it() {
        // A real 2.39:1 ladder: the frames are shorter than their rung names, so a
        // 1080 cap legitimately reaches the rendition marketed as 1440p.
        let heights = vec![1608, 1072, 808, 536];
        assert_eq!(delivered(&heights, QualityPref::P2160), Some(1608));
        assert_eq!(delivered(&heights, QualityPref::P1440), Some(1072));
        assert_eq!(delivered(&heights, QualityPref::P1080), Some(1072));
        assert_eq!(delivered(&heights, QualityPref::P720), Some(536));
    }

    #[test]
    fn caps_landing_on_one_rendition_are_a_single_choice() {
        // 1440p and 1080p both reach 1072 on the ladder above, so the menu must
        // offer one entry rather than two that behave identically.
        let heights = vec![1608, 1072, 808, 536];
        assert_eq!(
            delivered(&heights, QualityPref::P1440),
            delivered(&heights, QualityPref::P1080)
        );
    }

    #[test]
    fn a_cap_below_everything_still_plays_the_smallest() {
        // Refusing to play would be worse than exceeding the cap.
        assert_eq!(delivered(&[1080, 720], QualityPref::P360), Some(720));
    }

    #[test]
    fn relative_choices_take_the_ends_of_the_ladder() {
        let heights = vec![1608, 1072, 536];
        assert_eq!(delivered(&heights, QualityPref::Best), Some(1608));
        assert_eq!(delivered(&heights, QualityPref::Worst), Some(536));
    }

    #[test]
    fn an_unknown_ladder_promises_nothing() {
        assert_eq!(delivered(&[], QualityPref::P1080), None);
    }

    #[test]
    fn rungs_above_the_host_ceiling_collapse_together() {
        // 2160p and 1440p both yield 1080 on this host, so the menu must not
        // offer them as if they were distinct choices.
        let heights = vec![1080, 720, 480];
        assert_eq!(
            delivered(&heights, QualityPref::P2160),
            delivered(&heights, QualityPref::P1440)
        );
    }

    #[test]
    fn skipping_ahead_in_the_player_moves_the_continue_point() {
        // The regression: launch episode 8, reach episode 10 with the player's own
        // controls. Continuing from the launched index would offer episode 9 again.
        let eps = episodes(&[8.0, 9.0, 10.0, 11.0]);
        assert_eq!(resume_index(&eps, 0, Some(10.0)), 2);
    }

    #[test]
    fn staying_on_the_launched_episode_changes_nothing() {
        let eps = episodes(&[8.0, 9.0, 10.0]);
        assert_eq!(resume_index(&eps, 0, Some(8.0)), 0);
    }

    #[test]
    fn an_untracked_session_keeps_the_launched_position() {
        // Nothing recorded — a dry run, or a session too short to checkpoint.
        let eps = episodes(&[8.0, 9.0, 10.0]);
        assert_eq!(resume_index(&eps, 1, None), 1);
    }

    #[test]
    fn an_episode_outside_this_list_keeps_the_launched_position() {
        // Guards the filtered-range case, where the played episode need not appear.
        let eps = episodes(&[8.0, 9.0]);
        assert_eq!(resume_index(&eps, 1, Some(42.0)), 1);
    }

    #[test]
    fn half_episodes_are_matched_exactly() {
        let eps = episodes(&[1.0, 1.5, 2.0]);
        assert_eq!(resume_index(&eps, 0, Some(1.5)), 1);
    }
}
