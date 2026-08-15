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

use std::path::Path;
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
    request(socket, &command).await?.get("data")?.as_f64()
}

async fn get_flag(socket: &Path, property: &str) -> Option<bool> {
    let command = format!(r#"{{"command":["get_property","{property}"]}}"#);
    request(socket, &command).await?.get("data")?.as_bool()
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

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::UnixListener;

    fn socket_path(name: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("kuro-ipc-test-{name}.sock"));
        std::fs::remove_file(&path).ok();
        path
    }

    /// Stand-in for mpv's IPC socket.
    ///
    /// `answers` maps a property to the JSON mpv would return, or `None` for the
    /// "property unavailable" reply it sends whenever no file is loaded.
    fn spawn_raw_server(
        path: &Path,
        answers: Vec<(&'static str, Option<&'static str>)>,
    ) -> tokio::task::JoinHandle<()> {
        let listener = UnixListener::bind(path).expect("bind test socket");
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                    continue;
                }
                let unavailable = r#"{"error":"property unavailable"}"#.to_string();
                let reply = answers
                    .iter()
                    .find(|(property, _)| line.contains(property))
                    .and_then(|(_, value)| *value)
                    .map(|v| format!(r#"{{"data":{v},"error":"success"}}"#))
                    .unwrap_or(unavailable);
                let _ = reader.get_mut().write_all(reply.as_bytes()).await;
                let _ = reader.get_mut().write_all(b"\n").await;
            }
        })
    }

    fn spawn_server(
        path: &Path,
        answers: Vec<(&'static str, Option<f64>)>,
    ) -> tokio::task::JoinHandle<()> {
        let listener = UnixListener::bind(path).expect("bind test socket");
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                    continue;
                }
                let unavailable = r#"{"error":"property unavailable"}"#.to_string();
                let reply = answers
                    .iter()
                    .find(|(property, _)| line.contains(property))
                    .and_then(|(_, value)| *value)
                    .map(|v| format!(r#"{{"data":{v},"error":"success"}}"#))
                    .unwrap_or(unavailable);
                let _ = reader.get_mut().write_all(reply.as_bytes()).await;
                let _ = reader.get_mut().write_all(b"\n").await;
            }
        })
    }

    /// Replies success only when the request matches, so the assertion proves what
    /// was sent rather than merely that something was.
    fn spawn_expecting(
        path: &Path,
        must_contain: Vec<&'static str>,
    ) -> tokio::task::JoinHandle<()> {
        let listener = UnixListener::bind(path).expect("bind test socket");
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                    continue;
                }
                if !must_contain.iter().all(|needle| line.contains(needle)) {
                    // Hang up rather than reply: any JSON carrying an `error` field
                    // — including a rejection — reads as a valid answer, which
                    // would make the assertion below unfalsifiable.
                    continue;
                }
                let _ = reader
                    .get_mut()
                    .write_all(br#"{"data":null,"error":"success"}"#)
                    .await;
                let _ = reader.get_mut().write_all(b"\n").await;
            }
        })
    }

    #[tokio::test]
    async fn clearing_start_sets_the_property_to_none() {
        // Guards the regression where the launched episode's resume point leaked
        // onto every queued episode, opening unwatched ones part-way through.
        let path = socket_path("clear-start");
        let server = spawn_expecting(&path, vec!["set_property", "start", "none"]);

        assert!(clear_start(&path).await);

        server.abort();
        std::fs::remove_file(&path).ok();
    }

    #[tokio::test]
    async fn the_command_assertion_can_actually_fail() {
        // Proves the stand-in above rejects a mismatch, so the test that uses it
        // is measuring the command rather than passing unconditionally.
        let path = socket_path("clear-start-negative");
        let server = spawn_expecting(&path, vec!["set_property", "some-other-property"]);

        assert!(!clear_start(&path).await);

        server.abort();
        std::fs::remove_file(&path).ok();
    }

    #[tokio::test]
    async fn a_missing_socket_means_playback_ended() {
        let path = socket_path("missing");
        assert_eq!(poll_progress(&path).await, Poll::Closed);
    }

    #[tokio::test]
    async fn switching_episodes_does_not_look_like_a_closed_player() {
        // The regression: between playlist entries mpv answers "property
        // unavailable" for `time-pos`. Reporting that as `Closed` stopped the
        // progress recorder for good, so every episode after the first skip went
        // unrecorded.
        let path = socket_path("between-entries");
        let server = spawn_server(&path, vec![("time-pos", None)]);

        assert_eq!(poll_progress(&path).await, Poll::Unavailable);

        server.abort();
        std::fs::remove_file(&path).ok();
    }

    #[tokio::test]
    async fn a_playing_file_reports_its_index_and_position() {
        let path = socket_path("playing");
        let server = spawn_server(
            &path,
            vec![
                ("time-pos", Some(412.0)),
                ("duration", Some(1391.0)),
                ("playlist-pos", Some(1.0)),
            ],
        );

        let poll = poll_progress(&path).await;
        assert_eq!(
            poll,
            Poll::Progress(
                1,
                Progress {
                    position_secs: 412,
                    duration_secs: Some(1391),
                },
            ),
        );

        server.abort();
        std::fs::remove_file(&path).ok();
    }

    #[tokio::test]
    async fn a_player_holding_no_file_reports_idle() {
        // What a player that outlived its window looks like: still answering, but
        // with nothing loaded. Distinct from a playlist transition, so the caller
        // can end the episode instead of waiting forever.
        let path = socket_path("idle-core");
        let server = spawn_raw_server(
            &path,
            vec![("time-pos", None), ("idle-active", Some("true"))],
        );

        assert_eq!(poll_progress(&path).await, Poll::Idle);

        server.abort();
        std::fs::remove_file(&path).ok();
    }

    #[tokio::test]
    async fn a_loading_player_is_not_idle() {
        // Mid-transition: no position yet, but a file is on its way. Calling this
        // idle would cut the session off between episodes.
        let path = socket_path("loading");
        let server = spawn_raw_server(
            &path,
            vec![("time-pos", None), ("idle-active", Some("false"))],
        );

        assert_eq!(poll_progress(&path).await, Poll::Unavailable);

        server.abort();
        std::fs::remove_file(&path).ok();
    }

    #[tokio::test]
    async fn a_player_without_playlist_support_is_still_tracked() {
        // Very old mpv builds lack `playlist-pos`; the session is then a single item.
        let path = socket_path("no-playlist-pos");
        let server = spawn_server(&path, vec![("time-pos", Some(30.0))]);

        match poll_progress(&path).await {
            Poll::Progress(index, progress) => {
                assert_eq!(index, 0);
                assert_eq!(progress.position_secs, 30);
            }
            other => panic!("expected progress, got {other:?}"),
        }

        server.abort();
        std::fs::remove_file(&path).ok();
    }
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
/// Stop a launch-time `--start` from reaching later playlist entries.
///
/// `--start` is global: mpv re-applies it as each file loads, so the resume point of
/// the episode kuro launched would seek every queued episode to that same offset —
/// an unwatched episode opening part-way through. Clearing it once the first seek
/// has landed keeps resume working without leaking it forward.
pub async fn clear_start(socket: &Path) -> bool {
    let command = serde_json::json!({
        "command": ["set_property", "start", "none"]
    })
    .to_string();
    request(socket, &command).await.is_some()
}

