//! Inline arrow-key selection.
//!
//! Renders in the normal scrollback rather than the alternate screen, so a chosen
//! item stays visible in the terminal history afterwards — the list is erased on
//! exit and replaced by a single summary line.

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use std::io::Write;

/// Most rows shown at once; longer lists scroll.
const WINDOW: usize = 12;

pub struct Item {
    pub label: String,
    /// Dimmed text shown after the label, e.g. a provider name.
    pub hint: Option<String>,
}

impl Item {
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            hint: None,
        }
    }

    pub fn with_hint(label: impl Into<String>, hint: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            hint: Some(hint.into()),
        }
    }
}

/// What ended the selection.
pub enum Choice {
    Picked(usize),
    Cancelled,
}

/// Show a selectable list and return the chosen index.
///
/// Falls back to a numbered prompt when stdin is not a terminal, so the flow still
/// works over a pipe or in a script.
pub fn select(title: &str, items: &[Item]) -> Result<Choice> {
    if items.is_empty() {
        anyhow::bail!("nothing to choose from");
    }
    if !super::stdin_is_tty() || !super::stderr_is_tty() {
        return fallback_prompt(title, items);
    }

    enable_raw_mode()?;
    let result = interactive(title, items);
    disable_raw_mode()?;

    // Erase whatever the list left behind before returning, so the caller's own
    // output starts on a clean line.
    let mut err = std::io::stderr();
    let _ = write!(err, "\r\x1b[2K");
    let _ = err.flush();

    result
}

fn interactive(title: &str, items: &[Item]) -> Result<Choice> {
    let mut cursor = 0usize;
    let mut offset = 0usize;
    let mut drawn = 0usize;

    loop {
        drawn = draw(title, items, cursor, &mut offset, drawn)?;

        let ev = event::read()?;
        let Event::Key(key) = ev else { continue };
        if key.kind != KeyEventKind::Press {
            continue;
        }

        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            erase(drawn)?;
            return Ok(Choice::Cancelled);
        }

        match key.code {
            KeyCode::Up | KeyCode::Char('k') => cursor = cursor.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                cursor = (cursor + 1).min(items.len() - 1);
            }
            KeyCode::Home | KeyCode::Char('g') => cursor = 0,
            KeyCode::End | KeyCode::Char('G') => cursor = items.len() - 1,
            KeyCode::PageUp => cursor = cursor.saturating_sub(WINDOW),
            KeyCode::PageDown => cursor = (cursor + WINDOW).min(items.len() - 1),
            KeyCode::Enter => {
                erase(drawn)?;
                return Ok(Choice::Picked(cursor));
            }
            KeyCode::Esc | KeyCode::Char('q') => {
                erase(drawn)?;
                return Ok(Choice::Cancelled);
            }
            // Number keys jump straight to a row, matching the printed labels.
            KeyCode::Char(c) if c.is_ascii_digit() && c != '0' => {
                let n = c.to_digit(10).unwrap_or(1) as usize;
                if n <= items.len() {
                    cursor = n - 1;
                }
            }
            _ => {}
        }
    }
}

/// Redraw the list, returning how many lines were written.
fn draw(
    title: &str,
    items: &[Item],
    cursor: usize,
    offset: &mut usize,
    previously_drawn: usize,
) -> Result<usize> {
    // Keep the cursor inside the visible window.
    if cursor < *offset {
        *offset = cursor;
    } else if cursor >= *offset + WINDOW {
        *offset = cursor + 1 - WINDOW;
    }

    let mut err = std::io::stderr();
    if previously_drawn > 0 {
        write!(err, "\x1b[{previously_drawn}A")?;
    }

    let mut lines = 0;
    write!(err, "\r\x1b[2K\x1b[1m{title}\x1b[0m\r\n")?;
    lines += 1;

    let end = (*offset + WINDOW).min(items.len());
    for (i, item) in items.iter().enumerate().take(end).skip(*offset) {
        let selected = i == cursor;
        let marker = if selected { "▸" } else { " " };
        let number = i + 1;

        let label = if selected {
            format!("\x1b[36;1m{}\x1b[0m", item.label)
        } else {
            item.label.clone()
        };

        let hint = item
            .hint
            .as_ref()
            .map(|h| format!("  \x1b[2m{h}\x1b[0m"))
            .unwrap_or_default();

        write!(
            err,
            "\r\x1b[2K{marker} \x1b[2m{number:>2}.\x1b[0m {label}{hint}\r\n"
        )?;
        lines += 1;
    }

    let scroll = if items.len() > WINDOW {
        format!("  ·  {}/{}", cursor + 1, items.len())
    } else {
        String::new()
    };
    write!(
        err,
        "\r\x1b[2K\x1b[2m↑↓ move · ⏎ select · q back{scroll}\x1b[0m\r\n"
    )?;
    lines += 1;

    err.flush()?;
    Ok(lines)
}

fn erase(lines: usize) -> Result<()> {
    let mut err = std::io::stderr();
    if lines > 0 {
        write!(err, "\x1b[{lines}A")?;
        for _ in 0..lines {
            write!(err, "\r\x1b[2K\x1b[1B")?;
        }
        write!(err, "\x1b[{lines}A")?;
    }
    err.flush()?;
    Ok(())
}

/// Numbered prompt for non-interactive stdin.
fn fallback_prompt(title: &str, items: &[Item]) -> Result<Choice> {
    eprintln!("{title}");
    for (i, item) in items.iter().enumerate() {
        let hint = item
            .hint
            .as_ref()
            .map(|h| format!("  [{h}]"))
            .unwrap_or_default();
        eprintln!("{:>3}. {}{hint}", i + 1, item.label);
    }

    eprint!("Select [1-{}, enter for 1]: ", items.len());
    std::io::stderr().flush()?;

    let mut line = String::new();
    if std::io::stdin().read_line(&mut line)? == 0 {
        return Ok(Choice::Cancelled);
    }

    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Ok(Choice::Picked(0));
    }
    match trimmed.parse::<usize>() {
        Ok(n) if n >= 1 && n <= items.len() => Ok(Choice::Picked(n - 1)),
        _ => Ok(Choice::Cancelled),
    }
}

/// Read a free-text line, used for the download-range prompt.
pub fn prompt_line(prompt: &str) -> Result<Option<String>> {
    eprint!("{prompt}");
    std::io::stderr().flush()?;

    let mut line = String::new();
    if std::io::stdin().read_line(&mut line)? == 0 {
        return Ok(None);
    }
    let trimmed = line.trim().to_string();
    Ok((!trimmed.is_empty()).then_some(trimmed))
}
