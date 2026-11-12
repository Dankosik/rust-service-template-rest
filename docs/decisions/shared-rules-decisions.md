# Shared rules decisions

Recorded 2026-11-12. `crates/shared-rules` owns rules that several crates must
apply identically when they have no other common owner; its crate
documentation holds the membership test. This record keeps why the rules live
there, what was rejected, and when to revisit.

## Selection and cost

| Decision | Alternative and decisive evidence | Accepted cost and reopen condition |
| --- | --- | --- |
| One always-present crate with modules named after the authority behind each rule (`rfc6749`, `rfc6750`, `rfc7515`, `rfc7519`, `uri`, `otel_http`, `error_chain`, `jitter`, `secret_file`) | Twenty-one copies of nine rules lived in ten crates because optional profile crates cannot depend on each other or on `config`, and copies of one rule had already drifted (the secret-file row below). Rejected: documented twins (each rule kept N copies and N tests); modules in `operation-context`, `infra-http`, `infra-telemetry` or `config` (new provider edges into the inbound HTTP stack, the OpenTelemetry SDK or the loader, and wider crate charters); one crate per topic (one or two functions each); one source file included by `#[path]` (no single owner for readers or tooling); a derived template profile per module (each touches five or six initializer files). rust-analyzer's `stdx` and Materialize's `mz_ore` keep such code in one crate with a one-sentence charter. | The crate grows by review against its membership test; its size is no reason to split it, and modules that change together may stay together. Reopen as a separate crate when a group of modules needs what the others must not carry, such as a dependency or another dependency direction; a group whose consumers share one profile is already removed by that profile's marker. |
| No dependencies beyond `std`: the `shared-rules` role is `std_only` in `quality/architecture.json`, and `architecture-check` refuses every non-dev edge. When a rule sits inside dependency glue, only the rule moves: the `NumericDate` serde visitors of `infra-bearerauthn` and `infra-serviceauth` stay with them and call `rfc7519::numeric_date_seconds` | A catch-all crate's main cost is the dependencies it attracts (Go blog "Package names"; matklad, "Fast Rust Builds"). `mz_ore` makes any external crate optional and feature-gated. | The two visitors still repeat a dozen lines of serde scaffolding. Reopen when a rule cannot be separated from a crate's types: give that module a feature of its own name with an optional dependency rather than a required one. |
| The crate exists in every template projection; `infra-http` (`otel_http`) and `config` (`secret_file`) always consume it, so its workspace entry is used in every graph. A module whose consumers all belong to one profile carries that profile's marker; other modules stay as inert pure code. No module carries one today: `uri` lost its `outbound-auth` marker when `infra-bearerauthn` and the gRPC destination check in `config` began to call it | cargo-shear 1.14 reports an unused `[workspace.dependencies]` path entry unless `keep-path-dependencies` is set. A derived profile per module would remove the inert code at the initializer cost above. A consumer whose only uses sit inside a profile marker marks its dependency line the same way. | A service without a module's profiles keeps a few dozen lines of unused `pub` functions; they raise no warning and need no dependency. Reopen if no always-present crate consumes the crate: then set `keep-path-dependencies` or gate the crate on its consumers. |
| A secret file's value is its text without trailing `\n` and `\r` (`secret_file::value`), for `config --secrets-dir` and for the cache and PostgreSQL `password_file` | The readers had drifted: `--secrets-dir` stripped every trailing line break while `password_file` stripped one, so a file ending in `\n\n` gave two different passwords. Stripping all of them matches the loader's existing behavior and shell command substitution, which the official PostgreSQL image uses for `POSTGRES_PASSWORD_FILE` (`$(< file)` in `docker-entrypoint.sh`). Each owner keeps its own policy: `config` refuses a NUL byte, the providers an empty password. | A password that really ends in a line break cannot come from a file, as was already true for `--secrets-dir`. A `password_file` ending in more than one line break, or in a bare `\r`, now yields the password without them. Reopen if a supported provider needs a trailing line break in a secret. |
| `rfc6750::token` owns the Bearer `Authorization` grammar, including exactly one value, and `rfc7515::compact` the split of a compact JWS; `infra-bearerauthn` and `infra-serviceauth` map `NoBearer` and a missing part onto their own failure classes | The two parsers were declared twins in comments, kept apart only because `AUTHN=none` removes `infra-bearerauthn`; they accepted the same inputs. The shared version keeps `infra-bearerauthn`'s non-short-circuiting class check, which the compiler vectorizes on every authenticated request. | None beyond the move. |
| `otel_http::request_method` takes `&str`; `infra-http` passes `Method::as_str()` | A `match` on `http::Method` constants would need the `http` crate. Measured on Apple silicon (release, fat LTO, 200 million calls): 1.0 ns per call for the constant match, 1.9 ns for the string match. `infra-http` labels a request two or three times. | About 3 ns per request. Reopen if a profile of the request path shows the label. |
| `.jscpd.json` thresholds stay at 100 tokens and 15 lines; no new duplication gate | jscpd 5.4.2 on `crates/` at 13a298be: 10 clones at 100/15, 267 at 50/5, 1263 at 30/3, with 2, 22 and 87 of them across crates outside tests. `--similarity=0.9` reported 25 cross-crate pairs, about 20 of them alike only in shape (`as_str` matches, redacted `Debug`). No mode found the scope-token or method-label copies, which spell one rule with different tokens or fit on one line. The copies existed because no owner did. | A new copy of an extracted rule is caught only in review. Reopen when one appears: add an ast-grep rule for the rule's canonical spelling with `ignores:` for the owner file. |
| Endpoint URL text with any Unicode whitespace or control is refused before parsing (`uri::has_whitespace_or_control`), for the OAuth token URL in `config` and `infra-oauth2-client-credentials`, the gRPC destination in `config`, and the provider URLs of `infra-bearerauthn`, which also takes `uri::authority_has_userinfo` instead of its hand-written copy | Four copies had drifted: `config` and `infra-oauth2-client-credentials` refused every Unicode whitespace and control, `infra-bearerauthn` only ASCII ones. The WHATWG parser silently drops tabs, line breaks and edge controls, and percent-encodes the rest into a path the operator never wrote, so the stricter form is the one that keeps "the URL admitted is the URL written". | An issuer, JWKS, discovery or introspection URL holding U+00A0, U+0085, U+2028 or another non-ASCII whitespace or control is now refused at startup instead of being percent-encoded. Reopen if a supported provider publishes such a URL. |
| `rfc9110::media_type_is` owns the `Content-Type` match (type before `;`, optional whitespace trimmed, ASCII case ignored) for the OAuth token response and the authentication provider documents | `infra-oauth2-client-credentials` and `infra-bearerauthn` spelled it with `split_once` and `split(';').next()`; a header that `to_str` admits holds no whitespace but space and tab, so trimming those is the same as their `trim()`. Each caller keeps its policy for a missing or unreadable header. | None beyond the move. |
| `rfc7515::typ_subtype` reads a JWT `typ` as its `application/` subtype (RFC 7515 §4.1.9), for the access-token and generic-JWT checks of `infra-bearerauthn` and the configured service token type of `infra-serviceauth` | `infra-bearerauthn` listed both spellings of each type; `infra-serviceauth` stripped the prefix. No input was found on which they disagreed. | None beyond the move. |

