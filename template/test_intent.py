"""Unit tests for scripts/check-intent.py and the Makefile intent-check target."""

from __future__ import annotations

import importlib.util
import io
import shutil
import subprocess
import sys
import tempfile
import textwrap
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path

sys.dont_write_bytecode = True

ROOT = Path(__file__).resolve().parents[1]


def _load_check_intent():
    spec = importlib.util.spec_from_file_location("check_intent", ROOT / "scripts/check-intent.py")
    assert spec and spec.loader
    mod = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = mod
    spec.loader.exec_module(mod)
    return mod


check_intent = _load_check_intent()


def _write(path: Path, content: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(textwrap.dedent(content).lstrip("\n"), encoding="utf-8")


def _valid_ready_intent() -> str:
    return textwrap.dedent(
        """
        # Intent: Reject duplicate order creation conflicts deterministically

        status: ready

        ## Problem

        Callers retrying `POST /v1/orders` with a modified body receive a silent cached 200
        today (`crates/service/src/api.rs`).

        ## Desired outcome

        Conflicting idempotency replays fail deterministically with `409 Conflict` while
        identical replays keep returning the stored result.

        ## Affected actors and surfaces

        - Mobile checkout client — `POST /v1/orders` (`api/openapi/service.yaml`)

        ## Settled decisions

        ### I1 — Conflicting replay status code

        - **Binding:** `hard`
        - **Decision:** Return `409 Conflict` with error code `idempotency_conflict`.
        - **Rejected:** Returning `400 Bad Request` (client already handles `409` retries).
        - **Why:** External mobile clients distinguish conflict retries from validation bugs.

        ### I2 — Default retention window

        - **Binding:** `default`
        - **Decision:** Keep idempotency keys for 24 hours.
        - **Rejected:** 7-day retention (unneeded storage cost for short checkout flows).
        - **Why:** Matches sibling endpoint convention unless storage budget requires shorter TTL.

        ## Concrete scenarios

        - **Modified replay:** Given `Idempotency-Key: k1` already created order `o1` with amount `100`,
          when a caller resends `k1` with amount `200`, then the API returns `409` and `o1` stays `100`.

        ## Constraints & boundaries

        - **Hard constraints & `Must not`:**
          - Never create a second order row for the same `Idempotency-Key`.
        - **Suggested mechanisms (non-binding context):** Reuse existing Postgres idempotency table.
        - **Scope & non-goals:** Do not change batch import endpoints or add cross-region replication.

        ## Preferences & delegated choices

        - **Priority / tie-breakers:** Correctness and duplicate prevention beat tail latency.
        - **Delegated decisions:** Error response detail wording stays delegated within Problem Details schema.

        ## Assumptions & reopen triggers

        - **A1 — Pre-existing keys:** Existing in-flight rows follow the stored response on deploy — **Reopen if:** backfill is needed for legacy clients.

        ## Terminology

        - **Replay:** Second request carrying an existing `Idempotency-Key`. **Not:** New business order with the same cart items.

        ## Success signal

        Zero duplicate orders on mutated retries and 100% `409` responses on conflict integration tests.

        ## Open questions

        - Exact mobile SDK minimum version in production — **Owner:** `Research` | **Fallback:** `A1` | **Reopen if:** telemetry shows clients older than v3 cannot parse `409`.
        """
    ).lstrip("\n")


class CheckIntentTests(unittest.TestCase):
    def setUp(self) -> None:
        self.root = Path(self.enterContext(tempfile.TemporaryDirectory(prefix="intent-check-")))
        _write(self.root / "AGENTS.md", "# AGENTS\n")
        _write(self.root / "Cargo.toml", "[workspace]\n")
        _write(self.root / "crates/service/src/api.rs", "// api\n")
        _write(self.root / "api/openapi/service.yaml", "openapi: 3.1.0\n")
        self.task_dir = self.root / "specs/idempotency-conflict"

    def test_valid_ready_intent_passes_with_summary_and_zero_warnings(self) -> None:
        _write(self.task_dir / "intent.md", _valid_ready_intent())
        report = check_intent.check_intent(self.task_dir, repo_root=self.root)
        self.assertEqual(report.errors, [])
        self.assertEqual(report.warnings, [])
        self.assertEqual(report.status, "ready")
        self.assertEqual(len(report.decisions), 2)
        self.assertEqual(report.scenarios_count, 1)
        self.assertEqual(report.assumptions_count, 1)
        self.assertEqual(report.open_questions_count, 1)

        rendered = check_intent.format_report(report)
        self.assertIn("decisions=2 (1 hard, 1 default)", rendered)
        self.assertIn("scenarios=1, assumptions=1, open_questions=1", rendered)

    def test_draft_allows_remaining_frontier_and_unclosed_open_questions_but_ready_rejects_them(
        self,
    ) -> None:
        draft_text = (
            _valid_ready_intent()
            .replace("status: ready", "status: draft")
            .replace(
                "Exact mobile SDK minimum version in production — **Owner:** `Research` | **Fallback:** `A1` | **Reopen if:** telemetry shows clients older than v3 cannot parse `409`.",
                "Should cancelled orders reuse the same key?",
            )
            + "\n## Remaining frontier\n\n- Check replay behavior on cancelled orders.\n"
        )
        _write(self.task_dir / "intent.md", draft_text)

        draft_report = check_intent.check_intent(self.task_dir, repo_root=self.root)
        self.assertEqual(draft_report.errors, [])
        self.assertTrue(draft_report.has_remaining_frontier)
        self.assertIn("remaining_frontier=present", check_intent.format_report(draft_report))

        # Flipping to ready without removing Remaining frontier or closing Open questions must fail.
        _write(
            self.task_dir / "intent.md",
            draft_text.replace("status: draft", "status: ready"),
        )
        ready_report = check_intent.check_intent(self.task_dir, repo_root=self.root)
        joined = "\n".join(ready_report.errors)
        self.assertIn("draft-only section '## Remaining frontier' must be removed", joined)
        self.assertIn("requires non-requester 'Owner:'", joined)
        self.assertIn("requires an active 'Fallback:'", joined)
        self.assertIn("requires an objective 'Reopen if:'", joined)

        # Requester-owned Open question at ready must also fail.
        requester_open = _valid_ready_intent().replace(
            "**Owner:** `Research`",
            "**Owner:** `user`",
        )
        _write(self.task_dir / "intent.md", requester_open)
        req_report = check_intent.check_intent(self.task_dir, repo_root=self.root)
        self.assertIn("cannot keep an unanswered requester-owned question", "\n".join(req_report.errors))

    def test_rejects_missing_sections_placeholders_and_bad_decisions_or_assumptions(self) -> None:
        _write(
            self.task_dir / "intent.md",
            """
            # Intent: <placeholder title>

            status: blocked

            ## Problem

            TBD

            ## Desired outcome

            Return deterministic errors.

            ## Affected actors and surfaces

            - External caller — `POST /v1/orders`

            ## Settled decisions

            ### I1 — First decision

            - **Binding:** `maybe`
            - **Decision:** TBD
            - **Why:** N/A

            ### I1 — Duplicate ID

            - **Decision:** Missing binding
            - **Why:** Still duplicate

            ### I2 — Superseded decision

            Superseded by I99.

            ## Constraints & boundaries

            - **Hard constraints & `Must not`:** TBD
            - **Scope & non-goals:** N/A

            ## Terminology

            N/A

            ## Assumptions & reopen triggers

            - **A1 — First:** Keep old format
            - **A1 — Duplicate:** Repeat — **Reopen if:** TBD
            - Malformed bullet without ID
            """,
        )

        report = check_intent.check_intent(self.task_dir, repo_root=self.root)
        joined = "\n".join(report.errors)
        self.assertIn("missing non-empty '# Intent:", joined)
        self.assertIn("invalid or missing status 'blocked'", joined)
        self.assertIn("required section '## Problem' is empty or placeholder", joined)
        self.assertIn("missing required section '## Success signal'", joined)
        self.assertIn("invalid Binding '`maybe`'", joined)
        self.assertIn("duplicate decision ID I1", joined)
        self.assertIn("Superseded by unknown ID I99", joined)
        self.assertIn("missing non-empty 'Hard constraints", joined)
        self.assertIn("missing non-empty 'Scope & non-goals:'", joined)
        self.assertIn("optional section '## Terminology' is empty or placeholder", joined)
        self.assertIn("Assumptions & reopen triggers A1: missing objective 'Reopen if:'", joined)
        self.assertIn("duplicate assumption ID A1", joined)
        self.assertIn("entry must start with '**A<n>", joined)

    def test_superseded_decision_with_valid_target_and_missing_path_warning_and_cli(self) -> None:
        text = _valid_ready_intent().replace(
            "### I2 — Default retention window",
            """### I2 — Old retention window

- Superseded by I3.

### I3 — Default retention window""",
        )
        text = text.replace(
            "`crates/service/src/api.rs`",
            "`crates/service/src/nonexistent_planned.rs`",
        )
        _write(self.task_dir / "intent.md", text)

        report = check_intent.check_intent(self.task_dir, repo_root=self.root)
        self.assertEqual(report.errors, [])
        self.assertEqual(len(report.warnings), 1)
        self.assertIn("crates/service/src/nonexistent_planned.rs", report.warnings[0])
        self.assertIn("1 superseded", check_intent.format_report(report))

        stdout = io.StringIO()
        stderr = io.StringIO()
        with redirect_stdout(stdout), redirect_stderr(stderr):
            code = check_intent.main([str(self.task_dir / "intent.md"), "--repo", str(self.root)])
        self.assertEqual(code, 0)
        self.assertIn("Warnings:", stdout.getvalue())

        stdout = io.StringIO()
        stderr = io.StringIO()
        with redirect_stdout(stdout), redirect_stderr(stderr):
            missing_code = check_intent.main([str(self.root / "specs/missing")])
        self.assertEqual(missing_code, 1)
        self.assertIn("intent not found:", stderr.getvalue())

    def test_supersession_ignores_rejected_prose_and_code_fences_and_rejects_self_loop(
        self,
    ) -> None:
        text = _valid_ready_intent().replace(
            "- **Rejected:** 7-day retention (unneeded storage cost for short checkout flows).",
            "- **Rejected:** 7-day retention (I1, superseded by I2 after reopen).\n"
            "  ```text\n"
            "- **Status:** Superseded by I99\n"
            "  ```",
        )
        _write(self.task_dir / "intent.md", text)
        report = check_intent.check_intent(self.task_dir, repo_root=self.root)
        self.assertEqual(report.errors, [])
        self.assertIn("decisions=2 (1 hard, 1 default)", check_intent.format_report(report))

        # Self-supersession must fail, including via trailing **Status:** after Why.
        self_loop = _valid_ready_intent().replace(
            "### I2 — Default retention window\n\n- **Binding:** `default`\n"
            "- **Decision:** Keep idempotency keys for 24 hours.\n"
            "- **Rejected:** 7-day retention (unneeded storage cost for short checkout flows).\n"
            "- **Why:** Matches sibling endpoint convention unless storage budget requires shorter TTL.",
            "### I2 — Loop decision\n\n- Superseded by I2.",
        )
        _write(self.task_dir / "intent.md", self_loop)
        loop_report = check_intent.check_intent(self.task_dir, repo_root=self.root)
        self.assertIn("cannot supersede itself", "\n".join(loop_report.errors))

        trailing_status_unknown = _valid_ready_intent().replace(
            "- **Why:** Matches sibling endpoint convention unless storage budget requires shorter TTL.",
            "- **Why:** Matches sibling endpoint convention unless storage budget requires shorter TTL.\n"
            "- **Status:** Superseded by I99.",
        )
        _write(self.task_dir / "intent.md", trailing_status_unknown)
        unknown_report = check_intent.check_intent(self.task_dir, repo_root=self.root)
        self.assertIn("Superseded by unknown ID I99", "\n".join(unknown_report.errors))

        status_after_binding = _valid_ready_intent().replace(
            "### I2 — Default retention window\n\n- **Binding:** `default`\n"
            "- **Decision:** Keep idempotency keys for 24 hours.\n"
            "- **Rejected:** 7-day retention (unneeded storage cost for short checkout flows).\n"
            "- **Why:** Matches sibling endpoint convention unless storage budget requires shorter TTL.",
            "### I2 — Old retention window\n\n- **Binding:** `default`\n- **Status:** Superseded by I1.",
        )
        _write(self.task_dir / "intent.md", status_after_binding)
        status_report = check_intent.check_intent(self.task_dir, repo_root=self.root)
        self.assertEqual(status_report.errors, [])
        self.assertIn("1 superseded", check_intent.format_report(status_report))

    def test_open_questions_constraints_placeholders_assumptions_and_status_casing_regressions(
        self,
    ) -> None:
        # Valid short-form assumption with unbolded Reopen if: must pass.
        short_assumption = _valid_ready_intent().replace(
            "- **A1 — Pre-existing keys:** Existing in-flight rows follow the stored response on deploy — **Reopen if:** backfill is needed for legacy clients.",
            "- **A1:** Existing in-flight rows follow the stored response — Reopen if: legacy clients need backfill.",
        )
        _write(self.task_dir / "intent.md", short_assumption)
        self.assertEqual(check_intent.check_intent(self.task_dir, repo_root=self.root).errors, [])

        # Valid non-requester external owners containing words like owner/user/product must pass.
        valid_external_owners = (
            "Payments API owner",
            "External billing service owner",
            "User identity provider",
            "External product catalog API",
        )
        for owner in valid_external_owners:
            with self.subTest(valid_owner=owner):
                ok_oq = _valid_ready_intent().replace(
                    "**Owner:** `Research`",
                    f"**Owner:** {owner}",
                )
                _write(self.task_dir / "intent.md", ok_oq)
                self.assertEqual(check_intent.check_intent(self.task_dir, repo_root=self.root).errors, [])

        # Requester owner variants, empty Owner before Fallback, and pipe-bounded Reopen if: must fail at ready.
        oq_variants = (
            "- Q — **Owner:** `user` (waiting) | **Fallback:** `A1` | **Reopen if:** client fails",
            "- Q — **Owner:** user/requester | **Fallback:** `A1` | **Reopen if:** client fails",
            "- Q — **Owner:** user, product | **Fallback:** `A1` | **Reopen if:** client fails",
            "- Q — **Owner:** *requester* (PM) | **Fallback:** `A1` | **Reopen if:** client fails",
            "- Q — **Owner:** product-owner | **Fallback:** `A1` | **Reopen if:** client fails",
            "- Q — **Owner:** end-user | **Fallback:** `A1` | **Reopen if:** client fails",
            "- Q — **Owner:** [user](mailto:u@example.com) | **Fallback:** `A1` | **Reopen if:** client fails",
            "- Q — **Owner:** (user) | **Fallback:** `A1` | **Reopen if:** client fails",
            "- Q — **Owner:** **Fallback:** `A1` | **Reopen if:** client fails",
            "- Q — **Owner:** `Research` | **Reopen if:** TBD | **Fallback:** `A1`",
            "- Q — **Owner:** `Research` | **Reopen if:** | **Fallback:** `A1`",
        )
        for variant in oq_variants:
            with self.subTest(variant=variant):
                bad_oq = _valid_ready_intent().replace(
                    "- Exact mobile SDK minimum version in production — **Owner:** `Research` | **Fallback:** `A1` | **Reopen if:** telemetry shows clients older than v3 cannot parse `409`.",
                    variant,
                )
                _write(self.task_dir / "intent.md", bad_oq)
                res = check_intent.check_intent(self.task_dir, repo_root=self.root)
                self.assertNotEqual(res.errors, [], f"expected error for {variant}")

        # Constraints must inspect labels only and reject nested placeholder bullets;
        # bulleted placeholders in sections and uppercase Status: must fail.
        bad_constraints_and_casing = (
            _valid_ready_intent()
            .replace("status: ready", "Status: ready")
            .replace(
                "- **Hard constraints & `Must not`:**\n"
                "  - Never create a second order row for the same `Idempotency-Key`.\n"
                "- **Suggested mechanisms (non-binding context):** Reuse existing Postgres idempotency table.\n"
                "- **Scope & non-goals:** Do not change batch import endpoints or add cross-region replication.",
                "- **Hard constraints & `Must not`:**\n"
                "  - TBD\n"
                "- **Suggested mechanisms (non-binding context):** Reuse Postgres; must not violate non-goals.",
            )
            .replace(
                "- **Modified replay:** Given `Idempotency-Key: k1` already created order `o1` with amount `100`,\n"
                "  when a caller resends `k1` with amount `200`, then the API returns `409` and `o1` stays `100`.",
                "- TBD",
            )
        )
        _write(self.task_dir / "intent.md", bad_constraints_and_casing)
        res = check_intent.check_intent(self.task_dir, repo_root=self.root)
        joined = "\n".join(res.errors)
        self.assertIn("invalid or missing status None", joined)
        self.assertIn("missing non-empty 'Hard constraints", joined)
        self.assertIn("missing non-empty 'Scope & non-goals:'", joined)
        self.assertIn("optional section '## Concrete scenarios' is empty or placeholder", joined)
        self.assertEqual(res.scenarios_count, 0)

    def test_make_intent_check_resolves_slug_colliding_root_dir_and_paths(self) -> None:
        make_root = Path(self.enterContext(tempfile.TemporaryDirectory(prefix="make-intent-")))
        shutil.copy2(ROOT / "Makefile", make_root / "Makefile")
        (make_root / "tools").mkdir()
        shutil.copy2(ROOT / "tools/versions.env", make_root / "tools/versions.env")
        (make_root / "scripts").mkdir()
        shutil.copy2(ROOT / "scripts/check-intent.py", make_root / "scripts/check-intent.py")

        missing_arg = subprocess.run(
            ["make", "-s", "intent-check"],
            cwd=make_root,
            capture_output=True,
            text=True,
            check=False,
        )
        self.assertNotEqual(missing_arg.returncode, 0)
        self.assertIn("intent-check requires TASK=", missing_arg.stderr)

        (make_root / "migrations").mkdir()
        _write(
            make_root / "specs/migrations/intent.md",
            """
            # Intent: Reject invalid migration status
            status: ready

            ## Problem
            Status mismatch.

            ## Desired outcome
            Fail fast.

            ## Affected actors and surfaces
            - Operator — CLI

            ## Constraints & boundaries
            - **Hard constraints & `Must not`:** Never mutate applied history.
            - **Scope & non-goals:** Do not alter rollback semantics.

            ## Success signal
            Invalid migration status exits non-zero.
            """,
        )

        for target_arg in ("migrations", "specs/migrations", "specs/migrations/intent.md"):
            with self.subTest(task=target_arg):
                result = subprocess.run(
                    ["make", "-s", "intent-check", f"TASK={target_arg}"],
                    cwd=make_root,
                    capture_output=True,
                    text=True,
                    check=False,
                )
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn("(status: ready)", result.stdout)


if __name__ == "__main__":
    unittest.main()
