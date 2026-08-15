# kuro — Product Specification

> A terminal-native anime/donghua streaming client for macOS that scrapes pluggable
> provider sites, resolves a playable stream, and hands it to **IINA**.

**Status:** Draft v1.0 · **Date:** 2026-08-15 · **Target platform:** macOS (Apple Silicon + Intel)

---

## 1. Problem & Goals

Anime streaming sites are unreliable — they go down, get seized, change their DOM, or
rate-limit. Any client hard-wired to one site is dead the week that site is. Watching
also means fighting a browser: ads, popunders, a video element you can't scrub properly,
no resume, no queue.

`kuro` is a CLI that:

1. Searches across **multiple provider sites** in parallel.
2. Lets each provider be **toggled on/off** so a dead site can be silenced without
   uninstalling or editing code.
3. Runs a **dedicated scraper per provider**, isolated so one broken scraper cannot
   break the app.
4. Resolves the episode down to a real stream URL and **plays it in IINA**.
5. Remembers what you watched and where you stopped.

### Non-goals (v1)

- Downloading/archiving (a `kuro download` command is deferred to v1.2).
- Windows/Linux support (the player layer is abstracted, but only IINA is implemented).
- Accounts, sync, or any server-side component. `kuro` is strictly local.

---

## 2. Language & Technology Decision

### Recommendation: **Rust**

The user asked for research here rather than a default, so the reasoning is recorded
explicitly, including where the decision is genuinely close.

#### What the workload actually is

This app is **~95% I/O-bound HTML fetching and parsing**, plus process spawning. It is
*not* compute-bound. So raw execution speed — the usual reason to pick Rust or C++ — is
close to irrelevant here. The decision has to rest on other properties.

#### The four realistic candidates

| | Rust | Go | TypeScript (Bun) | C++ |
|---|---|---|---|---|
| Single self-contained binary | ✅ | ✅ | ⚠️ (Bun compile ~50 MB) | ✅ |
| HTTP + HTML parsing ecosystem | ✅ `reqwest`, `scraper` | ✅ `net/http`, `goquery` | ✅ best-in-class | ❌ weak, manual |
| TUI ecosystem | ✅ `ratatui` (strongest) | ✅ Bubble Tea | ⚠️ Ink (React overhead) | ❌ ncurses |
| Plugin/provider modelling | ✅ traits | ⚠️ interfaces, looser | ✅ structural types | ⚠️ verbose |
| Iteration speed on scrapers | ⚠️ recompile | ✅ | ✅ fastest | ❌ slowest |
| Memory safety on untrusted input | ✅ | ✅ | ✅ | ❌ **disqualifying** |
| Concurrency for multi-provider fanout | ✅ tokio | ✅ goroutines | ✅ async | ⚠️ manual |

**C++ is rejected outright.** This program parses hostile, malformed HTML from
adversarial sites. Manual memory management against untrusted input is the wrong trade
for zero benefit — there is no compute-bound hot path to justify it.

**TypeScript/Bun is the strongest challenger.** It is the fastest language to *write*
scrapers in, and the user already has `bun` installed. Its weakness is distribution:
either you ship `node_modules` and a runtime dependency, or you `bun build --compile` to
a ~50 MB binary. For a tool meant to be a permanent, invisible part of a terminal setup,
that is a real cost.

**Go is a legitimate choice** and would not be a mistake. It loses to Rust on two
points that matter for this specific design: `ratatui` is a materially better TUI
library than Bubble Tea for the browse/queue interface, and Rust's trait system plus
exhaustive `enum` matching models the "N providers, each of which can fail in M distinct
ways" domain more precisely than Go's `interface{}` + `error` convention.

#### The decisive argument, and its counter

The user's own framing — *"anime website goes down all the time"* — is an argument
**against** Rust: providers churn, scrapers need frequent edits, and a scripting language
edits faster than a compiled one.

