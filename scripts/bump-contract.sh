#!/usr/bin/env bash
#
# scripts/bump-contract.sh — Bump the cross-repo roadmap contract.
#
# WHAT THIS DOES
#   The roadmap-drift guardrail is frozen at a Git tag in tonythethompson/numan:
#     - numan-roadmap-contract/vN (e.g. .../v1).
#   Each sibling repo's CI workflow fetches the consolidated roadmap + drift
#   script by SHA from that tag. Bumping the contract is the only sanctioned
#   way to change what the cross-repo guardrail says, because:
#     - It reshuffles refs in three repos at once.
#     - It fails closed if the local drift check breaks.
#     - It refuses to issue the PR set unless the maintainer is recognized
#       on all three sibling repos.
#
# The script preserves the previous contract as a `.bak` tag for one cycle
# so a faulty bump can be reverted without breaking CI on the sibling repos.
#
# USAGE
#   scripts/bump-contract.sh <NEW_VERSION> [-r "reason for the bump"]
#   scripts/bump-contract.sh --init    # special-case: mint v1 from current state
#       Use --init ONCE. After v1 exists, all changes go via '<NEW_VERSION>'.
#
# EXAMPLES
#   scripts/bump-contract.sh 2 -r "numan-upgrade is shipped"
#   scripts/bump-contract.sh --init -r "freeze the post-audit v1"
#
# The script will:
#   1. Run scripts/check-roadmap-drift.py locally — refuse to proceed if any
#      sentinel rule fails.
#   2. Freeze CONTENT_SHA = HEAD (roadmap + drift script must already be on
#      the branch tip).
#   3. Rewrite CONTRACT_TAG + CONTRACT_SHA in:
#        .github/workflows/ci.yml
#        cross-repo-mirror/numan-plugins/.github/workflows/roadmap-drift.yml
#        cross-repo-mirror/numan-registry/.github/workflows/roadmap-drift.yml
#        cross-repo-mirror/README.md
#        cross-repo-mirror/*/docs/roadmap.md (blob URLs)
#      and refresh docs/contracts/roadmap-vN.md from the v1 template.
#   4. Commit the pin rewrite, then create annotated tag NEW_TAG at CONTENT_SHA.
#   5. Push the numan branch (tag stays local until after the numan PR merges).
#   6. Materialize + push sibling branches with the bumped workflow/roadmap,
#      then open coordinated PRs via `gh pr create`.
#
# PR-MERGE GATE
#   Merge numan first and publish the tag. Sibling CI fails closed until the
#   tag resolves to CONTRACT_SHA; merge sibling PRs only after that.
#
# REQUIREMENTS
#   - git, curl, sed, jq (or python3 for the same parsing), and the gh CLI
#     authenticated against the three repos.
#   - The author must be a maintainer on numan, numan-plugins, and
#     numan-registry (verified via gh api /collaborators). If you have
#     triage-write access only, the script refuses after step 5.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CONTRACT_DOC_DIR="$REPO_ROOT/docs/contracts"
DRIFT_SCRIPT="$REPO_ROOT/scripts/check-roadmap-drift.py"

# Repos. Order matters: numan is the source-of-truth broker.
REPO_OWNER="tonythethompson"
REPO_NUMAN="$REPO_OWNER/numan"
REPO_PLUGINS="$REPO_OWNER/numan-plugins"
REPO_REGISTRY="$REPO_OWNER/numan-registry"

# Files under numan that pin the contract.
NUMAN_CI="$REPO_ROOT/.github/workflows/ci.yml"
PLUGINS_YML="$REPO_ROOT/cross-repo-mirror/numan-plugins/.github/workflows/roadmap-drift.yml"
REGISTRY_YML="$REPO_ROOT/cross-repo-mirror/numan-registry/.github/workflows/roadmap-drift.yml"
MIRROR_README="$REPO_ROOT/cross-repo-mirror/README.md"
PLUGINS_ROADMAP="$REPO_ROOT/cross-repo-mirror/numan-plugins/docs/roadmap.md"
REGISTRY_ROADMAP="$REPO_ROOT/cross-repo-mirror/numan-registry/docs/roadmap.md"

usage() {
    sed -n '2,/^$/p' "$0" | sed 's/^# \{0,1\}//'
    exit 1
}

log() { printf '\033[1;34m[bump-contract]\033[0m %s\n' "$*"; }
err() { printf '\033[1;31m[bump-contract]\033[0m %s\n' "$*" >&2; }
warn() { printf '\033[1;33m[bump-contract]\033[0m %s\n' "$*" >&2; }

require_cmd() {
    for c in "$@"; do
        command -v "$c" >/dev/null 2>&1 || { err "missing required command: $c"; exit 2; }
    done
}