## Left with their owners

These look alike across crates but fail the membership test:

- the token lifetime checks in `infra-bearerauthn` and `infra-serviceauth`:
  each owner sets its own clock allowance, and they bound the start
  differently (`nbf` alone, or the later of `iat` and `nbf`); what remains
  alike is two comparisons;
- the Unix-time readers in `infra-serviceauth`, `infra-bearerauthn` and
  `infra-webhooks`: each returns a different failure (`Option<u64>`,
  `Result<u64, _>`, `Option<i64>`);
- the connection-age spread in `infra-http`: a different formula and random
  source from `jitter`;
- single-call conversions such as `saturating_micros` and `whole_micros`, which
  exist to name one conversion and its reason.
- the `crit` header checks of `infra-bearerauthn` and `infra-serviceauth`
  (RFC 7515 §4.1.11): they read different header types, and they differ only
  on `"crit": []` and `"crit": null`, which a producer must not send;
  `infra-serviceauth` refuses both and `infra-bearerauthn` ignores them, and
  the RFC permits either;
- the W3C trace-context admission in `infra-messaging` and `infra-jobs`: the
  256- and 512-byte bounds are the service's own and have two callers, and the
  crates differ on an empty value and on truncating `tracestate`;
- the bounded payload writers of `infra-messaging` and `infra-jobs` and the
  byte counters of the messaging outbox and webhook enqueue: the service's own
  mechanism with two callers each, and different capacity floors;
- the `u16` sample draws before `jitter::share_of_window` and the `/10` window
  each caller takes: the random source and the window are caller policy;
- the `Bearer` header builders in `infra-oauth2-client-credentials` and
  `infra-serviceauth`: one `format!` around transport-specific `HeaderValue`
  handling;
- the PEM certificate readers of `infra-cache` and `infra-grpc`: they need
  `rustls`, which this crate must not carry;
- the configuration-to-adapter mappings in `service` and `jobs-worker`
  (sampler, log format, session budgets): neither `config` nor
  `infra-telemetry` may depend on the other.
