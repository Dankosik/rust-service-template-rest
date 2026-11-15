#!/usr/bin/env bash
set -euo pipefail

script_dir=$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
exec env PYTHONDONTWRITEBYTECODE=1 python3 "${script_dir}/lib/template_init.py" "$@"