require_cmd git curl python3 gh

BRANCH_NAME="$(git symbolic-ref --short HEAD 2>/dev/null || true)"
[ -n "$BRANCH_NAME" ] || { err "not on a git branch (detached HEAD?); refusing to bump"; exit 6; }

DRY_RUN="${BUMP_DRY_RUN:-0}"

# --- 0. Parse args -------------------------------------------------------
INIT=0
NEW_VERSION=""
REASON=""
while [ $# -gt 0 ]; do
    case "$1" in
        --init) INIT=1; shift ;;
        -r|--reason) REASON="${2:-}"; shift 2 ;;
        -h|--help) usage ;;
        *)
            if [ -z "$NEW_VERSION" ]; then
                NEW_VERSION="$1"; shift
            else
                err "unexpected positional arg: $1"; usage
            fi
            ;;
    esac
done

if [ -z "$REASON" ]; then
    err "BUMP_REASON is required (-r '<one-line reason>')"
    exit 2
fi
if [ "$INIT" -eq 0 ] && ! [[ "$NEW_VERSION" =~ ^[0-9]+$ ]]; then
    err "NEW_VERSION must be a positive integer; got '$NEW_VERSION'"
    exit 2
fi
if [ "$INIT" -eq 0 ] && [ "$NEW_VERSION" -lt 2 ]; then
    err "NEW_VERSION must be >= 2 (v1 was minted via --init); got $NEW_VERSION"
    exit 2
fi

if [ "$INIT" -eq 1 ]; then
    VERSION_LABEL=1
    NEW_TAG="numan-roadmap-contract/v1"
    OLD_TAG="numan-roadmap-contract/v0-pre"
else
    VERSION_LABEL="$NEW_VERSION"
    NEW_TAG="numan-roadmap-contract/v${NEW_VERSION}"
    OLD_TAG="numan-roadmap-contract/v$(( NEW_VERSION - 1 ))"
fi

log "contract bump: $OLD_TAG  --[$REASON]-->  $NEW_TAG"

# Verify effective repository permissions before any local or remote mutation.
gh auth status >/dev/null 2>&1 || { err "gh not authenticated"; exit 5; }
check_maintainer_access() {
    local repo="$1" permission
    permission="$(gh api "repos/$repo" --jq '.permissions | if .admin then "admin" elif .maintain then "maintain" elif .push then "push" else "none" end')"
    case "$permission" in
        admin|maintain) ;;
        *) err "maintainer access required on $repo (effective permission: $permission)"; exit 5 ;;
    esac
}
check_maintainer_access "$REPO_NUMAN"
check_maintainer_access "$REPO_PLUGINS"
check_maintainer_access "$REPO_REGISTRY"

# --- 1. Drift pass on the local copy of the consolidated roadmap ---------
if [ ! -f "$DRIFT_SCRIPT" ]; then
    err "drift script not found at $DRIFT_SCRIPT; refusing to bump without sentinel validation"
    exit 3
fi
log "running scripts/check-roadmap-drift.py against the local consolidated roadmap"
if ! python3 "$DRIFT_SCRIPT"; then
    err "drift check failed — fix the consolidated roadmap before bumping"
    exit 3
fi

# --- 2. Freeze content SHA (roadmap + drift script already on tip) -------
CONTENT_SHA="$(git rev-parse HEAD)"
log "content SHA (freeze target) = $CONTENT_SHA"
log "new contract TAG = $NEW_TAG"

require_yml_has_pins() {
    local yml="$1"
    grep -E '^[[:space:]]*CONTRACT_TAG:[[:space:]]*' "$yml" >/dev/null || {
        err "$yml is missing CONTRACT_TAG pin; refuse"
        return 1
    }
    grep -E '^[[:space:]]*CONTRACT_SHA:[[:space:]]*[0-9a-f]{40,}' "$yml" >/dev/null || {
        err "$yml is missing CONTRACT_SHA pin; refuse"
        return 1
    }
}
require_yml_has_pins "$NUMAN_CI"
require_yml_has_pins "$PLUGINS_YML"
require_yml_has_pins "$REGISTRY_YML"

