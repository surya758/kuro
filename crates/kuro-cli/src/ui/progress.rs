//! Multi-download progress display.
//!
//! One row per episode, redrawn in place. This replaces passing yt-dlp's own bar
//! through, which only worked for a single download — several processes writing
//! progress to one terminal overwrite each other.

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::task::JoinHandle;

const BAR_WIDTH: usize = 22;
const REDRAW: Duration = Duration::from_millis(200);

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum State {
    Waiting,
    Running,
    Done,
    Failed,
}

#[derive(Debug, Clone)]
pub struct Row {
    pub label: String,
    pub state: State,
    pub downloaded: u64,
    /// yt-dlp's estimate, which drifts upward for HLS as more segments are seen.
    pub total: u64,
    pub speed: f64,
    pub eta: Option<u64>,
    pub note: Option<String>,
}

impl Row {
    fn new(label: String) -> Self {
        Self {
            label,
            state: State::Waiting,
            downloaded: 0,
            total: 0,
            speed: 0.0,
            eta: None,
            note: None,
        }
    }

    fn fraction(&self) -> f64 {
        match self.state {
            State::Done => 1.0,
            _ if self.total == 0 => 0.0,
            _ => (self.downloaded as f64 / self.total as f64).clamp(0.0, 1.0),
        }
    }
}

/// Shared handle passed to each download task.
#[derive(Clone)]
pub struct Progress {
    rows: Arc<Mutex<Vec<Row>>>,
    stop: Arc<AtomicBool>,
}

pub struct ProgressHandle {
    progress: Progress,
    renderer: Option<JoinHandle<()>>,
}

impl Progress {
    pub fn update(&self, index: usize, f: impl FnOnce(&mut Row)) {
        if let Ok(mut rows) = self.rows.lock() {
            if let Some(row) = rows.get_mut(index) {
                f(row);
            }
        }
    }

    /// Report a line of yt-dlp progress output. Non-progress lines are ignored.
    pub fn apply_ytdlp_line(&self, index: usize, line: &str) {
        let Some(parsed) = parse_progress_line(line) else {
            return;
        };
        self.update(index, |row| {
            row.state = State::Running;
            row.downloaded = parsed.downloaded;
            if parsed.total > 0 {
                row.total = parsed.total;
            }
            row.speed = parsed.speed;
            row.eta = parsed.eta;
        });
    }

    pub fn finish(&self, index: usize, ok: bool, note: Option<String>) {
        self.update(index, |row| {
            row.state = if ok { State::Done } else { State::Failed };
            row.note = note;
            row.speed = 0.0;
            row.eta = None;
        });
    }
}

impl ProgressHandle {
    /// Start rendering rows for `labels`.
    pub fn start(labels: Vec<String>) -> Self {
        let rows: Vec<Row> = labels.into_iter().map(Row::new).collect();
        let count = rows.len();
        let rows = Arc::new(Mutex::new(rows));
        let stop = Arc::new(AtomicBool::new(false));
        let enabled = super::stderr_is_tty();

        let progress = Progress {
            rows: Arc::clone(&rows),
            stop: Arc::clone(&stop),
        };

        if !enabled || count == 0 {
            return Self {
                progress,
                renderer: None,
            };
        }

        // Reserve the block so the first redraw has lines to move back over.
        {
            let mut err = std::io::stderr();
            for _ in 0..count {
                let _ = writeln!(err);
            }
            let _ = err.flush();
        }

        let renderer = tokio::spawn(async move {
            while !stop.load(Ordering::Relaxed) {
                render(&rows, count);
                tokio::time::sleep(REDRAW).await;
            }
            render(&rows, count);
        });

        Self {
            progress,
            renderer: Some(renderer),
        }
    }

    pub fn handle(&self) -> Progress {
        self.progress.clone()
    }

    /// Stop redrawing, leaving the final state on screen.
    pub async fn finish(mut self) {
        self.progress.stop.store(true, Ordering::Relaxed);
        if let Some(renderer) = self.renderer.take() {
            let _ = renderer.await;
        }
    }
}

fn render(rows: &Arc<Mutex<Vec<Row>>>, count: usize) {
    let Ok(rows) = rows.lock() else { return };
    let mut err = std::io::stderr();

    let _ = write!(err, "\x1b[{count}A");
    for row in rows.iter() {
        let _ = write!(err, "\r\x1b[2K{}\r\n", format_row(row));
    }
    let _ = err.flush();
}

