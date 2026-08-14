//! Playback.
//!
//! Only IINA is implemented, but the [`Player`] trait keeps the rest of the app
//! from depending on it directly.

pub mod iina;
pub mod ipc;

use async_trait::async_trait;
use kuro_core::{PlayerError, SkipTimes, Stream};

pub use iina::IinaPlayer;
pub use ipc::Progress;

#[derive(Debug, Clone, Default)]
pub struct PlaybackOpts {
    /// Shown in the player title bar instead of a CDN hash.
    pub title: Option<String>,
    /// Resume position in seconds.
    pub start_secs: Option<u64>,
    pub fullscreen: bool,
    /// Path for the mpv IPC socket, enabling position tracking.
    pub ipc_socket: Option<String>,
    /// Opening/ending intervals to jump over. Requires `skip_script`.
    pub skip: Option<SkipTimes>,
    /// Path to the mpv Lua script that performs the skipping.
    pub skip_script: Option<std::path::PathBuf>,
}

/// The mpv script that performs skipping, embedded so it works identically for a
/// `cargo install` and a Homebrew install with no asset-path lookup.
const SKIP_SCRIPT: &str = include_str!("kuro-skip.lua");

/// Write the skip script to a temp file and return its path.
///
/// Rewritten each run rather than cached, so an upgrade can never leave a stale
/// script behind.
pub fn write_skip_script() -> std::io::Result<std::path::PathBuf> {
    let path = std::env::temp_dir().join(format!("kuro-skip-{}.lua", std::process::id()));
    std::fs::write(&path, SKIP_SCRIPT)?;
    Ok(path)
}

pub struct PlayHandle {
    pub(crate) child: tokio::process::Child,
}

impl PlayHandle {
    /// Wait for the player to exit.
    pub async fn wait(&mut self) -> Result<(), PlayerError> {
        let status = self.child.wait().await?;
        match status.code() {
            Some(0) | None => Ok(()),
            Some(code) => Err(PlayerError::Exited(code)),
        }
    }
}

#[async_trait]
pub trait Player: Send + Sync {
    fn name(&self) -> &str;

    async fn is_available(&self) -> bool;

    /// The exact command that [`Player::play`] would run, for `--dry-run`.
    fn command_preview(&self, stream: &Stream, opts: &PlaybackOpts) -> Vec<String>;

    async fn play(&self, stream: &Stream, opts: &PlaybackOpts) -> Result<PlayHandle, PlayerError>;
}

/// A unique IPC socket path for one playback session.
/// Remove IPC sockets left behind by earlier runs.
///
/// A session ended with Ctrl-C never reaches its own cleanup, so without this the
/// temp directory accumulates a dead socket per interrupted playback.
pub fn sweep_stale_sockets() {
    const MAX_AGE: std::time::Duration = std::time::Duration::from_secs(6 * 60 * 60);

    let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let is_ours = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("kuro-mpv-") && n.ends_with(".sock"));
        if !is_ours {
            continue;
        }

        let stale = entry
            .metadata()
            .and_then(|m| m.modified())
            .map(|m| m.elapsed().map(|age| age > MAX_AGE).unwrap_or(false))
            .unwrap_or(false);

        if stale {
            std::fs::remove_file(&path).ok();
        }
    }
}

pub fn ipc_socket_path() -> String {
    std::env::temp_dir()
        .join(format!("kuro-mpv-{}.sock", std::process::id()))
        .display()
        .to_string()
}
