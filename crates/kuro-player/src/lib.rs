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
pub fn ipc_socket_path() -> String {
    std::env::temp_dir()
        .join(format!("kuro-mpv-{}.sock", std::process::id()))
        .display()
        .to_string()
}
