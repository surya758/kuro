//! Disk-backed HTTP response cache.
//!
//! Deliberately on disk rather than in memory: `kuro` is a CLI that exits between
//! commands, so an in-process cache would never survive long enough to help. The
//! win here is `kuro search x` twice, or `search` then `play`, not one long session.
//!
//! Every entry carries its own expiry, so search results can be short-lived while
//! episode lists live longer. Resolved stream URLs are never cached — they are
//! signed and short-lived, and a stale one fails at the player.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::debug;

/// How long a given kind of page stays fresh.
pub mod ttl {
    use std::time::Duration;

    pub const SEARCH: Duration = Duration::from_secs(5 * 60);
    pub const EPISODES: Duration = Duration::from_secs(15 * 60);
    pub const MIRRORS: Duration = Duration::from_secs(15 * 60);
}

#[derive(Clone, Debug)]
pub struct HttpCache {
    dir: Option<PathBuf>,
}

impl HttpCache {
    /// A cache writing under `dir`. Passing `None` disables caching entirely.
    pub fn new(dir: Option<PathBuf>) -> Self {
        Self { dir }
    }

    pub fn disabled() -> Self {
        Self { dir: None }
    }

    pub fn is_enabled(&self) -> bool {
        self.dir.is_some()
    }

    fn path_for(&self, url: &str) -> Option<PathBuf> {
        let dir = self.dir.as_ref()?;
        let mut hasher = DefaultHasher::new();
        url.hash(&mut hasher);
        // Not cryptographic — this only has to produce a stable, valid filename.
        Some(dir.join(format!("{:016x}.cache", hasher.finish())))
    }

    /// Fetch a fresh entry, or `None` if absent, expired, or unreadable.
    ///
    /// Cache problems are never fatal: any failure here just means a real request.
    pub fn get(&self, url: &str) -> Option<String> {
        let path = self.path_for(url)?;
        let raw = std::fs::read_to_string(&path).ok()?;

        let (header, body) = raw.split_once('\n')?;
        let expires_at: u64 = header.trim().parse().ok()?;

        if now_secs() >= expires_at {
            // Expired entries are removed on read, which keeps the directory from
            // growing without an explicit sweep.
            std::fs::remove_file(&path).ok();
            debug!(url, "cache entry expired");
            return None;
        }

        debug!(url, "cache hit");
        Some(body.to_string())
    }

    pub fn put(&self, url: &str, body: &str, ttl: Duration) {
        let Some(path) = self.path_for(url) else {
            return;
        };
        let Some(dir) = self.dir.as_ref() else {
            return;
        };

        if std::fs::create_dir_all(dir).is_err() {
            return;
        }

        let expires_at = now_secs() + ttl.as_secs();
        let contents = format!("{expires_at}\n{body}");

        // Temp-file-plus-rename so a concurrent reader never sees a partial body.
        let tmp = path.with_extension(format!("tmp{}", std::process::id()));
        if std::fs::write(&tmp, contents).is_ok() {
            std::fs::rename(&tmp, &path).ok();
        } else {
            std::fs::remove_file(&tmp).ok();
        }
    }

    /// Delete every cached response. Returns how many entries were removed.
    pub fn clear(&self) -> std::io::Result<usize> {
        let Some(dir) = self.dir.as_ref() else {
            return Ok(0);
        };

        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(e),
        };

        let mut removed = 0;
        for entry in entries.flatten() {
            if is_cache_file(&entry.path()) && std::fs::remove_file(entry.path()).is_ok() {
                removed += 1;
            }
        }
        Ok(removed)
    }

    /// Number of entries currently on disk, expired or not.
    pub fn len(&self) -> usize {
        let Some(dir) = self.dir.as_ref() else {
            return 0;
        };
        std::fs::read_dir(dir)
            .map(|entries| {
                entries
                    .flatten()
                    .filter(|e| is_cache_file(&e.path()))
                    .count()
            })
            .unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

fn is_cache_file(path: &Path) -> bool {
    path.extension().and_then(|s| s.to_str()) == Some("cache")
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A cache in a unique temp directory, cleaned up by the caller.
    fn temp_cache(name: &str) -> (HttpCache, PathBuf) {
        let dir = std::env::temp_dir().join(format!("kuro-cache-test-{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        (HttpCache::new(Some(dir.clone())), dir)
    }

    #[test]
    fn round_trips_a_body() {
        let (cache, dir) = temp_cache("roundtrip");
        cache.put("https://x.tld/a", "<html>hi</html>", Duration::from_secs(60));
        assert_eq!(cache.get("https://x.tld/a").as_deref(), Some("<html>hi</html>"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn bodies_containing_newlines_survive() {
        // The on-disk format splits on the first newline only; a multi-line body
        // must come back byte-identical.
        let (cache, dir) = temp_cache("newlines");
        let body = "<html>\n<body>\nline\n</body>\n</html>";
        cache.put("https://x.tld/b", body, Duration::from_secs(60));
        assert_eq!(cache.get("https://x.tld/b").as_deref(), Some(body));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn expired_entries_are_treated_as_missing() {
        let (cache, dir) = temp_cache("expiry");
        cache.put("https://x.tld/c", "stale", Duration::from_secs(0));
        assert_eq!(cache.get("https://x.tld/c"), None);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn distinct_urls_do_not_collide() {
        let (cache, dir) = temp_cache("collide");
        cache.put("https://x.tld/one", "first", Duration::from_secs(60));
        cache.put("https://x.tld/two", "second", Duration::from_secs(60));
        assert_eq!(cache.get("https://x.tld/one").as_deref(), Some("first"));
        assert_eq!(cache.get("https://x.tld/two").as_deref(), Some("second"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_disabled_cache_stores_nothing() {
        let cache = HttpCache::disabled();
        cache.put("https://x.tld/d", "body", Duration::from_secs(60));
        assert_eq!(cache.get("https://x.tld/d"), None);
        assert!(!cache.is_enabled());
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn clear_removes_entries_and_reports_the_count() {
        let (cache, dir) = temp_cache("clear");
        cache.put("https://x.tld/e", "1", Duration::from_secs(60));
        cache.put("https://x.tld/f", "2", Duration::from_secs(60));
        assert_eq!(cache.len(), 2);
        assert_eq!(cache.clear().expect("clear succeeds"), 2);
        assert!(cache.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }
}
