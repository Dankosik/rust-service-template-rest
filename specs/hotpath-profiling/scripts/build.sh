#!/usr/bin/env bash
# Execute ONLY on the approved disposable DigitalOcean host, after bootstrap.
set -euo pipefail
source /root/.cargo/env
cd /root/profiling/source
export CARGO_BUILD_JOBS=6
chown -R root:root /root/profiling/source /root/profiling/instrumented
rustup component add rustfmt clippy --toolchain 1.98.1
# Only public source is on this host; this local Git repository supplies build
# metadata without transferring the user's Git configuration or credentials.
git init -q
git -c user.name=profiling -c user.email=profiling@example.invalid add .
git -c user.name=profiling -c user.email=profiling@example.invalid commit -qm baseline
/usr/bin/time -v cargo build --release --locked -p service --bin service -p migrate --bin migrate \
  > /root/profiling/evidence/build-baseline.log 2>&1
cp target/release/service /root/profiling/bin/baseline
cp target/release/migrate /root/profiling/bin/migrate
cp -a /root/profiling/instrumented/. /root/profiling/source/
# Deliberate lockfile update for the optional profiler, not a validation side
# effect. Keep existing resolved packages; record the resulting lock diff.
cargo update --workspace > /root/profiling/evidence/lock-update.log 2>&1
git diff -- Cargo.lock > /root/profiling/evidence/lock.diff
cargo tree --locked -p service > /root/profiling/evidence/tree-default.txt
cargo tree --locked -p service --features hotpath,hotpath-alloc,hotpath-mcp,hotpath-cpu \
  > /root/profiling/evidence/tree-profile.txt
cargo fmt --all --check > /root/profiling/evidence/fmt.log 2>&1 || cargo fmt --all
/usr/bin/time -v cargo build --release --locked -p service --bin service \
  > /root/profiling/evidence/build-feature-off.log 2>&1
cp target/release/service /root/profiling/bin/feature-off
/usr/bin/time -v cargo build --release --locked -p service --bin service \
  --features hotpath,hotpath-alloc,hotpath-mcp,hotpath-cpu \
  > /root/profiling/evidence/build-profile.log 2>&1
cp target/release/service /root/profiling/bin/profile
sha256sum /root/profiling/bin/* > /root/profiling/evidence/binaries.sha256
apt-get install -y docker.io
docker run -d --name profiling-postgres --cpuset-cpus=2,3 --memory=2g \
  -p 127.0.0.1:5433:5432 -e POSTGRES_DB=app -e POSTGRES_USER=app \
  -e POSTGRES_PASSWORD=profiling-only \
  postgres:18@sha256:4ef4dbc939d61acea57712655ddb4b4ab27419c913f94cca0cd57cb3ea3c2280 \
  -c shared_preload_libraries=pg_stat_statements -c track_io_timing=on
until docker exec profiling-postgres pg_isready -h 127.0.0.1 -U app -d app; do sleep 1; done
docker exec profiling-postgres psql -U app -d app -c 'CREATE EXTENSION pg_stat_statements;'
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
