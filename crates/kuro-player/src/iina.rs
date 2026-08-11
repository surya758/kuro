//! IINA playback via `iina-cli`.
//!
//! `iina-cli` forwards any mpv option prefixed with `--mpv-`, which is what makes
//! header injection, resume, and a readable title possible without patching IINA.

use crate::{PlaybackOpts, PlayHandle, Player};
use async_trait::async_trait;
use kuro_core::{PlayerError, Stream};
use std::path::PathBuf;
use tracing::{debug, info};

/// Searched in order when no explicit path is configured.
const CANDIDATE_PATHS: &[&str] = &[
    "/Applications/IINA.app/Contents/MacOS/iina-cli",
    "/opt/homebrew/bin/iina",
    "/usr/local/bin/iina",
];

pub struct IinaPlayer {
    binary: PathBuf,
}

impl IinaPlayer {
    /// Locate `iina-cli`, preferring an explicit config path, then `PATH`, then
    /// the standard app bundle locations.
    pub fn discover(configured: Option<&str>) -> Result<Self, PlayerError> {
        if let Some(path) = configured {
            let p = PathBuf::from(path);
            if p.exists() {
                return Ok(Self { binary: p });
            }
            return Err(PlayerError::NotFound(path.to_string()));
        }

        if let Some(found) = which_on_path("iina-cli").or_else(|| which_on_path("iina")) {
            return Ok(Self { binary: found });
        }

        for candidate in CANDIDATE_PATHS {
            let p = PathBuf::from(candidate);
            if p.exists() {
                return Ok(Self { binary: p });
            }
        }

        Err(PlayerError::NotFound("iina-cli".to_string()))
    }

    pub fn binary(&self) -> &std::path::Path {
        &self.binary
    }

    /// Build the argument list. Separated from spawning so it can be asserted on
    /// in tests and shown by `--dry-run`.
    fn args(&self, stream: &Stream, opts: &PlaybackOpts) -> Vec<String> {
        let mut args = Vec::new();

        // Keep iina-cli attached so the process lifetime tracks playback rather
        // than returning the instant IINA launches.
        args.push("--keep-running".to_string());

        if let Some(title) = &opts.title {
            args.push(format!("--mpv-force-media-title={title}"));
        }

        // Rumble and Dailymotion CDNs key on Referer; without it the media URL 403s
        // even though it resolved fine.
        //
        // One `-append` per header rather than a single comma-joined value: mpv
        // splits that option on commas, and header values legitimately contain them
        // (`Accept: text/html,application/xhtml+xml`), which would corrupt them.
        for (name, value) in forwarded_headers(&stream.headers) {
            args.push(format!("--mpv-http-header-fields-append={name}: {value}"));
        }

        if let Some(start) = opts.start_secs.filter(|s| *s > 0) {
            args.push(format!("--mpv-start={start}"));
        }

        if opts.fullscreen {
            args.push("--mpv-fullscreen=yes".to_string());
        }

        if let Some(socket) = &opts.ipc_socket {
            args.push(format!("--mpv-input-ipc-server={socket}"));
        }

        args.push(stream.url.to_string());
        args
    }
}

/// Headers worth forwarding to the player.
///
/// Extractors return a full browser header set, but only these affect whether a
/// CDN serves the stream. Passing the rest is noise and risks tripping mpv's own
/// option parsing. Sorted so the generated command is deterministic.
fn forwarded_headers(headers: &std::collections::HashMap<String, String>) -> Vec<(&str, &str)> {
    const FORWARDED: &[&str] = &["referer", "user-agent", "origin", "cookie"];

    let mut kept: Vec<(&str, &str)> = headers
        .iter()
        .filter(|(name, _)| FORWARDED.contains(&name.to_ascii_lowercase().as_str()))
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();

    kept.sort_by_key(|(name, _)| name.to_ascii_lowercase());
    kept
}

fn which_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