This is neutralised by design rather than dismissed. `kuro` splits scraper logic in two:

- **Declarative selectors live in TOML**, not in Rust source. Roughly 80% of real-world
  scraper breakage is "a CSS class changed." That is a config edit and a `kuro provider
  reload` — **no recompile, no release**.
- **Only genuinely novel logic** (a new obfuscation scheme, a new auth handshake) touches
  Rust code.

With that escape hatch in place, the iteration-speed gap closes, and Rust's advantages —
one small binary with no runtime (3.7 MB as built), `cargo` as a first-class build
system, memory safety
against hostile input, and a strict type system that makes provider failures
*unignorable* — carry the decision.

#### Verdict

**Rust**, with a declarative-selector layer to preserve fast iteration.
Go is an acceptable substitute; TypeScript is acceptable if distribution size is not a
concern; C++ is not appropriate for this problem.

> **Revisited in v0.4.0:** the full-screen `ratatui` TUI was built and then removed —
> an inline prompt flow covered the same ground with far less surface. One of the
> arguments above (a stronger TUI ecosystem) therefore did not end up mattering.
> The decision still stands on the others: single-binary distribution, memory safety
> against hostile input, and typed provider failures.

> **Note on the local machine:** at the time of this decision neither `cargo` nor `go`
> was installed, while `node`, `bun`, `ffmpeg`, `mpv`, and `yt-dlp` were. Rust was set
> up with a one-time `rustup` install, which took under a minute — it was not a
> meaningful blocker.

### Dependency set

Shipped:

| Concern | Crate | Why |
|---|---|---|
| Async runtime | `tokio` | Parallel multi-provider fanout |
| HTTP | `reqwest` (rustls, cookies, gzip/brotli) | No OpenSSL linkage; cookie jar for sites that require it |
| HTML parsing + selectors | `scraper` (html5ever) | Browser-grade parsing of malformed markup |
| JSON | `serde` / `serde_json` | State, yt-dlp interop |
| Config | `toml` | Config and provider selector specs |
| CLI parsing | `clap` v4 (derive) + `clap_complete` | Subcommands, shell completions |
| Errors | `thiserror` (lib) / `anyhow` (bin) | Typed provider errors, ergonomic top level |
| Logging | `tracing` + `tracing-subscriber` | Structured per-provider diagnostics |
| Paths | `directories` | Platform-correct macOS locations |
| Time | `chrono` | Timestamps in history and health records |

Planned, not yet added — listed so the intent is on record:

| Concern | Crate | For |
|---|---|---|
| Fuzzy match | `nucleo-matcher` | Replacing the hand-rolled ranking in `orchestrator::rank` |
| Caching | `moka` | TTL cache for search/metadata (currently uncached) |
| Testing | `wiremock`, `insta` | M6 fixture-based scraper regression tests |

---

## 3. Architecture

A Cargo **workspace**, so the provider layer cannot reach into the UI and vice versa.

```
kuro/
├── Cargo.toml                    # workspace root
├── crates/
│   ├── kuro-cli/                 # binary: clap commands, browse flow, output rendering
│   ├── kuro-core/                # domain types, Provider trait, orchestration
│   ├── kuro-providers/           # one module per site + selector configs
│   ├── kuro-resolver/            # embed host → playable stream URL
│   ├── kuro-player/              # Player trait + IINA implementation
│   └── kuro-store/               # config, watch history, provider health
├── providers.d/                  # shipped declarative selector TOMLs
└── tests/fixtures/               # recorded HTML for offline scraper tests
```

### Dependency direction

```
kuro-cli ──► kuro-core ──► kuro-providers ──► (declarative selectors)
                 │
                 ├────────► kuro-resolver ──► yt-dlp / native extractors
                 ├────────► kuro-player   ──► IINA
                 └────────► kuro-store    ──► ~/Library/Application Support/kuro
```

