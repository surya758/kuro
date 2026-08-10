//! mpv JSON IPC, used to read playback position for resume.
//!
//! IINA embeds mpv, so `--mpv-input-ipc-server` gives a standard mpv IPC socket.
//! Position tracking is best-effort by design: if the socket never appears or the
//! connection drops, resume degrades to "watched / not watched" rather than
//! failing playback.

use std::path::Path;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tracing::debug;

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Progress {
    pub position_secs: u64,
    pub duration_secs: Option<u64>,
}

/// Ask mpv for a single numeric property.
async fn get_number(socket: &Path, property: &str) -> Option<f64> {
    let stream = UnixStream::connect(socket).await.ok()?;
    let mut reader = BufReader::new(stream);

    let request = format!(r#"{{"command":["get_property","{property}"]}}"#);
    reader.get_mut().write_all(request.as_bytes()).await.ok()?;
    reader.get_mut().write_all(b"\n").await.ok()?;

    // mpv emits asynchronous event lines on the same socket, so read until a line
    // carrying a `data` field appears rather than assuming the first line answers.
    let mut line = String::new();
    for _ in 0..16 {
        line.clear();
        let n = reader.read_line(&mut line).await.ok()?;
        if n == 0 {
            return None;
        }
        let value: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if let Some(data) = value.get("data").and_then(|d| d.as_f64()) {
            return Some(data);
        }
    }
    None
}

pub async fn read_progress(socket: &Path) -> Option<Progress> {
    let position = get_number(socket, "time-pos").await?;
    let duration = get_number(socket, "duration").await;

    Some(Progress {
        position_secs: position.max(0.0) as u64,
        duration_secs: duration.filter(|d| *d > 0.0).map(|d| d as u64),
    })
}

/// Poll the socket until it stops responding, returning the last position seen.
///
/// The socket only appears once IINA has started mpv, so early failures are
/// expected and tolerated until `startup_grace` elapses.
pub async fn track_until_exit(
    socket: &Path,
    interval: Duration,
    startup_grace: Duration,
) -> Option<Progress> {
    let started = std::time::Instant::now();
    let mut last: Option<Progress> = None;

    loop {
        tokio::time::sleep(interval).await;

        match read_progress(socket).await {
            Some(progress) => last = Some(progress),
            None => {
                // Before the grace period the player simply hasn't opened the
                // socket yet; after it, an unreadable socket means playback ended.
                if started.elapsed() > startup_grace {
                    debug!("mpv IPC socket closed; stopping position tracking");
                    return last;
                }
            }
        }
    }
}
