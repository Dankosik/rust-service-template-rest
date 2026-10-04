# T2 implementation result

```text
unit: T2
verdict: Implemented
candidate: crates/infra-http/src/observe.rs blob 002a082fdf8ee704905926c244418d2e27ff7b0c
provides: private borrowed HTTP span and status-label representations and emitted-value tests; display-name candidate rejected and removed
next_owner: root owns the remaining ordinary HTTP baseline comparison and final review recheck
```

Baseline: `observe.rs` blob `4cf33813623f368e62aaad0424d00719af62f7b8`,
preserved in the root-owned complete baseline archive recorded by the ledger.
The earlier hotpath annotations were already present and remain unchanged.

Production changes are confined to `observe.rs`:

- `method_attribute` borrows the nine existing standard method labels and
  retains exact owned extension-method text, including an actual `_OTHER` token.
- The protocol attribute receives the SDK's existing `Cow` directly.
- `STATUS_CODES` holds the 900 typed codes at static addresses; `status_label`
  uses their existing numeric rendering in the histogram label.
- Span naming uses its original eager `format!` followed by `.trim()`.
  The independently tested display-formatting candidate was removed after
  the RSS adoption failure described below. The method, protocol and metric
  changes remain independently removable.

The production request path, route ownership, response completion, access log,
status update and active-body lifetime retain their existing owners. No
dependency, public API, configuration, cache, production recorder or subscriber
change was introduced.

Test additions use the existing observation module. A local test recorder
checks actual histogram registration names and all three labels for every
numeric status 100..999; expected status strings come from integer formatting,
independent of the static table. This detects gaps, indexing errors and lost
labels that existing normal-status coverage does not cover. The existing
exported-span fixture exercises all standard methods, three extension tokens,
and all five public HTTP versions, checking exact exported names and values.
Existing matched/error and unmatched/absent-attribute tests remain the owners
of those cases. Tests add no production seams.

No local compiler, test, formatter, benchmark or executable reduction was run.
Root reported passing remote default/integration/profile compile diagnostics
and applied formatting, producing the original full candidate blob
`da5d891fae74dab9f472e735545fa27e487f9cb5`. Root subsequently reported correctness
checks passed but final review rejected its ordinary HTTP peak RSS adoption:
all six paired full-candidate peaks exceeded their baselines, including a
precommitted follow-up with disjoint ranges. Original evidence is retained by
root; allocated-byte savings do not override the RSS criterion.

Root isolated the name change in three ordinary HTTP pairs. The full candidate
had peaks 20.957..21.105 MiB; the variant restoring original name formatting
had peaks 20.871, 20.996 and 20.633 MiB. Full-candidate minus restored-name
paired differences were +0.08594, +0.10938 and +0.36719 MiB. On that evidence,
root selected independent rejection of the name-formatting candidate despite
its measured saving of approximately 16.3 allocated bytes and one allocation
per HTTP span. This repair removes only `name_route`, `separator`, and the
display macro field, restoring the original field. Cow attributes, the static
status table, tests and all other task changes remain intact.

The internal RSS mechanism is not established: subscriber debug-versus-string
visitors and their retained allocation capacity are a source-supported
hypothesis, not a measured explanation. Root owns three fresh paired ordinary
HTTP comparisons of this remaining candidate against the original baseline;
if RSS still fails, the status-table subchange is the next discriminator.
This implementation result makes no acceptance claim for the remaining
candidate. Writers stopped after this receipt; the T2 executor remains
available for final-validation repairs.