`kuro-core` is the only crate that knows about all the others. Providers know nothing
about the player; the player knows nothing about providers.

---

## 4. Domain Model

```rust
pub struct Series {
    pub provider_id: ProviderId,
    pub id: String,              // provider-local slug
    pub title: String,
    pub url: Url,
    pub poster: Option<Url>,
    pub year: Option<u16>,
    pub synopsis: Option<String>,
    pub genres: Vec<String>,
    pub status: SeriesStatus,    // Ongoing | Completed | Unknown
    pub total_episodes: Option<u32>,
}

pub struct Episode {
    pub series_id: String,
    pub number: f32,             // f32 so "12.5" specials are representable
    pub title: Option<String>,
    pub url: Url,
}

/// A candidate playback source, before resolution to a real stream.
pub struct Mirror {
    pub index: u8,
    pub label: String,           // "Rumble", "Dailymotion", ...
    pub page_url: Url,           // provider page holding the embed
    pub embed_url: Option<Url>,  // resolved lazily
}

/// A fully-resolved, directly playable stream.
pub struct Stream {
    pub url: Url,
    pub kind: StreamKind,        // Hls | Dash | ProgressiveMp4
    pub quality: Option<Quality>,
    pub headers: HashMap<String, String>, // Referer/User-Agent the CDN demands
}
```

---

## 5. The Provider Abstraction

Every site implements one trait. This is the core extension point.

```rust
#[async_trait]
pub trait Provider: Send + Sync {
    fn id(&self) -> ProviderId;
    fn display_name(&self) -> &str;
    fn base_url(&self) -> &Url;

    /// Cheap liveness check used by the health system.
    async fn health_check(&self, ctx: &FetchCtx) -> Result<(), ProviderError>;

    async fn search(&self, ctx: &FetchCtx, query: &str)
        -> Result<Vec<Series>, ProviderError>;

    async fn series_details(&self, ctx: &FetchCtx, series: &Series)
        -> Result<SeriesDetails, ProviderError>;

    async fn episodes(&self, ctx: &FetchCtx, series: &Series)
        -> Result<Vec<Episode>, ProviderError>;

    async fn mirrors(&self, ctx: &FetchCtx, episode: &Episode)
        -> Result<Vec<Mirror>, ProviderError>;

    /// Resolve one mirror to the third-party embed URL (e.g. a Rumble embed).
    async fn embed_url(&self, ctx: &FetchCtx, mirror: &Mirror)
        -> Result<Url, ProviderError>;

    /// Optional: catalogue browsing, where the site supports it.
    async fn latest(&self, _ctx: &FetchCtx, _page: u32)
        -> Result<Vec<Series>, ProviderError> { Ok(vec![]) }
}
```

### Typed failures

Provider errors are an exhaustive enum, so the orchestrator can react correctly instead
of treating every failure as "site down":

```rust
pub enum ProviderError {
    Network(reqwest::Error),      // retry with backoff
    Timeout,                      // retry once, then degrade
    RateLimited { retry_after: Option<Duration> },
    Blocked,                      // Cloudflare / WAF challenge
    ParseFailure { selector: String, context: String }, // ← site redesigned
    NotFound,
    Upstream { status: StatusCode },
}
```

`ParseFailure` carries the selector that failed — it is the signal that a selector TOML
needs updating, and it is reported to the user as exactly that, not as a generic error.

### Isolation guarantee

`kuro-core` runs every provider call inside a per-provider timeout and catches panics
(`futures::FutureExt::catch_unwind`). **A provider that hangs, panics, or returns
garbage degrades to "this provider returned nothing" — it never takes down the run.**

---

## 6. Provider Toggle System

The requirement: sites go down constantly, so toggling must be instant and require no
code change.

### Commands

