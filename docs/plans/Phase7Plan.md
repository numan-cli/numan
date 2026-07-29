# Phase 7 Plan: Polish, CI, Distribution

Take Numan from feature-complete core (Phases 1–6) to a distributable, polished CLI.

**Tracking issue:** [#12](https://github.com/tonythethompson/numan/issues/12)

---

## Status overview

| Slice | Theme | Status |
|-------|--------|--------|
| 7.1 | Distribution baseline | ✅ Done |
| 7.2 | `numan doctor` | ✅ Done |
| 7.3 | Daily-driver polish | ✅ Done |
| 7.4 | Onboarding path | ✅ Done |
| 7.5 | CI / release hardening | ✅ Done |
| 7.6 | Wider distribution | ✅ Done (winget community listing + automated update PRs) |

---

## 7.1 Distribution baseline ✅

Shipped in v0.1.0–v0.1.1:

- GitHub Release workflow (linux / windows / macOS × aarch64 + x86_64 mac)
- `CHANGELOG.md` + [docs/RELEASING.md](docs/RELEASING.md)
- crates.io publish (`cargo install numan-cli`)
- `numan init` / `numan init --refresh`
- Real-Nu acceptance CI job
- README install + quickstart (partial)

---

## 7.2 `numan doctor` ✅

**Spec:** [docs/numan-doctor.md](docs/numan-doctor.md)

Shipped:

- `numan doctor [--fix] [--yes] [--json] [--nupm-home PATH]`
- Full check catalog; repair tiers delegate to `init`, `activate`, `registry sync`
- `scan_on_doctor` config gate; `tests/doctor_test.rs`

---

## 7.3 Daily-driver polish ✅

Shipped:

1. **Shell completions** — `numan completions bash|fish|zsh|powershell` via `clap_complete`
2. **Error message UX pass** — `init`, `install`, `activate`, `nupm import` use `util::hints` aligned with doctor `fix` strings
3. **`--help` audit** — README command table + common flags aligned with clap definitions

---

## 7.4 Onboarding path ✅

Shipped:

1. **`numan init` onboarding checklist** — numbered next steps (sync, search, install, activate, doctor) after first init; official registry auto-configured when production trust root is present
2. **README** — copy-paste quick start block plus step-by-step sections
3. **Compatibility matrix** — promoted in README nupm section ([docs/nupm-compatibility.md](docs/nupm-compatibility.md))

---

## 7.5 CI / release hardening ✅

1. **Release gates on green CI** — tag pushes wait for CI check success; preflight runs fmt/clippy/test/package before build
2. **MSRV pin** — `rust-version = "1.88"` in `Cargo.toml` + MSRV CI job (`cargo +1.88 --locked`)
3. **PR checks** — `cargo deny` (advisories/licenses) and `cargo package --locked` on CI
4. **Release checklist** — [docs/RELEASING.md](docs/RELEASING.md) documents local pre-tag commands; CI also runs on tag pushes

---

## 7.6 Wider distribution ✅

Shipped:

1. **winget** — `packaging/winget/manifests/t/tonythethompson/numan/<version>/` (local `--manifest` install; winget-pkgs PR path documented)
2. **`cargo install --git`** — documented in README
3. **[docs/PACKAGING.md](docs/PACKAGING.md)** — release checksum update checklist

Deferred: Scoop manifest (low demand).

---

## Post-7.6: Official registry cutover ✅

Shipped in v0.1.4:

1. **Production trust root** — `src/core/official_registry.rs` pins `official-2026-07-01` (see `scripts/update-official-trust-root.sh`)
2. **Init auto-config** — `numan init` adds `registries.official` on first run when the built-in trust root is production
3. **Onboarding** — quick start is `init` → `registry sync` → `search` (no manual `registry add` for official)
4. **Distribution** — winget identifier `tonythethompson.numan`

---

## Deferred (not Phase 7)

**Phase 5** ([#11](https://github.com/tonythethompson/numan/issues/11)): source builds (5.2), plugin lifecycle gate (5.5). Snapshots/rollback (5.3) shipped — see [docs/snapshots-and-rollback.md](docs/snapshots-and-rollback.md).

---

## Toward 1.0

Phase 7 core work is complete. Remaining work before **1.0**:

| Milestone | Status |
|-----------|--------|
| winget community listing and updates | ✅ [PR #400470](https://github.com/microsoft/winget-pkgs/pull/400470); updates automated by [winget.yml](../../.github/workflows/winget.yml) |
| Official registry seeded packages | 🔄 [docs/registry-intake-roadmap.md](docs/registry-intake-roadmap.md) — stage 1 acceptance, then curated growth |
| Cross-platform dogfooding | 🔄 Fresh-install checklist on Linux, macOS, Windows |
| Source builds (5.2) | 🔜 [#11](https://github.com/tonythethompson/numan/issues/11) |
| Plugin lifecycle gate (5.5) | 🔜 [#22](https://github.com/tonythethompson/numan/issues/22) |

Stay on **0.1.x** until onboarding + distribution paths are stable; tag **1.0** when the table above is green and core lifecycle has no open P0/P1 issues.

---

## Recommended implementation order

```text
7.1 ✅ → 7.2 doctor → 7.3 polish → 7.4 onboarding → 7.5 CI hardening → 7.6 packagers
```
