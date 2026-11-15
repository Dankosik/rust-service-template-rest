---
name: rust-errors
description: "Failure semantics. Use when defining Rust errors, failure mappings, exit codes, degradation, or fallible completion."
---

# Rust Errors

**Failure semantics.** Follow failure from the operation that knows what happened to the consumer that must act on it. Distinguish rejected work, partial effects, uncertain outcomes, and completed work whose reporting or cleanup failed. Each boundary decides whether to stop, degrade, retry, or continue.

Design errors for their consumers. Preserve typed distinctions where a caller chooses recovery; an application boundary that only reports and exits may use an existing contextual error type. Do not create variants nobody consumes merely because code is in a crate. Public library error types implement `std::error::Error`, `Display`, and `Debug`, including when their variants already existed; `Result` itself imposes no such bound, so interoperability is an API decision the compiler does not supply automatically. Keep required variants and upstream causes, reusing `thiserror` here.

At the operation that knows the failed action, retain the safe resource identifier or input position needed to locate it. A rejected record should identify the record and failed rule when disclosure is permitted. Preserve a source when there is one, and check the diagnostic actually rendered: storing a source does not ensure the reporting boundary displays it. Keep error text concise and composable, with lowercase text and no trailing punctuation where convention requires it. Redact sensitive values throughout the chain, not only in the outer message. Preserve required precedence among failures and any promise of all-or-nothing output.

Use `From` for canonical conversions that need no call-site context. Use `map_err` to attach operation-specific context or convert a foreign error, and match when choosing recovery behavior. Classify structured failures by their kind rather than display-string matching. A retry needs knowledge of the operation's effect and identity; a timeout or broken connection does not by itself prove that nothing happened. An intentionally ignored cleanup failure needs an accepted best-effort policy; a printed error followed by a success result otherwise misleads callers and automation. Help or another successful early termination is a normal outcome, not an empty error sentinel.

When resource completion can fail, lose data, or wait, expose an explicit operation whose owner observes its outcome within the accepted lifecycle. `Drop` handles infallible release or best-effort cleanup without panicking or blocking; destruction does not establish durable flush or asynchronous completion. Keep the original operation outcome distinguishable from a later cleanup failure, and do not promise rollback where effects may already have escaped.

A task-boundary `catch_unwind` can provide defense in depth only when unwinding occurs; it neither repairs an interrupted invariant nor covers aborts. Observe spawned-task failures through their owner, and do not introduce a panic or silently recover poisoned mutex state merely to simplify an error path.

In this service, `crates/service-failure` owns the catalog of client-visible Problem responses and `crates/infra-http` its transport projection. The composition root in `crates/service/src/bootstrap` owns startup, degradation, shutdown outcomes, and exit policy. Preserve those mappings and sanitization at their current boundaries rather than inventing a parallel catalog or process-exit path.

Mentioning an error pattern or boundary does not require a new error framework or a test for every variant.
