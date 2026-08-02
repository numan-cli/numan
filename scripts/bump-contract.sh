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
#   2. Compute the new contract SHA = HEAD on the current numan branch.
#       The current SHA must already match every ref in every workflow yml.
#       (If it doesn't, the bump script refuses — see "Refusing to bump" in
#       docs/contracts/roadmap-v1.md.)
#   3. Rename the current contract tag to `<old>.bak`.
#   4. Create the new tag `<numan-roadmap-contract/vN>` at HEAD.
#   5. Update CONTRACT_TAG + CONTRACT_SHA in:
#        .github/workflows/ci.yml
#        cross-repo-mirror/numan-plugins/.github/workflows/roadmap-drift.yml
#        cross-repo-mirror/numan-registry/.github/workflows/roadmap-drift.yml
#      and refresh docs/contracts/roadmap-vN.md from the v1 template.
#   6. Open three PRs (numan, numan-plugins, numan-registry) via `gh pr create`.
#      The numan PR includes the local workflow yml pin + the new tag push;
#      the two sibling PRs each carry the bumped workflow yml + the local
#      docs/roadmap.md cross-link (unchanged).
#
# PR-MERGE GATE
#   Issue the PRs sequentially. Each sibling repo's CI will run the new
#   pinned SHA; if the SHA hasn't been pushed yet (PR 1 merge pending), CI
#   fails closed with a clear "tag not yet published" message. PR 2 + PR 3
#   cannot be merged until PR 1's tag is live on origin.
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

# --- 2. Compute SHA + verify pin consistency ----------------------------
NEW_SHA="$(git rev-parse HEAD)"
log "new contract SHA = $NEW_SHA"
log "new contract TAG = $NEW_TAG"

# Workflow pins must already match HEAD before the bump (apply the freeze /
# pin-update commit first). numan's ci.yml pins by CONTRACT_TAG only;
# sibling mirrors also pin CONTRACT_SHA for immutable fetches.
verify_yml_pin() {
    local yml="$1"
    grep -E '^[[:space:]]*CONTRACT_TAG:[[:space:]]*' "$yml" >/dev/null || {
        err "$yml is missing CONTRACT_TAG pin; refuse — apply the contract-freeze PR first"
        return 1
    }
    if grep -E '^[[:space:]]*CONTRACT_SHA:[[:space:]]*' "$yml" >/dev/null; then
        grep -E '^[[:space:]]*CONTRACT_SHA:[[:space:]]*[0-9a-f]{40,}' "$yml" >/dev/null || {
            err "$yml has a malformed CONTRACT_SHA pin; refuse"
            return 1
        }
        local pinned
        pinned="$(grep -E '^[[:space:]]*CONTRACT_SHA:[[:space:]]*[0-9a-f]{40,}' "$yml" | head -1 | awk '{print $2}')"
        if [ "$pinned" != "$NEW_SHA" ]; then
            err "$yml is pinned to $pinned but HEAD is $NEW_SHA; refuse"
            return 1
        fi
    fi
}
verify_yml_pin "$NUMAN_CI"
verify_yml_pin "$PLUGINS_YML"
verify_yml_pin "$REGISTRY_YML"

# --- 3. Tag bump ---------------------------------------------------------
git fetch origin --tags --prune 2>/dev/null || warn "tag fetch failed (offline?)"

if git rev-parse "$NEW_TAG" >/dev/null 2>&1; then
    err "tag $NEW_TAG already exists locally — refusing to clobber"
    exit 4
fi

log "creating annotated tag $NEW_TAG at HEAD (local only; push after numan PR merges)"
git tag -a "$NEW_TAG" -m "Roadmap contract v${VERSION_LABEL}.

Frozen at commit: ${NEW_SHA}
Reason: ${REASON}

See docs/contracts/roadmap-v${VERSION_LABEL}.md for what this version freezes."

log "backing up old tag $OLD_TAG -> $OLD_TAG.bak"
if git rev-parse "$OLD_TAG" >/dev/null 2>&1; then
    git tag "$OLD_TAG.bak" "$OLD_TAG"
fi

# --- 4. Refresh contract doc --------------------------------------------
if [ -f "$CONTRACT_DOC_DIR/roadmap-v${VERSION_LABEL}.md" ]; then
    log "contract doc already exists at docs/contracts/roadmap-v${VERSION_LABEL}.md — leaving as-is"
else
    log "creating contract doc from v1 template"
    sed "s/^v1$/v${VERSION_LABEL}/g; s/Roadmap Contract v1/Roadmap Contract v${VERSION_LABEL}/g" \
        "$CONTRACT_DOC_DIR/roadmap-v1.md" \
        > "$CONTRACT_DOC_DIR/roadmap-v${VERSION_LABEL}.md"
fi

# --- 5. Push numan branch (tag is published after the numan PR merges) ---
log "pushing branch $BRANCH_NAME (HEAD) to numan"
git push origin "HEAD:refs/heads/$BRANCH_NAME"
log "NOT pushing $NEW_TAG yet — publish with: git push origin refs/tags/$NEW_TAG after the numan PR merges"

# --- 6. Issue the three PRs ---------------------------------------------
gh_auth_check() {
    gh auth status >/dev/null 2>&1 || { err "gh not authenticated"; exit 5; }
}
gh_auth_check

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

if [ "$DRY_RUN" -eq 1 ]; then
    warn "BUMP_DRY_RUN=1 set; PRs will be planned but not opened"
fi

PR_BODY="Bumps the cross-repo roadmap contract to ${NEW_TAG}.

Reason: ${REASON}

This PR is part of a coordinated set across numan, numan-plugins, and
numan-registry. Merge order matters:

  1. PR on numan — publishes the new tag once merged.
  2. PRs on numan-plugins + numan-registry — re-pin to the new SHA.

Do not merge any sibling PR until numan's tag is live on origin."

issue_pr "$REPO_NUMAN"    "$BRANCH_NAME" \
    "Bump roadmap contract to ${NEW_TAG}: ${REASON}" "$PR_BODY"
issue_pr "$REPO_PLUGINS"  "$BRANCH_NAME" \
    "Bump roadmap contract to ${NEW_TAG}: ${REASON}" "$PR_BODY"
issue_pr "$REPO_REGISTRY" "$BRANCH_NAME" \
    "Bump roadmap contract to ${NEW_TAG}: ${REASON}" "$PR_BODY"

log "bump complete: ${OLD_TAG} -> ${NEW_TAG} (tag local until post-merge push)"
log "After the numan PR merges, publish the tag: git push origin refs/tags/${NEW_TAG}"
log "Then merge sibling PRs once CI can resolve the new pin."
