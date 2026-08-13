//! Interactive terminal UI.
//!
//! Scraping is slow and network-bound, so it never runs on the draw path: work is
//! spawned onto tokio tasks and reported back over a channel, leaving the event
//! loop free to redraw and accept input while a search is in flight.

use crate::app::App;
use crate::playback::{play, PlayRequest};
use anyhow::Result;
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use futures::StreamExt;
use kuro_core::{orchestrator, Episode, Provider, ProviderId, Series};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{Frame, Terminal};
use std::io::Stdout;
use std::sync::Arc;
use tokio::sync::mpsc;

type Term = Terminal<CrosstermBackend<Stdout>>;

#[derive(PartialEq, Eq, Clone, Copy)]
enum Screen {
    Search,
    Series,
    Providers,
}

/// Results of background work, delivered to the event loop.
enum Msg {
    Search(Result<Vec<Series>, String>, Vec<(ProviderId, String)>),
    Episodes(Result<Vec<Episode>, String>),
}

struct ProviderRow {
    id: String,
    display_name: String,
    enabled: bool,
    auto_disabled: bool,
    last_error: Option<String>,
}

struct Ui {
    screen: Screen,
    query: String,
    editing: bool,
    results: Vec<Series>,
    results_state: ListState,
    series: Option<Series>,
    episodes: Vec<Episode>,
    episodes_state: ListState,
    providers: Vec<ProviderRow>,
    providers_state: ListState,
    status: String,
    busy: bool,
    quit: bool,
    /// Set when the user picks an episode; the event loop drops out of the TUI,
    /// plays it, then restores.
    pending_play: Option<(Series, Episode)>,
}

impl Ui {
    fn new(app: &App) -> Self {
        let mut ui = Self {
            screen: Screen::Search,
            query: String::new(),
            editing: true,
            results: Vec::new(),
            results_state: ListState::default(),
            series: None,
            episodes: Vec::new(),
            episodes_state: ListState::default(),
            providers: Vec::new(),
            providers_state: ListState::default(),
            status: "Type to search, Enter to run.".to_string(),
            busy: false,
            quit: false,
            pending_play: None,
        };
        ui.reload_providers(app);
        ui
    }

    fn reload_providers(&mut self, app: &App) {
        let recheck = app.config.health.recheck_interval;
        self.providers = app
            .registry
            .all()
            .into_iter()
            .map(|p| {
                let id = p.id().as_str().to_string();
                let health = app.health.get(&id);
                ProviderRow {
                    display_name: p.display_name().to_string(),
                    enabled: app.config.is_enabled(&id),
                    auto_disabled: health.auto_disabled && !app.health.is_usable(&id, recheck),
                    last_error: health.last_error.clone(),
                    id,
                }
            })
            .collect();

        if self.providers_state.selected().is_none() && !self.providers.is_empty() {
            self.providers_state.select(Some(0));
        }
    }

    fn selected_series(&self) -> Option<&Series> {
        self.results.get(self.results_state.selected()?)
    }

    fn selected_episode(&self) -> Option<&Episode> {
        self.episodes.get(self.episodes_state.selected()?)
    }
}

/// Move a list selection by `delta`, clamped to the list bounds.
fn move_selection(state: &mut ListState, len: usize, delta: isize) {
    if len == 0 {
        state.select(None);
        return;
    }
    let current = state.selected().unwrap_or(0) as isize;
    let next = (current + delta).clamp(0, len as isize - 1);
    state.select(Some(next as usize));
}

// ---------------------------------------------------------------------------
// Terminal lifecycle
// ---------------------------------------------------------------------------

fn setup() -> Result<Term> {
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen)?;
    let terminal = Terminal::new(CrosstermBackend::new(stdout))?;
    Ok(terminal)
}

