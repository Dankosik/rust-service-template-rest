#!/usr/bin/env python3
"""Pure remaining-program arithmetic and durable one-hold custody checks."""

import copy
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest
from unittest import mock


ROOT = Path(__file__).resolve().parents[2]
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

    def test_watchdog_stops_its_controller_when_remaining_work_cannot_fit(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            control = root / "control"
            control.mkdir()
            attempt = root / "attempts" / "cell"
            attempt.mkdir(parents=True)
            (attempt / "service0.ready").touch()
            state = control / "budget.json"
            # This immutable old clock is deliberately expired at real execution.
            old = time.time_ns() // 1_000_000 - 2 * budget.LIMIT_MS
            expired = budget.initial_state(budget.campaign_clock(old, old), {}, old)
            state.write_text(json.dumps(expired))
            controller = """
import os,signal,subprocess,sys,time
child=None
def stop(signum,frame):
    child.wait(timeout=3)
    raise SystemExit(143)
signal.signal(signal.SIGTERM,stop)
child=subprocess.Popen([sys.executable,sys.argv[1],sys.argv[2],'watch',str(os.getpid())])
while child.poll() is None: time.sleep(0.01)
raise SystemExit(9)
"""
            result = subprocess.run(
                [sys.executable, "-c", controller,
                 str(ROOT / "scripts/lib/postgres_sustained_budget.py"), str(state)],
                check=False, capture_output=True, text=True, timeout=5,
            )
            self.assertEqual(result.returncode, 143, result.stderr)
            self.assertTrue((attempt / "stop").exists())
            receipt = json.loads((control / "budget-stop.json").read_text())
            self.assertIn("overhead exceeded", receipt["reason"])

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
    unittest.main()
