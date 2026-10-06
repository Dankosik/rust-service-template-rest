#!/usr/bin/env bash
# The Python owner supplies FIFO admission, authenticated nesting and custody.
set -euo pipefail
exec python3 "$(dirname "${BASH_SOURCE[0]}")/validation-lock.py" "$@"
