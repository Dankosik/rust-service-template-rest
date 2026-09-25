# Durable background jobs static fixtures

These are contrasting static instruction fixtures for the `rust-reliability`
reach into the durable background jobs universal discipline. They verify
routing and ownership text only; they do not measure model behavior or
establish delivery acceptance.

## Positive: a job kind's retry and lease decision

Prompt: “Add a job kind that calls a mail provider, choose its attempt
budget and timeout, and say what happens when the worker dies mid-call.”

Expected: `rust-reliability` owns the arithmetic: attempt timeout, attempt
budget, backoff, and the worker drain. The answer reaches the durable
background jobs discipline for lease, effect, and recovery. The claim is
expiring permission, not exclusive execution. The provider call carries an
idempotency key derived from the job identifier.

Rejected: treating the claim as proof of a single run, retrying inside a
request budget, or adding a configuration key for the attempt budget.

## Negative: a request budget with no durable work

Prompt: “Set the timeout of a synchronous provider call inside
`POST /widgets`.”

Expected: `rust-reliability` applies the request-budget inequality and does
not load the durable jobs discipline, because nothing outlives the request.

Rejected: moving the call to a job, or loading the discipline for a
request-budget question.
