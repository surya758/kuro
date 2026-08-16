//! `yt-dlp`-backed stream resolution.
//!
//! The providers targeted here don't host video — they embed third-party players
//! (Rumble and Dailymotion on the reference provider), and `yt-dlp` already
//! supports those and ~1800 other hosts. Delegating means `kuro` inherits upstream
//! fixes when a host changes instead of maintaining extractors of its own.

use crate::StreamResolver;
use async_trait::async_trait;
use kuro_core::{QualityPref, ResolveError, Stream, StreamKind};
use serde::Deserialize;
use std::collections::HashMap;
use tracing::{debug, warn};
use url::Url;

/// Machine-readable progress, one line per tick on stdout.
///
/// Fields: downloaded bytes, total (falling back to the estimate, which is all HLS
/// offers), speed in bytes/sec, and ETA in seconds. Unknown values arrive as `NA`.
pub const PROGRESS_TEMPLATE: &str = "download:KURO|%(progress.downloaded_bytes)s|\
     %(progress.total_bytes,progress.total_bytes_estimate)s|\
     %(progress.speed)s|%(progress.eta)s";

#[derive(Debug, Deserialize)]
struct YtDlpOutput {
    #[serde(default)]
    formats: Vec<YtDlpFormat>,
    /// Present when the extractor returns a single pre-muxed stream.
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    duration: Option<f64>,
    /// Which extractor matched, used to rebuild a canonical URL for the player.
    #[serde(default)]
    extractor: Option<String>,
    #[serde(default)]
    id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct YtDlpFormat {
    url: Option<String>,
    #[serde(default)]
    height: Option<u32>,
    #[serde(default)]
    tbr: Option<f64>,
    #[serde(default)]
    protocol: Option<String>,
    #[serde(default)]
    ext: Option<String>,
    #[serde(default)]
    vcodec: Option<String>,
    #[serde(default)]
    acodec: Option<String>,
    #[serde(default)]
    http_headers: Option<HashMap<String, String>>,
}

impl YtDlpFormat {
    fn has_video(&self) -> bool {
        // `None` means the extractor didn't say; Rumble's HLS ladder reports
        // "unknown" codecs but is genuinely muxed video, so absence is not "no".
        !matches!(self.vcodec.as_deref(), Some("none"))
    }

    fn has_audio(&self) -> bool {
        !matches!(self.acodec.as_deref(), Some("none"))
    }

    fn kind(&self) -> StreamKind {
        let proto = self.protocol.as_deref().unwrap_or("");
        let ext = self.ext.as_deref().unwrap_or("");
        if proto.contains("m3u8") || ext == "m3u8" {
            StreamKind::Hls
        } else if proto.contains("dash") || ext == "mpd" {
            StreamKind::Dash
        } else {
            StreamKind::ProgressiveMp4
        }
    }
}

pub struct YtDlpResolver {
    binary: String,
}

impl Default for YtDlpResolver {
    fn default() -> Self {
        Self::new("yt-dlp")
    }
}

impl YtDlpResolver {
    pub fn new(binary: impl Into<String>) -> Self {
        Self {
            binary: binary.into(),
        }
    }

    /// Whether the `yt-dlp` binary is present and runnable.
    ///
    /// `yt-dlp` is an optional runtime dependency: when it is missing, mirrors
    /// whose hosts need it are reported as unresolvable rather than the whole
    /// command failing.
    pub async fn is_available(&self) -> bool {
        tokio::process::Command::new(&self.binary)
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .await
            .map(|s| s.success())
            .unwrap_or(false)
    }

