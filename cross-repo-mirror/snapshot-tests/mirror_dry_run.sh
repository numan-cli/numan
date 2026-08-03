#!/usr/bin/env bash
# Local sanity check that each sibling-repo CI job will pass against the
# current state of numan/scripts/check-roadmap-drift.py and the
# consolidated roadmap. Mimics what each .github/workflows/roadmap-drift.yml
# step does, but entirely offline by using the local files.
#
# Usage:    bash cross-repo-mirror/snapshot-tests/mirror_dry_run.sh
# Expected: exit 0 with "pass" line for both numan-plugins and numan-registry.

set -euo pipefail

ROOT=$(cd "$(dirname "$0")/../.." && pwd)
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

for sibling in numan-plugins numan-registry; do
    echo "==> mirror dry-run for $sibling"
    # Map the sibling name to the corresponding repo-local roadmap content.
    local_roadmap="$ROOT/cross-repo-mirror/$sibling/docs/roadmap.md"
    test -f "$local_roadmap" \
        || { echo "FAIL: missing $local_roadmap"; exit 2; }

    # Stub ci.sh here: stage the consolidated roadmap, the local roadmap, and
    # the script into the temp dir the way the GitHub workflow will.
    cp "$ROOT/docs/plans/consolidated-multi-repo-roadmap.md" \
       "$TMP/consolidated.md"
    cp "$local_roadmap" "$TMP/local.md"
    cp "$ROOT/scripts/check-roadmap-drift.py" "$TMP/check.py"

    # Capture the status explicitly: under `set -e` a bare invocation would
    # abort the script before we could report the failing exit code.
    if CONSOLIDATED_ROADMAP="$TMP/consolidated.md" \
        LOCAL_ROADMAP="$TMP/local.md" \
        python "$TMP/check.py"; then
        echo "    pass: drift check exits 0"
    else
        rc=$?
        echo "FAIL: drift check exited $rc"
        exit "$rc"
    fi
    echo
done

echo "ALL MIRRORS PASS"
