#!/usr/bin/env bash
# Synchronize the portable, committed template surface without executing a
# derived service's Makefile or helpers.
set -euo pipefail

script_dir=$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
exec env PYTHONDONTWRITEBYTECODE=1 python3 "${script_dir}/lib/template_sync.py" "$@"
