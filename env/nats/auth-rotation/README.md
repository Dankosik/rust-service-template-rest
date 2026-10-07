# Synthetic NATS JWT rotation fixture

The existing messaging runner uses this configuration only in its own temporary
Compose project. The ordinary `NATS_URL` broker is never changed. The configuration
trusts an isolated operator, resolves its two accounts from memory and enables
64 MiB of JetStream file storage for the `transport-recovery` account. That account
allows eight streams, eight consumers, sixteen connections and 128 subscriptions.
It rejects bearer users. Users A and B require a signed server challenge; their
credentials have no expiry or activation bound.

These checked-in credentials are intentionally public synthetic test inputs.
They have authority only under this fixture's isolated operator. No operator,
account or system-user seed is retained. The temporary producer store and its
signing keys were removed after generation. The Rust test constructs an invalid
tuple from B's valid JWT and A's seed at runtime; it never logs credentials or
signatures. The real broker must reject that tuple before the same reconnecting
client can authenticate B and perform fresh useful work.

## Producer and public identities

Generated on 2026-10-07 with the installed official
[`nsc`](https://nats-io.github.io/nsc/). Its version flag reports `0.0.0-dev`;
`go version -m` identifies `github.com/nats-io/nsc/v2 v2.15.0`, module checksum
`h1:3qOeZ7iX2q3Ae9GrsgE6Z7EgW2uYa0Tr4y97z7l2pKE=`, built with Go 1.26.1,
`github.com/nats-io/jwt/v2 v2.8.2` and `github.com/nats-io/nkeys v0.4.16`.
It is a one-time fixture producer, not a runtime or CI dependency.

| Identity | Public NKey |
| --- | --- |
| Operator `transport-recovery-fixture` | `OABUHD3LJQZFGZM6UZRD72UAXD5FTLOSJVTDPOMC3FFW54HE4ATSQBLD` |
| System account `SYS` | `AA45UZSCQY5W4ZRIFZHCUA6EUFRKHTJINI5ZBVF4NEJBVQWUTZVUBGX2` |
| Account `transport-recovery` | `ACLZA46STBAGJNDQFUQD2TPC6RTGUPASKSEE6WAGPBC5TJCBY5DYGRDI` |
| User A | `UDBCQIADO7SFCGGE5FNGOTL66SCF6F3FL6WWOT75R36TCLHAVKM6TACY` |
| User B | `UDKZULILKPV3BALVMY6JQ63JRXETGP3E4EF4REATS2YFC43HLFIZPCXC` |

The server image remains the digest-pinned NATS 2.15.0 image in
[`env/docker-compose.yml`](../../docker-compose.yml). Its supported operator and
memory-resolver shape is illustrated by the upstream
[`operator.conf` fixture](https://github.com/nats-io/nats-server/blob/v2.15.0/test/configs/operator.conf).

## Regeneration

Run from the repository root with an already available official `nsc`. Each
invocation explicitly uses the newly created store. Regeneration produces new
public identities; update the table above when deliberately replacing the trust
root. Never import a real operator or retain the producer store.

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

## Existing runner and evidence

`make test-integration-messaging` runs ordinary suites followed by the private
JWT phase, including when `NATS_URL` supplies a shared ordinary broker. It stops
its owned ordinary phase before starting the auth configuration. The narrow
mode uses the same script and existing validation lock:

```bash
bash scripts/ci/validation-lock.sh -- bash scripts/ci/test-integration-messaging.sh --auth-only
```

The existing `--no-run` Cargo mode compiles both test executables, including the
auth case, before entering any broker lifecycle. It needs no Docker and supplies
no authentication proof. CI uses it with `NATS_URL=compile-only` to warm its cache.

The runner requires Docker, binds its owned broker to an ephemeral loopback
port and invokes exactly the ignored-outside-mode test
`credentials_file_rotation_is_authenticated_by_the_broker`. It supplies
`NATS_AUTH_URL`, `NATS_AUTH_CREDS_A` and `NATS_AUTH_CREDS_B`; absent inputs and zero
executed cases cannot pass. `nsc` is not used during execution.

`MESSAGING_RECEIPT_DIR` can name a task-owned output directory; otherwise the
script creates one in the system temporary directory and prints its path.
`auth-test.log` records the selected test result, `compose.log` records owned
startup/teardown, and `lifecycle.txt` records phase exits and cleanup readback.
Cleanup requires a successful `down -v` and no remaining containers, networks
or volumes carrying that exact project label. A teardown failure remains a
failed/unobserved result and never replaces an earlier primary test failure.
No runtime result is inferred from fixture generation or these source files.