```bash
kuro provider list                  # all providers + enabled state + health
kuro provider enable  luciferdonghua
kuro provider disable luciferdonghua
kuro provider only    luciferdonghua   # enable exactly one, disable the rest
kuro provider test    luciferdonghua   # run the full chain, print timings
kuro provider reload                   # re-read selector TOMLs without restart
```

### Config — `~/Library/Application Support/kuro/config.toml`

```toml
[general]
default_quality  = "1080p"       # best | 2160p | 1080p | 720p | 480p | worst
concurrency      = 6             # max providers queried at once
search_timeout   = "8s"

[player]
backend       = "iina"
fullscreen    = false
resume        = true             # seek to last position on replay

[providers.luciferdonghua]
enabled  = true
priority = 10                    # higher wins when merging duplicate results
mirrors  = ["rumble", "dailymotion"]   # preference order

[providers.example_backup]
enabled  = false
priority = 5

[health]
auto_disable_after_failures = 3   # consecutive failures → auto-disable
recheck_interval            = "30m"
```

### Automatic health management

`kuro-store` keeps a rolling failure record per provider. After
`auto_disable_after_failures` consecutive failures a provider is **auto-disabled** and
the user is told once:

```
⚠  luciferdonghua auto-disabled after 3 consecutive failures (last: ParseFailure at
   `div.eplister li a`). Re-enable with: kuro provider enable luciferdonghua
```

Auto-disabled providers are silently re-probed every `recheck_interval` and re-enable
themselves on success. This is the mechanism that makes "sites go down all the time"
a non-event rather than a support burden.

---

## 7. Stream Resolution Pipeline

The single most important architectural finding from the site research:

> **The provider does not host video.** It embeds third-party hosts — verified: **Rumble**
> and **Dailymotion**. Both are natively supported by `yt-dlp`.

So `kuro` never writes a video extractor for a host `yt-dlp` already handles. The
provider scraper's job ends at *"here is the embed URL."*

### The pipeline

```
Episode page
    │  provider.mirrors()
    ▼
[Mirror 1: Rumble] [Mirror 2: Dailymotion] [Mirror 3: …]
    │  provider.embed_url()   ← ordered by config `mirrors` preference
    ▼
https://rumble.com/embed/v75qb3o/?pub=4p006u
    │
    ▼
┌─────────────────── kuro-resolver ───────────────────┐
│ 1. Native extractor registered for this host?  → use it   │
│ 2. Otherwise → yt-dlp -J --no-warnings <embed_url>        │
│ 3. Select format matching config.default_quality          │
│ 4. Capture required headers (Referer, User-Agent)         │
└───────────────────────────────────────────────────────────┘
    │
    ▼
Stream { url, kind: Hls, quality: 1080p, headers }
    │  fallback: on failure, advance to the next mirror automatically
    ▼
kuro-player → IINA
```

### Resolver trait

```rust
#[async_trait]
pub trait StreamResolver: Send + Sync {
    fn can_handle(&self, url: &Url) -> bool;
    async fn resolve(&self, url: &Url, pref: QualityPref)
        -> Result<Vec<Stream>, ResolveError>;
}
```

Two implementations ship in v1:

- **`YtDlpResolver`** — the default. Shells out to `yt-dlp -J`, parses the format list
  from JSON, ranks by height/bitrate. Covers Rumble, Dailymotion, and ~1800 other hosts
  for free, and inherits `yt-dlp`'s updates when hosts change.
- **`NativeResolver`** — a registry for hosts `yt-dlp` does *not* cover (the small
  bespoke players some anime sites use). Each host is a small module implementing the
  same trait.

`yt-dlp` is treated as an **optional runtime dependency**: if absent, `kuro` reports
which mirrors it cannot resolve rather than failing outright, and native resolvers still
work.

### Mirror failover

Resolution walks mirrors in the configured preference order. A mirror whose embed
cannot be extracted, or that yields no playable format, is skipped and the next is
tried. Only when every mirror fails does the episode report as unplayable, listing
the per-mirror reason.

