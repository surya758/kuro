//! Activity indicator.
//!
//! Scraping takes seconds and used to look like a hang. The spinner runs on its own
//! task so it keeps ticking while the work awaits, and erases itself on completion
//! so the final output reads as if it were printed directly.

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::task::JoinHandle;

const FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const INTERVAL: Duration = Duration::from_millis(80);

pub struct Spinner {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    started: Instant,
    enabled: bool,
}

impl Spinner {
    /// Start spinning with `message`.
    ///
    /// A no-op when stderr is not a terminal, so piped output and CI logs stay free
    /// of control codes.
    pub fn start(message: impl Into<String>) -> Self {
        let message = message.into();
        let enabled = super::stderr_is_tty();
        let stop = Arc::new(AtomicBool::new(false));
        let started = Instant::now();

        if !enabled {
            return Self {
                stop,
                handle: None,
                started,
                enabled,
            };
        }

        let handle = {
            let stop = Arc::clone(&stop);
            tokio::spawn(async move {
                let mut frame = 0usize;
                while !stop.load(Ordering::Relaxed) {
                    let elapsed = started.elapsed().as_secs_f32();
                    // \r + clear-to-end keeps the line from smearing as the
                    // elapsed counter changes width.
                    eprint!(
                        "\r\x1b[2K{accent}{}{reset} {message} {dim}{elapsed:.1}s{reset}",
                        FRAMES[frame % FRAMES.len()],
                        accent = crate::ui::style::raw::ACCENT,
                        dim = crate::ui::style::raw::DIM,
                        reset = crate::ui::style::raw::RESET,
                    );
                    let _ = std::io::stderr().flush();
                    frame += 1;
                    tokio::time::sleep(INTERVAL).await;
                }
            })
        };

        Self {
            stop,
            handle: Some(handle),
            started,
            enabled,
        }
    }

    /// Stop and erase the spinner, leaving the cursor at the start of a clean line.
    pub async fn clear(mut self) {
        self.shutdown().await;
        if self.enabled {
            eprint!("\r\x1b[2K");
            let _ = std::io::stderr().flush();
        }
    }

    /// Stop the spinner and print a final line in its place.
    pub async fn finish(mut self, message: impl AsRef<str>) {
        let elapsed = self.started.elapsed().as_secs_f32();
        self.shutdown().await;

        if self.enabled {
            eprintln!("\r\x1b[2K{} \x1b[2m{elapsed:.1}s\x1b[0m", message.as_ref());
        } else {
            eprintln!("{}", message.as_ref());
        }
    }

    async fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.await;
        }
    }
}

impl Drop for Spinner {
    fn drop(&mut self) {
        // A spinner dropped on an early return (`?`) must not keep drawing over
        // whatever is printed next.
        self.stop.store(true, Ordering::Relaxed);
        if self.enabled && self.handle.is_some() {
            eprint!("\r\x1b[2K");
            let _ = std::io::stderr().flush();
        }
    }
}
