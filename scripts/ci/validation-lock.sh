#!/usr/bin/env bash
# The Python owner supplies FIFO admission, authenticated nesting and custody.
# --with-child-scopes opts one root into the immutable v3 ordinary-child API.
set -euo pipefail
exec python3 "$(dirname "${BASH_SOURCE[0]}")/validation-lock.py" "$@"