    pub async fn version(&self) -> Option<String> {
        let out = tokio::process::Command::new(&self.binary)
            .arg("--version")
            .output()
            .await
            .ok()?;
        if !out.status.success() {
            return None;
        }
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    /// Total media duration in seconds, used to compute watch progress.
    pub async fn duration_secs(&self, url: &Url) -> Option<u64> {
        let output = self.run_json(url).await.ok()?;
        output.duration.map(|d| d as u64)
    }

    /// Download rather than stream.
    ///
    /// The *embed* URL is handed to `yt-dlp`, not a resolved stream URL: yt-dlp
    /// then picks formats and muxes video with audio itself, which a single
    /// pre-resolved rendition cannot do.
    ///
    /// Progress is emitted on stdout in a machine-readable form and handed to
    /// `on_progress` line by line, so the caller can draw its own bars. yt-dlp's own
    /// bar is never used: it cannot be rendered sanely for several parallel
    /// downloads sharing one terminal.
    pub async fn download<F>(
        &self,
        url: &Url,
        pref: QualityPref,
        output_template: &str,
        mut on_progress: F,
    ) -> Result<(), ResolveError>
    where
        F: FnMut(&str) + Send,
    {
        use tokio::io::{AsyncBufReadExt, BufReader};

        let format = format_selector(pref);
        debug!(%url, %format, output_template, "invoking yt-dlp for download");

        let mut child = tokio::process::Command::new(&self.binary)
            .args([
                "--no-warnings",
                "--no-playlist",
                "--newline",
                "--progress-delta",
                "0.4",
                "--progress-template",
                PROGRESS_TEMPLATE,
                "-f",
                &format,
                "-o",
                output_template,
                url.as_str(),
            ])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    ResolveError::YtDlpMissing
                } else {
                    ResolveError::Io(e)
                }
            })?;

        let stdout = child.stdout.take().expect("stdout was piped");
        let stderr = child.stderr.take().expect("stderr was piped");

        // stderr is drained concurrently; leaving it unread would deadlock the
        // child once the pipe buffer fills on a verbose failure.
        let stderr_task = tokio::spawn(async move {
            let mut collected = String::new();
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                collected.push_str(&line);
                collected.push('\n');
            }
            collected
        });

        let mut lines = BufReader::new(stdout).lines();
        while let Some(line) = lines.next_line().await.map_err(ResolveError::Io)? {
            on_progress(&line);
        }

        let status = child.wait().await.map_err(ResolveError::Io)?;
        let stderr = stderr_task.await.unwrap_or_default();

        if !status.success() {
            let message = stderr
                .lines()
                .find(|l| l.contains("ERROR"))
                .map(str::to_string)
                .unwrap_or_else(|| {
                    format!("yt-dlp exited with status {}", status.code().unwrap_or(-1))
                });
            return Err(ResolveError::YtDlp(message));
        }
        Ok(())
    }

    async fn run_json(&self, url: &Url) -> Result<YtDlpOutput, ResolveError> {
        // Hosts rate-limit and their metadata APIs time out; a single blip should
        // not retire a mirror, which on some providers is the only one an episode
        // has. Permanent answers — removed, private, geo-blocked — fail at once.
        let mut attempt = 0u32;
        loop {
            attempt += 1;
            match self.run_json_once(url).await {
                Ok(output) => return Ok(output),
                Err(e) => {
                    let retryable = matches!(&e, ResolveError::YtDlp(m) if is_transient(m));
                    if !retryable || attempt > MAX_RESOLVE_ATTEMPTS {
                        return Err(e);
                    }
                    warn!(%url, attempt, error = %e, "retrying resolution");
                    tokio::time::sleep(RESOLVE_BACKOFF * attempt).await;
                }
            }
        }
    }

    async fn run_json_once(&self, url: &Url) -> Result<YtDlpOutput, ResolveError> {
        debug!(%url, "invoking yt-dlp");

        let output = tokio::process::Command::new(&self.binary)
            .args(["-J", "--no-warnings", "--no-playlist", url.as_str()])
            .output()
            .await
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    ResolveError::YtDlpMissing
                } else {
                    ResolveError::Io(e)
                }
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ResolveError::YtDlp(concise_error(&stderr)));
        }

        serde_json::from_slice(&output.stdout).map_err(|e| ResolveError::BadOutput(e.to_string()))
    }
}

/// How many times a transient resolution failure is worth repeating.
const MAX_RESOLVE_ATTEMPTS: u32 = 3;

