---
name: rust-axum
description: "Request path. Use when Axum route composition, extraction, middleware scope or order, fallbacks, or response lifetime changes which requests and responses a policy observes."
---

# Rust Axum

Draw the affected request path and its returning response, including early
rejection. A middleware's location decides what it can observe, protect and
annotate. A handler that is correct in isolation can be wrong when composed
with admission, authentication, timeouts or fallbacks.

## Choose the policy's scope

Place a policy where every intended path crosses it, before the effect it
controls. A global policy must account for unknown paths, wrong methods and
extractor failures; a feature policy should not silently govern unrelated
routes. A layer that short-circuits can bypass everything inside it, including
response decoration or observation. Check the nesting produced by the actual
construction API: builder order and repeated router layering need not read in
the same direction. When a conclusion depends on Axum or Tower behavior, inspect
the resolved version and its official documentation.

Decide whether state is shared by the intended population. Cloning a service
with an independent limiter per route does not create a service-wide limit.
One shared permit owner enforces the aggregate bound but also couples those
routes under load; independent owners are appropriate when isolation is the
accepted policy. Match that choice to real capacity and failure consequences.

Extraction belongs to the request path. Body-consuming extraction spends the
body and its budget; a rejection can leave the handler uncalled. Preserve the
accepted media type and failure identity at that edge. Fallbacks may lack a
matched route template, so observation needs a bounded unmatched category
rather than a caller-controlled path.

## Follow resources past response headers

Returning a response does not finish sending its body. A handler timeout or
permit can end while a buffered or streaming response still retains memory,
connections or upstream work. If the changed feature needs a bound over that
lifetime, custody must reach body completion, error or drop; after headers,
failure terminates the body rather than replacing the status. A timer that
runs only when the body is polled cannot release an unpolled retained body.
Small bounded responses may need no additional mechanism; large or stalled
responses change that cost decision.

For service HTTP composition, load [HTTP Architecture](../../../docs/architecture/http.md)
for the actual hardened chain, permitted extractors, route registration,
response ownership and server limits. A mounted-router observation can expose
ordering and fallback behavior; connection caps, header admission and drain
require the server boundary. Select the boundary the claim crosses rather than
requiring both for every route edit.
