#!/usr/bin/env bash
# Continue setup after the release binaries and PostgreSQL container exist.
set -euo pipefail
until docker exec profiling-postgres pg_isready -h 127.0.0.1 -U app -d app; do sleep 1; done
docker exec profiling-postgres psql -U app -d app -c 'CREATE EXTENSION IF NOT EXISTS pg_stat_statements;'
APP__POSTGRES__ENABLED=true \
APP__POSTGRES__DSN='postgres://app:profiling-only@127.0.0.1:5433/app?sslmode=disable' \
  /root/profiling/bin/migrate > /root/profiling/evidence/migrate.log 2>&1
curl -fsSL https://api.github.com/repos/grafana/k6/releases/latest \
  > /root/profiling/evidence/k6-release.json
asset=$(jq -r '.assets[] | select(.name | test("linux-amd64.tar.gz$")) | .browser_download_url' \
  /root/profiling/evidence/k6-release.json)
curl -fsSL "$asset" -o /root/profiling/k6.tar.gz
tar -xzf /root/profiling/k6.tar.gz -C /root/profiling
install /root/profiling/k6-*/k6 /usr/local/bin/k6
k6 version > /root/profiling/evidence/k6-version.txt
date -u --iso-8601=seconds > /root/profiling/evidence/build-completed.txt
bash /root/profiling/scripts/after-build.sh
