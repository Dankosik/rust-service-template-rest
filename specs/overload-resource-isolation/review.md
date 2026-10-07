# Independent research review

Source candidate: main SHA 5927ffbba351af2f7fb8635316bbfa4ae5b31da6.

Reviewer: fresh read-only collaboration reviewer /root/research_review, selected natively as gpt-6-astra/high with fork_turns none. Method: standalone Research adapter and shared Review. Reviewer did not edit files, run tests/builds/benchmarks or operate infrastructure.

## First result

Intent SHA256 de56b7c46c54f35b2022318e64153643f17be94348882fba486785aafc069863.
Initial report SHA256 338ddaa550fc4506b6132ca621e46f45be1accc51bdabd2dcb58441240a70aa1.

Verdict FAIL. R1: missing evidence closure for outbound HTTP resource work after OAuth authorization and outbound webhook delivery. Token-provider permits do not bound resource exchange; bounded complete-response client and delivery-kind max_concurrent already exist and must inform the minimum mechanism.

Research owner repaired only R1: added resource map rows, source-supported flow/deadline/body/pool accounting, overload and measurement scenarios, native extension points, minimum recommendation and coverage boundaries. Product sources and accepted research scope remained unchanged.

## Bounded delta recheck

Candidate: unchanged source HEAD and intent; report SHA256 a32897f40098f357ec72b336f31b2ca91b88d3b57e0debb714a384d687f4a46f.

Verdict PASS. Findings: none surviving, R1 closed. Reopen owner: none.

Reviewer falsifiers and results:

- OAuth token cap also covers resource HTTP: disproved by authorize then resource.execute with original deadline.
- Outbound HTTP timeout ends at headers: disproved by timeout around full body collection through EOF.
- Webhook delivery needs a new scheduler: existing Dispatcher::register(max_concurrent) maps to jobs max_running.
- Added test coverage claims a fresh pass: report labels tests inspected, not executed.

Unchanged-source review independently confirmed gRPC auth-before-admission, HTTP header-only permit, auth coalescing/mutex waits, S3 streaming versus SDK operation timeout, five-second native SQLx return patch, Tonic/Tower cancellation and buffering, async-nats defaults, bounded JetStream pull, and unbounded Tokio blocking queue. Missing runtime measurements remain explicitly downstream, not acceptance evidence.

Review authority is limited to research evidence and recommendation consistency. It does not approve product behavior, deployment or infrastructure changes.
