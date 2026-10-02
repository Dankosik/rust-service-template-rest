# Technical Design review

```text
candidate: baseline 67be869; design/reliability.md SHA-256 0d26ca996e9f408305ce44681b58e8341a28c057571e3622a720ca332b16f465
verdict: PASS
findings: none
evidence_boundary: fixed accepted Definition and Technical Design; current cache source; resolved redis 1.7.1 and backon 1.6.0 source; no build or runtime experiment
reopen_owner: none
```

Fresh independent reviewer: `/root/cache_design/design_review`, native
`reviewer-agent`, Astra/high, no inherited turns. Review completed 2026-10-02
through Technical Design Review and the Evidence Contract. Candidate and
Specification hashes matched before and after review. Specification SHA-256:
`04e4cea2793a6033f0a819ffd16becd32d713009f2fd36a7c0cbc5bb64948dad`.

Attempted falsifiers and disposition:

- Cancelled commands retain unanswered slots indefinitely: independent PING,
  retirement cancellation and bounded clone retention close the path.
  redis `client.rs:547-559` installs the shared driver handle;
  `aio/runtime.rs:52-74` aborts it on final-handle drop.
- Late old failures destroy the successor or replay writes: identity-fenced
  retirement and one dispatch close both paths.
- Public `C = 30 s` and externally bounded probes break the resource bound:
  command/connected-probe lifetimes and publication spacing are separately
  accounted; probe acquisition retains no connection.
- Rejected unchanged credentials strand traffic-independent recovery:
  readable-versus-authenticated state and periodic retries retain recovery;
  new setup rereads the file and unreadable files preserve a usable connection.
- Client replacement loses admitted TLS or keeps raw AUTH diagnostics:
  redis `tls.rs:32-58` places TLS parameters in cloned connection information;
  removing the streaming provider removes the raw-error branches.
- Jitter exceeds 2 s: backon `exponential.rs:202-252` confirms the issue;
  capping yielded sleeps supports the design's arithmetic.
- Ownership/adoption correction creates a speculative interface: the design
  retains the existing crate boundary, removes superseded owners, and puts
  concrete cache use in composition/adapters.

The review establishes mechanism and feasible proof, with the documented
async-scheduling and already-dispatched file-read limits. It does not establish
implementation correctness, local test results, CI, merge or deployment.

After PASS the phase owner changed only `Status: draft` to `Status: ready`.
The ready [design](design/reliability.md) SHA-256 is
`244e23bb9b197971031012e3029a7f1d4f3940cbadbb60574d31b593a7bd4430`;
Transition's unchanged-semantic-scope rule retains the verdict.