fn restore(terminal: &mut Term) -> Result<()> {
    disable_raw_mode()?;
    crossterm::execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

pub async fn run(app: &mut App) -> Result<()> {
    let mut terminal = setup()?;

    // Any error from here on must still restore the terminal, or the user's shell
    // is left in raw mode with no echo.
    let outcome = event_loop(app, &mut terminal).await;

    restore(&mut terminal)?;
    outcome
}

async fn event_loop(app: &mut App, terminal: &mut Term) -> Result<()> {
    let mut ui = Ui::new(app);
    let (tx, mut rx) = mpsc::channel::<Msg>(16);
    let mut events = EventStream::new();

    loop {
        terminal.draw(|f| draw(f, &mut ui))?;

        tokio::select! {
            maybe_event = events.next() => {
                match maybe_event {
                    Some(Ok(Event::Key(key))) if key.kind == KeyEventKind::Press => {
                        handle_key(app, &mut ui, key, &tx);
                    }
                    Some(Ok(_)) => {}
                    // Terminal closed or errored — leave rather than spin.
                    Some(Err(_)) | None => break,
                }
            }
            Some(msg) = rx.recv() => handle_msg(app, &mut ui, msg),
        }

        if let Some((series, episode)) = ui.pending_play.take() {
            restore(terminal)?;
            let result = launch(app, &series, &episode).await;
            *terminal = setup()?;
            terminal.clear()?;

            ui.status = match result {
                Ok(()) => format!("Finished Episode {}.", episode.number_label()),
                Err(e) => format!("Playback failed: {e}"),
            };
        }

        if ui.quit {
            break;
        }
    }

    Ok(())
}

async fn launch(app: &mut App, series: &Series, episode: &Episode) -> Result<()> {
    let provider = app
        .provider(series.provider_id.as_str())
        .ok_or_else(|| anyhow::anyhow!("provider `{}` is not loaded", series.provider_id))?;

    let start =
        app.history()?
            .resume_position(series.provider_id.as_str(), &series.id, episode.number);

    play(
        app,
        PlayRequest {
            provider,
            series,
            episode,
            mirror: None,
            start_secs: start,
        },
    )
    .await
}

// ---------------------------------------------------------------------------
// Background work
// ---------------------------------------------------------------------------

fn spawn_search(app: &App, ui: &mut Ui, tx: &mpsc::Sender<Msg>) {
    let query = ui.query.trim().to_string();
    if query.is_empty() {
        ui.status = "Type something to search for.".to_string();
        return;
    }

    let providers = app.active_providers();
    if providers.is_empty() {
        ui.status = "No providers are active — press `p` to enable one.".to_string();
        return;
    }

    ui.busy = true;
    ui.status = format!("Searching {} provider(s) for `{query}`…", providers.len());

    let ctx = app.ctx.clone();
    let timeout = app.config.general.search_timeout;
    let concurrency = app.config.general.concurrency;
    let priorities: Vec<(String, i32)> = providers
        .iter()
        .map(|p| {
            let id = p.id().as_str().to_string();
            let priority = app.config.priority(&id);
            (id, priority)
        })
        .collect();
    let tx = tx.clone();

    tokio::spawn(async move {
        let results =
            orchestrator::search_all(&providers, &ctx, &query, timeout, concurrency).await;

        let mut series = results.series;
        orchestrator::rank(&mut series, &query, |id| {
            priorities
                .iter()
                .find(|(pid, _)| pid == id.as_str())
                .map(|(_, p)| *p)
                .unwrap_or(0)
        });

        let failures = results
            .failures
            .into_iter()
            .map(|(id, e)| (id, e.to_string()))
            .collect();

        tx.send(Msg::Search(Ok(series), failures)).await.ok();
    });
}

fn spawn_episodes(app: &App, ui: &mut Ui, series: Series, tx: &mpsc::Sender<Msg>) {
    let Some(provider) = app.provider(series.provider_id.as_str()) else {
        ui.status = format!("Provider `{}` is not loaded.", series.provider_id);
        return;
    };

    ui.busy = true;
    ui.status = format!("Loading episodes for {}…", series.title);

    let ctx = app.ctx.clone();
    let tx = tx.clone();

    tokio::spawn(async move {
        let result = fetch_episodes(provider, ctx, series).await;
        tx.send(Msg::Episodes(result)).await.ok();
    });
}

async fn fetch_episodes(
    provider: Arc<dyn Provider>,
    ctx: kuro_core::FetchCtx,
    series: Series,
) -> Result<Vec<Episode>, String> {
    provider
        .episodes(&ctx, &series)
        .await
        .map_err(|e| match e.hint() {
            Some(hint) => format!("{e} ({hint})"),
            None => e.to_string(),
        })
}

fn handle_msg(app: &mut App, ui: &mut Ui, msg: Msg) {
    ui.busy = false;

    match msg {
        Msg::Search(result, failures) => {
            for (id, err) in &failures {
                app.note_failure(id, err);
            }
            app.save_health().ok();
            ui.reload_providers(app);

            match result {
                Ok(series) => {
                    ui.status = match (series.len(), failures.len()) {
                        (0, 0) => "No results.".to_string(),
                        (n, 0) => format!("{n} result(s)."),
                        (n, f) => format!("{n} result(s), {f} provider(s) failed."),
                    };
                    ui.results_state.select((!series.is_empty()).then_some(0));
                    ui.results = series;
                    ui.editing = false;
                }
                Err(e) => ui.status = format!("Search failed: {e}"),
            }
        }

        Msg::Episodes(Ok(episodes)) => {
            ui.status = format!("{} episode(s). Enter to play.", episodes.len());
            // Land on the first unwatched episode rather than the top of the list.
            let start = ui
                .series
                .as_ref()
                .and_then(|s| {
                    let history = app.history().ok()?;
                    let last = history.last_completed_episode(s.provider_id.as_str(), &s.id)?;
                    episodes.iter().position(|e| e.number > last)
                })
                .unwrap_or(0);

            ui.episodes_state
                .select((!episodes.is_empty()).then_some(start));
            ui.episodes = episodes;
            ui.screen = Screen::Series;
        }

        Msg::Episodes(Err(e)) => {
            ui.status = format!("Could not load episodes: {e}");
        }
    }
}

// ---------------------------------------------------------------------------
// Input
// ---------------------------------------------------------------------------

fn handle_key(app: &mut App, ui: &mut Ui, key: KeyEvent, tx: &mpsc::Sender<Msg>) {
    // Ctrl-C always quits, even mid-edit.
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        ui.quit = true;
        return;
    }

    // While typing, most keys are text rather than commands.
    if ui.editing && ui.screen == Screen::Search {
        match key.code {
            KeyCode::Char(c) => ui.query.push(c),
            KeyCode::Backspace => {
                ui.query.pop();
            }
            KeyCode::Enter => {
                ui.editing = false;
                spawn_search(app, ui, tx);
            }
            KeyCode::Esc => ui.editing = false,
            _ => {}
        }
        return;
    }

    match key.code {
        KeyCode::Char('q') => ui.quit = true,
        KeyCode::Char('/') if ui.screen == Screen::Search => {
            ui.editing = true;
            ui.status = "Enter to search, Esc to cancel.".to_string();
        }
        KeyCode::Char('p') => {
            ui.screen = if ui.screen == Screen::Providers {
                Screen::Search
            } else {
                ui.reload_providers(app);
                Screen::Providers
            };
        }
        KeyCode::Char('j') | KeyCode::Down => scroll(ui, 1),
        KeyCode::Char('k') | KeyCode::Up => scroll(ui, -1),
        KeyCode::PageDown => scroll(ui, 10),
        KeyCode::PageUp => scroll(ui, -10),
        KeyCode::Char('g') => scroll(ui, isize::MIN / 2),
        KeyCode::Char('G') => scroll(ui, isize::MAX / 2),
        KeyCode::Esc | KeyCode::Char('h') | KeyCode::Left => back(ui),
        KeyCode::Char(' ') if ui.screen == Screen::Providers => toggle_provider(app, ui),
        KeyCode::Enter | KeyCode::Char('l') | KeyCode::Right => confirm(app, ui, tx),
        _ => {}
    }
}

