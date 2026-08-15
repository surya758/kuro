# Changelog

## v0.6.0

### Added

- **New provider: animecube.** 2160p60 with real subtitle tracks — eight of them —
  where the other providers stop at 1080p with subtitles burned into the picture.
  Its catalogue and episode lists arrive as React Server Component payloads rather
  than markup, and the playable id sits behind a two-call API with a rotating token,
  so it is the first provider written in Rust rather than as a selector file.
- **An mpv backend.** Some sources publish no combined video+audio rendition and
  must be resolved by the player itself, which IINA cannot do — its bundled
  extractor is too old for those hosts and it buffers them badly. kuro uses mpv for
  exactly those and leaves everything else on IINA. `player.backend = "mpv"` forces
  it everywhere; `kuro doctor` reports both.
- **Hardware decoding under mpv.** 4K60 AV1 stutters when decoded on the CPU, which
  is the default; VideoToolbox is now requested.
- **RSC payload parsing**, reusable by any Next.js-backed provider.

### Changed

- **The quality menu shows what the host actually offers.** Opening it resolves the
  current episode and lists the real ladder, labelled with the height each choice
  delivers; options that resolve to the same rendition are collapsed. A fixed list
  both over-sold sources that stop at 1080p and under-sold ones that reach 4K.
- **Playback hints match the player in use** rather than always naming IINA's keys,
  and a source the player resolves reports its cap instead of "unknown".


## v0.5.1

### Fixed

- **Queued episodes opened part-way through.** `--start` is a global mpv option, so
  the resume point of the episode kuro launched was re-applied to every episode the
  lookahead queued — skipping ahead with ⌘→ dropped you into an unwatched episode at
  the previous one's timestamp. It is now cleared once the first seek has landed, so
  resume still works and stops there. Affected every provider; it only showed up when
  the launched episode had a resume point, which is why it came and went.

## v0.5.0

### Added

- **New provider: anidb.** Its backend is JSON and it serves its own HLS rather than
  embedding a third-party player, so there is no Rumble/Dailymotion middleman to
  break — and it carries multiple audio languages, which appear as mirrors, making
  mirror failover double as sub/dub choice. 1080p throughout.
- **Providers may declare a JSON format.** Selector files can now name fields
  instead of CSS selectors for episode and mirror lists, and derive API URLs from a
  series link with `{slug}`/`{id}`. Sites that render episodes client-side no longer
  need bespoke Rust.
- **An optional external fetcher.** Providers behind a challenge that inspects the
  TLS handshake can set `impersonate = true` and be fetched through
  [curl-impersonate](https://github.com/lexiforest/curl-impersonate). It is optional
  in the same way `yt-dlp` is: absent, it costs you that one provider and nothing
  else. `kuro doctor` reports it only when a provider actually needs it, and
  `general.impersonate_command` points at a binary that is not on `PATH`.
- **Search cards may be their own link.** A card layout that wraps the whole tile in
  one `<a>` no longer needs a nested selector that cannot match.
- **Script-configured players.** `selectors.embed.script_key` reads a stream URL out
  of a player config object for embeds built by JavaScript rather than an iframe.

## v0.4.6

### Fixed

- **"Back to episodes" reopened the same episode instead of the list.** It returned
  to the action menu for the episode just watched, which is not what the label says.
  Esc already went to the list; the button now agrees with it.
- **Closing the player window without quitting the app left kuro waiting.** An
  episode ended only when the launcher process exited — which happens on ⌘Q, but not
  on ⌘W, where IINA keeps running with no window. An episode now also ends when the
  player's IPC socket dies, or when the player sits with no file loaded for several
  polls.

## v0.4.5

### Fixed

- **"Next" offered an episode you had already watched.** The player queues the next
  couple of episodes, so ⌘→ can carry you well past what kuro launched — but the
  post-playback menu still counted from the launched episode. Starting at 8 and
  skipping to 10 offered "Next: Episode 9". It now continues from the episode you
  actually finished on.

## v0.4.4

### Fixed

- **Skipping to the next episode stopped watch history for the rest of the session.**
  Between playlist entries mpv reports no playback position, which the progress
  recorder read as "the player has closed" and shut itself down permanently. The
  first episode saved; every episode reached with ⌘→ went unrecorded, along with its
  progress. Only an unreachable socket now ends recording.

### Changed

- **Quality is presented as a cap rather than a promise.** A specific height has
  always been a ceiling — the closest rung at or below it plays, so asking for more
  than a host offers still works — but the menu advertised 2160p/1440p without
  saying that these embed hosts effectively never serve them. The menu now reads
  "Max quality", flags those rungs, and playback says so when the rendition came
  back lower than requested.

## v0.4.3

### Changed — breaking

- **`kuro list` is gone; use `kuro history`.** They were the same command under two
  names, which is one name too many. `kuro bookmark list` and `kuro provider list`
  are unaffected — those belong to their own subcommand groups.

## v0.4.2

### Fixed

- **A series whose episode list is empty was reported as a broken scraper.** Some
  series render the list container with nothing in it; that read as "the site's
  markup changed", which was both wrong and counted against provider health. Same
  class of bug as the empty-search-results one fixed in v0.3.2, now handled the same
  way — provider specs name the episode container.
- **Those series are watchable again.** When the list is empty, the latest-episode
  link elsewhere on the page is used instead, so the series is no longer a dead end.

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
