#!/usr/bin/env bash
# Run upgrade custody from the explicitly selected trusted template checkout.
set -euo pipefail

script_dir=$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
exec env -u PYTHONPATH -u PYTHONHOME PYTHONDONTWRITEBYTECODE=1 python3 "${script_dir}/lib/template_upgrade.py" "$@"