fn scroll(ui: &mut Ui, delta: isize) {
    match ui.screen {
        Screen::Search => move_selection(&mut ui.results_state, ui.results.len(), delta),
        Screen::Series => move_selection(&mut ui.episodes_state, ui.episodes.len(), delta),
        Screen::Providers => move_selection(&mut ui.providers_state, ui.providers.len(), delta),
    }
}

fn back(ui: &mut Ui) {
    match ui.screen {
        Screen::Search => {}
        Screen::Series => {
            ui.screen = Screen::Search;
            ui.status = "Back to results.".to_string();
        }
        Screen::Providers => ui.screen = Screen::Search,
    }
}

fn confirm(app: &mut App, ui: &mut Ui, tx: &mpsc::Sender<Msg>) {
    match ui.screen {
        Screen::Search => {
            if let Some(series) = ui.selected_series().cloned() {
                ui.series = Some(series.clone());
                spawn_episodes(app, ui, series, tx);
            }
        }
        Screen::Series => {
            if let (Some(series), Some(episode)) =
                (ui.series.clone(), ui.selected_episode().cloned())
            {
                ui.pending_play = Some((series, episode));
            }
        }
        Screen::Providers => toggle_provider(app, ui),
    }
}

fn toggle_provider(app: &mut App, ui: &mut Ui) {
    let Some(index) = ui.providers_state.selected() else {
        return;
    };
    let Some(row) = ui.providers.get(index) else {
        return;
    };

    let id = row.id.clone();
    let now_enabled = !app.config.is_enabled(&id);
    app.config.provider_mut(&id).enabled = now_enabled;

    // Re-enabling by hand clears the failure record, so the provider gets a clean
    // slate rather than tripping the auto-disable threshold immediately.
    if now_enabled {
        app.health.record_success(&id);
    }

    if let Err(e) = app.save_config().and_then(|()| app.save_health()) {
        ui.status = format!("Could not save: {e}");
        return;
    }

    ui.reload_providers(app);
    ui.status = format!("{id} {}.", if now_enabled { "enabled" } else { "disabled" });
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

fn draw(f: &mut Frame, ui: &mut Ui) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // search box
            Constraint::Min(5),    // body
            Constraint::Length(1), // status
            Constraint::Length(1), // keys
        ])
        .split(f.area());

    draw_query(f, ui, chunks[0]);

    match ui.screen {
        Screen::Search => draw_results(f, ui, chunks[1]),
        Screen::Series => draw_series(f, ui, chunks[1]),
        Screen::Providers => draw_providers(f, ui, chunks[1]),
    }

    draw_status(f, ui, chunks[2]);
    draw_keys(f, ui, chunks[3]);
}

