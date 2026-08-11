//! Playback.
//!
//! Only IINA is implemented, but the [`Player`] trait keeps the rest of the app
//! from depending on it directly.

pub mod iina;
pub mod ipc;

use async_trait::async_trait;
use kuro_core::{PlayerError, Stream};

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
