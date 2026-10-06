#!/usr/bin/env bash
# Keep one public entrypoint for command/generation custody on macOS and Linux.
# The sibling flock guard is permanent; the admission file is temporary.
set -euo pipefail

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
exec python3 "${script_dir}/validation-lock.py" "$@"