Embed extraction for all mirrors runs concurrently, since it costs one fetch each;
only *stream resolution* is sequential, because the first success ends the search.

> An additional `HEAD` pre-flight on the resolved URL was considered and dropped:
> these CDN URLs are signed and range-scoped, so a `HEAD` is not a reliable
> predictor of whether a `GET` will succeed. Resolution failure is the real signal.

---

## 8. Player Integration (IINA)

`iina-cli` accepts a URL and passes arbitrary mpv options through with an `--mpv-` prefix
(verified: `--mpv-*` supported, `--keep-running`, `--pip`, `--music-mode`).

```rust
#[async_trait]
pub trait Player: Send + Sync {
    fn name(&self) -> &str;
    async fn is_available(&self) -> bool;
    async fn play(&self, stream: &Stream, opts: &PlaybackOpts) -> Result<PlayHandle>;
}
```

### Invocation

```bash
iina-cli \
  --keep-running \
  --mpv-force-media-title="Martial God Asura S2 · Episode 15" \
  --mpv-http-header-fields="Referer: https://luciferdonghua.in/,User-Agent: Mozilla/5.0 …" \
  --mpv-start=612 \
  "https://hugh.cdn.rumble.cloud/video/…/chunklist.m3u8"
```

- `--mpv-http-header-fields` supplies the `Referer`/`User-Agent` many CDNs require. This
  is why `Stream` carries a `headers` map.
- `--mpv-force-media-title` gives a readable title bar instead of a CDN hash.
- `--mpv-start` implements resume from the stored position.

### Binary discovery

Resolve `iina-cli` in order: `config.player.path` → `$PATH` →
`/Applications/IINA.app/Contents/MacOS/iina-cli`. (On this machine both `/opt/homebrew/bin/iina`
and the bundle path are present.) If none is found, `kuro` errors with a Homebrew install
hint rather than a bare "not found."

### Position tracking

`--keep-running` keeps `iina-cli` attached for the session's lifetime. For resume,
`kuro-player` uses IINA's mpv IPC socket (`--mpv-input-ipc-server=<tmp socket>`) and
polls `time-pos`, writing the last position to the watch store on exit. If the socket is
unavailable, resume degrades gracefully to "watched / not watched" without a timestamp.

---

## 9. CLI Surface

```
kuro                                  # launch interactive TUI
kuro search <query>                   # search enabled providers, print ranked results
kuro watch  <query>                   # search → pick best match → pick episode → play
kuro play   <series> --ep 15          # direct play
kuro play   <series> --ep 15 --mirror dailymotion --quality 720p
kuro next                             # play next unwatched episode of last series
kuro continue                         # resume last episode at last position

kuro history                          # watch history
kuro bookmark add|rm|list <series>
kuro provider list|enable|disable|only|test|reload
kuro config edit|path|show
kuro doctor                           # verify IINA, yt-dlp, network, provider health
kuro completions <shell>
```

### Global flags

`--provider <id>` (restrict to one) · `--quality <q>` · `--json` (machine-readable
output for scripting) · `--no-color` · `-v/-vv` (tracing verbosity) · `--dry-run`
(resolve and print the stream URL without launching IINA).

### TUI (`ratatui`)

Four screens, vim keybindings throughout:

1. **Search** — live query, results grouped by provider, provider badges.
2. **Series** — synopsis, metadata, episode grid with watched/unwatched markers.
3. **Player launch** — mirror + quality selection, resolution progress.
4. **Providers** — toggle providers on/off inline with `space`; health status per row.

---

## 10. Reference Provider: `luciferdonghua`

Verified against the live site. This module is the template every future provider copies.

### Endpoints

| Operation | Pattern |
|---|---|
| Search | `GET /?s={query}` |
| Series | `GET /anime/{slug}/` |
| Episode | `GET /{series-slug}-episode-{nn}-lucifer-donghua/` |
| Mirror | `GET /{episode-slug}/v/{n}/` |

