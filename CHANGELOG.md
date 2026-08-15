# Changelog

## v0.4.1

### Changed

- **Activity and interaction now share one accent colour.** Spinners, the selected
  row and its marker, download progress bars, and the playback marker were mostly
  dim, so the parts of the output you act on blended into the parts you only read.
  They use a distinct accent instead, leaving green/red/yellow to mean
  success/failure/warning. `NO_COLOR=1` disables colour entirely.

### Documentation

- Build-from-source instructions are a visible section rather than a collapsed
  block, and cover the `cargo run` / `cargo build --release` workflow.

## v0.4.0

### Changed — breaking

- **One entry point.** `kuro search "<query>"` is now the only way to search.
  The bare-query shorthand (`kuro against the gods`) is gone, and so is the
  full-screen TUI that bare `kuro` used to open — three doors to the same room was
  more surface than the tool needs. Bare `kuro` prints help.
- **The query is a single argument, so quote it.** `kuro search "against the gods"`.
  Unquoted multi-word queries now fail with a clear error rather than being silently
  glued back together.
- `kuro watch` is an alias of `kuro search` rather than a second implementation of
  the same flow.

### Removed

- The `ratatui` TUI and its dependency.

## v0.3.3

### Fixed

- **The playlist panel showed raw CDN URLs instead of episode names.** A playlist
  entry's display name can only come from the playlist itself, so queued episodes
  are now appended as one-entry M3Us carrying an `#EXTINF` label rather than as bare
  URLs.
- **The window title stayed on the first episode after switching.** `force-media-title`
  is a global option, so the name passed at launch was pinned to every later entry.
  The title is now updated over IPC whenever the player moves to another episode.

## v0.3.2

### Fixed

- **A search with no matches was reported as a broken scraper.** Providers render an
  empty results container for an unmatched query, which the parser read as "the
  site's markup changed". That was noisy, and it counted against provider health —
  a few unmatched searches in a row would auto-disable a perfectly healthy site.
  Provider specs now name the results container, so "no matches" and "markup gone"
  are told apart.

### Added

- `kuro history` as an alias for `kuro list`. Without it, the bare-query shorthand
  quietly turned the typo into a search for the word "history".

## v0.3.1

### Fixed

- **Watch history was never saved when kuro was interrupted.** Progress was only
  written after the player exited, so Ctrl-C — the natural way to get the prompt
  back while IINA keeps playing — discarded the whole session, leaving `kuro list`,
  `continue` and `next` empty. Progress is now checkpointed every few seconds while
  the episode plays.
- IPC sockets from interrupted sessions are swept on the next run instead of
  accumulating in the temp directory.

### Changed

- Playback now prints the keys that actually switch episode. IINA's on-screen ⏪/⏩
  buttons are seek/speed controls, not playlist navigation — it binds ⌘→ / ⌘← for
  that, and ⇧⌘P for the playlist panel.

## v0.3.0

### Added

- **Next/previous part-way through an episode.** The following episodes are resolved
  in the background and appended to the player's playlist over mpv IPC, so IINA's own
  next/previous controls move between episodes without returning to kuro. Two are
  queued ahead — the CDN links are signed and short-lived, so queueing further would
  hand the player URLs that expire before it reaches them.
- **Activity indicators for the slow steps.** Mirror resolution and per-host stream
  resolution now show a spinner; previously both printed a line and then sat silent
  for several seconds.

### Changed

- After an episode finishes in the TUI, the selection moves to the next episode and
  the status line says so, instead of landing back on the one just watched.

### Fixed

- Watch history is now recorded per playlist position, so skipping ahead inside the
  player credits the episode actually watched rather than the one kuro launched.

## v0.2.1

### Fixed

- `kuro <query>` now behaves exactly like `kuro search <query>`. The shorthand
  bypassed the terminal/`--json` check and wrote only to stderr, so piping it or
  asking for JSON produced no output at all. Both forms are now the same command.

## v0.2.0

### Added

- **Interactive browsing.** `kuro <query>` searches, then lets you pick a series and
  episode with arrow keys and act on it — play, download the episode, download a
  range, change quality, or bookmark. After playback it offers the next episode.
  `q`/`Esc` steps back a level instead of exiting.
- **Activity indicator.** A spinner with elapsed time while searching, loading
  episodes and resolving mirrors, so long scrapes no longer look like a hang.
- **Download progress bars.** One row per episode with percentage, speed and ETA,
  driven by yt-dlp's machine-readable progress output. Works at any `-j`.
- **Quality selection** from inside the flow, not just `--quality`.

### Changed

- `kuro search` no longer dead-ends at a printed list; at a terminal it opens the
  browse flow. Piped or with `--json` it still prints a plain list.
- Colour is omitted when stdout is redirected, so piped output stays parseable.

### Fixed

- `--mirror` was ignored by `kuro download`.

## v0.1.1

### Added

- **Intro and outro skipping** (`--skip`). Titles resolve to a MyAnimeList id via
  AniList, then [AniSkip] supplies the intervals; a small mpv Lua script performs the
  seek. Coverage is crowd-sourced and thin for donghua, so "no data" is the common
  outcome and playback simply continues.
- **Concurrent downloads** (`-j/--jobs`, default 3). Mirror resolution happens up
  front, then downloads run in parallel with per-episode status lines instead of
  interleaved progress bars.

### Fixed

- Season-specific titles now resolve to their own MyAnimeList entry. Stripping the
  season suffix before searching would have matched season one and applied its
  opening times to every later season.

[AniSkip]: https://api.aniskip.com/

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
