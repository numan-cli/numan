# Handoff: Find & Promote 2–3 Nu 0.114-Compatible Plugins

## Context

Numan's official registry has **25 packages** but only **3** work on Nu 0.114.x (the current stable): `nu_plugin_skim`, `nu_plugin_desktop_notifications`, `nu_plugin_prometheus`. Catalog depth is the #1 adoption bottleneck. This task grows the 0.114-compatible set by 2–3 plugins through the full pipeline.

## Repos (all local)

| Repo | Path | Branch | Role |
|------|------|--------|------|
| numan | `d:\Dev\numan` | master | Client CLI (Rust) |
| numan-plugins | `d:\Dev\numan-plugins` | master | CI build pipeline |
| numan-registry | `d:\Dev\numan-registry` | main | Signed catalog |

## Pipeline (serial, per plugin)

```
Research → Promote to manifest → Build dispatch → Spec download → Registry intake → Staging → Lifecycle-prove → Production
```

## Step 1: Find Candidates

**Source of truth:** `d:\Dev\numan-plugins\docs\backlog.json` (45 entries, demand-ranked).

**Eligibility filter:**
- `nu-plugin` / `nu-protocol` dependency **>= 0.114.0** on a tagged release
- Tagged release exists (`has_release: true`) OR is `SOURCE_ONLY` (we CI-build from tag)
- Windows-buildable (pure Rust or only cross-platform deps), OR Windows excluded with concrete reason
- No existing `numan-plugins` release tag for that version (immutability)

**Already researched and blocked (skip these):**
- `PRE_0_112`: clipboard (0.110), dbus (0.101), audio_hook (0.110), vec (0.105), from_beancount (0.76), nuts (0.110)
- `PROMOTED`: skim, highlight, desktop_notifications, image, port_extension, prometheus
- `nu_plugin_bigquery`: nu-plugin 0.112.2 (eligible minor) but needs Google creds for lifecycle proof — skip unless you can prove without credentials

**What to do:**
1. For each `NO_RELEASE` or `SOURCE_ONLY` entry with stars >= 5, check the upstream GitHub repo for new tags/releases since 2026-07-22:
   ```bash
   gh api repos/<owner>/<name>/tags --jq ".[0:3][] | .name"
   ```
2. If a tag exists, check its Cargo.toml for the `nu-plugin` version:
   ```bash
   gh api repos/<owner>/<name>/contents/Cargo.toml?ref=<tag> --jq ".content" | base64 -d | grep "nu-plugin"
   ```
3. Also scan for **new plugins not in the backlog** — search GitHub for `nu_plugin` repos pushed recently with nu-plugin 0.114:
   ```bash
   gh search repos "nu_plugin" --sort updated --limit 30 --json fullName,updatedAt,description
   ```
4. Record findings in `backlog.json` with `c1_note` and updated `blocker` field.

**Priority candidates to check first** (high stars, unknown current state):
- `Euphrasiologist/nu_plugin_plot` (71 stars, was NO_RELEASE)
- `yybit/nu_plugin_compress` (42 stars, was NO_RELEASE)
- `fdncred/nu_plugin_emoji` (27 stars, was NO_RELEASE)
- `fdncred/nu_plugin_json_path` (22 stars, was NO_RELEASE)
- `fdncred/nu_plugin_parquet` (18 stars, was NO_RELEASE)
- `JosephTLyons/nu_plugin_units` (18 stars, was NO_RELEASE)

## Step 2: Promote to Manifest

For each viable candidate, add to `manifest.json` `active[]`:

```json
{
  "repo": "<owner>/<name>",
  "name": "<plugin_bin_name>",
  "owner": "<owner>",
  "plugin_bin": "<binary_name>",
  "tag": "<tag>",
  "source_commit": "<40-char sha from: gh api repos/<repo>/git/ref/tags/<tag> --jq .object.sha>",
  "version": "<semver without v prefix>",
  "exclude_targets": ["x86_64-apple-darwin"],
  "exclude_reason": "Intel Mac EOL; not supported",
  "nu_version": ">=0.114.0 <0.115.0",
  "verified_with": ["0.114.1"],
  "description": "<one-line>",
  "tags": ["plugin", "<category>", "ci-built"]
}
```

**Important:**
- `source_commit` must be the **dereferenced** commit SHA (for annotated tags: `gh api repos/<repo>/git/tags/<tag_sha> --jq .object.sha`)
- `x86_64-apple-darwin` is permanently excluded (Intel Mac is EOL; not worth the build surface)
- `aarch64-unknown-linux-gnu` may need exclusion if the plugin uses openssl-sys/native-tls (cross-compile fails)
- Validate: `python scripts/validate_manifest.py --only <name> --verify-upstream`

Open a PR on numan-plugins, merge after checks pass.

## Step 3: Build

