#!/usr/bin/env bash
#
# Safely update the built-in official-registry trust root in
# src/core/official_registry.rs (key_id, public_key_b64, production_url).
#
# This script only ever handles PUBLIC material — a key id, a public key,
# and a URL. It never reads, writes, or asks for a private key. It edits
# the source file and runs the relevant tests; it does NOT commit, push,
# or open a PR. Review the diff yourself before committing.
#
# Usage:
#   ./scripts/update-official-trust-root.sh --key-id ID --public-key-b64 B64 [--url URL] [--force]
#   ./scripts/update-official-trust-root.sh --from-pub-json PATH [--url URL] [--force]
#
# --from-pub-json takes a JSON file shaped like numan-registry's
# keys/official.pub ({"key_id": "...", "public_key_b64": "..."}) so you can
# point it straight at that file instead of retyping values by hand.
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TARGET_FILE="${REPO_ROOT}/src/core/official_registry.rs"

KEY_ID=""
PUBLIC_KEY_B64=""
FROM_PUB_JSON=""
URL=""
FORCE=0
RUN_TESTS=1

usage() {
  cat <<'EOF'
Usage:
  update-official-trust-root.sh --key-id ID --public-key-b64 B64 [--url URL] [--force]
  update-official-trust-root.sh --from-pub-json PATH [--url URL] [--force]

Options:
  --key-id ID            The key_id to set (e.g. official-2026-07-01).
  --public-key-b64 B64    The base64 Ed25519 public key to set.
  --from-pub-json PATH    Read key_id and public_key_b64 from a JSON file
                          shaped like numan-registry's keys/official.pub.
  --url URL               Set production_url (default: keep current value).
  --force                 Allow replacing an already non-placeholder trust
                           root (needed for legitimate key rotation).
  --no-test                Skip running `cargo test official_registry`.
  -h, --help              Show this help.
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --key-id) KEY_ID="$2"; shift 2 ;;
    --public-key-b64) PUBLIC_KEY_B64="$2"; shift 2 ;;
    --from-pub-json) FROM_PUB_JSON="$2"; shift 2 ;;
    --url) URL="$2"; shift 2 ;;
    --force) FORCE=1; shift ;;
    --no-test) RUN_TESTS=0; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "Unknown argument: $1" >&2; usage >&2; exit 2 ;;
  esac
done

if [[ ! -f "${TARGET_FILE}" ]]; then
  echo "FAIL: ${TARGET_FILE} does not exist." >&2
  exit 1
fi

if [[ -n "${FROM_PUB_JSON}" ]]; then
  if [[ -n "${KEY_ID}" || -n "${PUBLIC_KEY_B64}" ]]; then
    echo "FAIL: --from-pub-json cannot be combined with --key-id/--public-key-b64" >&2
    exit 2
  fi
  if [[ ! -f "${FROM_PUB_JSON}" ]]; then
    echo "FAIL: ${FROM_PUB_JSON} does not exist." >&2
    exit 1
  fi
  KEY_ID="$(python3 -c "import json,sys; print(json.load(open(sys.argv[1]))['key_id'])" "${FROM_PUB_JSON}")"
  PUBLIC_KEY_B64="$(python3 -c "import json,sys; print(json.load(open(sys.argv[1]))['public_key_b64'])" "${FROM_PUB_JSON}")"
  echo "Loaded from ${FROM_PUB_JSON}: key_id=${KEY_ID}"
fi

if [[ -z "${KEY_ID}" || -z "${PUBLIC_KEY_B64}" ]]; then
  echo "FAIL: --key-id and --public-key-b64 are required (directly or via --from-pub-json)" >&2
  usage >&2
  exit 2
fi

if [[ "${KEY_ID}" == "official-placeholder" || "${PUBLIC_KEY_B64}" == "PLACEHOLDER" ]]; then
  echo "FAIL: refusing to set the trust root to placeholder values via this script." >&2
  echo "      Edit ${TARGET_FILE} by hand if you really intend to revert to a placeholder." >&2
  exit 1
fi

if ! [[ "${KEY_ID}" =~ ^official-[0-9]{4}-[0-9]{2}-[0-9]{2}$ ]]; then
  echo "WARN: key_id '${KEY_ID}' does not match the official-YYYY-MM-01 convention." >&2
  echo "      Continuing, but double-check this is intentional." >&2