### Declarative selectors — `providers.d/luciferdonghua.toml`

The site runs a well-known WordPress anime theme with stable structural classes.

```toml
id           = "luciferdonghua"
display_name = "Lucifer Donghua"
base_url     = "https://luciferdonghua.in"

[endpoints]
search  = "/?s={query}"
series  = "/anime/{slug}/"

[selectors.search]
item    = "div.listupd article.bs"
title   = "h2[itemprop=headline]"
url     = "a[href]"
poster  = "img"

[selectors.series]
title    = "h1.entry-title"
synopsis = "div.synp div.entry-content"
poster   = "div.thumb img"
genres   = "div.genxed a"
info_row = "div.spe span"

[selectors.episodes]
item   = "div.eplister ul li a"
number = "div.epl-num"
title  = "div.epl-title"

[selectors.mirrors]
option      = "select.mirror option[value]"
index_attr  = "data-index"
value_attr  = "value"

[selectors.embed]
iframe = "iframe[src]"

[request]
user_agent = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0 Safari/537.36"
referer    = "https://luciferdonghua.in/"
```

### Verified end-to-end chain

Search `martial` → 10 series → `martial-god-asura-season-2` → 15+ episodes →
5 mirrors → mirror 1 = Rumble embed, mirror 2 = Dailymotion → `yt-dlp` →
HLS ladder **222k/360p through 9436k/2160p** → direct `.m3u8` chunklist URL.
Every step in the pipeline is confirmed working against the live site.

### Known quirks the scraper must handle

- **Obfuscated inline JS.** The homepage carries a base64 blob in a CSS custom property
  (`--mov`) doing anti-devtools/WebGL fingerprinting. It is irrelevant to scraping and is
  **ignored** — `kuro` never executes site JavaScript, which sidesteps the whole
  anti-bot layer.
- **Mirror pages are separate fetches.** `select.mirror option[value]` yields `/v/{n}/`
  page URLs, not embeds. Each must be fetched to extract its `<iframe src>`. These
  fetches are done **concurrently** and cached.
- **Mirror labels are empty in the HTML.** The `<option>` text is blank, so labels are
  derived from the embed host after resolution (`rumble.com` → "Rumble") rather than read
  from the page.
- **Slugs are inconsistent.** Some series slugs carry a year suffix (`martial-peak-2024`),
  some carry `-new` or `-edit`. Slugs must always be read from `href`, never constructed.
- **Ad/tracker iframes are present.** Embed extraction must filter against an allowlist
  of known video hosts; the first `<iframe>` on the page is frequently an ad.

---

## 11. Storage & State

macOS-correct paths via `directories`:

