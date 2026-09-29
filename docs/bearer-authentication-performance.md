# Bearer authentication performance

## Parsed-key JWS verification

A second September 28, 2026 pass started from `4824ffc`. Profiling showed that
outside the signature arithmetic, `jsonwebtoken::decode` rebuilt the aws-lc
RSA key (including its Montgomery setup) on every call, parsed the JOSE header
three times and the payload three times, and cloned the decoding key. The
retained changes are:

- Each admitted JWK is parsed once into an aws-lc `ParsedPublicKey` per
  algorithm it serves; the signature is verified against it directly.
- The header is decoded once with `jsonwebtoken`'s `Header` type, so header
  admission rules are unchanged. The payload is decoded once and read once:
  registered claims follow the rules `jsonwebtoken` applied, and claim strings
  borrow from the payload instead of being copied.
- Success counters are registered once per verifier; each request increments a
  handle instead of looking the key up in the recorder registry.
- The introspection cache key uses aws-lc SHA-256, which has vector code,
  instead of the portable `sha2` implementation.
- The bearer grammar check is one vectorizable pass, and the principal keeps
  the decoded payload instead of copying it into a second allocation.

Measured on a dedicated DigitalOcean c-4 (lon1, Xeon Platinum 8168, Ubuntu
24.04, Rust 1.98.1, workspace release profile with symbols). Each figure is the
median of five interleaved baseline/candidate process rounds, pinned to one
core. Instructions come from `perf stat` with setup subtracted. Both sides were
built with branch and function alignment
(`-mbranches-within-32B-boundaries`, 64-byte functions), because an unaligned
build of an unchanged tree moved RS256 between about 27.7 and 31 µs from code
placement alone.

| Workload | Time per operation | Instructions | Allocations (bytes) |
| --- | --- | --- | --- |
| RS256 authenticate | 39.8 → 23.6 µs (−41%) | −28% | 43 → 21 (3746 → 1382) |
| PS256 authenticate | 41.9 → 25.7 µs (−39%) | −26% | 43 → 21 |
| RS256, RFC 9068 profile | 39.7 → 23.6 µs (−40%) | −28% | 43 → 21 |
| ES256 authenticate | 76.0 → 63.8 µs (−16%) | −10% | 42 → 21 |
| EdDSA authenticate | 55.5 → 46.1 µs (−17%) | −12% | 42 → 21 |
| Cached introspection, 43-byte token | 1110 → 636 ns (−43%) | −42% | 3 → 1 |
| Cached introspection, 1 KiB token | 7.33 → 3.42 µs (−53%) | −50% | 3 → 1 |
| RS256, 4 threads, per operation | 18.5 → 11.8 µs (−36%) | | |
| Cached introspection, 4 threads | 720 → 426 ns (−41%) | | |

A default (unaligned) build gave RS256 −35%, ES256 −12% and cached
introspection −45%. Four threads on the four vCPUs ran RS256 about 2.1 times
faster than one thread both before and after the change; the cause (likely
host hyperthreads) was not investigated, and key-set sharing was not changed. The JWT path now spends about 76% of its time in aws-lc
signature arithmetic and hashing; cached introspection spends about a quarter
in SHA-256 (this CPU has no SHA extensions) and a quarter in `moka`.

Semantics were checked with a differential corpus of 1298 cases: signed tokens
varying every read claim through 31 value shapes, check-order pairs, duplicate
members, header members, the compact form and signature encodings, under both
token profiles, plus introspection responses. Every accept, reject and failure
class matched the baseline. Only the diagnostic `reason` label changed for some
malformed tokens: a wrongly typed `iss`, `aud` or `exp` is `malformed_claims`
rather than `missing_claim`, a fractional `exp` is `malformed_claims` rather
than `expired`, and an undecodable signature is `signature` rather than
`malformed_claims`. An undecodable signature is also rejected before key
lookup, so with an unknown `kid` it no longer requests a JWKS refresh.

Rejected or not retained:

- Parsing only the header members the verifier reads: about 3% fewer
  instructions, but it accepted signed headers whose unused members
  `jsonwebtoken` rejects, such as a non-array `x5c`.
- A faster cache or a non-cryptographic cache key: `moka` owns the coalesced
  fill and zero-lifetime retention, and the SHA-256 key keeps raw tokens out of
  memory.

## Allocation reductions

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

### Measurement scope

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
