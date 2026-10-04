#!/usr/bin/env bash
# Remote-only, serial measurements. Never run concurrently with builds/tests.
set -euo pipefail
cd /root/profiling
python3 scripts/run.py pilot-profile-fixed --binary profile --rate 2000 --duration 20
python3 scripts/run.py pilot-wrk2 --generator wrk2 --rate 8000 --duration 30
for round in 1 2 3; do
  python3 scripts/run.py mixed-8k-r"$round" --generator wrk2 --rate 8000
  python3 scripts/run.py mixed-16k-r"$round" --generator wrk2 --rate 16000
  python3 scripts/run.py logging-8k-r"$round" --generator wrk2 --rate 8000 --log-health
  python3 scripts/run.py no-sampling-8k-r"$round" --generator wrk2 --rate 8000 --sampler always_off
  python3 scripts/run.py webhook-800-r"$round" --workload webhook_mix --rate 800
  python3 scripts/run.py webhook-2000-r"$round" --workload webhook_mix --rate 2000
done
python3 scripts/run.py feature-off-8k --binary feature-off --generator wrk2 --rate 8000
python3 scripts/run.py profile-8k --binary profile --generator wrk2 --rate 8000 --cpu-snapshot
python3 scripts/run.py mixed-24k --generator wrk2 --rate 24000
python3 scripts/run.py diagnostics-8k --rate 8000 --scrape
python3 scripts/run.py webhook-4000 --workload webhook_mix --rate 4000
python3 scripts/run.py webhook-duplicate-2k --workload webhook_duplicate --rate 2000
python3 scripts/run.py webhook-large-100 --workload webhook_new --rate 100 --body-bytes 65536
python3 scripts/run.py webhook-pool16-2k --workload webhook_mix --rate 2000 --pool 16
python3 scripts/run.py profile-webhook-200 --binary profile --workload webhook_mix --rate 200 \
  --log-level 'info,sqlx::query=debug'
python3 scripts/run.py profile-webhook-large-50 --binary profile --workload webhook_new --rate 50 \
  --body-bytes 65536 --log-level 'info,sqlx::query=debug'
python3 scripts/summarize.py > evidence/summary.txt
date -u --iso-8601=seconds > evidence/campaign-completed.txt