# --- 3. Rewrite pins to NEW_TAG + CONTENT_SHA ---------------------------
rewrite_yml_pins() {
    local yml="$1"
    local tmp
    tmp="$(mktemp)"
    python3 - "$yml" "$NEW_TAG" "$CONTENT_SHA" "$tmp" <<'PY'
import re, sys
path, tag, sha, out = sys.argv[1:5]
text = open(path, encoding="utf-8").read()
text2, n_tag = re.subn(
    r"(^[ \t]*CONTRACT_TAG:[ \t]*).*$",
    rf"\g<1>{tag}",
    text,
    count=1,
    flags=re.M,
)
text2, n_sha = re.subn(
    r"(^[ \t]*CONTRACT_SHA:[ \t]*)[0-9a-fA-F]{40,}",
    rf"\g<1>{sha}",
    text2,
    count=1,
    flags=re.M,
)
if n_tag != 1 or n_sha != 1:
    raise SystemExit(f"{path}: failed to rewrite pins (tag={n_tag}, sha={n_sha})")
open(out, "w", encoding="utf-8").write(text2)
PY
    mv "$tmp" "$yml"
    local pinned_tag
    pinned_tag="$(grep -E '^[[:space:]]*CONTRACT_TAG:[[:space:]]*' "$yml" | head -1 | awk '{print $2}')"
    if [ "$pinned_tag" != "$NEW_TAG" ]; then
        err "$yml CONTRACT_TAG is '$pinned_tag' but NEW_TAG is '$NEW_TAG'"
        return 1
    fi
    local pinned_sha
    pinned_sha="$(grep -E '^[[:space:]]*CONTRACT_SHA:[[:space:]]*[0-9a-f]{40,}' "$yml" | head -1 | awk '{print $2}')"
    if [ "$pinned_sha" != "$CONTENT_SHA" ]; then
        err "$yml CONTRACT_SHA is '$pinned_sha' but CONTENT_SHA is '$CONTENT_SHA'"
        return 1
    fi
}

rewrite_yml_pins "$NUMAN_CI"
rewrite_yml_pins "$PLUGINS_YML"
rewrite_yml_pins "$REGISTRY_YML"

log "rewriting CONTRACT_SHA mentions in cross-repo-mirror/README.md"
python3 - "$MIRROR_README" "$CONTENT_SHA" <<'PY'
import re, sys
path, sha = sys.argv[1], sys.argv[2]
text = open(path, encoding="utf-8").read()
text2, n = re.subn(r"\b[0-9a-f]{40}\b", sha, text)
if n < 1:
    raise SystemExit(f"{path}: no 40-char SHA pins found to rewrite")
open(path, "w", encoding="utf-8").write(text2)
PY

rewrite_roadmap_blob_urls() {
    local md="$1"
    python3 - "$md" "$CONTENT_SHA" <<'PY'
import re, sys
path, sha = sys.argv[1], sys.argv[2]
text = open(path, encoding="utf-8").read()
text2, n = re.subn(
    r"https://github\.com/tonythethompson/numan/blob/(?:numan-roadmap-contract/v\d+|[0-9a-f]{7,40})/",
    f"https://github.com/tonythethompson/numan/blob/{sha}/",
    text,
)
if n < 1:
    raise SystemExit(f"{path}: no numan blob URLs found to rewrite")
open(path, "w", encoding="utf-8").write(text2)
PY
}
rewrite_roadmap_blob_urls "$PLUGINS_ROADMAP"
rewrite_roadmap_blob_urls "$REGISTRY_ROADMAP"

# --- 4. Refresh contract doc, then commit pins before tagging -----------
if [ -f "$CONTRACT_DOC_DIR/roadmap-v${VERSION_LABEL}.md" ]; then
    log "contract doc already exists at docs/contracts/roadmap-v${VERSION_LABEL}.md — leaving as-is"
else
    log "creating contract doc from v1 template"
    sed "s/^v1$/v${VERSION_LABEL}/g; s/Roadmap Contract v1/Roadmap Contract v${VERSION_LABEL}/g" \
        "$CONTRACT_DOC_DIR/roadmap-v1.md" \
        > "$CONTRACT_DOC_DIR/roadmap-v${VERSION_LABEL}.md"
fi

log "committing pin rewrite (tag will point at content SHA $CONTENT_SHA)"
if [ "$DRY_RUN" -eq 1 ]; then
    warn "BUMP_DRY_RUN=1 set; skipping git commit/tag/push/PR"
else
    git add \
        "$NUMAN_CI" "$PLUGINS_YML" "$REGISTRY_YML" \
        "$MIRROR_README" "$PLUGINS_ROADMAP" "$REGISTRY_ROADMAP" \
        "$CONTRACT_DOC_DIR/roadmap-v${VERSION_LABEL}.md"
    git commit -m "chore: pin roadmap contract to ${NEW_TAG}

Freeze content at ${CONTENT_SHA}.
Reason: ${REASON}"
fi

git fetch origin --tags --prune 2>/dev/null || warn "tag fetch failed (offline?)"

if git rev-parse "$NEW_TAG" >/dev/null 2>&1; then
    err "tag $NEW_TAG already exists locally — refusing to clobber"
    exit 4
fi

