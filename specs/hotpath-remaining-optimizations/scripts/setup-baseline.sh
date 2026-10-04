#!/usr/bin/env bash
# Only on droplet 606085569; baseline custody precedes candidate source edits.
set -euo pipefail
source /root/.cargo/env
export DEBIAN_FRONTEND=noninteractive CARGO_BUILD_JOBS=6 SQLX_OFFLINE=true
apt-get install -y docker.io docker-compose-v2 python3-psycopg2 zlib1g-dev
systemctl stop postgresql
mkdir -p /root/profiling/bin /root/profiling/scripts /root/profiling/evidence
cd /root/remaining/baseline
git init -q
git -c user.name=benchmark -c user.email=benchmark@example.invalid add .
git -c user.name=benchmark -c user.email=benchmark@example.invalid commit -qm baseline
/usr/bin/time -v cargo build --release --locked -p service --bin service -p migrate --bin migrate \
  > /root/remaining/evidence/build-baseline.log 2>&1
cp target/release/service /root/profiling/bin/baseline
cp target/release/migrate /root/profiling/bin/migrate
/usr/bin/time -v cargo build --release --locked -p service --bin service \
  --features hotpath,hotpath-alloc,hotpath-mcp,hotpath/hotpath-prometheus \
  > /root/remaining/evidence/build-profile-baseline.log 2>&1
cp target/release/service /root/profiling/bin/profile-baseline
docker run -d --name profiling-postgres --cpuset-cpus=2,3 --memory=2g \
  -p 127.0.0.1:5433:5432 -e POSTGRES_DB=app -e POSTGRES_USER=app \
  -e POSTGRES_PASSWORD=profiling-only \
  postgres:18@sha256:4ef4dbc939d61acea57712655ddb4b4ab27419c913f94cca0cd57cb3ea3c2280 \
  -c shared_preload_libraries=pg_stat_statements -c track_io_timing=on
until docker exec profiling-postgres pg_isready -h 127.0.0.1 -U app -d app; do sleep 1; done
docker exec profiling-postgres psql -U app -d app -c 'CREATE EXTENSION pg_stat_statements;'
APP__POSTGRES__ENABLED=true \
APP__POSTGRES__DSN='postgres://app:profiling-only@127.0.0.1:5433/app?sslmode=disable' \
  /root/profiling/bin/migrate > /root/remaining/evidence/migrate.log 2>&1
curl -fsSL https://api.github.com/repos/grafana/k6/releases/tags/v2.3.0 \
  > /root/remaining/evidence/k6-release.json
asset=$(jq -r '.assets[] | select(.name | test("linux-amd64.tar.gz$")) | .browser_download_url' /root/remaining/evidence/k6-release.json)
curl -fsSL "$asset" -o /root/remaining/k6.tar.gz
tar -xzf /root/remaining/k6.tar.gz -C /root/remaining
install /root/remaining/k6-*/k6 /usr/local/bin/k6
k6 version > /root/remaining/evidence/k6-version.txt
sha256sum /root/profiling/bin/* > /root/remaining/evidence/baseline-binaries.sha256
date -u --iso-8601=seconds > /root/remaining/evidence/baseline-ready.txt
