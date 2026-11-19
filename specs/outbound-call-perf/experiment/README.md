# Outbound call performance experiment

Run this overlay on the single authorized Linux comparison host, from the same frozen overlay for every artifact. The source archives come from the pinned commits in the task ledger. The overlay mounts a Cargo example in each private source archive; it adds only example dependencies that already have locked versions. The original checkout, manifests, and lockfile remain untouched. The example uses the system allocator and the repository's release profile.

## Prepare and recover the host

Use [the accepted comparison protocol](../design/comparison.md#cost-and-teardown) for creation, billing, recovery, and deletion. Before issuing the single create request, persist an off-host receipt and arrange a separate off-host cleanup process that can execute with the existing authenticated access. Include these values in the receipt.

| Field | Value to record |
| --- | --- |
| Identity | Unique exact name with task nonce, authorized project/account locator, region, SKU, and permitted creation interval |
| Spend | Verified price and billing granularity, extras, US$5 cap, first possible charge time, export/deletion reserve, workload stop and cleanup cutoffs |
| Recovery | Authorized control-plane locator without credentials, cleanup process locator, reconciliation rule, pre-create absence, and provider ID when received |
| Closure | Last verified export, deletion attempts and timestamps, exact provider ID, authoritative absence or terminal deletion, and remaining exposure if closure fails |

The cleanup process first reconciles the original create within the recorded name, account, region, size, and interval. It only uses a positively reconciled ID. If a deletion response is ambiguous, verify the exact ID through an authenticated provider read before retrying. Record the final provider state and retire the cleanup process after closure. Store the receipts in the task evidence directory off-host throughout the run.

Transfer a Git bundle or other authorized local checkout containing the frozen baseline, D1–D6, and final-selection commits, together with the frozen overlay. Record the overlay commit and archive checksum in an off-host receipt. On the host, install the pinned Rust toolchain from `rust-toolchain.toml`, a native Rust build environment, Python 3.11 or later, `curl`, `taskset`, and OpenSSL. Install Heaptrack and its analysis tools for the retention comparison.

Ubuntu Heaptrack 1.5.0 requires the [compatibility launcher used for the retained-memory measurements](../evidence/heaptrack-compat.json). It omits the unsupported quiet flag and keeps profiler output separate from the caller's JSON.

From the overlay directory, set `source_repo` to the transferred Git checkout, `experiment_root` to an empty directory outside the overlay, and `base_revision`, `candidate_revision`, and `final_revision` to the full commit IDs. Use the same clean build environment each time. To amortize build inputs, set `CARGO_TARGET_DIR` to one experiment-only absolute scratch directory and invoke builds serially. The script copies each final binary into its artifact directory before any subsequent build can overwrite the Cargo example.

```sh
python3 -m venv "$experiment_root/venv"
"$experiment_root/venv/bin/pip" install --require-hashes -r fixture-requirements.txt

python3 prepare.py --source "$source_repo" --revision "$base_revision" \
  --destination "$experiment_root/base"
python3 prepare.py --source "$source_repo" --revision "$candidate_revision" \
  --destination "$experiment_root/candidate"

for variant in host-insertion first-frame shared-cache exact-authority; do
  python3 prepare.py --source "$source_repo" --revision "$candidate_revision" \
    --destination "$experiment_root/$variant" --variant "$variant"
done
```

Each `build.json` records the source commit, dependency lock, overlay hashes, variant patch hash, toolchain, release inputs, and binary hash. The runner validates the binary hashes and common build inputs before starting a fixture. Use separate matched `--heap-symbols` builds for Heaptrack to preserve attribution. In the timing artifacts the repository's stripping policy applies.

## Run and export

Set `caller_cpus` and `fixture_cpus` to disjoint Linux CPU lists sized to keep the fixture out of saturation. Use the same placement and worker count for each pairing, and put the workload stop time in `workload_stop` as an RFC 3339 UTC timestamp. Set `run_output` to a new directory and record the Droplet SKU, image, and provider ID in a credential-free JSON file passed as `--host-receipt`. Run `run.py --list-scenarios` to inspect available cells. The default matrix is the complete set of core timing and demand cells. Select individual diagnostic cells with repeated `--scenario` flags.

```sh
"$experiment_root/venv/bin/python" run.py \
  --artifact "base=$experiment_root/base/outbound_perf" \
  --artifact "candidate=$experiment_root/candidate/outbound_perf" \
  --output "$run_output" --caller-cpus "$caller_cpus" --fixture-cpus "$fixture_cpus" \
  --host-receipt "$host_receipt" --overlay-receipt "$overlay_receipt" \
  --deadline-utc "$workload_stop" --pairs 4 --calls 2048 --warmup-calls 256
```

Pilot on the authorized host only. Use the same offered and warm-up counts on both sides. Reduce counts if necessary to keep each service hit window wholly before the early-refresh trigger or inside the positively gated refresh period, as applicable. Keep pilot data and tuned population counts. In each trial, TLS, transports, and tokens warm before caller CPU collection starts. Every caller process starts with `SSL_CERT_FILE` set to the fresh fixture CA. The gRPC connector trusts the same CA explicitly and negotiates h2. Fixture calls use synthetic secrets in a temporary directory, and receipts exclude those secrets.

The default buffered matrix keeps the empty HEAD cell and measures empty POST, PUT, and PATCH separately. The fixture checks the method, origin-form target, Host, body byte count, and all values of both framing headers. The empty cells supply a length and transfer encoding at admission so the peer can confirm their replacement or removal.

Ordinary buffered, retained, and credential-hit cells require established HTTPS connections before the window and zero new HTTPS connections during it. Headers-only, post-header failure, and retry diagnostics can establish connections as part of their workload. Paired peer work includes established HTTP and gRPC connection deltas.

| Variant pairing | Cells |
| --- | --- |
| Candidate vs. `host-insertion` | `http-buffered-canonical`, `http-buffered-alternate`, `http-buffered-caller-host`, `http-buffered-ipv6`, `http-buffered-retry` |
| Candidate vs. `first-frame` | Timing with `http-buffered-empty`, `http-buffered-one`, `http-buffered-multi`, `http-buffered-tiny`, `http-buffered-large`; independent Heaptrack runs with `http-retain-one`, `http-retain-tiny`, `http-retain-large` |
| Candidate vs. `shared-cache` | All four `service-before-{1,64}` and `service-refresh-{1,64}` cells and `service-demand` |
| Candidate vs. `exact-authority` | `http-buffered-canonical`, `http-buffered-alternate`, `http-buffered-mixed`, `http-buffered-ipv6`, and HTTP refusal cells for caller Host, scheme, port, origin, and userinfo |

Forced idle-close receipts establish reconnect and exactly one resource request per offered call. Build two additional candidate artifacts with `--retry-probe`, one also using `--variant host-insertion`, and select only `--scenario http-buffered-unstarted-retry` for that pairing. The diagnostic injects one retryable unstarted cancellation after HTTP/1 URI-form conversion on a positively reused connection. It records the branch hit and checks that the retry reaches the peer once with the configured Host, origin-form target, and original body. The runner excludes these fault-injected timings from paired deltas. The authority probe records explicit HTTPS default and empty ports through the public admission predicate. Actual case-variant and mixed HTTP cells record wire Host and target separately. Keep the fixed-URL pool key and record the default-port disposition from source unless the protocol's reopening evidence is available.

For retained cells, pass `--instrument heaptrack` and use matching `--heap-symbols` artifacts. Repeat `http-retain-tiny` in an independent matched run with `--hold-ms 10` alongside the default 1000 ms. The caller retains response bodies only and records the fill, hold, and release phases. Analyze the archived Heaptrack traces with `heaptrack_print` or Heaptrack GUI to attribute live and peak allocations to receive versus response assembly. Use the process memory markers as corroboration. The adverse tiny cell places a one-byte body behind a large header in the same TLS write. The unconditional first-frame variant is comparison-only under the accepted ownership bound.

For each run, save the output directory off-host during execution and again before the workload stop. Copy finalized files incrementally with the authorized SSH endpoint, for example with `rsync -a --exclude='*.tmp'`, into the task evidence directory. Keep `run.json`, `summary.json`, per-cell `cell.json`, raw caller and fixture files, build receipts, and Heaptrack traces where applicable. Re-read the off-host JSON and verify transferred hashes or sizes before deleting the host. The runner records invalid and interrupted cells, discards their paired timing claim, and retains earlier paired observations. Keep evidence from missed deadlines or invalid workload cells with its limitation.

After selecting any eligible variant and validating its shipping change through the owning task, freeze the final revision, build it without a variant patch, and run the default core matrix against the original baseline. Record a disposition for each of the five alternatives using the applicable behavior, performance, and resource evidence. Finish under the authorized cleanup cutoff even when some cells remain incomplete.
