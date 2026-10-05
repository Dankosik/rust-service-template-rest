#!/usr/bin/env bash
set -euo pipefail
cd /root/profiling
python3 scripts/run.py p2-mixed --binary profile2 --generator wrk2 --rate 8000 --duration 45 --cpu-snapshot
python3 scripts/run.py p2-webhook-small --binary profile2 --workload webhook_new --rate 100 --duration 45 \
  --log-level 'info,sqlx::query=debug' --cpu-snapshot
python3 scripts/run.py p2-webhook-large --binary profile2 --workload webhook_new --rate 100 --duration 45 \
  --body-bytes 65536 --log-level 'info,sqlx::query=debug' --cpu-snapshot
python3 scripts/run.py p2-alloc-counts --binary profile2 --workload webhook_new --rate 100 \
  --body-bytes 65536 --log-level 'info,sqlx::query=debug' --alloc-metric count
python3 scripts/summarize.py > evidence/summary.txt
