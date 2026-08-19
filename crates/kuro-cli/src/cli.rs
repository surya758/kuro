//! Command-line surface.

use clap::{Parser, Subcommand};
use kuro_core::QualityPref;

/// Which episodes a command should act on.
///
/// Borrowed from `ani-cli`, whose `-e 5` / `-r 1-5` split turns out to be the
/// natural shape here too: one episode to watch, a range to queue or download.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum EpisodeSpec {
    Single(f32),
    Range(f32, f32),
}

impl EpisodeSpec {
    /// Episodes from `available` that this spec selects, in ascending order.
    pub fn select<'a>(&self, available: &'a [f32]) -> Vec<&'a f32> {
        match self {
            Self::Single(n) => available
                .iter()
                .filter(|e| (**e - *n).abs() < f32::EPSILON)
                .collect(),
            Self::Range(lo, hi) => available
                .iter()
                .filter(|e| **e >= *lo && **e <= *hi)
                .collect(),
        }
    }
}

impl std::fmt::Display for EpisodeSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Single(n) => write!(f, "{n}"),
            Self::Range(lo, hi) => write!(f, "{lo}-{hi}"),
        }
    }
}

impl std::str::FromStr for EpisodeSpec {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        if s.is_empty() {
            return Err("empty episode spec".to_string());
        }

        // Split on the first `-` that isn't leading, so `1-5` is a range while a
        // bare `12.5` stays a single episode. Indexed by char boundary rather than
        // byte, so junk input cannot panic here.
        let separator = s
            .char_indices()
            .skip(1)
            .find(|(_, c)| *c == '-')
            .map(|(i, _)| i);

        if let Some(idx) = separator {
            let (lo, hi) = (s[..idx].trim(), s[idx + 1..].trim());
            let lo: f32 = lo
                .parse()
                .map_err(|_| format!("`{lo}` is not an episode number"))?;
            let hi: f32 = hi
                .parse()
                .map_err(|_| format!("`{hi}` is not an episode number"))?;
            if hi < lo {
                return Err(format!("range {lo}-{hi} runs backwards"));
            }
            return Ok(Self::Range(lo, hi));
        }

        s.parse()
            .map(Self::Single)
            .map_err(|_| format!("`{s}` is not an episode number or range like 1-5"))
    }
}

#[derive(Parser, Debug)]
#[command(
    name = "kuro",
    about = "Terminal anime streaming — scrapes pluggable providers, plays in IINA",
    version,
    propagate_version = true
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,

    /// Restrict the run to a single provider.
    #[arg(long, short, global = true, env = "KURO_PROVIDER")]
    pub provider: Option<String>,

    /// Cap quality (best/worst/2160p/1080p/720p/480p/360p).
    ///
    /// A ceiling, not a demand: the closest rung at or below it plays, so a host
    /// that stops at 1080p still works when you ask for more.
    #[arg(long, short, global = true, env = "KURO_QUALITY")]
    pub quality: Option<QualityPref>,

    /// Pick the Nth search result (1-based) instead of the best match.
    #[arg(long, short = 'S', global = true, value_name = "N")]
    pub select_nth: Option<usize>,

    /// Emit machine-readable JSON instead of formatted text.
    #[arg(long, global = true)]
    pub json: bool,

    /// Resolve the stream and print the command without launching the player.
    #[arg(long, global = true)]
    pub dry_run: bool,

    /// Bypass the page cache for this run.
    #[arg(long, global = true)]
    pub no_cache: bool,

    /// Skip openings and endings, where AniSkip has data for the episode.
    #[arg(long, global = true, env = "KURO_SKIP")]
    pub skip: bool,

    /// Increase log verbosity (-v info, -vv debug, -vvv trace).
    #[arg(long, short, global = true, action = clap::ArgAction::Count)]
    pub verbose: u8,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Search every enabled provider, then pick what to watch.
    ///
    /// Quote the query: `kuro search "against the gods"`.
    #[command(alias = "watch")]
    Search {
        #[arg(value_name = "QUERY")]
        query: String,
    },

    /// Play an episode, or queue a range, of the best-matching series.
    Play {
        #[arg(value_name = "QUERY")]
        query: String,

        /// Episode number or range, e.g. `15` or `1-5`. A range plays in order.
        #[arg(long, short = 'e', value_name = "SPEC")]
        ep: Option<EpisodeSpec>,

        /// Prefer a specific embed host, e.g. `rumble`.
        #[arg(long, short = 'm')]
        mirror: Option<String>,
    },

    /// Download an episode instead of streaming it.
    Download {
        #[arg(value_name = "QUERY")]
        query: String,

        /// Episode number or range, e.g. `15` or `1-5`.
        #[arg(long, short = 'e', value_name = "SPEC")]
        ep: Option<EpisodeSpec>,

        /// Download every episode of the series.
        #[arg(long, conflicts_with = "ep")]
        all: bool,

        /// Prefer a specific embed host.
        #[arg(long, short = 'm')]
        mirror: Option<String>,

        /// Directory to save into.
        #[arg(long, short = 'o', default_value = ".")]
        out: std::path::PathBuf,

        /// Episodes to download at once.
        #[arg(long, short = 'j', default_value_t = 3, value_name = "N")]
        jobs: usize,
    },

    /// Play the next unwatched episode of the most recently watched series.
    Next,

    /// Resume the most recent episode at its last position.
    #[command(name = "continue")]
    Continue,

    /// Show watch history.
    History {
        /// Maximum entries to show.
        #[arg(long, default_value_t = 20)]
        limit: usize,

        /// Erase the watch history instead of showing it.
        #[arg(long)]
        clear: bool,
    },

    /// Manage followed series.
    Bookmark {
        #[command(subcommand)]
        action: BookmarkAction,
    },

    /// Manage providers.
    Provider {
        #[command(subcommand)]
        action: ProviderAction,
    },

    /// Inspect configuration.
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },

    /// Inspect or clear the page cache.
    Cache {
        #[command(subcommand)]
        action: CacheAction,
    },

    /// Check the environment: player, yt-dlp, provider health.
    Doctor,

    /// Generate a shell completion script.
    Completions { shell: clap_complete::Shell },
}

