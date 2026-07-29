#!/usr/bin/env bash
#
# Emit GitHub Release notes for TAG from CHANGELOG.md (Keep a Changelog format).
# Does not append "Full Changelog: ..." compare links — those stay in CHANGELOG only.
#
# Usage:
#   ./scripts/release-notes-from-changelog.sh vX.Y.Z
#   ./scripts/release-notes-from-changelog.sh X.Y.Z
#
set -euo pipefail

tag="${1:-}"
if [[ -z "$tag" ]]; then
  echo "usage: release-notes-from-changelog.sh <tag>" >&2
  exit 1
fi

version="${tag#v}"
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
changelog="${CHANGELOG_PATH:-${repo_root}/CHANGELOG.md}"

if [[ ! -f "$changelog" ]]; then
  echo "changelog not found: $changelog" >&2
  exit 1
fi

tmp="$(mktemp)"
trap 'rm -f "$tmp"' EXIT

awk -v ver="$version" '
  /^## \[/ {
    if (emit) exit
    if ($0 ~ "^## \\[" ver "\\]") {
      emit = 1
      print
      next
    }
    next
  }
  emit { print }
' "$changelog" >"$tmp"

if [[ ! -s "$tmp" ]]; then
  echo "no CHANGELOG section found for version ${version} (expected '## [${version}]')" >&2
  exit 1
fi

cat "$tmp"
