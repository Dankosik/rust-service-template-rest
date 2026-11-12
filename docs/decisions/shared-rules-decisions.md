# Shared rules decisions

Recorded 2026-11-12. `crates/shared-rules` owns rules that several crates must
apply identically when they have no other common owner; its crate
documentation holds the membership test. This record keeps why the rules live
there, what was rejected, and when to revisit.

## Selection and cost

| Decision | Alternative and decisive evidence | Accepted cost and reopen condition |
| --- | --- | --- |
| One always-present crate with modules named after the authority behind each rule (`rfc6749`, `rfc6750`, `rfc7515`, `rfc7519`, `uri`, `otel_http`, `error_chain`, `jitter`, `secret_file`) | Twenty-one copies of nine rules lived in ten crates because optional profile crates cannot depend on each other or on `config`, and copies of one rule had already drifted (the secret-file row below). Rejected: documented twins (each rule kept N copies and N tests); modules in `operation-context`, `infra-http`, `infra-telemetry` or `config` (new provider edges into the inbound HTTP stack, the OpenTelemetry SDK or the loader, and wider crate charters); one crate per topic (one or two functions each); one source file included by `#[path]` (no single owner for readers or tooling); a derived template profile per module (each touches five or six initializer files). rust-analyzer's `stdx` and Materialize's `mz_ore` keep such code in one crate with a one-sentence charter. | The crate grows by review against its membership test, not by a hard limit. Reopen as a thematic split when several modules change for one reason or the crate passes about ten modules. |
| No dependencies beyond `std`: the `shared-rules` role is `std_only` in `quality/architecture.json`, and `architecture-check` refuses every non-dev edge. When a rule sits inside dependency glue, only the rule moves: the `NumericDate` serde visitors of `infra-bearerauthn` and `infra-serviceauth` stay with them and call `rfc7519::numeric_date_seconds` | A catch-all crate's main cost is the dependencies it attracts (Go blog "Package names"; matklad, "Fast Rust Builds"). `mz_ore` makes any external crate optional and feature-gated. | The two visitors still repeat a dozen lines of serde scaffolding. Reopen when a rule cannot be separated from a crate's types: give that module a feature of its own name with an optional dependency rather than a required one. |
| The crate exists in every template projection; `infra-http` (`otel_http`) and `config` (`secret_file`) always consume it, so its workspace entry is used in every graph. A module whose consumers all belong to one profile carries that profile's marker (`uri`, `outbound-auth`); other modules stay as inert pure code | cargo-shear 1.14 reports an unused `[workspace.dependencies]` path entry unless `keep-path-dependencies` is set. A derived profile per module would remove the inert code at the initializer cost above. A consumer whose only uses sit inside a profile marker marks its dependency line the same way. | A service without a module's profiles keeps a few dozen lines of unused `pub` functions; they raise no warning and need no dependency. Reopen if no always-present crate consumes the crate: then set `keep-path-dependencies` or gate the crate on its consumers. |
| A secret file's value is its text without trailing `\n` and `\r` (`secret_file::value`), for `config --secrets-dir` and for the cache and PostgreSQL `password_file` | The readers had drifted: `--secrets-dir` stripped every trailing line break while `password_file` stripped one, so a file ending in `\n\n` gave two different passwords. Stripping all of them matches the loader's existing behavior and shell command substitution, which the official PostgreSQL image uses for `POSTGRES_PASSWORD_FILE` (`$(< file)` in `docker-entrypoint.sh`). Each owner keeps its own policy: `config` refuses a NUL byte, the providers an empty password. | A password that really ends in a line break cannot come from a file, as was already true for `--secrets-dir`. A `password_file` ending in more than one line break, or in a bare `\r`, now yields the password without them. Reopen if a supported provider needs a trailing line break in a secret. |
| `rfc6750::token` owns the Bearer `Authorization` grammar, including exactly one value, and `rfc7515::compact` the split of a compact JWS; `infra-bearerauthn` and `infra-serviceauth` map `NoBearer` and a missing part onto their own failure classes | The two parsers were declared twins in comments, kept apart only because `AUTHN=none` removes `infra-bearerauthn`; they accepted the same inputs. The shared version keeps `infra-bearerauthn`'s non-short-circuiting class check, which the compiler vectorizes on every authenticated request. | None beyond the move. |
| `otel_http::request_method` takes `&str`; `infra-http` passes `Method::as_str()` | A `match` on `http::Method` constants would need the `http` crate. Measured on Apple silicon (release, fat LTO, 200 million calls): 1.0 ns per call for the constant match, 1.9 ns for the string match. `infra-http` labels a request two or three times. | About 3 ns per request. Reopen if a profile of the request path shows the label. |
| `.jscpd.json` thresholds stay at 100 tokens and 15 lines; no new duplication gate | jscpd 5.4.2 on `crates/` at 13a298be: 10 clones at 100/15, 267 at 50/5, 1263 at 30/3, with 2, 22 and 87 of them across crates outside tests. `--similarity=0.9` reported 25 cross-crate pairs, about 20 of them alike only in shape (`as_str` matches, redacted `Debug`). No mode found the scope-token or method-label copies, which spell one rule with different tokens or fit on one line. The copies existed because no owner did. | A new copy of an extracted rule is caught only in review. Reopen when one appears: add an ast-grep rule for the rule's canonical spelling with `ignores:` for the owner file. |

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