#[derive(Subcommand, Debug)]
pub enum BookmarkAction {
    Add {
        query: String,
    },
    Rm {
        series_id: String,
    },
    /// List followed series, flagging any with a recently released episode.
    List,
    /// Re-fetch every bookmarked series and report episodes released since the
    /// last check.
    Check {
        /// How many days count as "recent".
        #[arg(long, default_value_t = kuro_store::DEFAULT_NEW_WINDOW_DAYS, value_name = "DAYS")]
        within: i64,

        /// Show every bookmark, not just the ones with something new.
        #[arg(long)]
        all: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum ProviderAction {
    /// List providers with their enabled state and health.
    List,
    Enable {
        id: String,
    },
    Disable {
        id: String,
    },
    /// Enable exactly one provider and disable the rest.
    Only {
        id: String,
    },
    /// Run the full scrape chain against a provider and report timings.
    Test {
        id: String,
    },
    /// Re-read selector TOMLs and report what loaded.
    Reload,
}

#[derive(Subcommand, Debug)]
pub enum CacheAction {
    /// Show where the cache lives and how many entries it holds.
    Status,
    /// Delete every cached page.
    Clear,
}

#[derive(Subcommand, Debug)]
pub enum ConfigAction {
    /// Print the config file path.
    Path,
    /// Print the effective configuration.
    Show,
    /// Write a fully-populated config file if none exists.
    Init,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn spec(s: &str) -> EpisodeSpec {
        EpisodeSpec::from_str(s).expect("valid spec")
    }

    #[test]
    fn parses_a_single_episode() {
        assert_eq!(spec("15"), EpisodeSpec::Single(15.0));
        assert_eq!(spec(" 7 "), EpisodeSpec::Single(7.0));
    }

    #[test]
    fn parses_a_decimal_episode_rather_than_a_range() {
        // Specials are numbered like 12.5; the dot must not be read as a range.
        assert_eq!(spec("12.5"), EpisodeSpec::Single(12.5));
    }

    #[test]
    fn parses_a_range() {
        assert_eq!(spec("1-5"), EpisodeSpec::Range(1.0, 5.0));
        assert_eq!(spec("10 - 12"), EpisodeSpec::Range(10.0, 12.0));
    }

    #[test]
    fn rejects_nonsense() {
        assert!(EpisodeSpec::from_str("").is_err());
        assert!(EpisodeSpec::from_str("abc").is_err());
        assert!(EpisodeSpec::from_str("1-x").is_err());
        // A backwards range is a typo, not an empty selection.
        assert!(EpisodeSpec::from_str("9-2").is_err());
    }

    #[test]
    fn rejects_multibyte_junk_without_panicking() {
        assert!(EpisodeSpec::from_str("第1話").is_err());
        assert!(EpisodeSpec::from_str("—").is_err());
    }

    #[test]
    fn selects_matching_episodes_only() {
        let available = [1.0, 2.0, 3.0, 4.0, 5.0, 12.5];

        assert_eq!(spec("3").select(&available), vec![&3.0]);
        assert_eq!(spec("2-4").select(&available), vec![&2.0, &3.0, &4.0]);
        assert_eq!(spec("12.5").select(&available), vec![&12.5]);

        // A range that overshoots yields what exists, not an error.
        assert_eq!(spec("4-99").select(&available), vec![&4.0, &5.0, &12.5]);
        assert!(spec("50").select(&available).is_empty());
    }
}