pub async fn set_media_title(socket: &Path, title: &str) -> bool {
    let command = serde_json::json!({
        "command": ["set_property", "force-media-title", title]
    })
    .to_string();
    request(socket, &command).await.is_some()
}

/// Outcome of one progress poll.
///
/// Separating "no position right now" from "the player is gone" is the whole point.
/// mpv reports `time-pos` as unavailable whenever no file is loaded — which includes
/// the gap between playlist entries, i.e. every time the viewer skips to the next
/// episode. Collapsing that into the same answer as a dead socket ends progress
/// recording for the rest of the session, so a single episode change silently loses
/// all later history.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Poll {
    /// Playlist index and position.
    Progress(usize, Progress),
    /// The player is alive but has nothing to report *yet* — loading a file, or
    /// moving between playlist entries.
    Unavailable,
    /// The player is alive with nothing loaded at all.
    ///
    /// Distinct from [`Poll::Unavailable`], which looks identical through
    /// `time-pos` alone. A player that outlives its window — closing IINA's window
    /// with ⌘W quits the window, not the app — keeps answering on the socket, so
    /// this is the only thing separating "the viewer is done" from "the next
    /// episode is a moment away".
    Idle,
    /// The socket is unreachable: playback is over.
    Closed,
}

pub async fn poll_progress(socket: &Path) -> Poll {
    // An unreachable socket is one end-of-playback signal. Checking the connection
    // on its own keeps that verdict independent of whether any given property
    // happens to be readable at this instant.
    if UnixStream::connect(socket).await.is_err() {
        return Poll::Closed;
    }

    let Some(position) = get_number(socket, "time-pos").await else {
        // No position has two causes worth telling apart, and `idle-active` is what
        // tells them apart: an idle core holds no file, a busy one is mid-load.
        return match get_flag(socket, "idle-active").await {
            Some(true) => Poll::Idle,
            _ => Poll::Unavailable,
        };
    };
    let duration = get_number(socket, "duration").await;
    // Absent on very old mpv builds; treat the session as a single item then.
    let index = get_number(socket, "playlist-pos").await.unwrap_or(0.0);

    Poll::Progress(
        index.max(0.0) as usize,
        Progress {
            position_secs: position.max(0.0) as u64,
            duration_secs: duration.filter(|d| *d > 0.0).map(|d| d as u64),
        },
    )
}
