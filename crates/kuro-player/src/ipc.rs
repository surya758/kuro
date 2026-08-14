//! mpv JSON IPC.
//!
//! IINA embeds mpv, so `--mpv-input-ipc-server` gives a standard mpv IPC socket.
//! Two things run over it: reading playback position for resume, and appending
//! upcoming episodes to the playlist so the player's own next/previous controls
//! work part-way through an episode.
//!
//! Everything here is best-effort. If the socket never appears or the connection
//! drops, resume degrades to "watched / not watched" and the playlist simply holds
//! one episode.

use std::collections::BTreeMap;
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

/// Send one command and read until a reply carrying `data` appears.
///
/// mpv interleaves asynchronous event lines on the same socket, so the first line
/// back is frequently not the answer.
async fn request(socket: &Path, command: &str) -> Option<serde_json::Value> {
    let stream = UnixStream::connect(socket).await.ok()?;
    let mut reader = BufReader::new(stream);

    reader.get_mut().write_all(command.as_bytes()).await.ok()?;
    reader.get_mut().write_all(b"\n").await.ok()?;

    let mut line = String::new();
    for _ in 0..16 {
        line.clear();
        if reader.read_line(&mut line).await.ok()? == 0 {
            return None;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        if value.get("error").is_some() {
            return Some(value);
        }
    }
    None
}

async fn get_number(socket: &Path, property: &str) -> Option<f64> {
    let command = format!(r#"{{"command":["get_property","{property}"]}}"#);
    request(socket, &command)
        .await?
        .get("data")?
        .as_f64()
}

/// Add a stream to the end of the player's playlist, labelled `title`.
///
/// Goes via a one-entry M3U rather than `loadfile`, because a playlist entry's
/// display name can only come from the playlist itself: `loadfile` — even with a
/// per-file `force-media-title` — leaves `playlist/N/title` unset, so the player's
/// playlist panel falls back to showing the raw CDN URL.
pub async fn append_to_playlist(socket: &Path, url: &str, title: &str) -> bool {
    let Some(playlist) = write_entry_playlist(url, title) else {
        return false;
    };

    let command = serde_json::json!({
        "command": ["loadlist", playlist.to_string_lossy(), "append"]
    })
    .to_string();

    let ok = request(socket, &command).await.is_some();
    debug!(url, title, ok, "appended to playlist");
    ok
}

/// Write a single-entry M3U carrying the episode name.
fn write_entry_playlist(url: &str, title: &str) -> Option<std::path::PathBuf> {
    // A newline in the title would forge extra playlist entries.
    let title = title.replace(['\n', '\r'], " ");
    let name = format!(
        "kuro-entry-{}-{}.m3u",
        std::process::id(),
        // Distinguishes concurrent entries without pulling in a random source.
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );

    let path = std::env::temp_dir().join(name);
    let body = format!("#EXTM3U\n#EXTINF:-1,{title}\n{url}\n");
    std::fs::write(&path, body).ok()?;
    Some(path)
}

/// Retitle the player window.
///
/// `--force-media-title` is a global option, so the title passed at launch would
/// otherwise stay pinned to the first episode for every later playlist entry.
pub async fn set_media_title(socket: &Path, title: &str) -> bool {
    let command = serde_json::json!({
        "command": ["set_property", "force-media-title", title]
    })
    .to_string();
    request(socket, &command).await.is_some()
}

pub async fn read_progress(socket: &Path) -> Option<(usize, Progress)> {
    let position = get_number(socket, "time-pos").await?;
    let duration = get_number(socket, "duration").await;
    // Absent on very old mpv builds; treat the session as a single item then.
    let index = get_number(socket, "playlist-pos").await.unwrap_or(0.0);

    Some((
        index.max(0.0) as usize,
        Progress {
            position_secs: position.max(0.0) as u64,
            duration_secs: duration.filter(|d| *d > 0.0).map(|d| d as u64),
        },
    ))
}

/// Poll until the socket stops responding, returning the last position seen for
/// each playlist index.
///
/// Keyed by index rather than a single value so that skipping ahead mid-episode
/// still records progress against the right episode.
pub async fn track_until_exit(
    socket: &Path,
    interval: Duration,
    startup_grace: Duration,
) -> BTreeMap<usize, Progress> {
    let started = std::time::Instant::now();
    let mut seen: BTreeMap<usize, Progress> = BTreeMap::new();

    loop {
        tokio::time::sleep(interval).await;

        match read_progress(socket).await {
            Some((index, progress)) => {
                seen.insert(index, progress);
            }
            None => {
                // Before the grace period the player simply has not opened the
                // socket yet; after it, an unreadable socket means playback ended.
                if started.elapsed() > startup_grace {
                    debug!("mpv IPC socket closed; stopping position tracking");
                    return seen;
                }
            }
        }
    }
}