fn format_row(row: &Row) -> String {
    let (mark, colour) = match row.state {
        State::Waiting => ("·", "\x1b[2m"),
        State::Running => ("↓", "\x1b[36m"),
        State::Done => ("✓", "\x1b[32m"),
        State::Failed => ("✗", "\x1b[31m"),
    };

    match row.state {
        State::Waiting => format!(
            "  {colour}{mark} {:<10}\x1b[0m \x1b[2mqueued\x1b[0m",
            row.label
        ),
        State::Failed => format!(
            "  {colour}{mark} {:<10}\x1b[0m \x1b[31m{}\x1b[0m",
            row.label,
            row.note.as_deref().unwrap_or("failed")
        ),
        State::Done => format!(
            "  {colour}{mark} {:<10}\x1b[0m {} \x1b[2m{}\x1b[0m",
            row.label,
            bar(1.0),
            human_bytes(row.downloaded.max(row.total))
        ),
        State::Running => {
            let pct = row.fraction() * 100.0;
            let eta = row
                .eta
                .map(|s| format!("ETA {}", clock(s)))
                .unwrap_or_else(|| "ETA --".to_string());
            format!(
                "  {colour}{mark} {:<10}\x1b[0m {} \x1b[2m{pct:>3.0}%  {:>9}/s  {eta}\x1b[0m",
                row.label,
                bar(row.fraction()),
                human_bytes(row.speed as u64)
            )
        }
    }
}

fn bar(fraction: f64) -> String {
    let filled = (fraction.clamp(0.0, 1.0) * BAR_WIDTH as f64).round() as usize;
    format!(
        "\x1b[36m{}\x1b[0m\x1b[2m{}\x1b[0m",
        "█".repeat(filled),
        "░".repeat(BAR_WIDTH - filled)
    )
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{value:.0} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn clock(secs: u64) -> String {
    format!("{}:{:02}", secs / 60, secs % 60)
}

struct ParsedProgress {
    downloaded: u64,
    total: u64,
    speed: f64,
    eta: Option<u64>,
}

/// yt-dlp emits `KURO|<downloaded>|<total>|<speed>|<eta>` via `--progress-template`.
/// Unknown fields arrive as `NA`, and byte counts arrive as floats.
fn parse_progress_line(line: &str) -> Option<ParsedProgress> {
    let rest = line.strip_prefix("KURO|")?;
    let mut fields = rest.split('|');

    let downloaded = number(fields.next()?)? as u64;
    let total = number(fields.next()?).unwrap_or(0.0) as u64;
    let speed = number(fields.next()?).unwrap_or(0.0);
    let eta = number(fields.next()?).map(|v| v as u64);

    Some(ParsedProgress {
        downloaded,
        total,
        speed,
        eta,
    })
}

fn number(field: &str) -> Option<f64> {
    let field = field.trim();
    if field.is_empty() || field == "NA" || field == "None" {
        return None;
    }
    field.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_full_progress_line() {
        let p = parse_progress_line("KURO|207176|33722400.0|102366.09|181.08").expect("parses");
        assert_eq!(p.downloaded, 207176);
        assert_eq!(p.total, 33722400);
        assert_eq!(p.eta, Some(181));
    }

    #[test]
    fn tolerates_unknown_fields() {
        // Early in a download yt-dlp reports NA for eta, and sometimes for total.
        let p = parse_progress_line("KURO|1024|NA|705.68|NA").expect("parses");
        assert_eq!(p.downloaded, 1024);
        assert_eq!(p.total, 0);
        assert_eq!(p.eta, None);
    }

    #[test]
    fn ignores_non_progress_output() {
        assert!(parse_progress_line("[RumbleEmbed] Extracting URL: https://x").is_none());
        assert!(parse_progress_line("").is_none());
    }

    #[test]
    fn fraction_is_clamped_and_safe_without_a_total() {
        let mut row = Row::new("E1".into());
        row.state = State::Running;
        assert_eq!(row.fraction(), 0.0, "no total yet must not divide by zero");

        row.total = 100;
        row.downloaded = 250;
        assert_eq!(
            row.fraction(),
            1.0,
            "an over-run estimate cannot exceed 100%"
        );

        // A finished row reads as complete even if the estimate never caught up.
        row.state = State::Done;
        row.total = 0;
        assert_eq!(row.fraction(), 1.0);
    }

    #[test]
    fn bytes_render_in_sensible_units() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1536), "1.5 KiB");
        assert_eq!(human_bytes(5 * 1024 * 1024), "5.0 MiB");
    }

    #[test]
    fn bar_is_always_the_declared_width() {
        for f in [0.0, 0.33, 1.0, 5.0, -1.0] {
            let rendered = bar(f);
            let blocks = rendered.matches('█').count() + rendered.matches('░').count();
            assert_eq!(blocks, BAR_WIDTH, "fraction {f} produced {blocks} cells");
        }
    }
}
