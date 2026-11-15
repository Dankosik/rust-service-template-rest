#!/usr/bin/env bash
set -euo pipefail

script_dir=$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
exec env -u PYTHONPATH -u PYTHONHOME PYTHONDONTWRITEBYTECODE=1 python3 -B -E "${script_dir}/lib/template_init.py" "$@"
