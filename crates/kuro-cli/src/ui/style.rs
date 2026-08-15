//! Terminal styling.
//!
//! One accent colour marks everything the tool is *doing* or asking you to act on —
//! spinners, the selected row, progress bars, the playback marker. Status keeps the
//! conventional green/red/yellow, so "this is active" never has to compete with
//! "this succeeded" or "this failed".

/// Orchid. A 256-colour index rather than one of the basic eight, so it stays
/// distinct from the green/red/yellow used for status and reads on both light and
/// dark backgrounds.
const ACCENT_FG: &str = "\x1b[38;5;170m";
const ACCENT_FG_BOLD: &str = "\x1b[1;38;5;170m";
const DIM_FG: &str = "\x1b[2m";
const RESET: &str = "\x1b[0m";

/// Whether to emit colour at all.
///
/// Gated on stderr, where all the interactive output goes; honours `NO_COLOR`.
fn enabled() -> bool {
    std::env::var_os("NO_COLOR").is_none() && super::stderr_is_tty()
}

pub fn accent(text: impl AsRef<str>) -> String {
    wrap(ACCENT_FG, text.as_ref())
}

pub fn accent_bold(text: impl AsRef<str>) -> String {
    wrap(ACCENT_FG_BOLD, text.as_ref())
}

pub fn dim(text: impl AsRef<str>) -> String {
    wrap(DIM_FG, text.as_ref())
}

fn wrap(code: &str, text: &str) -> String {
    if enabled() {
        format!("{code}{text}{RESET}")
    } else {
        text.to_string()
    }
}

/// Raw codes, for the few places that build one escape-laden line by hand.
pub mod raw {
    pub const ACCENT: &str = super::ACCENT_FG;
    pub const DIM: &str = super::DIM_FG;
    pub const RESET: &str = super::RESET;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn styling_is_a_no_op_when_colour_is_off() {
        // Tests do not run against a terminal, so this exercises the disabled path.
        assert_eq!(accent("hello"), "hello");
        assert_eq!(dim("hello"), "hello");
    }

    #[test]
    fn accent_is_distinct_from_the_status_colours() {
        // Green/red/yellow are reserved for success/failure/warning; the accent must
        // not collide with them or "active" and "succeeded" would look alike.
        for status in ["\x1b[32m", "\x1b[31m", "\x1b[33m"] {
            assert_ne!(raw::ACCENT, status);
            assert_ne!(ACCENT_FG_BOLD, status);
        }
    }
}
