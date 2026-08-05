#!/usr/bin/env bash
# Manually push packaging/homebrew/numan.rb to tonythethompson/homebrew-numan.
# Prefer the Publish to Homebrew tap workflow once HOMEBREW_TAP_TOKEN is set
# on the numan repo.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
FORMULA_SRC="${ROOT}/packaging/homebrew/numan.rb"
TAP_REPO="${TAP_REPO:-tonythethompson/homebrew-numan}"
WORKDIR="${TMPDIR:-/tmp}/homebrew-numan-sync-$$"

if ! command -v gh >/dev/null 2>&1; then
  echo "error: gh (GitHub CLI) is not installed or not on PATH; see https://cli.github.com/" >&2
  exit 1
fi

if ! command -v python3 >/dev/null 2>&1; then
  echo "error: python3 is not installed or not on PATH" >&2
  exit 1
fi

if [[ ! -f "${FORMULA_SRC}" ]]; then
  echo "missing ${FORMULA_SRC}; run scripts/render_homebrew_formula.py --write first" >&2
  exit 1
fi

cleanup() { rm -rf "${WORKDIR}"; }
trap cleanup EXIT

gh repo clone "${TAP_REPO}" "${WORKDIR}"
mkdir -p "${WORKDIR}/Formula"
cp "${FORMULA_SRC}" "${WORKDIR}/Formula/numan.rb"
VERSION="$(
  python3 -c "import re,sys; t=open(sys.argv[1],encoding='utf-8').read(); m=re.search(r'version\\s+\"([^\"]+)\"', t); print(m.group(1) if m else 'unknown')" \
    "${FORMULA_SRC}"
)"
cd "${WORKDIR}"
git config user.name "${GIT_AUTHOR_NAME:-numan-maintainer}"
git config user.email "${GIT_AUTHOR_EMAIL:-github@trackdub.com}"
git add Formula/numan.rb
if git diff --cached --quiet; then
  echo "Tap already has the same formula; nothing to commit"
  exit 0
fi
git commit -m "numan ${VERSION}"
git push origin HEAD
echo "Pushed ${TAP_REPO} Formula/numan.rb (${VERSION})"
