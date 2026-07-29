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
- Compare links at the bottom of `CHANGELOG.md` (`[0.1.5]: https://github.com/...`) stay in the file only; they are not copied into release notes.

Preview release notes before tagging:

```bash
bash scripts/release-notes-from-changelog.sh v0.1.5
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
3. Merge to `master` and **wait for CI to pass** on the release commit.
4. Tag and push:

   ```bash
   git tag v0.1.5
   git push origin master
   git push origin v0.1.5
   ```

5. The [Release workflow](https://github.com/tonythethompson/numan/actions/workflows/release.yml) waits for green CI on the tagged commit, runs preflight checks, then builds archives and publishes.
6. Confirm platform archives and `SHA256SUMS` on GitHub Releases.
7. Confirm the **Publish to crates.io** job succeeds (requires `CRATES_IO_TOKEN` repository secret).
8. Confirm the [`Publish to WinGet`](../.github/workflows/winget.yml) workflow opens the update PR after the release is published.
9. Update README current-release links and reproducible-install tag examples only after the GitHub Release is published, so documentation never points users at an unavailable tag.

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
| winget | See [PACKAGING.md](PACKAGING.md) |
