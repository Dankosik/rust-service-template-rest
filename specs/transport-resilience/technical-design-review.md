# Technical Design review

## Current bounded gRPC delta

Status: ready. Fresh reviewer: `/root/transport_design/grpc_delta_review`,
read-only `reviewer-agent`, native model `gpt-6-astra`, effort `high`.
Method: Technical Design Review, limited to the clarified gRPC traces. The
original review below remains valid for unaffected decisions.

```text
candidate:
  design/transport.md:
    sha256: 024a3aa15b53a64a9641aaa3c4f60339f98bfbdc84c87af045cd71226f5f61b1
  research/native-mechanisms.md:
    sha256: 5330d3365fe0cd6b68b7588875d53551be526e6b5116333f5650106b13bdbf8e
  authoritative spec.md:
    sha256: d82429e639fad6bf6cf011008653404ea1e7bd783b0fb1c7d6661b96d59f73f4
verdict: PASS
findings: none
evidence_boundary: Fresh bounded read-only review of the gRPC clarification,
  pinned dependency source and existing gRPC contract. All hashes matched
  before/after review. No edits, builds, tests or runtime proof by reviewer.
reopen_owner: none
```

Attempted falsifiers:

- Tonic wraps the TLS-capable connector, so its native timer covers the DNS,
  TCP and TLS wait while the future is driven.
- Tower may stop polling after cancelled buffered calls; the retained native
  timeout keeps its original deadline and does not gain another five seconds
  when readiness resumes.
- Tonic restores Idle and delivers the retained error through one call; a
  subsequent call can redial through the same client.
- Tokio polls the inner future before its timer. The design permits a ready
  result after idle expiry and promises neither eager idle cleanup nor strict
  late-ready rejection.
- Caller deadlines, TLS verification, observed terminal status and unknown
  effects after dispatch keep their meaning. No automatic RPC replay, new
  HTTP/2 handshake bound or stream lifetime policy was introduced.

The reviewed [Definition delta](definition-review.md) owns this explicit
clarification. Native mechanism and file ownership are unchanged; T2 owns code
and regression evidence. The post-review design change is lifecycle status and
review wording only; [transition](technical-design-transition.md) records its
current identity.

## Original scope and C1 repair

Status: ready. Reviewer: `/root/transport_design/technical_review`, fresh
read-only `reviewer-agent`, native model `gpt-6-astra`, effort `xhigh`.
Method: [Technical Design Review](../../docs/spec-first-workflow/phases/technical-design-review.md).

```text
candidate:
  base: 5927ffbba351af2f7fb8635316bbfa4ae5b31da6
  design/transport.md:
    sha256: fcc4076cdc7ba901eb1fdd5c573bb841257e397881a1dae544ab34e10f6c9460
  research/native-mechanisms.md:
    sha256: f507827b62d22811200eb122ab02355db8311fd3b096c908da608b098bd0f3a8
  authoritative spec.md:
    sha256: cbd092c9c29e462424a43324454ae4e1cc72089f0b50fdb7b5e15e902ad6d51f
verdict: PASS
findings: none; C1 closed
evidence_boundary: Read-only source/design review and one bounded delta recheck.
  No edits, builds, tests, new environments or runtime proof by reviewer.
reopen_owner: none
```

The initial fixed design
`2572e151ad2c5c0976a48028178b4d37ae98e3eeb4748b645be50f7e259e35d1`
received CONCERNS for one native compatibility point. C1 anchored the last-
Client-drop rule: async-nats Subscriber retains its own command sender and may
outlive that Client, while the original proposed close watch did not. The
template's `Consumer -> Delivery -> Arc<Shared>` retained Client, so the
reviewer found a bounded native risk rather than an application failure.

The owner repaired the design by carrying the same close-request sender through
Subscriber and its existing unsubscribe-on-drop task. The same reviewer used
the single permitted delta recheck and returned PASS. A retained Subscriber
now keeps the implicit lifetime open, while explicit force-close still wins;
the repair fits the same two native files without a public Subscriber API or
new lifecycle owner. Added Smithy archive metadata changed custody evidence
only. Runtime proof remains with Implementation.

The original review's unaffected findings carry forward:

- PG options preserve bare IPv6/TLS identity; the native TCP race preserves
  resolver-order all-failure classification and sends only its winner to TLS.
- Tonic's supported custom-lazy route wraps TLS in the dial timer; reqwest
  applies its sub-budget to both native candidate division and full connection.
- NATS's attempt encloses DNS and handshakes; runner-level cancellation reaches
  recovery outside command polling. Completion differs from a close request.
  TLS-first/discovery suppression support the trust matrix without downgrade.
- Smithy's native TCP timeout propagation preserves the outer timer and explicit
  SDK finality/retry owners; no supported equally narrow builder hook was found.
- Every accepted behavior has a mechanism, owner and proving surface. All
  conditional fallback items, source/profile custody and retirement are closed.

These original candidate hashes matched before/after that review. Its immediate
post-review change was lifecycle status and review link only. The later bounded
gRPC delta is reviewed separately above; unaffected conclusions are retained.
