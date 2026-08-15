# kuro

Terminal anime/donghua streaming for macOS. Searches multiple provider sites,
resolves a real stream, and plays it in [IINA](https://iina.io).

[![CI](https://github.com/surya758/kuro/actions/workflows/ci.yml/badge.svg)](https://github.com/surya758/kuro/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/surya758/kuro?color=blue)](https://github.com/surya758/kuro/releases/latest)
[![License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Anime sites go down constantly, so every site is a separate scraper behind a toggle,
selectors live in config rather than code, and a provider that keeps failing takes
itself out of rotation until it recovers.

Type what you want to watch:

```
$ kuro search "against the gods"
⠋ Searching 2 provider(s)… 1.3s
Found 10 result(s) 1.3s

Results for “against the gods”
▸  1. Against the Gods (2023)                 luciferdonghua
   2. Against the Gods (Ni Tian Xie Shen) 3D  donghuastream
   3. Against the Gods Season 2               donghuastream
↑↓ move · ⏎ select · q back
```

Pick a series, pick an episode, pick what to do with it:

```
Against the Gods (2023) · Episode 1
▸  1. ▶  Play
   2. ⬇  Download this episode
   3. ⬇  Download a range…
   4. ⚙  Quality  1080p
   5. ☆  Bookmark series
   6. ←  Back to episodes
↑↓ move · ⏎ select · q back
```

Watched episodes are marked, part-watched ones show a resume point, and playback
rolls into the next episode when it ends. `q` steps back a level rather than
quitting, so a wrong pick costs nothing.

The next couple of episodes are queued into the player's playlist while you watch,
so IINA's ⌘→ / ⌘← jump between episodes without coming back to kuro.

Everything is scriptable too — piping or `--json` skips the menus entirely.

## Contents

- [Install](#install)
  - [Homebrew](#homebrew)
  - [From source](#from-source)
- [Usage](#usage)
  - [Command map](#command-map)
- [Providers](#providers)
  - [anidb and curl-impersonate](#anidb-and-curl-impersonate)
  - [Adding a site](#adding-a-site)
- [Configuration](#configuration)
- [How it works](#how-it-works)
- [Development](#development)
- [Status](#status)
- [Legal](#legal)

## Install

### Homebrew

```sh
brew tap surya758/tap
brew trust surya758/tap     # Homebrew 6+ requires this for third-party taps
brew install kuro

brew install --cask iina    # the player
kuro doctor                 # verify everything is wired up
```

`yt-dlp` comes in as a dependency. Homebrew also installs shell completions.

### From source

Needs [Rust](https://rustup.rs) 1.75+ and macOS.

```sh
brew install --cask iina                  # the player
brew install yt-dlp                       # stream resolution and downloads

git clone https://github.com/surya758/kuro
cd kuro
cargo install --path crates/kuro-cli      # installs `kuro` to ~/.cargo/bin

kuro completions zsh > ~/.zfunc/_kuro     # bash, fish, elvish, powershell too
kuro doctor                               # verify everything is wired up
```

To hack on it without installing, `cargo run -- search "<query>"` works from the
repo, and `cargo build --release` leaves a binary at `target/release/kuro`.

<details>
<summary>Uninstalling</summary>

```sh
brew uninstall kuro
brew untap surya758/tap
```

Or, if you built from source, `cargo uninstall kuro-cli`.

Either way, config and history live outside the binary. To remove those too:

```sh
rm -rf ~/Library/Application\ Support/kuro   # config, history, bookmarks, health
rm -rf ~/Library/Caches/kuro                 # cached pages
```

`kuro cache clear` empties just the cache and `kuro history --clear` just the watch
history, if you would rather keep the rest.

</details>

macOS only.

## Usage

`kuro search` is the way in. **Quote the query** — it is a single argument:

```sh
kuro search "against the gods"
```

Every other command works without prompts, for scripting or when you already know
what you want:

```sh
kuro search "<query>"                      # interactive; a plain list when piped
kuro play   "<query>" --ep 15              # play a specific episode
kuro play   "<query>" --ep 1-5             # queue a range, played in order
kuro play   "<query>" --ep 15 --quality 720p --mirror dailymotion
kuro next                                  # next unwatched episode
kuro continue                              # resume where you stopped
kuro history                               # watch history (--clear to erase)
kuro bookmark add "<query>"                # follow a series
kuro bookmark list                         # what you are following

kuro download "<query>" --ep 15 -o ~/Anime # save instead of stream
kuro download "<query>" --ep 1-5 -o ~/Anime
kuro download "<query>" --all -o ~/Anime   # the whole series, 3 at a time
kuro download "<query>" --all -j 6         # ...or six
```

Add `--skip` to a play command to jump openings and endings, where [AniSkip] has
data for the episode. Coverage is crowd-sourced and thin for donghua — kuro says so
and plays on when there is nothing to skip.

Global flags: `--provider <id>`, `--quality <best|1080p|720p|…>`,
`-S/--select-nth N` (take the Nth search result instead of the best match, for
scripting), `--json`, `--dry-run` (print the resolved stream URL and player command,
launch nothing), `--no-cache`, `-v`/`-vv` for logs.

`KURO_QUALITY`, `KURO_PROVIDER` and `KURO_SKIP` set the defaults for `--quality`,
`--provider` and `--skip`.

Scraped pages are cached on disk, so a repeated search is near-instant. `kuro cache
status` shows where and how much; `kuro cache clear` empties it.

Spinners, the selected row, progress bars and the playback marker share one accent
colour so activity stands apart from green/red status. Set `NO_COLOR=1` to turn all
of it off.

### Command map

```
kuro search "<query>"       search · pick series · pick episode · play/download
                            plain list when piped or --json
kuro play "<q>" --ep 15     direct, no prompts
kuro download "<q>" --all   direct, no prompts
kuro next | continue        resume from watch history
kuro history | bookmark     history and follows
kuro provider | config | cache | doctor
```

## Providers

Bundled: **luciferdonghua**, **donghuastream**, **anidb**.

`anidb` needs one extra binary — see [below](#anidb-and-curl-impersonate). The other
two work out of the box.

```sh
kuro provider list                        # state + health for each
kuro provider disable <id>                # site is down, silence it
kuro provider only    <id>                # use just this one
kuro provider test    <id>                # run the full chain, with timings
kuro provider reload                      # re-read selector files
```

A provider that fails 3 times in a row disables itself and re-probes every 30
minutes until the site is back. Both numbers are configurable.

### anidb and curl-impersonate

anidb.app is behind a challenge that inspects the TLS handshake, so no ordinary HTTP
client reaches it — headers make no difference. Fetching it needs
[curl-impersonate](https://github.com/lexiforest/curl-impersonate), which performs a
browser-shaped handshake.

**Installing via Homebrew? Nothing to do** — it comes as a dependency.

Building from source, install it yourself (it is not in Homebrew core):

```sh
brew install surya758/tap/curl-impersonate   # or, by hand:

# Apple Silicon; use x86_64-macos on Intel
curl -LO https://github.com/lexiforest/curl-impersonate/releases/download/v2.1.0/curl-impersonate-v2.1.0.arm64-macos.tar.gz
mkdir -p ~/.local/bin && tar xzf curl-impersonate-*.tar.gz -C ~/.local/bin
export PATH="$HOME/.local/bin:$PATH"          # add to your shell profile
```

`kuro doctor` reports `✓ impersonate` once it is found. Not on `PATH`? Point kuro at
it instead:

```toml
[general]
impersonate_command = "/Users/you/.local/bin/curl-impersonate"
```

Only anidb's scraping calls go through it. Video comes from a CDN that is not
challenged, so playback is ordinary. Without the binary, anidb fails and the other
providers are unaffected.

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
cargo test                # 103 tests, all offline
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

Working: interactive search and browse, playback with mirror failover and playlist
queueing, downloads, provider toggling/health, watch history and resume, disk
caching.

Not built yet:

- **Other players and platforms.** IINA on macOS only, though `Player` is a trait.

Not supported: sites behind a Cloudflare challenge. `kuro` never runs page
JavaScript, so a challenge-gated site cannot be scraped — `kuro provider test`
reports `Blocked` for these.

No `fzf` dependency: selection is handled by the built-in TUI.

[AniSkip]: https://api.aniskip.com/

## Legal

`kuro` automates navigation of publicly reachable pages and hands URLs to a local
player. It hosts, stores, and redistributes nothing.

The sites it targets generally do not hold distribution rights to what they serve,
and streaming from them may be unlawful where you live. Which providers you enable
is your decision.

## License

MIT — see [LICENSE](LICENSE).
