# Synthetic NATS JWT rotation fixture

`scripts/ci/test-integration-messaging.sh` uses this isolated NATS operator and
memory-resolver configuration in its own temporary Compose project; the shared
`NATS_URL` broker is never modified.

These checked-in credentials are intentionally public synthetic test inputs
trusted only by this fixture's isolated operator. No operator, account, or
system-user seed is retained; the temporary producer store and signing keys were
deleted after generation.

## Producer and public identities

Generated once with official [`nsc`](https://nats-io.github.io/nsc/) (`v2.15.0`)
for the digest-pinned NATS server in [`env/docker-compose.yml`](../../docker-compose.yml)
(modeled on upstream [`operator.conf`](https://github.com/nats-io/nats-server/blob/v2.15.2/test/configs/operator.conf)):

| Identity | Public NKey |
| --- | --- |
| Operator `transport-recovery-fixture` | `OABUHD3LJQZFGZM6UZRD72UAXD5FTLOSJVTDPOMC3FFW54HE4ATSQBLD` |
| System account `SYS` | `AA45UZSCQY5W4ZRIFZHCUA6EUFRKHTJINI5ZBVF4NEJBVQWUTZVUBGX2` |
| Account `transport-recovery` | `ACLZA46STBAGJNDQFUQD2TPC6RTGUPASKSEE6WAGPBC5TJCBY5DYGRDI` |
| User A | `UDBCQIADO7SFCGGE5FNGOTL66SCF6F3FL6WWOT75R36TCLHAVKM6TACY` |
| User B | `UDKZULILKPV3BALVMY6JQ63JRXETGP3E4EF4REATS2YFC43HLFIZPCXC` |

## Regeneration

Run from the repository root with official `nsc` installed. Regeneration creates
new public identities and JWTs—update the table above and the exact literal
allowlists in [`.gitleaks.toml`](../../../.gitleaks.toml) together, and never
import a real operator or retain `$fixture_store`:

```bash
set -euo pipefail
umask 077
fixture_store=$(mktemp -d "${TMPDIR:-/tmp}/transport-recovery-nsc.XXXXXX")
fixture_dir="$PWD/env/nats/auth-rotation"
trap 'rm -rf -- "$fixture_store"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
nsc --all-dirs "$fixture_store" add operator --name transport-recovery-fixture --sys --start 0 --expiry 0
nsc --all-dirs "$fixture_store" add account --name transport-recovery --start 0 --expiry 0
nsc --all-dirs "$fixture_store" edit account --name transport-recovery --disallow-bearer \
  --js-disk-storage 64MiB --js-mem-storage 0 --js-streams 8 --js-consumer 8 \
  --conns 16 --subscriptions 128 --payload 1MiB
nsc --all-dirs "$fixture_store" add user --account transport-recovery --name user-a --start 0 --expiry 0
nsc --all-dirs "$fixture_store" add user --account transport-recovery --name user-b --start 0 --expiry 0
nsc --all-dirs "$fixture_store" generate creds --account transport-recovery --name user-a --output-file "$fixture_dir/user-a.creds"
nsc --all-dirs "$fixture_store" generate creds --account transport-recovery --name user-b --output-file "$fixture_dir/user-b.creds"
nsc --all-dirs "$fixture_store" generate config --mem-resolver --config-file "$fixture_store/server.conf"
cat >"$fixture_dir/nats-server.conf" <<'CONFIG'
# Synthetic C6 JWT fixture; generated trust material is documented in README.md.
port: 4222
http_port: 8222
jetstream {
  store_dir: "/data/jetstream"
  max_file_store: 64MB
}

CONFIG
cat "$fixture_store/server.conf" >>"$fixture_dir/nats-server.conf"
chmod 644 "$fixture_dir/nats-server.conf" "$fixture_dir/user-a.creds" "$fixture_dir/user-b.creds"
```
