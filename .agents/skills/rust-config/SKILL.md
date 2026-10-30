---
name: rust-config
description: "Configuration meaning. Use when a Rust configuration value needs a source, precedence, default, validation boundary, secret policy, or coherent runtime snapshot."
---

# Rust Config

Give each value one semantic owner: the component that knows what it means,
which states are valid and what its default promises. Trace all admitted
sources through precedence and decoding to the value runtime code consumes.
A second environment read or fallback inside a consumer can invalidate an
otherwise correct startup snapshot.

## Preserve distinctions at the boundary

Absence permits an omitted-value policy; an explicit empty override is supplied
input. Treating both as missing can unexpectedly revive a lower-precedence
secret or endpoint. Decide whether empty means unset, invalid or a real empty
value from the key's meaning and the local source policy. Likewise, unknown
keys can signal a misspelling or an incompatible rollout; silently ignoring them
changes startup behavior rather than merely improving parser convenience.

Use types to remove ambiguity where the consumer depends on it: a duration,
address or finite mode can prevent contradictory interpretations of a string.
Typed decoding does not establish a valid range or relationship. Keep intrinsic
constraints with the value's owner, cross-field constraints where their meaning
is known, and composition constraints where all affected components are visible.
Do not duplicate a rule in several consumers to compensate for the wrong owner.

A default is operational behavior. An added tuning key costs documentation,
rollout compatibility and future support; keep a code-owned constant when the
accepted service has no operator variation. Introduce a key when a real
independent operating choice justifies those costs. A changed default or new
required key also needs consideration of old configurations and rollback, not
only the new binary's happy startup.

## Validate before effects

Build a coherent typed snapshot before opening dependent resources. An immutable
snapshot makes every consumer see one admitted configuration. Hot reload changes
that promise: it needs atomic replacement, invalid-update behavior and custody
of resources created from the old value. Choose it only when the requested
rotation or runtime behavior requires it; reading a mounted file once does not
establish ongoing rotation.

For key placement, supported sources, precedence, empty overrides, secret
carriers and local key-authoring obligations, load [Configuration Source
Policy](../../../docs/configuration-source-policy.md). For a runtime-budget
relationship, also load [Operation Budgets](../../../docs/operation-budgets.md).
Keep diagnostics useful by naming the key and expected rule without exposing
secret values. Observe the ambiguity the change introduces through the real
loader or validator; adequate existing coverage can be reused, and an unchanged
source policy does not need a ceremonial new policy line.
