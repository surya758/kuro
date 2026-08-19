# kuro

Terminal anime/donghua streaming for macOS. Searches multiple sites, resolves a real
stream, and plays it in [IINA](https://iina.io).

[![CI](https://github.com/surya758/kuro/actions/workflows/ci.yml/badge.svg)](https://github.com/surya758/kuro/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/surya758/kuro?color=blue)](https://github.com/surya758/kuro/releases/latest)
[![License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Anime sites go down constantly, so every site is a separate scraper behind a toggle,
selectors live in config rather than code, and a provider that keeps failing takes
itself out of rotation until it recovers.

```
$ kuro search "against the gods"

Results for “against the gods”
▸  1. Against the Gods (2023)                 luciferdonghua
   2. Against the Gods (Ni Tian Xie Shen) 3D  donghuastream
   3. Against the Gods Season 2               donghuastream
↑↓ move · ⏎ select · q back
```

Pick a series, pick an episode, then play, download, or queue a range. Watched
episodes are marked and part-watched ones show a resume point. `q` steps back a
level rather than quitting. The next couple of episodes are queued into the player,
so IINA's ⌘→ / ⌘← jump between them without returning to kuro.

## Install

```sh
brew tap surya758/tap
brew trust surya758/tap     # Homebrew 6+ requires this for third-party taps
brew install kuro

brew install --cask iina    # the player
kuro doctor                 # verify everything is wired up
```

`yt-dlp`, `curl-impersonate`, `mpv` and shell completions come along with it.

IINA remains the player. `mpv` is used only for sources IINA cannot play — see
[Players](#players).

<details>
<summary>From source</summary>

Needs [Rust](https://rustup.rs) 1.75+ and macOS.

```sh
brew install --cask iina yt-dlp surya758/tap/curl-impersonate
git clone https://github.com/surya758/kuro && cd kuro
cargo install --path crates/kuro-cli
kuro doctor
```

`kuro completions zsh > ~/.zfunc/_kuro` for completions (bash, fish, elvish,
powershell too).

</details>

<details>
<summary>Uninstalling</summary>

```sh
brew uninstall kuro && brew untap surya758/tap
# or, from source: cargo uninstall kuro-cli

rm -rf ~/Library/Application\ Support/kuro   # config, history, bookmarks
rm -rf ~/Library/Caches/kuro                 # cached pages
```

`kuro cache clear` and `kuro history --clear` remove just those, if you would rather
keep the rest.

</details>

## Usage

`kuro search` is the way in. **Quote the query** — it is a single argument.

```sh
kuro search "against the gods"             # interactive; plain list when piped

kuro play   "<query>" --ep 15              # a specific episode, no prompts
kuro play   "<query>" --ep 1-5             # queue a range, played in order
kuro next                                  # next unwatched episode
kuro continue                              # resume where you stopped

kuro download "<query>" --ep 1-5 -o ~/Anime
kuro download "<query>" --all -j 6         # whole series, six at a time

kuro history                               # --clear to erase
kuro bookmark add "<query>"                # and `bookmark list`
kuro bookmark check                        # what aired since you last looked
kuro provider | config | cache | doctor
```

Add `--skip` to jump openings and endings, where [AniSkip] has data — coverage is
crowd-sourced and thin for donghua, so kuro says when there is nothing to skip.

`bookmark check` reports what has come out on the series you follow; `bookmark list`
shows the same ★ badge offline. Streaming providers publish no air dates, so kuro
asks AniList for a broadcast schedule — where one exists, recency is a fact
(`aired 4 days ago`) and works from the moment you follow a series. Coverage is
anime-shaped: donghua rarely have a schedule, and those fall back to watching the
episode list grow between checks, reported as `seen` rather than `aired`. That
fallback baselines on the first sighting rather than announcing a whole back
catalogue. `--within <DAYS>` (default 7) sets how long something stays news.

Global flags: `--provider`, `--quality <best|1080p|720p|…>` (a cap: the closest rung
at or below it plays), `-S N` to take the Nth result, `--json`, `--dry-run`,
`--no-cache`, `-v`/`-vv`. `KURO_QUALITY`, `KURO_PROVIDER` and `KURO_SKIP` set
defaults. `NO_COLOR=1` disables colour.

## Providers

Bundled: **luciferdonghua**, **donghuastream**, **anidb**, **animecube**.

**animecube** serves 4K60 with real subtitle tracks, where the others top out at
1080p with subtitles burned into the picture. It needs `mpv` — see below.

```sh
kuro provider list                        # state + health for each
kuro provider disable <id>                # site is down, silence it
kuro provider only    <id>                # use just this one
kuro provider test    <id>                # run the full chain, with timings
kuro provider reload                      # re-read selector files
```

A provider that fails 3 times in a row disables itself and re-probes every 30
minutes. Both numbers are configurable.

anidb sits behind a challenge that inspects the TLS handshake, so it is fetched
through [curl-impersonate](https://github.com/lexiforest/curl-impersonate) — a
Homebrew dependency, so there is nothing to do. Only its scraping calls use it;
video comes from an ordinary CDN. Without it, anidb fails and nothing else does. If
the binary is not on `PATH`, set `general.impersonate_command` to its full path.

### Players

IINA plays everything by default. Some sources publish no combined video+audio
rendition and have to be resolved by the player itself, which IINA cannot do — its
bundled extractor is too old for those hosts and it buffers them badly. kuro uses
`mpv` for exactly those, automatically, and leaves everything else on IINA.

To use one player throughout:

```toml
[player]
backend = "mpv"     # or "iina", the default
```

`kuro doctor` reports both. Without mpv, animecube will not play; nothing else is
affected.

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
url   = "a[href]"          # omit when the item is itself the link

[selectors.episodes]
item = "div.eplister ul li a"
# format = "json"          # for sites with a JSON API instead of markup

[selectors.mirrors]
option = "select.mirror option[value]"

[selectors.embed]
iframe = "iframe"
# script_key = "file"      # for players configured by script, not an iframe
```

Files here shadow the bundled providers, so when a site changes its markup you edit
a selector and run `kuro provider reload` — no rebuild. `kuro provider test <id>`
shows exactly which step breaks.

## Configuration

`~/Library/Application Support/kuro/config.toml` — `kuro config init` writes a
populated one.

```toml
[general]
default_quality = "best"   # a cap; set a height to limit bandwidth
concurrency     = 6
cache           = true

[player]
backend    = "iina"
resume     = true
fullscreen = false

[health]
auto_disable_after_failures = 3
recheck_interval            = "30m"

[providers.luciferdonghua]
enabled  = true
priority = 10                          # higher wins when merging duplicates
mirrors  = ["rumble", "dailymotion"]   # preferred embed hosts, best first
```

## How it works

```
search ─► series ─► episodes ─► mirrors ─► embed URL ─► yt-dlp ─► stream ─► IINA
                                   │                                  ▲
                                   └── each mirror tried in turn ──────┘
```

Most of these sites embed third-party hosts (Rumble, Dailymotion, …) rather than
hosting video, so scrapers stop at the embed URL and `yt-dlp` does the extraction —
kuro inherits upstream fixes instead of maintaining extractors. Page JavaScript is
never executed.

## Development

```sh
cargo test                # all offline
cargo run -- search foo
```

Scrapers are tested against recorded HTML in `tests/fixtures/`, so CI never touches
the network. When a site redesigns, those tests fail — that's the signal to
re-record and update the selector file.

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

macOS and IINA only for now, though `Player` is a trait. No `fzf` dependency —
selection is built in.

[AniSkip]: https://api.aniskip.com/

## Legal

`kuro` automates navigation of publicly reachable pages and hands URLs to a local
player. It hosts, stores, and redistributes nothing.

The sites it targets generally do not hold distribution rights to what they serve,
and streaming from them may be unlawful where you live. Which providers you enable
is your decision.

## License

MIT — see [LICENSE](LICENSE).
