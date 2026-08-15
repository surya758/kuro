//! Playback in mpv.
//!
//! The plain counterpart to [`crate::iina`]. IINA is nicer to watch in, but it
//! wraps mpv and filters what reaches it: external audio options are dropped
//! without complaint, its bundled extractor is too old for some hosts, and it
//! buffers badly on sources that publish no muxed rendition. mpv takes every option
//! directly, so it plays what IINA cannot.
//!
//! Every option here is the one IINA receives via its `--mpv-` prefix, minus the
//! prefix — the two argument builders stay deliberately parallel.

use crate::{PlayHandle, PlaybackOpts, Player};
use async_trait::async_trait;
use kuro_core::{PlayerError, Stream};
use std::path::PathBuf;
use tracing::{debug, info};

/// Searched in order when no explicit path is configured.
const CANDIDATE_PATHS: &[&str] = &["/opt/homebrew/bin/mpv", "/usr/local/bin/mpv"];

pub struct MpvPlayer {
    binary: PathBuf,
}

impl MpvPlayer {
    pub fn discover(configured: Option<&str>) -> Result<Self, PlayerError> {
        if let Some(path) = configured {
            let p = PathBuf::from(path);
            if p.exists() {
                return Ok(Self { binary: p });
            }
            return Err(PlayerError::NotFound(path.to_string()));
        }

        if let Some(found) = super::which_on_path("mpv") {
            return Ok(Self { binary: found });
        }

        for candidate in CANDIDATE_PATHS {
            let p = PathBuf::from(candidate);
            if p.exists() {
                return Ok(Self { binary: p });
            }
        }

        Err(PlayerError::NotFound("mpv".to_string()))
    }

    fn args(&self, stream: &Stream, opts: &PlaybackOpts) -> Vec<String> {
        let mut args = Vec::new();

        if let Some(title) = &opts.title {
            args.push(format!("--force-media-title={title}"));
        }

        // One `-append` per header: the plain option is comma-split, and header
        // values legitimately contain commas.
        for (name, value) in super::forwarded_headers(&stream.headers) {
            args.push(format!("--http-header-fields-append={name}: {value}"));
        }

        // Sources with no muxed rendition are resolved by the player. Naming the
        // extractor explicitly keeps this working when several are on PATH.
        if let Some(format) = &stream.ytdl_format {
            args.push(format!("--ytdl-format={format}"));
            if let Some(ytdl) = super::which_on_path("yt-dlp") {
                args.push(format!(
                    "--script-opts-append=ytdl_hook-ytdl_path={}",
                    ytdl.display()
                ));
            }
        }

        if let Some(start) = opts.start_secs.filter(|s| *s > 0) {
            args.push(format!("--start={start}"));
        }

        if opts.fullscreen {
            args.push("--fullscreen=yes".to_string());
        }

        if let Some(socket) = &opts.ipc_socket {
            args.push(format!("--input-ipc-server={socket}"));
        }

        if let (Some(skip), Some(script)) = (opts.skip, opts.skip_script.as_ref()) {
            args.push(format!("--script={}", script.display()));

            let (op_start, op_end) = skip.op.map_or((-1.0, -1.0), |i| (i.start, i.end));
            let (ed_start, ed_end) = skip.ed.map_or((-1.0, -1.0), |i| (i.start, i.end));

            for (key, value) in [
                ("op_start", op_start),
                ("op_end", op_end),
                ("ed_start", ed_start),
                ("ed_end", ed_end),
            ] {
                args.push(format!("--script-opts-append=kuroskip-{key}={value}"));
            }
        }

        args.push(stream.url.to_string());
        args
    }
}

#[async_trait]
impl Player for MpvPlayer {
    fn name(&self) -> &str {
        "mpv"
    }

    fn binary(&self) -> &std::path::Path {
        &self.binary
    }

    async fn is_available(&self) -> bool {
        self.binary.exists()
    }

    fn command_preview(&self, stream: &Stream, opts: &PlaybackOpts) -> Vec<String> {
        let mut preview = vec![self.binary.display().to_string()];
        preview.extend(self.args(stream, opts));
        preview
    }

    async fn play(&self, stream: &Stream, opts: &PlaybackOpts) -> Result<PlayHandle, PlayerError> {
        let args = self.args(stream, opts);
        debug!(binary = %self.binary.display(), ?args, "launching mpv");
        info!(url = %stream.url, quality = stream.quality_label(), "starting playback");

        let child = tokio::process::Command::new(&self.binary)
            .args(&args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()?;

        Ok(PlayHandle { child })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kuro_core::StreamKind;
    use std::collections::HashMap;
    use url::Url;

    fn player() -> MpvPlayer {
        MpvPlayer {
            binary: PathBuf::from("/usr/bin/true"),
        }
    }

    fn stream() -> Stream {
        Stream {
            url: Url::parse("https://cdn.example.com/a.m3u8").expect("valid url"),
            kind: StreamKind::Hls,
            height: Some(1080),
            bitrate_kbps: None,
            headers: HashMap::new(),
            ytdl_format: None,
        }
    }

    #[test]
    fn options_carry_no_mpv_prefix() {
        // The prefix is IINA's passthrough convention; sending it to mpv itself
        // would make every option unrecognised.
        let opts = PlaybackOpts {
            title: Some("Ep 1".to_string()),
            start_secs: Some(30),
            ..PlaybackOpts::default()
        };
        let args = player().args(&stream(), &opts);

        assert!(args.iter().all(|a| !a.starts_with("--mpv-")), "{args:?}");
        assert!(args.contains(&"--force-media-title=Ep 1".to_string()));
        assert!(args.contains(&"--start=30".to_string()));
    }

    #[test]
    fn the_stream_url_is_always_last() {
        let args = player().args(&stream(), &PlaybackOpts::default());
        assert_eq!(args.last().unwrap(), "https://cdn.example.com/a.m3u8");
    }

    #[test]
    fn a_delegated_source_passes_its_format_selector() {
        let mut s = stream();
        s.ytdl_format = Some("bv*+ba/b".to_string());
        let args = player().args(&s, &PlaybackOpts::default());

        assert!(args.contains(&"--ytdl-format=bv*+ba/b".to_string()));
    }

    #[test]
    fn an_ordinary_source_asks_for_no_extractor() {
        // Only unmuxed sources defer to the player; the rest are pre-resolved and
        // must not pay for an extractor round-trip.
        let args = player().args(&stream(), &PlaybackOpts::default());
        assert!(
            !args.iter().any(|a| a.starts_with("--ytdl-format")),
            "{args:?}"
        );
    }
}