fn draw_query(f: &mut Frame, ui: &Ui, area: Rect) {
    let border = if ui.editing {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let cursor = if ui.editing { "▌" } else { "" };
    let text = format!(" {}{cursor}", ui.query);

    let widget = Paragraph::new(text).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(border)
            .title(" kuro "),
    );
    f.render_widget(widget, area);
}

fn draw_results(f: &mut Frame, ui: &mut Ui, area: Rect) {
    let items: Vec<ListItem> = ui
        .results
        .iter()
        .map(|s| {
            let year = s
                .year
                .filter(|y| !s.title.contains(&y.to_string()))
                .map(|y| format!(" ({y})"))
                .unwrap_or_default();

            ListItem::new(Line::from(vec![
                Span::raw(format!("{}{year}  ", s.title)),
                Span::styled(
                    format!("[{}]", s.provider_id),
                    Style::default().fg(Color::DarkGray),
                ),
            ]))
        })
        .collect();

    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(" Results "))
        .highlight_style(
            Style::default()
                .bg(Color::Cyan)
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▸ ");

    f.render_stateful_widget(list, area, &mut ui.results_state);
}

fn draw_series(f: &mut Frame, ui: &mut Ui, area: Rect) {
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(area);

    let items: Vec<ListItem> = ui
        .episodes
        .iter()
        .map(|e| {
            let title = e.title.as_deref().unwrap_or("");
            ListItem::new(format!("Episode {:<6} {title}", e.number_label()))
        })
        .collect();

    let title = ui
        .series
        .as_ref()
        .map(|s| format!(" {} ", s.title))
        .unwrap_or_else(|| " Episodes ".to_string());

    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(title))
        .highlight_style(
            Style::default()
                .bg(Color::Cyan)
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▸ ");

    f.render_stateful_widget(list, columns[0], &mut ui.episodes_state);

    let synopsis = ui
        .series
        .as_ref()
        .and_then(|s| s.synopsis.clone())
        .unwrap_or_else(|| "No synopsis available.".to_string());

    let details = Paragraph::new(synopsis)
        .wrap(Wrap { trim: true })
        .block(Block::default().borders(Borders::ALL).title(" Details "));

    f.render_widget(details, columns[1]);
}

