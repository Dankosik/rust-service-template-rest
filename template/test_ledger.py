"""Unit tests for scripts/check-ledger.py."""

from __future__ import annotations

import importlib.util
import io
import sys
import tempfile
import textwrap
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path

sys.dont_write_bytecode = True

ROOT = Path(__file__).resolve().parents[1]


def _load_check_ledger():
    spec = importlib.util.spec_from_file_location("check_ledger", ROOT / "scripts/check-ledger.py")
    assert spec and spec.loader
    mod = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = mod
    spec.loader.exec_module(mod)
    return mod


check_ledger = _load_check_ledger()


def _write(path: Path, content: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(textwrap.dedent(content).lstrip("\n"), encoding="utf-8")


def _build_valid_multi_task_fixture(task_dir: Path) -> None:
    _write(
        task_dir / "spec.md",
        """
        # Specification: Rate limiting and audit hooks

        status: ready
        Basis: intent.md

        ## Behavior delta

        ### R1 — Enforce per-tenant rate limit
        Reject requests over quota with 429.

        ### R2 — Emit audit event on rejection
        Record structured audit event when rejected.

        ## Preserved guarantees

        - **G1 — Unthrottled requests unchanged:** Valid traffic under limit keeps returning 200.
        """,
    )
    _write(
        task_dir / "design/design.md",
        """
        # Technical Design: Rate limiter seam

        status: ready
        Basis: intent.md, spec.md

        ## Key decisions

        ### D1 — Shared limiter contract
        Pin RateLimitDecision in infra-http.

        ## Rust architecture, seams, and ownership

        ### S1 — RateLimitLayer seam
        Public middleware boundary in infra-http.
        """,
    )
    _write(
        task_dir / "tasks.md",
        """
        # Goal: Enforce per-tenant rate limit with audit hooks
        status: draft
        Completion: Over-quota HTTP requests return 429 and emit an audit event while G1 under-quota traffic stays 200.
        Global constraints: Pin header `X-RateLimit-Remaining` across T2 and T3.

        ## Tasks

        - [ ] T1: Define shared rate-limit decision contract [shared-contract]
          - Depends on: none
          - Packet: tasks/01-shared-contract.md
        - [ ] T2: Enforce per-tenant HTTP rate limit
          - Depends on: T1
          - Packet: tasks/02-http-limit.md
        - [ ] T3: Emit audit event on rejected requests
          - Depends on: T1, T2 [acceptance], release approval [effect:release]
          - Packet: tasks/03-audit-hook.md
        """,
    )
    _write(
        task_dir / "tasks/01-shared-contract.md",
        """
        # T1 — Define shared rate-limit decision contract
        Shape: shared-contract

        Outcome:
        Adds compiling `RateLimitDecision` and `RateLimitError` types in `infra-http`.

        Depends on:
        - none

        Sources:
        - design/design.md#d1-shared-limiter-contract — pinned decision type
        - design/design.md#s1-ratelimitlayer-seam — middleware seam
        - crates/infra-http/src/lib.rs — crate exports

        Provides:
        - `infra_http::rate_limit::RateLimitDecision` and `RateLimitError::Exceeded { retry_after: Duration }`

        Boundary:
        - In scope: contract types and exports
        - Out of scope: HTTP enforcement and audit emission

        Mutable owners:
        - crates/infra-http — rate_limit

        Exclusive locks:
        - crates/infra-http/src/lib.rs

        Final validation:
        - Observable claim: `infra_http::rate_limit::RateLimitDecision` compiles and rejects negative retry windows.
        - Additional proof: none

        Reopen if:
        - Uncached crate `governor@0.8` API differs from design assumption (Technical Design)
        """,
    )
    _write(
        task_dir / "tasks/02-http-limit.md",
        """
        # T2 — Enforce per-tenant HTTP rate limit

        Outcome:
        HTTP middleware rejects over-quota requests with 429 instead of forwarding them.

        Depends on:
        - T1 — consumes `RateLimitDecision`

        Sources:
        - spec.md#r1-enforce-per-tenant-rate-limit — 429 behavior
        - spec.md#preserved-guarantees — G1 under-quota requests stay 200

        Provides:
        - `infra_http::rate_limit::RateLimitLayer` wired into service router

        Boundary:
        - In scope: middleware and router wiring
        - Out of scope: audit event persistence

        Mutable owners:
        - crates/service — http_limit

        Exclusive locks:
        - crates/service/src/api.rs

        Final validation:
        - Observable claim: Over-quota HTTP call returns 429 with `X-RateLimit-Remaining: 0`; paired G1 holds.
        - Additional proof: none

        Reopen if:
        - none
        """,
    )
    _write(
        task_dir / "tasks/03-audit-hook.md",
        """
        # T3 — Emit audit event on rejected requests

        Outcome:
        Audit subscriber emits structured rejection event when rate-limit error fires.

        Depends on:
        - T1 — consumes `RateLimitError`
        - T2 [acceptance] — assembled 429 path emits audit event
        - release approval [effect:release] — external rollout approval

        Sources:
        - spec.md#r2-emit-audit-event-on-rejection — audit payload contract

        Provides:
        - none (leaf)

        Boundary:
        - In scope: audit subscriber and telemetry hook
        - Out of scope: external release execution

        Mutable owners:
        - crates/infra-observability — audit

        Exclusive locks:
        - none

        Final validation:
        - Observable claim: Rejected request emits `rate_limit.rejected` audit event (absent on base commit); paired G1 holds.
        - Additional proof: none

        Reopen if:
        - none
        """,
    )


class CheckLedgerTests(unittest.TestCase):
    def setUp(self) -> None:
        self.root = Path(self.enterContext(tempfile.TemporaryDirectory(prefix="ledger-check-")))
        self.task_dir = self.root / "specs/rate-limit"

    def test_valid_multi_task_ledger_computes_parallel_waves_and_anchors(self) -> None:
        _build_valid_multi_task_fixture(self.task_dir)
        report = check_ledger.check_ledger(self.task_dir)
        self.assertEqual(report.errors, [])
        self.assertEqual(report.warnings, [])
        self.assertEqual(report.status, "draft")
        self.assertEqual(report.task_count, 3)
        self.assertEqual(report.upstream_statuses, {"spec.md": "ready", "design/design.md": "ready"})
        self.assertEqual([w.tasks for w in report.waves], [["T1"], ["T2", "T3"]])
        self.assertEqual(report.anchor_counts["spec R"], (2, 2, []))
        self.assertEqual(report.anchor_counts["spec G"], (1, 1, []))
        self.assertEqual(report.anchor_counts["design D"], (1, 1, []))
        self.assertEqual(report.anchor_counts["design S"], (1, 1, []))
        self.assertEqual(len(report.reopen_risks), 1)

        rendered = check_ledger.format_report(report)
        self.assertIn("Wave 1 (Ready Frontier): T1", rendered)
        self.assertIn("Wave 2: T2, T3 [parallel]", rendered)
        self.assertIn("Anchor coverage: spec R=2/2; spec G=1/1; design D=1/1; design S=1/1", rendered)

    def test_shared_lock_and_dir_prefix_and_owner_variants_serialize_frontier(self) -> None:
        _build_valid_multi_task_fixture(self.task_dir)
        t2_path = self.task_dir / "tasks/02-http-limit.md"
        t3_path = self.task_dir / "tasks/03-audit-hook.md"

        # Directory lock without trailing slash overlaps child file path.
        t2_path.write_text(
            t2_path.read_text(encoding="utf-8").replace(
                "Exclusive locks:\n- crates/service/src/api.rs",
                "Exclusive locks:\n- migrations",
            ),
            encoding="utf-8",
        )
        t3_path.write_text(
            t3_path.read_text(encoding="utf-8").replace(
                "Exclusive locks:\n- none",
                "Exclusive locks:\n- migrations/0042_audit.sql",
            ),
            encoding="utf-8",
        )
        lock_report = check_ledger.check_ledger(self.task_dir)
        self.assertEqual(lock_report.errors, [])
        self.assertEqual([w.tasks for w in lock_report.waves], [["T1"], ["T2"], ["T3"]])
        self.assertIn("Exclusive lock: migrations overlaps migrations/0042_audit.sql", lock_report.waves[1].serializations[0][2])

        # Overlapping Mutable owners with backticks and crate-prefix variants serialize even with locks=none.
        t2_path.write_text(
            t2_path.read_text(encoding="utf-8")
            .replace("Exclusive locks:\n- migrations", "Exclusive locks:\n- none")
            .replace("Mutable owners:\n- crates/service — http_limit", "Mutable owners:\n- `crates/service` — `http_limit`"),
            encoding="utf-8",
        )
        t3_path.write_text(
            t3_path.read_text(encoding="utf-8")
            .replace("Exclusive locks:\n- migrations/0042_audit.sql", "Exclusive locks:\n- none")
            .replace("Mutable owners:\n- crates/infra-observability — audit", "Mutable owners:\n- service::http_limit"),
            encoding="utf-8",
        )
        owner_report = check_ledger.check_ledger(self.task_dir)
        self.assertEqual(owner_report.errors, [])
        self.assertEqual([w.tasks for w in owner_report.waves], [["T1"], ["T2"], ["T3"]])
        self.assertIn("Mutable owner: service", owner_report.waves[1].serializations[0][2])

    def test_rejects_unready_upstream_and_leftover_workflow_plan(self) -> None:
        _build_valid_multi_task_fixture(self.task_dir)
        _write(self.task_dir / "intent.md", "# Intent: Rate limiting\n\nstatus: draft\n")
        (self.task_dir / "spec.md").write_text(
            (self.task_dir / "spec.md").read_text(encoding="utf-8").replace("status: ready", "status: draft"),
            encoding="utf-8",
        )
        (self.task_dir / "tasks.md").write_text(
            (self.task_dir / "tasks.md").read_text(encoding="utf-8").replace("status: draft", "status: ready"),
            encoding="utf-8",
        )
        _write(self.task_dir / "workflow-plan.md", "# Pre-ledger notes\n")

        report = check_ledger.check_ledger(self.task_dir)
        joined = "\n".join(report.errors)
        self.assertIn("upstream artifact intent.md has status 'draft'", joined)
        self.assertIn("upstream artifact spec.md has status 'draft'", joined)
        self.assertIn("workflow-plan.md must be deleted", joined)

    def test_validates_intent_status_and_decision_anchor_references(self) -> None:
        _build_valid_multi_task_fixture(self.task_dir)
        _write(
            self.task_dir / "intent.md",
            """
            # Intent: Enforce per-tenant rate limit

            status: ready

            ## Settled decisions

            ### I1 — Per-tenant 429 behavior

            - **Binding:** `hard`
            - **Decision:** Reject requests over quota with 429.
            - **Why:** Prevents noisy neighbors from starving other tenants.
            """,
        )
        spec_path = self.task_dir / "spec.md"
        spec_path.write_text(
            spec_path.read_text(encoding="utf-8")
            + "\nTraces to `intent.md#i1-per-tenant-429-behavior`.\n",
            encoding="utf-8",
        )
        ok_report = check_ledger.check_ledger(self.task_dir)
        self.assertEqual(ok_report.errors, [])
        self.assertEqual(ok_report.upstream_statuses["intent.md"], "ready")

        # Draft intent.md and broken intent.md#i99 anchor must both fail.
        (self.task_dir / "intent.md").write_text(
            (self.task_dir / "intent.md").read_text(encoding="utf-8").replace("status: ready", "status: draft"),
            encoding="utf-8",
        )
        t2_path = self.task_dir / "tasks/02-http-limit.md"
        t2_path.write_text(
            t2_path.read_text(encoding="utf-8").replace(
                "- spec.md#r1-enforce-per-tenant-rate-limit — 429 behavior",
                "- spec.md#r1-enforce-per-tenant-rate-limit — 429 behavior\n"
                "- intent.md#i99-deleted-decision — stale reference",
            ),
            encoding="utf-8",
        )
        bad_report = check_ledger.check_ledger(self.task_dir)
        joined = "\n".join(bad_report.errors)
        self.assertIn("upstream artifact intent.md has status 'draft'", joined)
        self.assertIn("T2: references unknown intent.md decision anchor I99", joined)

        # Removing intent.md while references remain must also report dangling anchors.
        (self.task_dir / "intent.md").unlink()
        missing_intent_report = check_ledger.check_ledger(self.task_dir)
        missing_joined = "\n".join(missing_intent_report.errors)
        self.assertIn("spec.md: references intent.md#I1, but intent.md is missing", missing_joined)
        self.assertIn("T2: references intent.md#I99, but intent.md is missing", missing_joined)

    def test_rejects_cycle_distinctly_from_non_topological_order(self) -> None:
        # Non-topological order without any cycle.
        _build_valid_multi_task_fixture(self.task_dir)
        tasks_path = self.task_dir / "tasks.md"
        _write(
            tasks_path,
            """
            # Goal: Out-of-order acyclic graph
            status: draft
            Completion: Both R1, R2, G1, D1, S1 verified.

            ## Tasks

            - [ ] T2: Enforce per-tenant HTTP rate limit
              - Depends on: T1
              - Packet: tasks/02-http-limit.md
            - [ ] T1: Define shared rate-limit decision contract [shared-contract]
              - Depends on: none
              - Packet: tasks/01-shared-contract.md
            - [ ] T3: Emit audit event on rejected requests
              - Depends on: T1, T2 [acceptance], release approval [effect:release]
              - Packet: tasks/03-audit-hook.md
            """,
        )
        topo_report = check_ledger.check_ledger(self.task_dir)
        topo_joined = "\n".join(topo_report.errors)
        self.assertIn("non-topological order (T2 depends on T1", topo_joined)
        self.assertNotIn("cyclic dependency", topo_joined)

        # Actual cycle.
        _build_valid_multi_task_fixture(self.task_dir)
        tasks_path.write_text(
            tasks_path.read_text(encoding="utf-8").replace("- Depends on: none", "- Depends on: T2"),
            encoding="utf-8",
        )
        t1_path = self.task_dir / "tasks/01-shared-contract.md"
        t1_path.write_text(
            t1_path.read_text(encoding="utf-8").replace("Depends on:\n- none", "Depends on:\n- T2 — cycle"),
            encoding="utf-8",
        )
        cycle_report = check_ledger.check_ledger(self.task_dir)
        cycle_joined = "\n".join(cycle_report.errors)
        self.assertIn("cyclic dependency detected", cycle_joined)
        self.assertIn("initial Ready Frontier is empty", cycle_joined)

    def test_rejects_parity_drift_orphans_bad_locks_and_leaf_producer(self) -> None:
        _build_valid_multi_task_fixture(self.task_dir)
        _write(self.task_dir / "tasks/99-orphan.md", "# T99 — Stray\n")
        t1_path = self.task_dir / "tasks/01-shared-contract.md"
        t1_text = t1_path.read_text(encoding="utf-8")
        t1_text = t1_text.replace("Shape: shared-contract", "Shape: prefactor")
        t1_text = t1_text.replace(
            "Provides:\n- `infra_http::rate_limit::RateLimitDecision` and `RateLimitError::Exceeded { retry_after: Duration }`",
            "Provides:\n- none (leaf)",
        )
        t1_text = t1_text.replace(
            "Exclusive locks:\n- crates/infra-http/src/lib.rs",
            "Exclusive locks:\n- crates/infra-http/src/lib.rs:42",
        )
        t1_path.write_text(t1_text, encoding="utf-8")

        # Depends on parity drift on T2.
        t2_path = self.task_dir / "tasks/02-http-limit.md"
        t2_path.write_text(
            t2_path.read_text(encoding="utf-8").replace("- T1 — consumes `RateLimitDecision`", "- none"),
            encoding="utf-8",
        )

        report = check_ledger.check_ledger(self.task_dir)
        joined = "\n".join(report.errors)
        self.assertIn("orphan packet file", joined)
        self.assertIn("Shape 'prefactor' does not match tasks.md shape tag 'shared-contract'", joined)
        self.assertIn("downstream tasks depend on T1, so Provides cannot be 'none'", joined)
        self.assertIn("must be a canonical repo-relative path", joined)
        self.assertIn("02-http-limit.md: Depends on [] does not match tasks.md ['T1']", joined)

    def test_bare_links_markdown_links_and_anchor_coverage_rules(self) -> None:
        _build_valid_multi_task_fixture(self.task_dir)
        # Valid Markdown links in Packet: and anchored Sources: + prose mentioning spec.md after ' - ' must PASS.
        tasks_path = self.task_dir / "tasks.md"
        tasks_path.write_text(
            tasks_path.read_text(encoding="utf-8").replace(
                "- Packet: tasks/01-shared-contract.md",
                "- Packet: [tasks/01-shared-contract.md](tasks/01-shared-contract.md)",
            ),
            encoding="utf-8",
        )
        t2_path = self.task_dir / "tasks/02-http-limit.md"
        t2_path.write_text(
            t2_path.read_text(encoding="utf-8").replace(
                "- spec.md#r1-enforce-per-tenant-rate-limit — 429 behavior",
                "- [spec.md](../spec.md#r1-enforce-per-tenant-rate-limit) - mirrors spec.md 429 rule",
            ),
            encoding="utf-8",
        )
        ok_report = check_ledger.check_ledger(self.task_dir)
        self.assertEqual(ok_report.errors, [])

        # Bare relative '../spec.md', bare '../design/design.md', uncovered R2/D1/S1, and prose 'R1 - 2 retry' non-range.
        t1_path = self.task_dir / "tasks/01-shared-contract.md"
        t1_path.write_text(
            t1_path.read_text(encoding="utf-8")
            .replace("- design/design.md#d1-shared-limiter-contract — pinned decision type", "- ../design/design.md — whole design")
            .replace("- design/design.md#s1-ratelimitlayer-seam — middleware seam\n", ""),
            encoding="utf-8",
        )
        t2_path.write_text(
            t2_path.read_text(encoding="utf-8").replace(
                "- spec.md#preserved-guarantees — G1 under-quota requests stay 200",
                "- spec.md#r1-enforce-per-tenant-rate-limit — R1 - 2 retry budget",
            ),
            encoding="utf-8",
        )
        t3_path = self.task_dir / "tasks/03-audit-hook.md"
        t3_path.write_text(
            t3_path.read_text(encoding="utf-8").replace(
                "- spec.md#r2-emit-audit-event-on-rejection — audit payload contract",
                "- ../spec.md — full specification",
            ),
            encoding="utf-8",
        )
        # Also remove G1 from tasks.md/T2/T3 to test spec G warning.
        tasks_path.write_text(
            tasks_path.read_text(encoding="utf-8").replace("while G1 under-quota", "while under-quota"),
            encoding="utf-8",
        )
        for p in (t2_path, t3_path):
            p.write_text(p.read_text(encoding="utf-8").replace("paired G1 holds", "paired guarantee holds"), encoding="utf-8")

        bad_report = check_ledger.check_ledger(self.task_dir)
        joined = "\n".join(bad_report.errors)
        self.assertIn("T3: Sources cites bare 'spec.md'", joined)
        self.assertIn("T1: Sources cites bare 'design.md'", joined)
        self.assertIn("uncovered spec.md behavior rules", joined)
        self.assertIn("R2", joined)
        self.assertIn("uncovered design/design.md decisions", joined)
        self.assertIn("D1", joined)
        self.assertIn("uncovered design/design.md seams", joined)
        self.assertIn("S1", joined)
        self.assertIn("unreferenced spec.md preserved guarantees", "\n".join(bad_report.warnings))
        self.assertIn("G1", "\n".join(bad_report.warnings))

    def test_external_t_gate_trailing_checklist_ergonomics_and_integrated_waves(self) -> None:
        _build_valid_multi_task_fixture(self.task_dir)
        tasks_path = self.task_dir / "tasks.md"
        tasks_path.write_text(
            tasks_path.read_text(encoding="utf-8")
            .replace("release approval [effect:release]", "Terraform [effect:apply]")
            .replace("- [ ] T1:", "- [x] T1:"),
            encoding="utf-8",
        )
        t1_path = self.task_dir / "tasks/01-shared-contract.md"
        t1_path.write_text(
            t1_path.read_text(encoding="utf-8").replace("Shape: shared-contract", "Shape: [shared-contract]"),
            encoding="utf-8",
        )
        t2_path = self.task_dir / "tasks/02-http-limit.md"
        t2_path.write_text(
            t2_path.read_text(encoding="utf-8").replace("Reopen if:\n- none", "Reopen if:\n- `none.`")
            + "\n## Lead Checklist\n- [ ] Wire middleware\n- [ ] Check headers\n",
            encoding="utf-8",
        )
        t3_path = self.task_dir / "tasks/03-audit-hook.md"
        t3_path.write_text(
            t3_path.read_text(encoding="utf-8").replace(
                "release approval [effect:release]",
                "Terraform [effect:apply]",
            ),
            encoding="utf-8",
        )
        report = check_ledger.check_ledger(self.task_dir)
        self.assertEqual(report.errors, [])
        self.assertEqual(report.integrated_tasks, ["T1"])
        self.assertEqual([w.tasks for w in report.waves], [["T2", "T3"]])
        self.assertEqual([tid for tid, _ in report.reopen_risks], ["T1"])

    def test_blocked_stop_rules_and_cli_exit_codes(self) -> None:
        _build_valid_multi_task_fixture(self.task_dir)
        tasks_path = self.task_dir / "tasks.md"

        # Blocked stop is forbidden when status != blocked.
        tasks_path.write_text(
            tasks_path.read_text(encoding="utf-8").replace(
                "status: draft",
                "status: draft\nBlocked stop: Stale blocker",
            ),
            encoding="utf-8",
        )
        bad_draft = check_ledger.check_ledger(self.task_dir)
        self.assertIn("'Blocked stop:' must be omitted when status is 'draft'", "\n".join(bad_draft.errors))

        # Blocked status requires Blocked stop and permits unready upstream.
        _build_valid_multi_task_fixture(self.task_dir)
        (self.task_dir / "design/design.md").write_text(
            (self.task_dir / "design/design.md").read_text(encoding="utf-8").replace("status: ready", "status: blocked"),
            encoding="utf-8",
        )
        tasks_path.write_text(
            tasks_path.read_text(encoding="utf-8").replace("status: draft", "status: blocked"),
            encoding="utf-8",
        )
        missing_stop = check_ledger.check_ledger(self.task_dir)
        self.assertIn("Blocked stop:", "\n".join(missing_stop.errors))

        tasks_path.write_text(
            tasks_path.read_text(encoding="utf-8").replace(
                "status: blocked",
                "status: blocked\nBlocked stop: Governor API mismatch (Technical Design)",
            ),
            encoding="utf-8",
        )
        valid_blocked = check_ledger.check_ledger(self.task_dir)
        self.assertEqual(valid_blocked.errors, [])

        # CLI exit codes & fresh draft runtime-field rejection.
        _build_valid_multi_task_fixture(self.task_dir)
        stdout = io.StringIO()
        stderr = io.StringIO()
        with redirect_stdout(stdout), redirect_stderr(stderr):
            code = check_ledger.main([str(self.task_dir / "tasks.md")])
        self.assertEqual(code, 0)
        self.assertIn("Execution waves:", stdout.getvalue())

        tasks_path.write_text(
            tasks_path.read_text(encoding="utf-8").replace("status: draft", "status: draft\nBase: deadbeef"),
            encoding="utf-8",
        )
        stdout = io.StringIO()
        stderr = io.StringIO()
        with redirect_stdout(stdout), redirect_stderr(stderr):
            code = check_ledger.main([str(self.task_dir)])
        self.assertEqual(code, 1)
        self.assertIn("fresh draft ledger must omit Orchestrator runtime fields", stderr.getvalue())


if __name__ == "__main__":
    unittest.main()
