# kuro

Terminal anime/donghua streaming for macOS. Searches multiple provider sites,
resolves a real stream, and plays it in [IINA](https://iina.io).

Anime sites go down constantly, so every site is a separate scraper behind a toggle,
selectors live in config rather than code, and a provider that keeps failing takes
itself out of rotation until it recovers.

```
$ kuro search martial god asura
  1. Martial God Asura              [donghuastream]
  2. Martial God Asura (2023)       [luciferdonghua]
  3. Martial God Asura Season 2     [donghuastream]

$ kuro play "martial god asura season 2" --ep 15
→ Martial God Asura Season 2 [luciferdonghua]
  found 5 mirror(s), resolving…
  trying Rumble …
▶  Martial God Asura Season 2 · Episode 15  [Rumble · 1080p]
```

## Install

```sh
brew install yt-dlp                       # required for stream resolution
brew install --cask iina                  # the player

cargo install --path crates/kuro-cli      # installs `kuro` to ~/.cargo/bin
kuro doctor                               # verify everything is wired up
```

Needs Rust 1.75+ and macOS. `kuro completions zsh > ~/.zfunc/_kuro` for tab
completion (`bash`, `fish`, `elvish`, `powershell` also supported).

## Usage

```sh
kuro search <query>                       # search all enabled providers
kuro watch  <query>                       # search, pick series + episode, play
kuro play   <query> --ep 15               # play a specific episode
kuro play   <query> --ep 15 --quality 720p --mirror dailymotion
kuro next                                 # next unwatched episode
kuro continue                             # resume where you stopped
kuro list                                 # watch history
kuro bookmark add <query>                 # follow a series
```

Global flags: `--provider <id>`, `--quality <best|1080p|720p|…>`, `--json`,
`--dry-run` (print the resolved stream URL and player command, launch nothing),
`-v`/`-vv` for logs.

## Providers

Bundled: **luciferdonghua**, **donghuastream**.

```sh
kuro provider list                        # state + health for each
kuro provider disable <id>                # site is down, silence it
kuro provider only    <id>                # use just this one
kuro provider test    <id>                # run the full chain, with timings
kuro provider reload                      # re-read selector files
```

A provider that fails 3 times in a row disables itself and re-probes every 30
minutes until the site is back. Both numbers are configurable.

### Adding a site

Most sites need no Rust — drop a selector file in
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
# value_encoding = "base64_html"   # if options hold encoded <iframe> markup

[selectors.embed]
iframe = "iframe"
meta   = "meta[itemprop=embedUrl]"   # for JS-mounted players
```

Files here shadow the bundled providers, so when a site changes its markup you edit
a selector and run `kuro provider reload` — no rebuild. Run
`kuro provider test <id>` to see exactly which step breaks.

Sites needing genuinely novel logic get a module in `crates/kuro-providers/`
implementing the `Provider` trait.

## Configuration

`~/Library/Application Support/kuro/config.toml` — `kuro config init` writes a
populated one.

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
priority = 10                    # higher wins when merging duplicate results
mirrors  = ["rumble", "dailymotion"]   # preferred embed hosts, best first
```

## How it works

```
search ─► series ─► episodes ─► mirrors ─► embed URL ─► yt-dlp ─► stream ─► IINA
                                   │                                  ▲
                                   └── each mirror tried in turn ──────┘
```

These sites embed third-party hosts (Rumble, Dailymotion, …) rather than hosting
video, so scrapers stop at "here is the embed URL" and `yt-dlp` does the extraction.
That means `kuro` inherits upstream fixes instead of maintaining extractors.

Page JavaScript is never executed, which sidesteps the anti-bot layer entirely.

## Development

```sh
cargo test                # 63 tests, all offline
cargo run -- search foo
```

Scrapers are tested against recorded HTML in `tests/fixtures/`, so they run in CI
without touching the network. When a site redesigns, those tests fail — that's the
signal to re-record and update the selector file.

| Crate | Role |
|---|---|
| `kuro-core` | Domain types, `Provider` trait, cross-provider orchestration |
| `kuro-providers` | Scrapers + the declarative selector engine |
| `kuro-resolver` | Embed URL → playable stream |
| `kuro-player` | `Player` trait + IINA, mpv IPC for resume |
| `kuro-store` | Config, history, bookmarks, provider health |
| `kuro-cli` | Commands and output |

[`PRODUCT_SPEC.md`](PRODUCT_SPEC.md) has the full design and the language trade-off
analysis.

## Status

Search, playback, provider toggling/health, history and resume all work. The
interactive TUI is not built yet — bare `kuro` prints help. Response caching is not
implemented; every command refetches.

## Legal

`kuro` automates navigation of publicly reachable pages and hands URLs to a local
player. It hosts, stores, and redistributes nothing.

The sites it targets generally do not hold distribution rights to what they serve,
and streaming from them may be unlawful where you live. Which providers you enable
is your decision.

## License

MIT — see [LICENSE](LICENSE).
