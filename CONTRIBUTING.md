# Contributing

## Adding a provider

Most sites need no Rust. Copy an existing spec from `providers.d/`, adjust the
selectors, and drop it in `~/Library/Application Support/kuro/providers.d/` to test
locally — files there shadow the bundled ones.

```sh
kuro provider test <id>     # runs the whole chain and shows where it breaks
```

The output names the failing step, and a `ParseFailure` names the exact selector
that stopped matching.

Two things that vary between sites and are already handled declaratively:

- **Mirror encoding.** Some sites put a URL in the `<option value>`; others put
  base64-encoded `<iframe>` markup. Set `value_encoding = "base64_html"` for the
  latter.
- **JS-mounted players.** Some mirrors have no iframe at all and advertise the
  player only via `<meta itemprop="embedUrl">`. Set `meta` under
  `[selectors.embed]`.

If a site genuinely needs new logic, add a module in `crates/kuro-providers/`
implementing the `Provider` trait and register it in `registry.rs`.

**Sites behind a Cloudflare challenge are out of scope.** `kuro` never executes page
JavaScript, and working around a bot challenge is not something this project does.

### Before opening a PR

Record fixtures so the scraper is tested offline:

```sh
curl -A "Mozilla/5.0 …" "https://site.tld/?s=test" -o tests/fixtures/<id>/search.html
```

Strip `<script>` and `<style>` bodies from what you record — they are never executed,
they are most of the bytes, and they carry the site's ad code. Then add tests to
`crates/kuro-providers/tests/fixtures.rs` asserting counts and hosts, not exact
titles (sites re-arrange their catalogues constantly).

## Development

```sh
cargo test                        # 84 tests, all offline
cargo clippy --all-targets
cargo fmt --all
```

CI runs all three on macOS. Live checks are manual, via `kuro provider test` — a
red build caused by someone else's site going down is noise.

## Releasing

The Homebrew formula lives in a separate tap repo, [`surya758/homebrew-tap`], and
pins a git tag plus its revision rather than a tarball, so no `sha256` is involved.

1. Bump `version` in the workspace `Cargo.toml`, update `CHANGELOG.md`.
2. Commit, then `git tag -a vX.Y.Z -m "vX.Y.Z"` and push with `--tags`.
3. In the tap repo, update **both** `tag:` and `revision:` in `Formula/kuro.rb`
   (`git rev-parse vX.Y.Z^{commit}` gives the revision) and push.
4. `brew install --build-from-source surya758/tap/kuro` to verify.

Both fields must move together — a stale `revision` silently installs the old
version even though the tag looks right.

[`surya758/homebrew-tap`]: https://github.com/surya758/homebrew-tap
