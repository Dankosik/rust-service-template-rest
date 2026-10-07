#!/usr/bin/env bash
# Opt-in owned broker sessions. This runner never builds, pulls or prunes.
set -euo pipefail
ROOT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
cd "${ROOT_DIR}"
case "${1:-}" in
    demo|rehearse|measure)
        if ! bash scripts/ci/validation-lock.sh --assert-held; then
            exec bash scripts/ci/validation-lock.sh -- bash "$0" "$@"
        fi
        ;;
esac
exec python3 scripts/ci/messaging-recovery.py "$@"
