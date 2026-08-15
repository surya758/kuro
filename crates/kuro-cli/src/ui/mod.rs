//! Terminal presentation: spinners, selection lists, progress bars.
//!
//! Everything here degrades when the terminal is not interactive. Control codes go
//! to stderr so that `--json` and piped stdout stay machine-readable.

pub mod progress;
pub mod select;
pub mod spinner;
pub mod style;

use std::io::IsTerminal;

pub use progress::ProgressHandle;
pub use select::{prompt_line, select, Choice, Item};
pub use spinner::Spinner;

pub fn stderr_is_tty() -> bool {
    std::io::stderr().is_terminal()
}

pub fn stdin_is_tty() -> bool {
    std::io::stdin().is_terminal()
}

pub fn stdout_is_tty() -> bool {
    std::io::stdout().is_terminal()
}

/// Dim text for **stdout**, or the bare string when stdout is redirected.
///
/// Escape codes in piped output corrupt anything downstream that greps or parses
/// it, so colour is opt-in on being a terminal. Interactive output goes to stderr
/// and uses [`style`] instead.
pub fn dim_stdout(text: impl AsRef<str>) -> String {
    if stdout_is_tty() {
        format!("\x1b[2m{}\x1b[0m", text.as_ref())
    } else {
        text.as_ref().to_string()
    }
}

/// Whether the full interactive flow can run.
pub fn interactive() -> bool {
    stdin_is_tty() && stderr_is_tty()
}
