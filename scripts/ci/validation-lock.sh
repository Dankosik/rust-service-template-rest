#!/usr/bin/env bash
# The Python owner supplies FIFO admission, authenticated nesting and custody.
# --with-retained-compose opts one root into the immutable v3 stop-only Compose capability.
set -euo pipefail
exec python3 "$(dirname "${BASH_SOURCE[0]}")/validation-lock.py" "$@"