```bash
cd d:\Dev\numan-plugins
gh workflow run build.yml -f only="<plugin_name_1>,<plugin_name_2>"
```

Monitor:
```bash
gh run list --workflow=build.yml --limit 1 --json databaseId,status,conclusion
gh run view <id> --json jobs --jq ".jobs[] | {name, conclusion}"
```

**Known failure modes:**
- `aarch64-unknown-linux-gnu` cross + openssl-sys → exclude target, re-dispatch
- `x86_64-apple-darwin` → already excluded
- Tag-to-commit mismatch → fix `source_commit` in manifest

On success, download spec artifacts:
```bash
gh run download <run_id> --name spec-<plugin_name> --dir d:\Dev\numan-registry\specs\
```

## Step 4: Registry Intake

```bash
cd d:\Dev\numan-registry
git checkout main && git pull origin main
git checkout -b intake/<plugin-name>-<version>

# For each spec:
python scripts/add-package.py --spec specs/spec-<plugin_name>.json --write

# Local checks (all must pass):
python scripts/scan_for_secrets.py
python scripts/preflight.py
python scripts/validate.py --index registry/index.json --sig registry/index.json.sig --pub keys/official.pub --skip-artifacts
cargo run --locked --manifest-path tools/numan-parser-check/Cargo.toml -- registry/index.json
python scripts/lint-manifest-index.py --index registry/index.json --manifest d:\Dev\numan-plugins\manifest.json

# Update docs/intake-state.json (add to ready[], add changelog entry)
# Commit, push, open PR
gh pr create --title "Intake <owner>/<name>@<version>" --body "..." --base main
```

After PR merges → staging auto-runs on main push. Wait for green.

## Step 5: Production + Lifecycle Prove

```bash
# Dispatch production
gh workflow run production.yml --ref main -f reason="Intake <owner>/<name>@<version>"

# Approve the environment gate
gh api repos/tonythethompson/numan-registry/actions/runs/<run_id>/pending_deployments
gh api repos/tonythethompson/numan-registry/actions/runs/<run_id>/pending_deployments -X POST --input - <<'EOF'
{"environment_ids":[17544354564],"state":"approved","comment":"<reason>"}
EOF

# After production is live, lifecycle-prove:
python scripts/lifecycle-prove.py --package <owner>/<name> --numan d:\Dev\numan\target\release\numan.exe
```

**Environment:** Nu 0.114.1 is on PATH (`nu --version`). The local `numan` binary is v0.1.5 (installed via `cargo install --path d:\Dev\numan`). If lifecycle-prove needs a fresh build: `cd d:\Dev\numan && cargo build --release`.

Lifecycle-prove runs: `init → registry sync → search → info → install → activate → doctor → list → deactivate → remove → gc`. All 11 steps must pass.

## Step 6: Bookkeeping

- Update `backlog.json`: set `blocker: "PROMOTED"`, add `c1_note` with date and details
- Update `docs/intake-state.json` in numan-registry
- Update `d:\Dev\numan\docs\plans\consolidated-multi-repo-roadmap.md` Wave 2 research queue checkboxes

## Constraints & Gotchas

1. **Plugin ABI is Nu-minor-scoped.** A plugin built against nu-plugin 0.114.x ONLY works with Nu 0.114.x. The `nu_version` field must be `>=0.114.0 <0.115.0`.
2. **Immutability.** Never rebuild an existing release tag. If bytes change, bump version.
3. **`source_commit` must be exact.** The build workflow verifies `git ls-remote` tag → commit matches the manifest.
4. **Staging uses an ephemeral signing key.** The client can't verify staging without `NUMAN_ALLOW_UNSIGNED=1`. Lifecycle-prove runs against **production** (after dispatch), not staging.
5. **Production needs environment approval** via the API (see Step 5).
6. **`gen_spec.py` is all-or-nothing** — if any target in the matrix fails, no spec is emitted. Exclude failing targets in manifest and re-dispatch.
7. **Don't use `--all-targets` with clippy** in the numan repo (CI doesn't gate it).
8. **PowerShell escaping** — prefer `--body-file` for `gh pr create` bodies with special chars.

## Success Criteria

- [ ] 2–3 new plugins in `manifest.json` `active[]` with `nu_version: ">=0.114.0 <0.115.0"`
- [ ] Successful build run with spec artifacts downloaded
- [ ] Registry PR(s) merged, staging green
- [ ] Production dispatched and live
- [ ] Lifecycle-prove passes on Windows x64 / Nu 0.114.1 for each plugin
- [ ] `numan search` on Nu 0.114.1 shows the new plugins as compatible
- [ ] Backlog + roadmap docs updated

## Time Estimate

~30–45 min per plugin once a viable candidate is identified. Research (Step 1) is the variable — many backlog entries will still be too old. Budget 1–2 hours total for 2–3 promotions if candidates exist.
