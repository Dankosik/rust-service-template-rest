---
name: rust-api-contract
description: "Observable contract. Use when a Rust service REST change alters client-visible status, data, security, or retry behavior, or when the served API and OpenAPI may disagree."
---

# Rust API Contract

Start with what an existing caller can distinguish, including callers that
never read OpenAPI. Follow input admission, execution and response through
status, media type, headers, body presence, fields, security and retry outcome.
A generated schema can be internally consistent while describing behavior the
server does not implement.

## Compatibility follows consumers

Compare the old and proposed behavior using a concrete caller. Adding a
response enum variant can break an exhaustive client; making a request field
optional can be compatible while making the corresponding response field
optional breaks a reader that relied on it. An absent member, explicit `null`
and a defaulted value may have different meanings to patch logic, signatures,
caches or generated clients. Preserve those distinctions until the accepted
contract says they are interchangeable.

Separate representable values from admitted values. A Rust type or schema may
express a range without enforcing it at the request boundary. Conversely, a
stricter decoder can reject previously accepted traffic even when the schema
diff is empty. Match the actual validation and error response to the promise;
do not add stricter admission merely because a type permits it.

Security and retries are part of compatibility. An omitted security declaration
can inherit a default while an explicit empty declaration removes it. Resolve
the effective policy after composition, not from one handler annotation. A
failure after a possible effect cannot promise a safe fresh retry merely because
its HTTP status resembles a pre-dispatch failure. Preserve an operation's
identity and uncertainty contract through transport mapping.

## Keep authority and observation aligned

For a changed HTTP contract, load [HTTP Architecture](../../../docs/architecture/http.md)
for code-first composition, effective security, Problem ownership, compatibility
approval and regeneration. Edit the authoritative source and consume its derived
document; choosing a different source because it is easier to patch creates drift.
The committed contract is a useful review artifact, while the mounted response
establishes what callers receive.

Choose observations for the disputed distinction: serialized absence/null,
media type, error identity, headers or retry effects. Schema checks and mounted
behavior answer complementary questions. Reuse adequate evidence rather than
requiring a new case for every unchanged status. A deliberate breaking change
may be simpler than a compatibility adapter when accepted consumers can migrate;
an adapter earns its ongoing cost only for a current compatibility obligation.
