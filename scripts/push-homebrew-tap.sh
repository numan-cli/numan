#!/usr/bin/env bash
# Manually push packaging/homebrew/numan.rb to tonythethompson/homebrew-numan.
# Prefer the Publish to Homebrew tap workflow once HOMEBREW_TAP_TOKEN is set.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
FORMULA_SRC="${ROOT}/packaging/homebrew/numan.rb"
TAP_REPO="${TAP_REPO:-tonythethompson/homebrew-numan}"
WORKDIR="${TMPDIR:-/tmp}/homebrew-numan-sync-$$"

if [[ ! -f "${FORMULA_SRC}" ]]; then
  echo "missing ${FORMULA_SRC}; run scripts/render_homebrew_formula.py --write first" >&2
  exit 1
fi

cleanup() { rm -rf "${WORKDIR}"; }
trap cleanup EXIT

gh repo clone "${TAP_REPO}" "${WORKDIR}"
mkdir -p "${WORKDIR}/Formula"
cp "${FORMULA_SRC}" "${WORKDIR}/Formula/numan.rb"
VERSION="$(python3 - <<'PY' "${FORMULA_SRC}"
import re, sys
text = open(sys.argv[1], encoding="utf-8").read()
m = re.search(r'version\s+"([^"]+)"', text)
print(m.group(1) if m else "unknown")
PY
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