/// Grows with each attempt, to let a rate-limited host recover.
const RESOLVE_BACKOFF: std::time::Duration = std::time::Duration::from_millis(700);

/// Whether an extractor error is worth trying again.
///
/// Deliberately a small allowlist: retrying a video that is genuinely gone wastes
/// the viewer's time three times over, so only network-shaped failures qualify.
fn is_transient(message: &str) -> bool {
    const SIGNS: &[&str] = &[
        "timed out",
        "timeout",
        "connection reset",
        "connection aborted",
        "temporary failure",
        "temporarily unavailable",
        "too many requests",
        "http error 5",
        "read operation",
        "transporterror",
        "unable to download",
    ];
    let lower = message.to_ascii_lowercase();
    SIGNS.iter().any(|s| lower.contains(s))
}

/// The readable part of a yt-dlp failure.
///
/// Its errors carry a full Python exception chain — the same message repeated
/// inside `(caused by ...)`, plus connection-pool internals. Printed verbatim that
/// buries the one useful clause under several lines of noise.
fn concise_error(stderr: &str) -> String {
    let line = stderr
        .lines()
        .find(|l| l.contains("ERROR"))
        .unwrap_or_else(|| stderr.trim());

    let line = line.split(" (caused by").next().unwrap_or(line);
    let line = line.trim().trim_start_matches("ERROR: ").trim();

    // Collapse the connection-pool detail, which names a host and port already
    // implied by the request.
    let cleaned = match line.find("HTTPSConnectionPool") {
        Some(i) => {
            let head = line[..i].trim_end().trim_end_matches(':').trim();
            format!("{head}: the host stopped responding")
        }
        None => line.to_string(),
    };

    const LIMIT: usize = 160;
    if cleaned.chars().count() > LIMIT {
        let short: String = cleaned.chars().take(LIMIT).collect();
        format!("{short}…")
    } else {
        cleaned
    }
}

/// A URL the *player's* extractor will recognise.
///
/// Embed URLs resolve fine for yt-dlp but not always for the hook inside a player:
/// mpv fetches a Dailymotion player page, finds an SVG, and exits with no video.
/// The canonical page for the same video is what it accepts. Hosts not known to
/// need rewriting are passed through untouched.
fn canonical_for_player(output: &YtDlpOutput, original: &Url) -> Url {
    match (output.extractor.as_deref(), output.id.as_deref()) {
        (Some("dailymotion"), Some(id)) => {
            Url::parse(&format!("https://www.dailymotion.com/video/{id}"))
                .unwrap_or_else(|_| original.clone())
        }
        _ => original.clone(),
    }
}

/// A yt-dlp format expression for the requested quality.
///
/// Each falls back to a pre-muxed stream (`/b`) because many of these hosts serve
/// only combined renditions, where a strict `video+audio` selector matches nothing.
fn format_selector(pref: QualityPref) -> String {
    match pref.target_height() {
        None => match pref {
            QualityPref::Worst => "wv*+wa/w".to_string(),
            _ => "bv*+ba/b".to_string(),
        },
        Some(h) => {
            format!("bv*[height<={h}]+ba/b[height<={h}]/bv*+ba/b")
        }
    }
}

/// Order candidate formats best-first for the requested quality.
///
/// For a specific height, formats at or below the target come first (closest
/// first), then anything above it ascending — so asking for 1080p on a host that
/// only offers 1440p still plays rather than failing.
fn rank_formats(mut streams: Vec<Stream>, pref: QualityPref) -> Vec<Stream> {
    match pref.target_height() {
        None => {
            streams.sort_by(|a, b| {
                let (ha, hb) = (a.height.unwrap_or(0), b.height.unwrap_or(0));
                match pref {
                    QualityPref::Worst => ha.cmp(&hb),
                    _ => hb.cmp(&ha),
                }
            });
        }
        Some(target) => {
            streams.sort_by_key(|s| {
                let h = s.height.unwrap_or(0);
                if h <= target {
                    // At-or-below: closest to the target wins.
                    (0u8, target - h)
                } else {
                    // Above: smallest overshoot wins.
                    (1u8, h - target)
                }
            });
        }
    }
    streams
}