| Path | Contents |
|---|---|
| `~/Library/Application Support/kuro/config.toml` | User config |
| `~/Library/Application Support/kuro/providers.d/*.toml` | User selector overrides (shadow shipped defaults) |
| `~/Library/Application Support/kuro/history.json` | Watch history + resume positions |
| `~/Library/Application Support/kuro/bookmarks.json` | Followed series |
| `~/Library/Application Support/kuro/health.json` | Per-provider failure counts, auto-disable state |
| `~/Library/Caches/kuro/` | HTTP response cache (TTL'd) |

```jsonc
// history.json
{
  "entries": [{
    "provider_id": "luciferdonghua",
    "series_id":   "martial-god-asura-season-2",
    "series_title":"Martial God Asura Season 2",
    "episode":     15,
    "position_secs": 612,
    "duration_secs": 1440,
    "completed":   false,
    "watched_at":  "2026-08-15T18:30:00Z"
  }]
}
```

Writes are atomic (temp file + rename) so a crash mid-write cannot corrupt history.

---

## 12. Cross-Cutting Concerns

### Rate limiting & politeness — *implemented*
A per-host semaphore in `FetchCtx` bounds concurrent in-flight requests to a provider
(default 4), and every request carries a realistic browser `User-Agent` plus the
provider's configured `Referer`. Concurrent mirror fetches share the same limiter.

> A token-bucket rate limit (`governor`) was specified originally and deliberately not
> built: the semaphore already prevents bursts, and no provider has returned `429` in
> testing. It stays available if one starts to.

### Retries — *implemented*
Exponential backoff on `Network`/`Timeout`/`RateLimited`/`Upstream`, honouring an
explicit `Retry-After` when present. `ParseFailure`, `NotFound`, and `Blocked` are
**not** retried — retrying a selector that no longer matches just wastes time and
hammers the site. This is enforced by `ProviderError::is_retryable`.

### Caching — *implemented*
Disk-backed cache in `~/Library/Caches/kuro`, with per-entry TTLs: search results
5 min, episode lists and mirror pages 15 min. Resolved streams are never cached —
CDN URLs are signed and short-lived. `--no-cache` bypasses it for one run;
`kuro cache clear` empties it.

> The spec originally called for an in-memory `moka` cache. That was wrong for this
> program: `kuro` is a CLI that exits between commands, so an in-process cache would
> almost never be read back. On disk, a repeated search goes from **1.37 s to
> 0.03 s**; in memory it would have saved nothing.

### Testing strategy

Built (48 unit tests, all offline and network-free):
- **Parsing logic** — episode-number extraction, slug handling, year detection
  (including the multi-byte-title case that panicked in early live testing), and
  ad-iframe rejection in embed extraction.
- **Behavioural rules** — health auto-disable thresholds and announce-once semantics,
  completion/resume thresholds, quality-ladder ranking, mirror preference ordering,
  and the exact argument list handed to IINA.
- **Shipped provider specs** are parsed in a test, so a malformed selector TOML cannot
  be released.

Planned (M6):
- **Fixture tests.** Real HTML recorded into `tests/fixtures/` and asserted offline, so
  scraper regressions are caught without touching the network. The directory exists;
  the fixtures do not yet.
- **`insta` snapshots** for parsed output, so selector changes surface as reviewable diffs.
- **`wiremock`** for network-error paths (timeouts, 429, 503, Cloudflare pages).

Live checks run manually via `kuro provider test`, never in CI — a red build caused by
someone else's site going down is noise.

### Observability
`tracing` spans per provider call. `-vv` prints each URL fetched, status, timing, and the
exact selector that failed on `ParseFailure`.

---

## 13. Milestones

| # | Deliverable | Scope | Status |
|---|---|---|---|
| **M1** | Walking skeleton | Workspace, `Provider` trait, `luciferdonghua` search+episodes, `kuro search` | ✅ done |
| **M2** | Playback | Mirror extraction, `YtDlpResolver`, IINA launch, `kuro play` — **first watchable episode** | ✅ done |
| **M3** | Provider system | Declarative selector loading, toggle commands, health tracking + auto-disable | ✅ done |
| **M4** | State | History, resume via mpv IPC, bookmarks, `kuro continue` / `kuro next` | ✅ done |
| **M5** | TUI | `ratatui` search/series/provider screens | ⊘ removed in v0.4.0 — the inline browse flow replaced it |
| **M6** | Hardening | Second provider, fixture test suite, `kuro doctor`, Homebrew formula | ✅ done |
| **v1.2** | Downloading | `kuro download`, single episode or whole series | ✅ done (pulled forward) |

M2 is the point at which the tool is genuinely usable; everything after is leverage.

**M6 outcome — the abstraction held.** `donghuastream.org` was added with **zero
provider-specific Rust**: one selector TOML, plus one *generic* capability
(`value_encoding = "base64_html"`) that now benefits any site in that theme family.
It differs from the reference provider in a real way — mirror options carry
base64-encoded `<iframe>` markup instead of links to sub-pages — and that difference
turned out to be expressible declaratively rather than needing a bespoke module.

---

## 14. Risks

| Risk | Mitigation |
|---|---|
| Site redesign breaks scraping | Declarative selectors → TOML edit, no recompile. `ParseFailure` names the dead selector. |
| Provider disappears permanently | Multi-provider by design; auto-disable keeps it out of the way. |
| Cloudflare / WAF challenge | Detected as `Blocked`, surfaced distinctly. **Confirmed real:** `anidb.app` returns `cf-mitigated: challenge` and a zero-byte 403 to every path but `robots.txt`, even with a complete browser header set. Such a site cannot be supported without executing page JavaScript, and building a challenge solver is out of scope — it circumvents an access control the operator deliberately deployed. The legitimate path is reusing the user's *own* browser session cookie (what `yt-dlp --cookies-from-browser` does), which is not yet implemented. |
| Embed host adds DRM | Reported as unresolvable; mirror failover moves to the next host. |
| `yt-dlp` drifts out of date | `kuro doctor` checks version and warns; `yt-dlp` is optional, not required. |
| CDN URL expiry mid-session | URLs are never cached; re-resolve on replay. |

---

## 15. Legal Note

`kuro` is a client that automates navigation of publicly reachable web pages and hands
URLs to a local media player — the same category as `ani-cli`, `mpv`'s ytdl hook, or a
browser with an ad blocker. It hosts, stores, and redistributes nothing.

That said, the provider sites it targets generally do not hold distribution rights to
what they serve, and at least one carries a DMCA-takedown badge. Streaming from them may
be unlawful in some jurisdictions regardless of how the request is made. This is stated
so the trade-off is explicit; the choice of which providers to enable is the user's, and
the toggle system exists in part to make that choice easy to revisit.

---

## Appendix A — Adding a New Provider

1. `cp providers.d/luciferdonghua.toml providers.d/newsite.toml`, edit selectors.
2. If the site's structure fits the declarative model, **stop — you are done.**
3. If it needs custom logic, add `crates/kuro-providers/src/newsite.rs` implementing
   `Provider`, and register it in `registry.rs`.
4. Record fixtures: `kuro provider test newsite --record-fixtures`.
5. Add `[providers.newsite]` to config with an initial `priority`.

## Appendix B — Verified Environment (2026-08-15)

| Component | Status |
|---|---|
| IINA | ✅ `/Applications/IINA.app` + `/opt/homebrew/bin/iina`; `iina-cli` supports `--mpv-*` passthrough |
| `yt-dlp` | ✅ installed — resolves Rumble and Dailymotion to full HLS ladders |
| `mpv`, `ffmpeg` | ✅ installed |
| `node`, `bun` | ✅ installed |
| `cargo` / `rustc` | ✅ 1.97.1 — installed via `rustup` during M1 |
| `luciferdonghua.in` | ✅ reachable, HTTP 200, full scrape chain verified |

### Verified after implementation

| Check | Result |
|---|---|
| `kuro provider test luciferdonghua` | ✅ reachable → search → episodes → mirrors → Rumble embed, all green |
| `kuro play … --ep 15 --dry-run` | ✅ resolves to a signed Rumble CDN `chunklist.m3u8` |
| Resolved stream decodes | ✅ `ffprobe` reports h264 1920×1080 + aac, 998 s |
| mpv opens the stream | ✅ `Video (h264 1920x1080 30 fps)`, `Audio (aac 2ch 44100 Hz)` |
| Test suite | ✅ 48 passing, 0 build warnings |

> **Note on `ffprobe`:** it rejects Rumble's HLS segments by default because they carry
> a `.tar` extension, which is not in ffmpeg's `allowed_segment_extensions` whitelist
> (`-extension_picky 0` bypasses it). This was investigated as a possible playback
> blocker and is **not** one — mpv, and therefore IINA, opens the same stream without
> any extra option. No workaround is needed in the player arguments.
