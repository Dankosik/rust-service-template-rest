#!/usr/bin/env bash
set -euo pipefail
cd /root/profiling
python3 scripts/run.py webhook-pool16-2k --workload webhook_mix --rate 2000 --pool 16
python3 scripts/run.py profile-webhook-200 --binary profile --workload webhook_mix --rate 200 \
  --log-level 'info,sqlx::query=debug'
python3 scripts/run.py profile-webhook-large-50 --binary profile --workload webhook_new --rate 50 \
  --body-bytes 65536 --log-level 'info,sqlx::query=debug'
bash scripts/additional.sh