fn draw_providers(f: &mut Frame, ui: &mut Ui, area: Rect) {
    let items: Vec<ListItem> = ui
        .providers
        .iter()
        .map(|p| {
            let (mark, style) = if !p.enabled {
                ("[ ]", Style::default().fg(Color::DarkGray))
            } else if p.auto_disabled {
                ("[!]", Style::default().fg(Color::Yellow))
            } else {
                ("[x]", Style::default().fg(Color::Green))
            };

            let mut spans = vec![
                Span::styled(format!("{mark} "), style),
                Span::raw(format!("{:<20}", p.display_name)),
                Span::styled(
                    format!("{:<18}", p.id),
                    Style::default().fg(Color::DarkGray),
                ),
            ];

            if let Some(err) = &p.last_error {
                spans.push(Span::styled(
                    truncate(err, 40),
                    Style::default().fg(Color::Red),
                ));
            }

            ListItem::new(Line::from(spans))
        })
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Providers — space to toggle "),
        )
        .highlight_style(Style::default().add_modifier(Modifier::BOLD))
        .highlight_symbol("▸ ");

    f.render_stateful_widget(list, area, &mut ui.providers_state);
}

fn draw_status(f: &mut Frame, ui: &Ui, area: Rect) {
    let prefix = if ui.busy { "… " } else { "" };
    let widget =
        Paragraph::new(format!(" {prefix}{}", ui.status)).style(Style::default().fg(Color::Gray));
    f.render_widget(widget, area);
}

fn draw_keys(f: &mut Frame, ui: &Ui, area: Rect) {
    let keys = match ui.screen {
        _ if ui.editing => "Enter search · Esc cancel · Ctrl-C quit",
        Screen::Search => "/ search · j/k move · Enter open · p providers · q quit",
        Screen::Series => "j/k move · Enter play · Esc back · q quit",
        Screen::Providers => "space toggle · j/k move · Esc back · q quit",
    };

    let widget = Paragraph::new(format!(" {keys}")).style(Style::default().fg(Color::DarkGray));
    f.render_widget(widget, area);
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    // Char-based, not byte-based: provider errors routinely contain non-ASCII.
    let kept: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{kept}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_is_clamped_to_the_list() {
        let mut state = ListState::default();
        move_selection(&mut state, 3, 1);
        assert_eq!(state.selected(), Some(1));
        move_selection(&mut state, 3, 100);
        assert_eq!(state.selected(), Some(2), "cannot run past the end");
        move_selection(&mut state, 3, -100);
        assert_eq!(state.selected(), Some(0), "cannot run before the start");
    }

    #[test]
    fn empty_list_has_no_selection() {
        let mut state = ListState::default();
        state.select(Some(2));
        move_selection(&mut state, 0, 1);
        assert_eq!(state.selected(), None);
    }

    #[test]
    fn truncate_counts_characters_not_bytes() {
        // A byte-based truncate would panic or split these characters.
        assert_eq!(truncate("abc", 10), "abc");
        assert_eq!(truncate("日本語のエラー", 3), "日本…");
    }
}