#[async_trait]
impl StreamResolver for YtDlpResolver {
    fn name(&self) -> &str {
        "yt-dlp"
    }

    fn can_handle(&self, _url: &Url) -> bool {
        // yt-dlp has a generic extractor, so it is the catch-all. Native resolvers
        // are consulted first and only exist for hosts yt-dlp does not cover.
        true
    }

    async fn available_heights(&self, url: &Url) -> Result<Vec<u32>, ResolveError> {
        let output = self.run_json(url).await?;
        let mut heights: Vec<u32> = output
            .formats
            .iter()
            .filter(|f| f.has_video())
            .filter_map(|f| f.height)
            .collect();

        heights.sort_unstable_by(|a, b| b.cmp(a));
        heights.dedup();
        Ok(heights)
    }

    async fn resolve(&self, url: &Url, pref: QualityPref) -> Result<Vec<Stream>, ResolveError> {
        let output = self.run_json(url).await?;

        let to_stream = |f: &YtDlpFormat| -> Option<Stream> {
            let parsed = Url::parse(f.url.as_ref()?).ok()?;
            Some(Stream {
                url: parsed,
                kind: f.kind(),
                height: f.height,
                bitrate_kbps: f.tbr.map(|t| t as u32),
                headers: f.http_headers.clone().unwrap_or_default(),
                ytdl_format: None,
            })
        };

        // Prefer muxed formats; a video-only rendition would play silent, so those
        // are used only when the host offers nothing better.
        let mut streams: Vec<Stream> = output
            .formats
            .iter()
            .filter(|f| f.has_video() && f.has_audio())
            .filter_map(to_stream)
            .collect();

        // Nothing pre-muxed. Rather than pick a rendition, hand the page URL back
        // and let the player's extractor resolve it: these CDNs serve individual
        // rendition URLs slowly and unreliably to a plain HTTP client, and an
        // external audio track cannot be attached through the player's command line.
        if streams.is_empty() && output.formats.iter().any(|f| f.has_video()) {
            debug!(%url, "no muxed format; deferring resolution to the player");
            streams.push(Stream {
                url: canonical_for_player(&output, url),
                kind: StreamKind::Hls,
                // Unknown until the player resolves it; guessing would misreport
                // what actually plays.
                height: None,
                bitrate_kbps: None,
                headers: HashMap::new(),
                ytdl_format: Some(format_selector(pref)),
            });
        }

        // Some extractors return a single top-level URL instead of a format list.
        if streams.is_empty() {
            if let Some(direct) = output.url.as_ref().and_then(|u| Url::parse(u).ok()) {
                streams.push(Stream {
                    url: direct,
                    kind: StreamKind::Hls,
                    height: None,
                    bitrate_kbps: None,
                    headers: HashMap::new(),
                    ytdl_format: None,
                });
            }
        }

        if streams.is_empty() {
            return Err(ResolveError::NoFormats {
                url: url.to_string(),
            });
        }

        Ok(rank_formats(streams, pref))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stream(height: u32) -> Stream {
        Stream {
            url: Url::parse("https://example.com/x.m3u8").expect("valid url"),
            kind: StreamKind::Hls,
            height: Some(height),
            bitrate_kbps: None,
            headers: HashMap::new(),
            ytdl_format: None,
        }
    }

    fn heights(streams: &[Stream]) -> Vec<u32> {
        streams.iter().filter_map(|s| s.height).collect()
    }

    #[test]
    fn format_selectors_always_fall_back_to_a_muxed_stream() {
        // Hosts that only serve combined renditions would match nothing without
        // the trailing `/b`, and the download would fail outright.
        for pref in [
            QualityPref::Best,
            QualityPref::Worst,
            QualityPref::P1080,
            QualityPref::P360,
        ] {
            let sel = format_selector(pref);
            let last = sel.rsplit('/').next().expect("non-empty selector");
            assert!(
                last == "b" || last == "w",
                "{pref:?} must end in a pre-muxed fallback, got `{sel}`"
            );
        }
    }

    #[test]
    fn specific_quality_selector_bounds_the_height() {
        let sel = format_selector(QualityPref::P720);
        assert!(sel.contains("height<=720"), "got `{sel}`");
    }

    #[test]
    fn best_prefers_the_highest_rendition() {
        let ranked = rank_formats(
            vec![stream(720), stream(2160), stream(1080)],
            QualityPref::Best,
        );
        assert_eq!(heights(&ranked)[0], 2160);
    }

    #[test]
    fn worst_prefers_the_lowest_rendition() {
        let ranked = rank_formats(
            vec![stream(720), stream(2160), stream(360)],
            QualityPref::Worst,
        );
        assert_eq!(heights(&ranked)[0], 360);
    }

    #[test]
    fn specific_quality_picks_exact_match_first() {
        let ranked = rank_formats(
            vec![stream(480), stream(1080), stream(2160)],
            QualityPref::P1080,
        );
        assert_eq!(heights(&ranked)[0], 1080);
    }

    #[test]
    fn specific_quality_steps_down_before_up() {
        let ranked = rank_formats(vec![stream(720), stream(1440)], QualityPref::P1080);
        assert_eq!(heights(&ranked)[0], 720);
    }

    #[test]
    fn specific_quality_still_plays_when_only_higher_exists() {
        let ranked = rank_formats(vec![stream(1440), stream(2160)], QualityPref::P1080);
        assert_eq!(heights(&ranked)[0], 1440);
    }

    #[test]
    fn network_shaped_failures_are_retried() {
        assert!(is_transient(
            "ERROR: Unable to download JSON metadata: Read timed out"
        ));
        assert!(is_transient("ERROR: HTTP Error 503: Service Unavailable"));
        assert!(is_transient("ERROR: Connection reset by peer"));
        assert!(is_transient("ERROR: Too Many Requests"));
    }

    #[test]
    fn a_video_that_is_gone_fails_at_once() {
        // Retrying these would cost the viewer three waits for the same answer.
        assert!(!is_transient("ERROR: Video unavailable"));
        assert!(!is_transient("ERROR: This video is private"));
        assert!(!is_transient("ERROR: Unsupported URL"));
    }

    #[test]
    fn the_exception_chain_is_stripped_from_errors() {
        let raw = "ERROR: [dailymotion] kAbC: Unable to download JSON metadata:                    HTTPSConnectionPool(host='graphql.api.dailymotion.com', port=443):                    Read timed out. (read timeout=20.0) (caused by TransportError(\"...\"))";
        let out = concise_error(raw);
        assert!(!out.contains("caused by"), "{out}");
        assert!(!out.contains("HTTPSConnectionPool"), "{out}");
        assert!(out.chars().count() <= 161, "{out}");
    }

    #[test]
    fn a_plain_error_survives_intact() {
        assert_eq!(
            concise_error("ERROR: Video unavailable"),
            "Video unavailable"
        );
    }

    #[test]
    fn video_only_formats_are_detected() {
        let f = YtDlpFormat {
            url: Some("https://x/y".into()),
            height: Some(1080),
            tbr: None,
            protocol: None,
            ext: None,
            vcodec: Some("avc1".into()),
            acodec: Some("none".into()),
            http_headers: None,
        };
        assert!(f.has_video());
        assert!(!f.has_audio());
    }

    #[test]
    fn unknown_codecs_are_treated_as_present() {
        // Rumble reports "unknown" for its muxed HLS ladder; excluding those would
        // leave the reference provider unplayable.
        let f = YtDlpFormat {
            url: Some("https://x/y".into()),
            height: Some(1080),
            tbr: None,
            protocol: Some("m3u8_native".into()),
            ext: Some("mp4".into()),
            vcodec: None,
            acodec: None,
            http_headers: None,
        };
        assert!(f.has_video());
        assert!(f.has_audio());
        assert_eq!(f.kind(), StreamKind::Hls);
    }
}
