#!/usr/bin/env bash
# Opt-in owned broker sessions. This runner never builds, pulls or prunes.
set -euo pipefail
ROOT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
cd "${ROOT_DIR}"
exec python3 scripts/ci/messaging-recovery.py "$@"
