//! Command-line surface.

use clap::{Parser, Subcommand};
use kuro_core::QualityPref;

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
    #[arg(long, short, global = true)]
    pub provider: Option<String>,

    /// Override the configured quality (best/worst/2160p/1080p/720p/480p/360p).
    #[arg(long, short, global = true)]
    pub quality: Option<QualityPref>,

    /// Emit machine-readable JSON instead of formatted text.
    #[arg(long, global = true)]
    pub json: bool,

    /// Resolve the stream and print the command without launching the player.
    #[arg(long, global = true)]
    pub dry_run: bool,

    /// Bypass the page cache for this run.
    #[arg(long, global = true)]
    pub no_cache: bool,

    /// Increase log verbosity (-v info, -vv debug, -vvv trace).
    #[arg(long, short, global = true, action = clap::ArgAction::Count)]
    pub verbose: u8,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Search every enabled provider.
    Search {
        query: Vec<String>,
    },

    /// Search, pick a series and episode interactively, then play.
    Watch {
        query: Vec<String>,
    },

    /// Play a specific episode of the best-matching series.
    Play {
        query: Vec<String>,

        /// Episode number.
        #[arg(long, short = 'e')]
        ep: Option<f32>,

        /// Prefer a specific embed host, e.g. `rumble`.
        #[arg(long, short = 'm')]
        mirror: Option<String>,
    },

    /// Download an episode instead of streaming it.
    Download {
        query: Vec<String>,

        /// Episode number.
        #[arg(long, short = 'e')]
        ep: Option<f32>,

        /// Download every episode of the series.
        #[arg(long, conflicts_with = "ep")]
        all: bool,

        /// Prefer a specific embed host.
        #[arg(long, short = 'm')]
        mirror: Option<String>,

        /// Directory to save into.
        #[arg(long, short = 'o', default_value = ".")]
        out: std::path::PathBuf,
    },

    /// Play the next unwatched episode of the most recently watched series.
    Next,

    /// Resume the most recent episode at its last position.
    #[command(name = "continue")]
    Continue,

    /// Show watch history.
    List {
        /// Maximum entries to show.
        #[arg(long, default_value_t = 20)]
        limit: usize,
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
    Completions {
        shell: clap_complete::Shell,
    },
}

#[derive(Subcommand, Debug)]
pub enum BookmarkAction {
    Add { query: Vec<String> },
    Rm { series_id: String },
    List,
}

#[derive(Subcommand, Debug)]
pub enum ProviderAction {
    /// List providers with their enabled state and health.
    List,
    Enable { id: String },
    Disable { id: String },
    /// Enable exactly one provider and disable the rest.
    Only { id: String },
    /// Run the full scrape chain against a provider and report timings.
    Test { id: String },
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
