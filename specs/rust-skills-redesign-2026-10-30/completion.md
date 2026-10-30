# Local instruction-system validation

The assembled 16-method system passes its required mechanical checks and all
three executable examples. Independent final review passed after one bounded
policy-preservation repair. These results
establish local instruction consistency and example validity; they do not
establish native skill loading, educational benefit or improved model behavior.

Candidate: `candidate.json`, SHA-256
`9b1ff2e307bb5f25eb63737d2dee256508a59c60d17454c3e1ad3c4228f2720d`.
The initial manifest was
`e4abfc8488734e058ff29ea3b0a15530541a74f82befeb84b205fcb90eba6f11`;
all 109 file/symlink/absence records matched before initial final validation.
Base: `38a3723660b71849be222397699625ef5c308473`. All mutable lanes had returned
and stopped before this boundary. The bounded review repair below changed only
the existing Logging policy owner; all unaffected proof is reused.

## Execution provenance

The root was the sole ledger writer; `/root/implementation_lead` owned
implementation assembly and, after a separate assignment, global Completion.
Four fresh `worker-agent` lanes used native `gpt-6-astra`, `high`, in disjoint
writable scopes. Each returned a terminal implemented result without descendants,
per-lane checks or review before assembly:

| Worker | Exclusive implementation scope |
| --- | --- |
| `/root/implementation_lead/core_outcomes` | New rust-design and representation reference; rust-errors and outcome reference; removal of coder, idiomatic and structural-quality directories |
| `/root/implementation_lead/runtime_evidence` | Tokio, reliability, testing, verification, debugging and performance methods |
| `/root/implementation_lead/boundaries_platform` | API contract, Axum, config, observability, security, SQLx, dependencies and delivery-platform methods |
| `/root/implementation_lead/format_consumers` | Authoring/validation format, Python checker, help text, AGENTS routing and retained release-profile rule, two workflow consumers, review carrier, neighbor contract and classifier samples |

The Lead alone integrated the otherwise uncovered telemetry rules into the
existing Configuration Source Policy, generated selected views using
`/opt/homebrew/bin/rtk proxy make claude-skills-sync qwen-skills-sync` (26 links
per view), and recorded candidate/proof/review evidence. Mandatory policy was
traced to AGENTS, architecture, lifecycle, configuration, persistence,
contribution and validation owners before duplicated method text was removed.
The final review exposed and closed the remaining general privacy omission.
The older bounded-executor model names in the adapter were unavailable in the
callable catalog; supported Astra was used without substituting newer
Luna/Terra models. Only the format lane's `ast.parse` ran as coding feedback.

## Executed proof

Commands ran from `/Users/daniil/Projects/Opensource/rust-service-template-rest`.
The prefix `/opt/homebrew/bin/rtk proxy` retained exact tool output. Container
checks used their existing validation-lock targets serially. Each native
yield used an early 1–10 second observation checkpoint; yielded sessions were
joined to a terminal result before their proof was recorded.

| Exact command after the prefix | Terminal result |
| --- | --- |
| `make check-instructions` | Exit 0; 26 skills: 17 decision, 9 workflow, 0 service-owned; 5 canonical roles; Codex runtime and Claude/Qwen views current |
| `git diff --check` | Exit 0; no whitespace errors |
| `make changed-surfaces-check` | Exit 0; existing classifier self-test |
| `make docs-check` | Exit 0; image-input coverage passed; offline lychee: 3250 total, 1712 unique, 2153 OK, 0 errors, 1097 excluded |
| `make shellcheck SHELL_FILES=scripts/ci/changed-surfaces.sh` | Exit 0; only the changed shell surface |
| `/Users/daniil/.cargo/bin/rustdoc --version` | `rustdoc 1.99.0 (b940084d7 2026-09-28)`; matches `rust-toolchain.toml` |
| `/Users/daniil/.cargo/bin/rustdoc --help` | Exit 0; native `--test` and `--edition` options confirmed |
| `/Users/daniil/.cargo/bin/rustdoc --test --edition=2024 .agents/skills/rust-design/references/representation-examples.md` | Exit 0; 2 passed, 0 failed, 0 ignored, 0 filtered; 0.78 seconds including compilation |
| `/Users/daniil/.cargo/bin/rustdoc --test --edition=2024 .agents/skills/rust-errors/references/outcome-examples.md` | Exit 0; 1 passed, 0 failed, 0 ignored, 0 filtered; 0.35 seconds including compilation |

The first reference's SHA-256 is
`87a2fc43ba2625bb65723786da83ac95f4e24893d0d0a81f6a85ef3318bc582a`;
the second is
`8c6c45a325fc6646097ddb43eef23ef448dfb4326014de6a061345eb82e597c7`.
The assertions cover validated construction and mutation, equality/order/hash
coherence, collection behavior and optional-versus-required parsing. They ran
as native Markdown doctests, independently of the toolchain-free instruction
checker. No Cargo package, example runner or new CI gate was introduced.

## Focused checker observation

The format lane selected this ephemeral probe under `test-audit`; it ran once
at the assembled final boundary and exited 0 with
`Decision body and reference shape boundaries passed`. Its temporary directory
was removed by `TemporaryDirectory`; no permanent fixture or test seam was added.
The intentionally missing Markdown link demonstrates that link resolution
remains outside this shape checker and under `docs-check`.

