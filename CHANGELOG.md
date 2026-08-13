# Changelog

## v0.1.0

First release.

### Added

- **Interactive TUI** (`kuro` with no arguments) — search, series and episode
  browsing, and a provider screen where `space` toggles a site inline. Vim keys.
- **Playback in IINA** with automatic mirror failover: mirrors are resolved
  concurrently, ordered by host preference, and tried in turn until one plays.
- **Two providers**, both declarative: `luciferdonghua` and `donghuastream`.
- **Declarative provider engine** — selectors live in TOML, so a site redesign is a
  config edit plus `kuro provider reload` rather than a rebuild. Handles both
  URL-linked and base64-encoded mirror options, and players mounted from JavaScript
  that advertise themselves only via `<meta itemprop="embedUrl">`.
- **Provider health** — a site that fails repeatedly disables itself and quietly
  re-probes until it recovers.
- **Downloads** — `kuro download`, single episode, a range, or a whole series.
- **Episode ranges** — `--ep 1-5` queues playback in order, or downloads the range.
- **Watch history and resume**, tracked through IINA's mpv IPC socket.
- **Disk page cache** with per-page TTLs; a repeated search drops from ~1.4 s to
  ~0.03 s. `--no-cache` bypasses it.
- **Bookmarks**, `kuro doctor`, shell completions, `--json` output, `--dry-run`,
  `-S/--select-nth` for scripting, and `KURO_QUALITY` / `KURO_PROVIDER`.

### Notes

- macOS only. IINA is the sole player backend, though the `Player` trait is not
  tied to it.
- `yt-dlp` handles stream extraction, so `kuro` inherits its fixes rather than
  maintaining per-host extractors.
- Sites behind a Cloudflare challenge are not supported — page JavaScript is never
  executed.
