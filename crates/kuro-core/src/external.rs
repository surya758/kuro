//! An external HTTP client, for sites the built-in one cannot reach.
//!
//! Some providers sit behind a browser challenge that inspects the TLS handshake
//! itself, so no combination of headers from `reqwest` gets through — the request
//! is refused before a single header is read. Those providers declare
//! `impersonate = true` in their spec and are fetched by shelling out to a curl
//! build that performs a browser-shaped handshake.
//!
//! This is deliberately the same arrangement as `yt-dlp`: an optional binary the
//! user installs, discovered at runtime, whose absence disables one provider rather
//! than breaking the program. Nothing here is provider-specific — the spec decides
//! who needs it.

use crate::error::ProviderError;
use std::time::Duration;
use tokio::process::Command;
use tracing::debug;
use url::Url;

/// Default binary name, overridable through config.
pub const DEFAULT_IMPERSONATE_COMMAND: &str = "curl-impersonate";

/// Which browser the handshake should look like.
///
/// Kept current-ish on purpose: challenge operators eventually stop accepting
/// fingerprints of long-retired browser versions.
const IMPERSONATE_TARGET: &str = "chrome136";

#[derive(Debug, Clone)]
pub struct ExternalFetcher {
    command: String,
}

impl ExternalFetcher {
    pub fn new(command: impl Into<String>) -> Self {
        Self {
            command: command.into(),
        }
    }

    pub fn command(&self) -> &str {
        &self.command
    }

    /// Whether the binary is present and runnable.
    pub async fn is_available(&self) -> bool {
        Command::new(&self.command)
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .await
            .map(|s| s.success())
            .unwrap_or(false)
    }

    /// GET `url`, returning the body.
    ///
    /// The status is appended to the body via `--write-out` rather than inferred
    /// from the exit code, which cannot tell a challenge from a missing page — and
    /// that difference decides whether the provider is penalised.
    pub async fn get(
        &self,
        url: &Url,
        referer: Option<&str>,
        user_agent: Option<&str>,
        timeout: Duration,
    ) -> Result<String, ProviderError> {
        let mut cmd = Command::new(&self.command);
        cmd.arg("--impersonate")
            .arg(IMPERSONATE_TARGET)
            .arg("--silent")
            .arg("--show-error")
            .arg("--location")
            .arg("--max-time")
            .arg(timeout.as_secs().max(1).to_string())
            // Report the status separately from the body: curl's own exit codes
            // do not distinguish 403 from 404, and the difference decides whether
            // this counts against the provider's health.
            .arg("--write-out")
            .arg("\n%{http_code}");

        if let Some(referer) = referer {
            cmd.arg("--referer").arg(referer);
        }
        if let Some(user_agent) = user_agent {
            cmd.arg("--user-agent").arg(user_agent);
        }
        cmd.arg(url.as_str());

        debug!(%url, command = %self.command, "fetching via external client");

        let output = cmd.output().await.map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                ProviderError::FetcherMissing {
                    command: self.command.clone(),
                }
            } else {
                ProviderError::Fetcher(e.to_string())
            }
        })?;

        if !output.status.success() && output.stdout.is_empty() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ProviderError::Fetcher(
                stderr
                    .trim()
                    .lines()
                    .last()
                    .unwrap_or("no output")
                    .to_string(),
            ));
        }

        let body = String::from_utf8_lossy(&output.stdout).into_owned();
        let (body, status) = split_status(&body);
        classify(status, body)
    }
}

/// Split the trailing `--write-out` status line from the body.
///
/// Returns a zero status when the marker is missing, which the caller treats as
/// "no HTTP status was observed" rather than guessing success.
fn split_status(raw: &str) -> (&str, u16) {
    match raw.rfind('\n') {
        Some(i) => {
            let status = raw[i + 1..].trim().parse().unwrap_or(0);
            (&raw[..i], status)
        }
        None => (raw, 0),
    }
}

/// Map an HTTP status onto the same errors the built-in client produces, so
/// provider health and retry behaviour do not depend on which client was used.
fn classify(status: u16, body: &str) -> Result<String, ProviderError> {
    match status {
        // A zero status means curl never reported one; trust the body instead.
        0 | 200..=299 => Ok(body.to_string()),
        429 => Err(ProviderError::RateLimited { retry_after: None }),
        403 | 503 => Err(ProviderError::Blocked),
        404 => Err(ProviderError::NotFound),
        other => Err(ProviderError::Upstream { status: other }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_status_line_is_split_off_the_body() {
        let (body, status) = split_status("<html>hi</html>\n200");
        assert_eq!(body, "<html>hi</html>");
        assert_eq!(status, 200);
    }

    #[test]
    fn a_body_containing_newlines_keeps_all_of_them() {
        // Only the final line is the marker; JSON and HTML are full of the rest.
        let (body, status) = split_status("{\n  \"a\": 1\n}\n200");
        assert_eq!(body, "{\n  \"a\": 1\n}");
        assert_eq!(status, 200);
    }

    #[test]
    fn a_challenge_is_reported_as_blocked() {
        // The whole reason this module exists: a 403 here must look identical to
        // one from the built-in client, or health tracking diverges by client.
        assert!(matches!(
            classify(403, "<html>Just a moment</html>"),
            Err(ProviderError::Blocked)
        ));
    }

    #[test]
    fn a_missing_status_marker_does_not_fail_the_request() {
        assert_eq!(classify(0, "body").unwrap(), "body");
    }

    #[test]
    fn upstream_errors_keep_their_status() {
        assert!(matches!(
            classify(500, ""),
            Err(ProviderError::Upstream { status: 500 })
        ));
    }
}