```sh
/opt/homebrew/bin/rtk proxy python3 - <<'PY'
import runpy
import tempfile
from pathlib import Path
m = runpy.run_path('scripts/check-skills.py')
body = m['check_decision_body']
valid = [
    '# Probe\n\nShort.\n## Detail\n- [owner](missing.md)\n\n| A | B |\n| - | - |\n| x | y |\n',
    '# Probe\n\n' + 'word ' * 600,
    '# Probe\n```rust\n# hidden Rust setup\nassert!(true);\n```\n',
    '# Probe\n~~~text\n# inside tilde fence\n~~~\n',
    '# Probe\n````text\n```\n# still fenced\n````\n',
]
for text in valid:
    assert not body('rust-probe', text), body('rust-probe', text)
for text in ('', 'Text first\n# Probe\nBody', '# Probe\n', '# Probe\nBody\n# Duplicate\nMore'):
    assert body('rust-probe', text), repr(text)
with tempfile.TemporaryDirectory(prefix='skill-shape-') as tmp:
    skill = Path(tmp) / 'rust-probe'
    skill.mkdir()
    (skill / 'LICENSE').write_bytes(m['ROOT_LICENSE'].read_bytes())
    (skill / 'SKILL.md').write_text('---\nname: rust-probe\ndescription: "Use for shape verification."\n---\n' + valid[0])
    refs = skill / 'references'
    refs.mkdir()
    (refs / 'example.md').write_text('# Example\n\nContext.\n')
    assert not m['check'](skill, None), m['check'](skill, None)
    unsupported = refs / 'example.py'
    unsupported.write_text('pass\n')
    assert any('only references/*.md' in p for p in m['check'](skill, None))
    unsupported.unlink()
    (refs / 'nested').mkdir()
    assert any('only references/*.md' in p for p in m['check'](skill, None))
print('Decision body and reference shape boundaries passed')
PY
```

## Identity and preservation

The manifest records were compared to current bytes, symlink targets and absent
paths by a one-shot standard-library Python observation. Enumerating
`.agents/skills/rust-*/SKILL.md` returned exactly 16 methods. This search
returned exit 1 and no matches, consistent with no retired names in the live
owners it inspected:

```sh
/opt/homebrew/bin/rtk proxy rg -n 'rust-(coder|idiomatic|structural-quality)' AGENTS.md .agents .claude .qwen .codex docs scripts make --glob '!roadmap.md' --glob '!research/**' --glob '!consumer-lifecycle/**'
```

The historical bundle was read using the exact algorithm in
`planning-review.md`: sorted `rglob('*')`, symlink target bytes without following,
regular-file bytes, relative path + NUL + kind + NUL + SHA-256 digest per entry.
It still contains **535 entries, 1,728,795 regular-file bytes**, aggregate SHA-256
`e814e77f451f5e912dc73534c75c52999233df76b493816a248e2f7f7695978e`.
No historical references or bundle bytes were rewritten.

## Review and claim limits

Fresh reviewer `/root/implementation_lead/final_review` uses native
`reviewer-agent`, `gpt-6-astra`, `high`, without inherited turns, against the
fixed manifest and accepted Specification/Design. It received all terminal
check results and owns an independent static falsification of the complete
changed instruction system. Its first verdict was FAIL with one TASK_DEFECT:
the old observability method's universal privacy prohibition had not survived
in a mandatory canonical owner. A bounded non-secret body in a new internal span
was the concrete instruction-level falsifier; no runtime leak was claimed.

After the reviewer returned and stopped, the Lead repaired its own existing
`docs/configuration-source-policy.md#logging` scope: every telemetry field now
explicitly excludes secrets, tokens, bodies and personal data, including new
application signals outside built-in HTTP, SDK and panic instrumentation. No
interface, acceptance boundary or risk surface expanded. The source manifest
was refreshed to the current hash above. `git diff --check` passed again;
`/opt/homebrew/bin/rtk proxy make -s docs-check` passed with 3250 total, 1712
unique, 2153 OK, 0 errors and 1097 excluded. The same reviewer's bounded delta
recheck returned PASS with no findings; its retained receipt is
`implementation-review.md`. The initial docs receipt is superseded for this
file; checker, classifier, shell and example inputs remain unchanged and their
passing results are reused. All readers and command sessions have returned.

Comparative evaluation remains the already-delivered future plan. No model
comparison, experimental fixture suite, runtime build, new dependency, remote
write, commit, push, PR, merge, deployment or global setting change occurred.
Workflow invocation and existing release gates remain outside the rewrite.

```text
unit: Completion
verdict: Accepted
candidate: candidate.json sha256 9b1ff2e307bb5f25eb63737d2dee256508a59c60d17454c3e1ad3c4228f2720d
evidence: required local instruction, links, focused checker, classifier, scoped shell and three native Markdown doctests passed; 16 Rust methods and selected views agree; historical comparison identity preserved
review: independent full-system review plus one bounded delta recheck; final PASS, no surviving findings
invalidated_receipts: initial docs-check for the repaired Logging policy, superseded by passing rerun; initial review FAIL superseded by bounded PASS
next_owner: root Ledger Orchestrator records final acceptance and task closeout
```

## Closeout

The root recorded Completion as Accepted on the fixed candidate above after all
writers, readers and command sessions stopped. The execution-only ledger and
task packet were retired under the repository's Cleanup owner; no remaining
Markdown link depends on them. This receipt retains their required execution
provenance and final outcome. Intent, specification, design/dispositions,
baseline identities, review evidence and the deferred evaluation plan remain
durable decision and recovery inputs for the later evaluation. Historical
comparison artifacts, unrelated files and shared resources were preserved.
