# kuro

Terminal anime/donghua streaming for macOS. Searches pluggable provider sites,
resolves a real stream, and plays it in **IINA**.

Anime sites go down constantly. `kuro` is built around that: every site is a
separate scraper behind a toggle, selectors live in config rather than code, and a
provider that keeps failing takes itself out of rotation until it recovers.

See [`PRODUCT_SPEC.md`](PRODUCT_SPEC.md) for the full design and the language
trade-off analysis.

## Requirements

| | |
|---|---|
| macOS | Apple Silicon or Intel |
| [IINA](https://iina.io) | the player |
| [`yt-dlp`](https://github.com/yt-dlp/yt-dlp) | `brew install yt-dlp` — resolves embed hosts |
| Rust | 1.75+, to build |

## Build

```sh
cargo build --release
./target/release/kuro doctor      # verify IINA, yt-dlp, providers
```

## Use

```sh
kuro search martial peak                  # search every enabled provider
kuro watch  martial peak                  # search, pick series + episode, play
kuro play   "martial god asura season 2" --ep 15
kuro play   ... --ep 15 --quality 720p --mirror dailymotion
kuro next                                 # next unwatched episode
kuro continue                             # resume where you stopped
kuro list                                 # watch history
```

Add `--dry-run` to any play command to print the resolved stream URL and the exact
player command without launching anything.

## Providers

```sh
kuro provider list                        # state + health for each
kuro provider disable luciferdonghua      # site is down, silence it
kuro provider only    luciferdonghua      # use just this one
kuro provider test    luciferdonghua      # run the full chain, with timings
kuro provider reload                      # re-read selector TOMLs
```

A provider that fails `health.auto_disable_after_failures` times in a row disables
itself, and re-probes every `health.recheck_interval` until the site is back.

### Adding a site

Most sites need no Rust at all — only a selector file in
`~/Library/Application Support/kuro/providers.d/`:

```toml
id           = "example"
display_name = "Example"
base_url     = "https://example.tld"

[endpoints]
search = "/?s={query}"

[selectors.search]
item  = "div.listupd article.bs"
title = "h2"
url   = "a[href]"

[selectors.episodes]
item = "div.eplister ul li a"

[selectors.mirrors]
option = "select.mirror option[value]"

[selectors.embed]
iframe = "iframe"
```

Files here shadow the built-in providers, so when a site changes its markup you fix
a selector and run `kuro provider reload` — no rebuild.

Sites needing genuinely novel logic get a module in `crates/kuro-providers/`
implementing the `Provider` trait directly.

## How it works

```
search ─► series ─► episodes ─► mirrors ─► embed URL ─► yt-dlp ─► stream ─► IINA
                                   │                                  ▲
                                   └── each mirror tried in turn ──────┘
```

Providers stop at "here is the embed URL". Because the sites embed third-party
hosts (Rumble, Dailymotion, …) rather than hosting video, stream extraction is
delegated to `yt-dlp` — so `kuro` inherits upstream fixes instead of maintaining
extractors of its own.

## Configuration

`~/Library/Application Support/kuro/config.toml` (`kuro config init` writes a populated one):

```toml
[general]
default_quality = "1080p"
concurrency     = 6

[player]
backend    = "iina"
resume     = true
fullscreen = false

[health]
auto_disable_after_failures = 3
recheck_interval            = "30m"

[providers.luciferdonghua]
enabled  = true
priority = 10
mirrors  = ["rumble", "dailymotion"]
```

## Layout

| Crate | Role |
|---|---|
| `kuro-core` | Domain types, `Provider` trait, cross-provider orchestration |
| `kuro-providers` | Per-site scrapers + the declarative selector engine |
| `kuro-resolver` | Embed URL → playable stream |
| `kuro-player` | `Player` trait + IINA, mpv IPC for resume |
| `kuro-store` | Config, history, bookmarks, provider health |
| `kuro-cli` | Commands and output |

## Status

M1–M4 are done: search, playback, the provider toggle/health system, and watch
history with resume. The `ratatui` TUI (M5) is not built yet — bare `kuro` prints
help for now.

## Legal

`kuro` automates navigation of publicly reachable pages and hands URLs to a local
player. It hosts, stores, and redistributes nothing.

The provider sites it targets generally do not hold distribution rights to what they
serve, and streaming from them may be unlawful where you live. Which providers you
enable is your decision.
