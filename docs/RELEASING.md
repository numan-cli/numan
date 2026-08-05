# Releasing Numan

## Versioning

- Follow [Semantic Versioning](https://semver.org/).
- Single source of truth: `version` in `Cargo.toml` (crate `numan-cli`, binary `numan`).
- MSRV: `rust-version` in `Cargo.toml` (currently **1.88**); enforced in CI with `cargo +1.88 check --locked`.
- Git tags use a `v` prefix: `v0.1.0`.

## Changelog

- Maintain [CHANGELOG.md](../CHANGELOG.md) using [Keep a Changelog](https://keepachangelog.com/).
- Move items from `[Unreleased]` into a dated version section before tagging.
- **GitHub Release body** is taken from that version's section in `CHANGELOG.md` via `scripts/release-notes-from-changelog.sh` — real Added/Changed/Fixed bullets, not auto-generated "Full Changelog: vX...vY" compare links.
- Compare links at the bottom of `CHANGELOG.md` (`[X.Y.Z]: https://github.com/...`) stay in the file only; they are not copied into release notes.

Preview release notes before tagging:

```bash
bash scripts/release-notes-from-changelog.sh vX.Y.Z
```

## Release checklist

Run locally before tagging (matches CI + release preflight):

```bash
cargo fmt --all -- --check
cargo clippy -- -D warnings
cargo test
cargo package --locked
```

Then:

1. Bump `version` in `Cargo.toml`.
2. Update `CHANGELOG.md` (new section, clear `[Unreleased]`).
3. Verify the README that will ship in the crate and tagged source is correct for the target release. Prefer version-independent links to the Releases page and `vX.Y.Z` examples; do not claim an unpublished tag is already available.
4. Merge to `master` and **wait for CI to pass** on the release commit.
5. Tag and push, replacing `X.Y.Z` with the release version:

   ```bash
   release_version=X.Y.Z
   git tag "v${release_version}"
   git push origin master
   git push origin "v${release_version}"
   ```

6. The [Release workflow](https://github.com/tonythethompson/numan/actions/workflows/release.yml) waits for green CI on the tagged commit, runs preflight checks, then builds archives and publishes.
7. Confirm platform archives and `SHA256SUMS` on GitHub Releases.
8. Confirm the **Publish to crates.io** job succeeds (requires `CRATES_IO_TOKEN` repository secret).
9. Confirm the [`Publish to WinGet`](../.github/workflows/winget.yml) workflow verifies the `winget-release-ready` artifact and published Windows release asset, then opens the update PR after the `v*.*.*` tag-triggered Release workflow completes (manual recovery: dispatch with required `release_tag`).
10. Confirm the [`Publish to Homebrew tap`](../.github/workflows/homebrew.yml) workflow verifies the `homebrew-release-ready` artifact and pushes `Formula/numan.rb` to [`tonythethompson/homebrew-numan`](https://github.com/tonythethompson/homebrew-numan) (requires `HOMEBREW_TAP_TOKEN`; manual recovery: dispatch with required `release_tag`).
11. After publication, update documentation only if it needs links that depend on newly created release pages or assets; do not use this step to repair README content already shipped in the crate or tag.

**Do not tag until CI is green on `master`.** The release workflow gates on CI check results for tag pushes; pushing a tag on a failing commit blocks publication.

## CI jobs (reference)

| Job | Purpose |
|-----|---------|
| Test | `cargo test` on Linux, Windows, macOS |
| Clippy | `cargo clippy -- -D warnings` |
| Format | `cargo fmt --all -- --check` |
| MSRV | `cargo check` on pinned `rust-version` |
| Package | `cargo package --locked` (crates.io manifest sanity) |
| Deny | `cargo deny` advisories + licenses |
| Real-Nu acceptance | `cargo test -- --ignored` with Nu 0.113 |

## crates.io

- Package name: **`numan-cli`** (install with `cargo install numan-cli`).
- Binary name: **`numan`**.
- Set `CRATES_IO_TOKEN` in GitHub repository secrets before the first publish.
- Dry-run locally: `cargo publish --dry-run`.

## Install paths users should see

| Method | Command |
|--------|---------|
| GitHub Release | Download archive from [Releases](https://github.com/tonythethompson/numan/releases) |
| crates.io | `cargo install numan-cli` |
| From source | `cargo install --path .` or `cargo install --git https://github.com/tonythethompson/numan` |
| Homebrew | `brew tap tonythethompson/numan && brew install numan` (see [PACKAGING.md](PACKAGING.md)) |
| winget | `winget install tonythethompson.numan` (see [PACKAGING.md](PACKAGING.md)) |