fi

python3 - "${PUBLIC_KEY_B64}" <<'PY'
import base64
import sys

value = sys.argv[1]
try:
    raw = base64.b64decode(value, validate=True)
except Exception as exc:
    print(f"FAIL: public key is not valid base64: {exc}")
    sys.exit(1)
if len(raw) != 32:
    print(f"FAIL: public key must decode to 32 bytes, got {len(raw)}")
    sys.exit(1)
PY

if [[ -n "${URL}" ]] && ! [[ "${URL}" =~ ^https:// ]]; then
  echo "FAIL: --url must start with https://" >&2
  exit 1
fi

read -r CURRENT_KEY_ID CURRENT_PUBLIC_KEY < <(
  python3 - "${TARGET_FILE}" <<'PY'
import re
import sys

text = open(sys.argv[1], "r", encoding="utf-8").read()
key_id = re.search(r'key_id:\s*"([^"]+)"', text).group(1)
pub = re.search(r'public_key_b64:\s*"([^"]+)"', text).group(1)
print(key_id, pub)
PY
)

if [[ "${CURRENT_KEY_ID}" != "official-placeholder" && "${CURRENT_PUBLIC_KEY}" != "PLACEHOLDER" && "${FORCE}" -ne 1 ]]; then
  echo "FAIL: ${TARGET_FILE} already has a non-placeholder trust root (key_id=${CURRENT_KEY_ID})." >&2
  echo "       Re-run with --force if this is a deliberate key rotation." >&2
  exit 1
fi

echo "=============================================================="
echo " Updating built-in official-registry trust root"
echo "=============================================================="
echo "This only edits PUBLIC material (key id, public key, URL)."
echo "It never reads or writes a private key."
echo "=============================================================="
echo

python3 - "${TARGET_FILE}" "${KEY_ID}" "${PUBLIC_KEY_B64}" "${URL}" <<'PY'
import re
import sys

target_path, key_id, public_key_b64, url = sys.argv[1:5]

with open(target_path, "r", encoding="utf-8") as f:
    text = f.read()

block_re = re.compile(
    r'(pub const OFFICIAL_REGISTRY: OfficialRegistry = OfficialRegistry \{.*?\};\n)',
    re.DOTALL,
)
match = block_re.search(text)
if not match:
    print("FAIL: could not locate the OFFICIAL_REGISTRY const block")
    sys.exit(1)

block = match.group(1)
new_block = block

if url:
    new_block = re.sub(
        r'production_url: "[^"]*"',
        lambda _: f'production_url: "{url}"',
        new_block,
        count=1,
    )

new_block = re.sub(
    r'key_id: "[^"]*"',
    lambda _: f'key_id: "{key_id}"',
    new_block,
    count=1,
)
new_block = re.sub(
    r'public_key_b64: "[^"]*"',
    lambda _: f'public_key_b64: "{public_key_b64}"',
    new_block,
    count=1,
)

text = text[: match.start()] + new_block + text[match.end() :]

with open(target_path, "w", encoding="utf-8") as f:
    f.write(text)

print("OK: OFFICIAL_REGISTRY const updated")
PY

echo
echo "--- diff (review before committing) ---"
if command -v git >/dev/null 2>&1 && git -C "${REPO_ROOT}" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  git -C "${REPO_ROOT}" --no-pager diff -- "${TARGET_FILE}"
else
  cat "${TARGET_FILE}"
fi

if [[ "${RUN_TESTS}" -eq 1 ]]; then
  if command -v cargo >/dev/null 2>&1; then
    echo
    echo "--- cargo test official_registry ---"
    (cd "${REPO_ROOT}" && cargo test official_registry)
  else
    echo
    echo "WARN: cargo not found. Run 'cargo test official_registry' manually before committing." >&2
  fi
fi

echo
echo "Next steps (this script did NOT commit, push, or open a PR):"
echo "  1. Review the diff above."
echo "  2. git -C \"${REPO_ROOT}\" add src/core/official_registry.rs"
echo "  3. git -C \"${REPO_ROOT}\" commit -m 'Set production official-registry trust root'"
echo "  4. Push and open a PR against numan-cli/numan, separate from PR #23,"
echo "     per docs/production-cutover-checklist.md Step F in numan-registry."
