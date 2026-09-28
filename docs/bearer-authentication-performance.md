# Bearer authentication allocation reductions

The September 28, 2026 investigation compared `infra-bearerauthn` at
`9631b0020e9efbf5df5026e005d898d083ce0db5` with eleven isolated hypotheses and
three combinations. The retained combination, C7, consists of:

- One bearer credential grammar scan, without a redundant ASCII pass.
- In-place scope sorting and deduplication instead of a temporary BTreeSet.
- Immutable per-algorithm JWT Validation prepared before request admission.
- Moving client_id instead of cloning it during identity normalization.
- An Arc-backed immutable Principal identity, keeping the const expiry accessor.
- Static metric keys for known successful transports/engines, with the original
  path for failures, cancellation and other transport labels.
- UTF-8 validation and exact JSON whitespace trimming followed by the original
  introspection envelope/active-claim decoders, avoiding a RawValue copy/pass.

No JWT signature or claim check, provider limit, refresh rule, cache lifetime,
metric label, redaction or custom-claim duplicate-member rule is relaxed.
The delivery uses the measured request-path mechanisms; startup/test setup
reuses one ClaimPolicy instead of temporary clones, and formatting is normalized.

## Measurement scope

The test host was one dedicated DigitalOcean c-4 in lon1: four vCPUs, 8 GiB RAM,
Intel Xeon Platinum 8280, Ubuntu 24.04, Rust 1.98.1. Release builds used the
committed Cargo.lock, debug=1 and strip=none for profiling, identically on both
sides. Four process pairs alternated AB/BA; each process ran three warmup batches
and nine measured batches. Values below are medians of process medians.
Allocator instrumentation ran separately, subtracting N=0 setup from N>0 runs.
Allocation bytes mean allocator traffic, not retained heap or whole-service RSS.

Fixed-input JWT runs reused synthetic key/token files, with a normally checked
expiry of 4000000000. Earlier changing-input JWT samples were excluded. The
Prometheus case used the existing real exporter, public authenticate, a real
loopback TLS fixture for the initial cache fill and an additional principal
clone; it asserted exactly one provider request and exported outcome counts.
It does not measure the entire HTTP chain or an external IdP.

| Workload | Baseline → C7 | Allocator calls/op |
| --- | --- | --- |
| Cached introspection + Prometheus + principal clone | 1.846 → 0.987 microseconds (46.5% less time) | approximately 23.26 → 3.26 |
| Full RS256 authenticate | 28.541 → 28.224 microseconds (1.1% less time) | 78 → 63 |
| Full ES256 authenticate | 62.890 → 61.766 microseconds (1.8% less time) | 77 → 62 |
| RS256 authenticate + principal clone | 28.430 → 28.148 microseconds | 86 → 63 |
| ES256 authenticate + principal clone | 62.650 → 61.530 microseconds | 85 → 62 |

Isolated introspection JSON pass removal reduced decoding time by 19.7–29.4%
for responses of 497/4337/60241 bytes. A 16-scope Principal clone fell from
625.6 to 9.9 ns and from 20 allocations to zero. Cold Principal construction
adds one 128-byte allocation. Scope deduplication can retain spare Vec slots:
2 scopes retained 4 slots instead of 2 (+48 bytes), and 128 input/97 distinct
scopes retained 128 slots instead of 97 (+744 bytes); 16 scopes were unchanged.

Metric specialization alone regressed the isolated RS256 case by 3.5%, while
improving the Prometheus/cache/clone case by 8.3%. C7 was measured as a complete
combination; individual percentages are not additive. A JWKS index did not
provide a material gain across 1–1024 keys. Direct custom-claim deserialization
was rejected despite being faster because it changed last-member-wins behavior.

Existing release tests passed for compatible candidates. Differential probes
matched baseline outputs on 73,728 bearer inputs and 23,567 introspection inputs,
including invalid UTF-8, malformed inactive responses, duplicate fields and the
65,536/65,537-byte retained-payload boundary. This finite corpus complements
source review; it is not a universal equivalence proof or a release gate.

These are historical prototype measurements, not an exact-head production
capacity claim. Compiler, feature unification, payload and hardware can change
the result. The research harness and raw captures remain task artifacts; no
permanent benchmark target or test-only production export was added. Evidence
aggregation SHA-256:
`7d7b84ee51bf2b64f65b99b28e342d36dd0182c8a08931116139291b1153cb7d`.
The exported capture archive SHA-256 is
`279d6906d00de66df4f05912c4a5db30d081cbfadd2f904696129207b146707a`.
The temporary host was deleted after the investigation.
