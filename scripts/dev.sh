#!/usr/bin/env bash
# Run the numan CLI locally with the Infisical `dev` environment injected.
#
# Usage:
#   scripts/dev.sh list
#   scripts/dev.sh install owner/name
#
# Requires the Infisical CLI, one `infisical login`, and `infisical init` in this
# repository. See ../INFISICAL.md (workspace root) for the full setup.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

exec infisical run --env=dev -- cargo run -- "$@"