#[async_trait]
impl Player for IinaPlayer {
    fn name(&self) -> &str {
        "iina"
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
        debug!(binary = %self.binary.display(), ?args, "launching IINA");
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

    fn player() -> IinaPlayer {
        IinaPlayer {
            binary: PathBuf::from("/usr/bin/true"),
        }
    }

    fn stream_with_headers(headers: HashMap<String, String>) -> Stream {
        Stream {
            url: Url::parse("https://cdn.example.com/chunklist.m3u8").expect("valid url"),
            kind: StreamKind::Hls,
            height: Some(1080),
            bitrate_kbps: None,
            headers,
        }
    }

    #[test]
    fn headers_are_passed_through_as_mpv_fields() {
        let mut headers = HashMap::new();
        headers.insert("Referer".to_string(), "https://site.tld/".to_string());
        let args = player().args(&stream_with_headers(headers), &PlaybackOpts::default());

        assert!(args.contains(
            &"--mpv-http-header-fields-append=Referer: https://site.tld/".to_string()
        ));
    }

    #[test]
    fn each_header_gets_its_own_append_so_commas_survive() {
        // A single comma-joined value would corrupt this header: mpv splits the
        // option on commas, and the value contains them.
        let mut headers = HashMap::new();
        headers.insert("Referer".to_string(), "https://site.tld/".to_string());
        headers.insert(
            "User-Agent".to_string(),
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7), Chrome".to_string(),
        );
        let args = player().args(&stream_with_headers(headers), &PlaybackOpts::default());

        let appends: Vec<_> = args
            .iter()
            .filter(|a| a.starts_with("--mpv-http-header-fields-append="))
            .collect();
        assert_eq!(appends.len(), 2);
        assert!(appends
            .iter()
            .any(|a| a.ends_with("10_15_7), Chrome")));
    }

    #[test]
    fn irrelevant_extractor_headers_are_not_forwarded() {
        let mut headers = HashMap::new();
        headers.insert("Referer".to_string(), "https://site.tld/".to_string());
        headers.insert("Accept".to_string(), "text/html,application/xml".to_string());
        headers.insert("Sec-Fetch-Mode".to_string(), "navigate".to_string());

        let args = player().args(&stream_with_headers(headers), &PlaybackOpts::default());
        let appends: Vec<_> = args
            .iter()
            .filter(|a| a.starts_with("--mpv-http-header-fields-append="))
            .collect();

        assert_eq!(appends.len(), 1, "only Referer should survive: {appends:?}");
    }

    #[test]
    fn no_header_flag_when_none_are_required() {
        let args = player().args(&stream_with_headers(HashMap::new()), &PlaybackOpts::default());
        assert!(!args
            .iter()
            .any(|a| a.starts_with("--mpv-http-header-fields")));
    }

    #[test]
    fn resume_position_becomes_mpv_start() {
        let opts = PlaybackOpts {
            start_secs: Some(612),
            ..Default::default()
        };
        let args = player().args(&stream_with_headers(HashMap::new()), &opts);
        assert!(args.contains(&"--mpv-start=612".to_string()));
    }

    #[test]
    fn zero_start_is_omitted_rather_than_passed_as_zero() {
        let opts = PlaybackOpts {
            start_secs: Some(0),
            ..Default::default()
        };
        let args = player().args(&stream_with_headers(HashMap::new()), &opts);
        assert!(!args.iter().any(|a| a.starts_with("--mpv-start=")));
    }

    #[test]
    fn stream_url_is_always_the_final_argument() {
        let opts = PlaybackOpts {
            title: Some("Show · Episode 1".to_string()),
            start_secs: Some(30),
            fullscreen: true,
            ipc_socket: Some("/tmp/sock".to_string()),
        };
        let args = player().args(&stream_with_headers(HashMap::new()), &opts);
        assert_eq!(args.last().expect("has args"), "https://cdn.example.com/chunklist.m3u8");
    }
}