if [ "$DRY_RUN" -eq 0 ]; then
    log "creating annotated tag $NEW_TAG at content SHA $CONTENT_SHA (local only)"
    git tag -a "$NEW_TAG" "$CONTENT_SHA" -m "Roadmap contract v${VERSION_LABEL}.

Frozen at commit: ${CONTENT_SHA}
Reason: ${REASON}

See docs/contracts/roadmap-v${VERSION_LABEL}.md for what this version freezes."

    log "backing up old tag $OLD_TAG -> $OLD_TAG.bak"
    if git rev-parse "$OLD_TAG" >/dev/null 2>&1; then
        git tag "$OLD_TAG.bak" "$OLD_TAG" 2>/dev/null || true
    fi
fi

# --- 5. Push numan branch -----------------------------------------------
gh_auth_check() {
    gh auth status >/dev/null 2>&1 || { err "gh not authenticated"; exit 5; }
}
gh_auth_check

if [ "$DRY_RUN" -eq 0 ]; then
    log "pushing branch $BRANCH_NAME (HEAD) to numan"
    git push origin "HEAD:refs/heads/$BRANCH_NAME"
    log "NOT pushing $NEW_TAG yet — publish with: git push origin refs/tags/$NEW_TAG after the numan PR merges"
fi

# --- 6. Materialize sibling branches, then open PRs ---------------------
issue_pr() {
    local repo="$1" branch="$2" title="$3" body="$4"
    log "opening PR on $repo branch=$branch: $title"
    if [ "$DRY_RUN" -eq 1 ]; then
        echo "DRY-RUN: gh pr create --repo $repo --head $branch --title $title --body <...>"
        return 0
    fi
    gh pr create \
        --repo "$repo" \
        --head "$branch" \
        --title "$title" \
        --body "$body" \
        --label "roadmap-contract-bump" \
        >/dev/null
}

materialize_sibling() {
    local repo="$1" sibling_dir="$2"
    local work
    work="$(mktemp -d)"
    log "materializing $repo branch $BRANCH_NAME from $sibling_dir"
    if [ "$DRY_RUN" -eq 1 ]; then
        echo "DRY-RUN: would clone $repo, copy workflow+roadmap, commit, push $BRANCH_NAME"
        rm -rf "$work"
        return 0
    fi
    gh repo clone "$repo" "$work" -- --depth 1
    (
        cd "$work"
        git checkout -B "$BRANCH_NAME"
        mkdir -p .github/workflows docs
        cp "$REPO_ROOT/cross-repo-mirror/$sibling_dir/.github/workflows/roadmap-drift.yml" \
            .github/workflows/roadmap-drift.yml
        cp "$REPO_ROOT/cross-repo-mirror/$sibling_dir/docs/roadmap.md" \
            docs/roadmap.md
        git add .github/workflows/roadmap-drift.yml docs/roadmap.md
        git commit -m "chore: pin roadmap contract to ${NEW_TAG}

Freeze content at ${CONTENT_SHA}.
Reason: ${REASON}"
        git push -u origin "HEAD:refs/heads/$BRANCH_NAME"
    )
    rm -rf "$work"
}

if [ "$DRY_RUN" -eq 1 ]; then
    warn "BUMP_DRY_RUN=1 set; PRs will be planned but not opened"
fi

PR_BODY="Bumps the cross-repo roadmap contract to ${NEW_TAG}.

Reason: ${REASON}

This PR is part of a coordinated set across numan, numan-plugins, and
numan-registry. Merge order matters:

  1. PR on numan — publishes the new tag once merged (\`git push origin refs/tags/${NEW_TAG}\`).
  2. PRs on numan-plugins + numan-registry — re-pin to CONTRACT_SHA=${CONTENT_SHA}.

Do not merge any sibling PR until numan's tag is live on origin and
resolves to ${CONTENT_SHA}."

issue_pr "$REPO_NUMAN" "$BRANCH_NAME" \
    "Bump roadmap contract to ${NEW_TAG}: ${REASON}" "$PR_BODY"

materialize_sibling "$REPO_PLUGINS" "numan-plugins"
materialize_sibling "$REPO_REGISTRY" "numan-registry"

issue_pr "$REPO_PLUGINS" "$BRANCH_NAME" \
    "Bump roadmap contract to ${NEW_TAG}: ${REASON}" "$PR_BODY"
issue_pr "$REPO_REGISTRY" "$BRANCH_NAME" \
    "Bump roadmap contract to ${NEW_TAG}: ${REASON}" "$PR_BODY"

log "bump complete: ${OLD_TAG} -> ${NEW_TAG} (tag local until post-merge push)"
log "After the numan PR merges, publish the tag: git push origin refs/tags/${NEW_TAG}"
log "Then merge sibling PRs once CI can resolve the new pin."
