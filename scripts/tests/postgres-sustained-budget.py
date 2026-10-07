#!/usr/bin/env python3
"""Pure budget checks by default; --queue-caller-check selects real Q scope proof.

The opt-in falsifier uses an isolated queue domain, never the lab or shared lock.
"""

import copy
import importlib.util
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time
import unittest
from unittest import mock


ROOT = Path(__file__).resolve().parents[2]
QUEUE_CALLER_CHECK = "--queue-caller-check" in sys.argv
sys.dont_write_bytecode = True
SPEC = importlib.util.spec_from_file_location(
    "postgres_sustained_budget", ROOT / "scripts/lib/postgres_sustained_budget.py"
)
budget = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(budget)


class BudgetTests(unittest.TestCase):
    def setUp(self):
        self.resumed = budget.ABSENCE_MS + 9_000_000
        self.clock = budget.campaign_clock(budget.ORIGINAL_START_MS, self.resumed, {
            "previous_absence_unix_ms": budget.ABSENCE_MS,
            "previous_exported_unix_ms": budget.HOLD_START_MS,
            **{name: "a" * 64 for name in (
                "prior_export_sha256", "prior_absence_sha256",
                "continued_absence_sha256", "review_receipt_sha256",
            )},
        })
        self.state = budget.initial_state(
            self.clock, {"resident": 45_457}, self.resumed
        )

    def test_admission_reserves_the_twenty_cell_branch_and_every_cost(self):
        observed = budget.observe(self.state, self.resumed)
        self.assertEqual(observed["remaining_charged_ms"], 16_151_918)
        self.assertEqual(observed["maximum_remaining_stages_ms"], 13_574_543)
        self.assertEqual(observed["replacement_reserve_ms"], 840_000)
        self.assertEqual(observed["cleanup_reserve_ms"], 1_200_000)
        self.assertEqual(observed["other_overhead_limit_ms"], 537_375)
        self.assertEqual(observed["maximum_remaining_required_ms"], 16_151_918)
        self.assertEqual(observed["charged_elapsed_ms"], 48_082)
        late = copy.deepcopy(self.clock)
        late["effective_deadline_unix_ms"] -= 537_376
        with self.assertRaisesRegex(ValueError, "maximum remaining program"):
            budget.initial_state(late, {"resident": 45_457}, self.resumed)

    def test_native_closure_spends_bounded_overhead_without_refunding_wall(self):
        start = self.resumed
        budget.transition(self.state, "begin", "cell-1", start)
        budget.transition(self.state, "finish", "cell-1", start + 375_000)
        report = budget.observe(self.state, start + 375_000)
        self.assertEqual(report["other_overhead_elapsed_ms"], 15_000)
        self.assertEqual(report["charged_elapsed_ms"], 423_082)
        self.assertEqual(report["effective_deadline_unix_ms"], self.clock["effective_deadline_unix_ms"])
        with self.assertRaisesRegex(ValueError, "overhead exceeded"):
            budget.observe(self.state, start + 360_000 + 537_376)

    def test_clone_and_drop_share_one_reset_bound_and_reject_overlap(self):
        start = self.resumed
        budget.transition(self.state, "begin", "reset-1", start)
        with self.assertRaisesRegex(ValueError, "overlapping"):
            budget.transition(self.state, "begin", "cell-1", start)
        budget.transition(self.state, "pause", "reset-1", start + 40_000)
        budget.transition(self.state, "begin", "reset-1", start + 40_100)
        with self.assertRaisesRegex(ValueError, "stage exceeded"):
            budget.transition(self.state, "finish", "reset-1", start + 60_101)

    def test_shorter_confirmation_releases_only_unspent_fixed_stages(self):
        start = self.resumed
        with self.assertRaisesRegex(ValueError, "unfinished screening"):
            budget.transition(self.state, "confirmation", "16", start)
        for index in range(1, 9):
            budget.transition(self.state, "begin", f"cell-{index}", start)
            start += 360_000
            budget.transition(self.state, "finish", f"cell-{index}", start)
        before = budget.observe(self.state, start)
        after = budget.transition(self.state, "confirmation", "16", start)
        self.assertEqual(
            before["maximum_remaining_stages_ms"] - after["maximum_remaining_stages_ms"],
            4 * (360_000 + 60_000),
        )
        self.assertEqual(after["other_overhead_limit_ms"], 537_375)
        self.assertEqual(after["replacement_reserve_ms"], 840_000)
        self.assertEqual(after["effective_deadline_unix_ms"], before["effective_deadline_unix_ms"])
        with self.assertRaisesRegex(ValueError, "backwards"):
            budget.observe(self.state, start - 1)
        with self.assertRaisesRegex(ValueError, "incomplete comparison"):
            budget.transition(self.state, "close-matrix", "", start)
        for index in range(9, 17):
            budget.transition(self.state, "begin", f"cell-{index}", start)
            start += 360_000
            budget.transition(self.state, "finish", f"cell-{index}", start)
        closed = budget.transition(self.state, "close-matrix", "", start)
        self.assertEqual(closed["replacement_reserve_ms"], 0)
        self.assertEqual(closed["effective_deadline_unix_ms"], before["effective_deadline_unix_ms"])

    def test_consumed_recovery_cannot_be_recreated_or_overwritten(self):
        with tempfile.TemporaryDirectory() as directory:
            receipt = Path(directory) / "recovery.json"
            original = {"campaign_clock": self.clock}
            budget.consume_recovery(receipt, original)
            with self.assertRaises(FileExistsError):
                budget.consume_recovery(receipt, {"campaign_clock": {"reset": True}})
            self.assertEqual(json.loads(receipt.read_text()), original)

    @unittest.skipUnless(QUEUE_CALLER_CHECK, "real Q scope proof requires --queue-caller-check")
    def test_q_cancel_stops_foreground_pipeline_and_descendant_with_parent_cleanup_alive(self):
        # The former Python-parent watchdog test never entered Bash's deferred
        # foreground trap boundary. This uses actual Q APIs and actual process
        # groups; no fixture implements cancellation or fabricates Q receipts.
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            root = base / "evidence"
            control = root / "control"
            control.mkdir(parents=True)
            source = base / "source"
            (source / "scripts/ci").mkdir(parents=True)
            for name in ("validation-lock.sh", "validation-lock.py"):
                shutil.copy2(ROOT / "scripts/ci" / name, source / "scripts/ci" / name)
            helper = ROOT / "scripts/lib/postgres_sustained_budget.py"
            context_path = control / "work-context.json"
            queue = source / "scripts/ci/validation-lock.sh"
            workload = base / "foreground.py"
            workload.write_text("""
import os, pathlib, signal, subprocess, sys, time
base, role = pathlib.Path(sys.argv[1]), sys.argv[2]
signal.signal(signal.SIGTERM, lambda *_: None)
if role == 'foreground':
    subprocess.Popen([sys.executable, __file__, str(base), 'descendant'], preexec_fn=os.setpgrp)
(base / (role + '.ready')).touch()
deadline = time.monotonic() + 20
while time.monotonic() < deadline:
    (base / (role + '.heartbeat')).write_text(str(time.monotonic_ns()))
    time.sleep(.025)
""")
            work = source / "scripts/postgres-sustained.sh"
            work.write_text("""#!/usr/bin/env bash
set -euo pipefail
trap 'printf trapped >"${TEST_ROOT}/child-trap"; exit 143' TERM
python3 "${TEST_HELPER}" "${TEST_ROOT}/evidence/control/budget.json" begin reset-1
python3 "${TEST_ROOT}/foreground.py" "${TEST_ROOT}/evidence" foreground | cat
printf survived >"${TEST_ROOT}/after-foreground"
""")
            # Carry 38 seconds already spent in this fixed 60-second reset.
            # The real guard must cancel after about two seconds, retaining the
            # unchanged 15-second Q tail inside the original hard ceiling.
            now = time.time_ns() // 1_000_000
            started = now - 38_000
            clock = budget.campaign_clock(started, started)
            state = budget.initial_state(clock, {}, started)
            state["stages"]["reset-1"]["used_ms"] = 38_000
            budget.write_json(control / "budget.json", state)
            budget.write_json(root / "campaign.json", {"campaign_clock": clock})
            budget.write_json(root / "source.json", {"fixture": "actual Q with bounded foreground work"})
            context = {"version": 1, "output": str(root), "source_copy": str(source),
                       "build_root": str(base), "queue_script": str(queue),
                       "work_command": ["bash", str(work), "--work-scope", str(context_path)],
                       "source_manifest_sha256": hashlib.sha256((root / "source.json").read_bytes()).hexdigest(),
                       "campaign_manifest_sha256": hashlib.sha256((root / "campaign.json").read_bytes()).hexdigest()}
            budget.write_json(context_path, context)
            controller = base / "parent.sh"
            controller.write_text("""#!/usr/bin/env bash
set -uo pipefail
python3 "${TEST_HELPER}" "${TEST_CONTEXT}" run-scope
run_status=$?
bash "${TEST_QUEUE}" --assert-held || exit 20
printf '%s' "${run_status}" >"${TEST_ROOT}/parent-after-run"
python3 "${TEST_HELPER}" "${TEST_CONTEXT}" cleanup-scope || exit 21
printf alive >"${TEST_ROOT}/parent-after-cleanup"
python3 "${TEST_HELPER}" "${TEST_ROOT}/evidence" export failed "${TEST_STARTED}" || exit 22
""")
            environment = {key: value for key, value in os.environ.items()
                           if key not in ("VALIDATION_LOCK_DIR", "VALIDATION_LOCK_DOMAIN", "VALIDATION_LOCK_TOKEN",
                                          "VALIDATION_LOCK_HELD", "VALIDATION_LOCK_CHILD")}
            environment.update(VALIDATION_LOCK_DIR=str(base / "isolated.lock"),
                               VALIDATION_LOCK_TIMEOUT_SECONDS="8", TEST_ROOT=str(base),
                               TEST_HELPER=str(helper), TEST_CONTEXT=str(context_path),
                               TEST_QUEUE=str(queue), TEST_STARTED=str(started))
            result = subprocess.run(
                ["bash", str(queue), "--with-child-scopes", "--", "bash", str(controller)],
                env=environment, check=False, capture_output=True, text=True, timeout=45,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual((base / "parent-after-run").read_text(), "1")
            self.assertEqual((base / "parent-after-cleanup").read_text(), "alive")
            self.assertFalse((base / "after-foreground").exists())
            self.assertTrue((root / "foreground.ready").exists())
            self.assertTrue((root / "descendant.ready").exists())
            custody = json.loads((control / "child-scope.json").read_text())
            acknowledgement = custody["cancel_acknowledgement"]
            self.assertTrue(acknowledgement["accepted"])
            self.assertFalse(acknowledgement["ordinary_stop"])
            snapshot = json.loads((control / "child-status.json").read_text())
            self.assertTrue(snapshot["ordinary_stop"])
            self.assertTrue(snapshot["wait_completed"])
            self.assertFalse(snapshot["no_command_effect"])
            self.assertEqual(snapshot["unresolved_resources"], [])
            self.assertEqual(snapshot["cancel_at_ns"], acknowledgement["cancel_at_ns"])
            self.assertEqual(json.loads((root / "resource-absence.json").read_text())["status"], "verified")
            retained = json.loads((root / "export.json").read_text())
            self.assertEqual(retained["status"], "failed")
            self.assertIn("foreground.heartbeat", {row["path"] for row in retained["files"]})
            final_budget = json.loads((control / "budget.json").read_text())
            self.assertLess(final_budget["stages"]["reset-1"]["used_ms"], 60_000)
            self.assertEqual(final_budget["clock"], clock)
            self.assertEqual(final_budget["replacement_reserve_ms"], 840_000)
            self.assertEqual(final_budget["cleanup_reserve_ms"], 1_200_000)
            finality = json.loads((control / "scope-result.json").read_text())
            self.assertEqual(finality["status"], "failed")
            self.assertTrue(finality["cleanup_complete"])
            self.assertEqual(finality["pending_q_calls"], [])
            before = [(root / (role + ".heartbeat")).read_text() for role in ("foreground", "descendant")]
            time.sleep(.1)
            self.assertEqual(before, [(root / (role + ".heartbeat")).read_text()
                                      for role in ("foreground", "descendant")])

    def test_deadline_crossing_during_export_keeps_failure_and_nonzero_exit(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "campaign.json").write_text(json.dumps({"campaign_clock": self.clock}))
            deadline = self.clock["effective_deadline_unix_ms"]
            with mock.patch.object(budget.time, "time_ns", side_effect=[
                (deadline - 1) * 1_000_000, (deadline + 1) * 1_000_000,
            ]):
                complete = budget.export_evidence(root, "observations_complete", budget.ORIGINAL_START_MS)
            self.assertFalse(complete)
            report = json.loads((root / "export.json").read_text())
            self.assertEqual(report["status"], "failed")
            self.assertEqual(report["deadline_failure_observed_unix_ms"], deadline + 1)
            old = time.time_ns() // 1_000_000 - 2 * budget.LIMIT_MS
            (root / "campaign.json").write_text(json.dumps({"campaign_clock": budget.campaign_clock(old, old)}))
            result = subprocess.run(
                [sys.executable, str(ROOT / "scripts/lib/postgres_sustained_budget.py"),
                 str(root), "export", "observations_complete", str(old)],
                check=False, capture_output=True, text=True, timeout=5,
            )
            self.assertEqual(result.returncode, 1, result.stderr)
            self.assertEqual(json.loads((root / "export.json").read_text())["status"], "failed")


if __name__ == "__main__":
    if QUEUE_CALLER_CHECK:
        sys.argv.remove("--queue-caller-check")
        unittest.main(defaultTest="BudgetTests.test_q_cancel_stops_foreground_pipeline_and_descendant_with_parent_cleanup_alive")
    else:
        unittest.main()
