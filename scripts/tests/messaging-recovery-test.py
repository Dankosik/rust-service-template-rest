#!/usr/bin/env python3
"""Controller state tests; native broker evidence belongs to the owned demo."""

import contextlib
import importlib.util
from pathlib import Path
import tempfile
import sys
import time
import unittest
from unittest import mock

SPEC = importlib.util.spec_from_file_location("messaging_recovery", Path(__file__).resolve().parents[1] / "ci/messaging-recovery.py")
controller = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(controller)


class Custody(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        path = Path(self.directory.name)
        generation = "a" * 24
        controller.atomic(path / "session.json", {
            "version": 1, "generation": generation, "project": "messaging-recovery-" + generation,
            "directory": str(path.resolve()), "expires_at": time.time() + 900,
            "status": "active", "topology_frozen": False, "containers": {},
            "source_stream": "SOURCE", "dlq_stream": "DLQ",
        })
        (path / "selections").mkdir()
        (path / "evidence").mkdir()
        self.session = controller.Session(path)

    def selected(self, publication="ambiguous", generation=None):
        path = self.session.path / "selections/selected.json"
        controller.atomic(path, {"version": 1, "generation": generation or self.session.data["generation"],
                          "source_stream": "SOURCE", "record": {"stream": "DLQ", "sequence": 42}})
        controller.atomic(path.with_suffix(".state.json"), {
            "version": 1, "selection_sha256": controller.sha(path.read_bytes()),
            "publication": publication, "retirement": "retained",
            "ack": {"stream": "SOURCE", "sequence": 7, "duplicate": False} if publication == "confirmed" else None,
        })
        return path

    def test_crash_keeps_journal_and_frozen_topology_after_controller_lock_releases(self):
        with mock.patch.object(self.session, "verify"):
            with self.assertRaisesRegex(RuntimeError, "simulated controller crash"):
                with self.session.lock(), self.session.operation("retire", "selected"):
                    raise RuntimeError("simulated controller crash")
            resumed = controller.Session(self.session.path)
            self.assertTrue(resumed.data["topology_frozen"])
            with resumed.lock(), mock.patch.object(resumed, "verify"):
                with self.assertRaisesRegex(controller.Refused, "abandoned_operation"):
                    with resumed.operation("redrive", "selected"):
                        self.fail("a second operation escaped retained custody")

    def test_other_generation_selection_cannot_dispatch(self):
        self.selected("confirmed", "b" * 24)
        with mock.patch.object(self.session, "request") as request:
            with self.assertRaisesRegex(controller.Refused, "selection_generation"):
                self.session.retire("selected")
            request.assert_not_called()

    def test_ambiguous_publication_retains_record_without_native_delete(self):
        self.selected()
        with mock.patch.object(self.session, "request") as request:
            result = self.session.retire("selected")
            self.assertEqual(result["publication"], "ambiguous")
            self.assertEqual(result["retirement"], "refused")
            request.assert_not_called()

    def test_stale_record_after_confirmed_publication_is_not_deleted(self):
        self.selected("confirmed")
        with mock.patch.object(self.session, "operation", return_value=contextlib.nullcontext()), \
                mock.patch.object(self.session, "exact_record", return_value="stale"), \
                mock.patch.object(self.session, "request") as request:
            result = self.session.retire("selected")
            self.assertEqual(result["retirement"], "stale")
            self.assertEqual(result["publication"], "confirmed")
            request.assert_not_called()

    def test_immutable_selection_creation_does_not_clobber_crash_retry_identity(self):
        path = self.selected("confirmed")
        original = path.read_bytes()
        with self.assertRaises(FileExistsError):
            controller.atomic(path, {"record": "replacement"}, exclusive=True)
        self.assertEqual(path.read_bytes(), original)
        self.assertEqual(path.stat().st_mode & 0o777, 0o600)


# template:begin outbox:capacity-measurement-tests
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "ci"))
import messaging_recovery_capacity as capacity


class CapacityAccounting(unittest.TestCase):
    def test_rates_use_observed_intervals_and_distinct_durable_boundaries(self):
        before = {"observed_unix": 10.0, "offered": 10, "admitted": 9, "puback_confirmed": 8, "applied": 5}
        after = {"observed_unix": 14.0, "offered": 30, "admitted": 21, "puback_confirmed": 20, "applied": 17}
        self.assertEqual(capacity.rates(before, after), {
            "offered_per_second": 5.0, "admitted_per_second": 3.0,
            "puback_confirmed_per_second": 3.0, "applied_per_second": 3.0,
        })
        self.assertTrue(all(value is None for value in capacity.rates(None, after).values()))
        with self.assertRaisesRegex(ValueError, "nonmonotonic"):
            capacity.rates(after, before)
        after["puback_confirmed"] = 7
        with self.assertRaisesRegex(ValueError, "nonmonotonic"):
            capacity.rates(before, after)

    def test_native_metric_projection_withholds_logs_and_nonfinite_values(self):
        raw = b"""# HELP safe help
postgres_transaction_duration_seconds_count{outcome="committed"} 4
db_client_connection_count{db_client_connection_state="used"} 2
messaging_publish_work_in_flight 1
messaging_unknown NaN
Authorization: Bearer private-test-value
unrelated_secret{key="private-test-value"} 1
"""
        self.assertEqual(capacity.metrics(raw), {
            'postgres_transaction_duration_seconds_count{outcome="committed"}': 4,
            'db_client_connection_count{db_client_connection_state="used"}': 2,
            'messaging_publish_work_in_flight': 1,
        })

    def test_partial_observation_write_does_not_invent_a_completed_offer(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "load.jsonl"
            path.write_bytes(b'{"id":"confirmed"}\n{"id":"interrupted"')
            self.assertEqual(capacity.complete_lines(path), [{"id": "confirmed"}])
# template:end outbox:capacity-measurement-tests


if __name__ == "__main__":
    unittest.main()
