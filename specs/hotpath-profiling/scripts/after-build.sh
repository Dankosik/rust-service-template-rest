#!/usr/bin/env bash
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive
apt-get install -y python3-psycopg2 docker-compose-v2
systemctl stop postgresql
# CPU sampling helper from the maintainer's public release, with its checksum.
cd /root/profiling
curl -fsSL https://api.github.com/repos/mstange/samply/releases/tags/samply-v0.13.1 \
  > evidence/samply-release.json
asset=$(jq -r '.assets[] | select(.name == "samply-x86_64-unknown-linux-gnu.tar.xz") | .browser_download_url' evidence/samply-release.json)
curl -fsSL "$asset" -o samply-x86_64-unknown-linux-gnu.tar.xz
curl -fsSL "$asset.sha256" -o samply-x86_64-unknown-linux-gnu.tar.xz.sha256
sha256sum -c samply-x86_64-unknown-linux-gnu.tar.xz.sha256 > evidence/samply-checksum.txt
tar -xf samply-x86_64-unknown-linux-gnu.tar.xz
install samply-x86_64-unknown-linux-gnu/samply /usr/local/bin/samply
samply --version > evidence/samply-version.txt
sysctl -w kernel.perf_event_paranoid=1 > evidence/perf-permissions.txt
shellcheck -x scripts/*.sh > evidence/shellcheck.txt
python3 -m py_compile scripts/*.py
