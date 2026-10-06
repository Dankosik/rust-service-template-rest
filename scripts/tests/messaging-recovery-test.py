#!/usr/bin/env python3
"""Controller state tests; native broker evidence belongs to the owned demo."""

import contextlib
import importlib.util
import io
import json
from pathlib import Path
import tempfile
import sys
import time
import unittest
from unittest import mock

SPEC = importlib.util.spec_from_file_location("messaging_recovery", Path(__file__).resolve().parents[1] / "ci/messaging-recovery.py")
controller = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(controller)


class ResourceAdmission(unittest.TestCase):
    @staticmethod
    def sample(**changes):
        return {"free_disk_bytes": 3 * controller.GIB, "host_cpus": 4, "host_load_one": 4,
                "docker_cpus": 3, "container_memory_limits_bounded": True,
                "available_container_memory_bytes": 3 * controller.GIB + controller.GIB // 2,
                **changes}

    def test_load_cooldown_resamples_once_and_retains_original_deadline(self):
        path = Path(".")
        with mock.patch.object(controller, "resource_snapshot", side_effect=[self.sample(host_load_one=8), self.sample()]) as sample, \
                mock.patch.object(controller.time, "time", return_value=100), \
                mock.patch.object(controller.time, "sleep") as sleep, \
                mock.patch.object(controller.sys, "stderr", io.StringIO()) as output:
            admitted = controller.resource_admission(path, deadline=1000, cooldown=True)
        sleep.assert_called_once_with(60)
        self.assertEqual(sample.call_args_list, [mock.call(path, 1000), mock.call(path, 1000)])
        reports = [json.loads(line) for line in output.getvalue().splitlines()]
        self.assertEqual([row["sample"]["host_load_one"] for row in reports], [8, 4])
        self.assertEqual([row["admitted"] for row in reports], [False, True])
        self.assertEqual(admitted["samples"], [{key: value for key, value in row.items() if key != "event"} for row in reports])

    def test_persistent_load_stops_after_one_cooldown(self):
        with mock.patch.object(controller, "resource_snapshot", return_value=self.sample(host_load_one=8)) as sample, \
                mock.patch.object(controller.time, "time", return_value=100), \
                mock.patch.object(controller.time, "sleep") as sleep, \
                mock.patch.object(controller.sys, "stderr", io.StringIO()):
            with self.assertRaisesRegex(controller.Refused, "host_load_above_cpu_count"):
                controller.resource_admission(Path("."), deadline=1000, cooldown=True)
        sleep.assert_called_once_with(60)
        self.assertEqual(sample.call_count, 2)

    def test_capacity_refusal_reports_samples_without_creating_a_session(self):
        cases = (
            ({"docker_cpus": 2}, "insufficient_docker_cpu_capacity"),
            ({"container_memory_limits_bounded": False}, "unbounded_container_memory"),
            ({"available_container_memory_bytes": 3 * controller.GIB + controller.GIB // 2 - 1}, "insufficient_container_memory"),
            ({"free_disk_bytes": 3 * controller.GIB - 1}, "fixture_requires_2GiB_floor_plus_1GiB_data"),
        )
        for change, reason in cases:
            with self.subTest(reason=reason), tempfile.TemporaryDirectory() as directory:
                session = Path(directory) / "new-session"
                snapshot = self.sample(host_load_one=8, **change)
                with mock.patch.object(controller, "resource_snapshot", return_value=snapshot), \
                        mock.patch.object(controller.time, "sleep") as sleep, \
                        mock.patch.object(controller, "run") as native, \
                        mock.patch.object(controller.sys, "stderr", io.StringIO()) as output:
                    with self.assertRaisesRegex(controller.Refused, reason):
                        controller.start(session, Path(directory))
                self.assertFalse(session.exists())
                sleep.assert_not_called()
                native.assert_not_called()
                report = json.loads(output.getvalue())
                self.assertEqual(report["reason"], reason)
                self.assertEqual(report["sample"], snapshot)

    def test_replacement_generation_does_not_receive_another_cooldown(self):
        with tempfile.TemporaryDirectory() as directory, \
                mock.patch.object(controller, "resource_snapshot", return_value=self.sample(host_load_one=8)), \
                mock.patch.object(controller.time, "time", return_value=100), \
                mock.patch.object(controller.time, "sleep") as sleep, \
                mock.patch.object(controller.sys, "stderr", io.StringIO()):
            with self.assertRaisesRegex(controller.Refused, "host_load_above_cpu_count"):
                controller.start(Path(directory) / "replacement", Path(directory), deadline=1000)
        sleep.assert_not_called()

    def test_cooldown_cannot_extend_the_parent_deadline(self):
        with mock.patch.object(controller, "resource_snapshot", return_value=self.sample(host_load_one=8)), \
                mock.patch.object(controller.time, "time", return_value=100), \
                mock.patch.object(controller.time, "sleep") as sleep, \
                mock.patch.object(controller.sys, "stderr", io.StringIO()):
            with self.assertRaisesRegex(controller.Refused, "resource_admission_deadline"):
                controller.resource_admission(Path("."), deadline=160, cooldown=True)
        sleep.assert_not_called()

    def test_resource_report_withholds_raw_docker_metadata(self):
        with mock.patch.object(controller.shutil, "disk_usage", return_value=mock.Mock(free=3 * controller.GIB)), \
                mock.patch.object(controller.os, "cpu_count", return_value=4), \
                mock.patch.object(controller.os, "getloadavg", return_value=(4, 3, 2)), \
                mock.patch.object(controller, "native_json", return_value={"MemTotal": 4 * controller.GIB, "NCPU": 3,
                                                                          "Name": "private-host-value"}), \
                mock.patch.object(controller, "run", return_value=(0, b"")), \
                mock.patch.object(controller.sys, "stderr", io.StringIO()) as output:
            admitted = controller.resource_admission(Path("."), deadline=time.time() + 900)
        self.assertEqual(admitted["host_load_one"], 4)
        self.assertEqual(admitted["docker_cpus"], 3)
        self.assertNotIn("private-host-value", output.getvalue())


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

    def test_startup_failure_retains_first_operation_and_only_safe_owned_state(self):
        self.session.data["containers"] = {"nats1": {"id": "owned-container"}}
        observed = [{"Id": "owned-container", "Config": {"Env": ["PRIVATE=withheld-value"]},
                     "State": {"Status": "exited", "Running": False, "ExitCode": 1, "OOMKilled": False,
                               "Error": "withheld-value", "Health": {"Status": "unhealthy",
                                                                       "Log": [{"Output": "withheld-value"}]}}}]
        with mock.patch.object(controller, "native_json", return_value=observed) as inspect:
            with self.assertRaisesRegex(controller.Refused, "capture_failed"):
                with self.session.startup_diagnostics():
                    self.session.startup_phase("broker_up")
                    self.session.remember_startup_failure(controller.Refused(
                        "native_command_failed", command_class="docker_compose", exit_code=1))
                    self.session.startup_phase("resource_capture")
                    raise controller.Refused("capture_failed", command_class="docker_inspect", exit_code=2)
        inspect.assert_called_once_with(["docker", "inspect", "owned-container"], timeout=5)
        evidence = next((self.session.path / "evidence").glob("*-startup-failure.json")).read_text()
        self.assertNotIn("withheld-value", evidence)
        self.assertEqual(json.loads(evidence)["result"], {
            "phase": "broker_up", "command_class": "docker_compose", "exit_code": 1,
            "container_state_status": "observed", "containers": {"nats1": {
                "Status": "exited", "Running": False, "ExitCode": 1, "OOMKilled": False,
                "Health.Status": "unhealthy",
            }},
        })

    def test_native_config_receipt_withholds_parser_text_and_keeps_image_entrypoint(self):
        def parser(*args, **kwargs):
            self.assertNotIn("--entrypoint", args)
            self.assertEqual(args[-9:], ("nats1", "timeout", "-k", "5", "20", "nats-server",
                                        "--test", "--config", "/session/node.conf"))
            controller.private_text(kwargs["private_output"],
                                    "nats-server: /auth/server.conf:7:3: private value withheld-value\n")
            return 1, b""

        with mock.patch.object(self.session, "compose", side_effect=parser):
            with self.assertRaisesRegex(controller.Refused, "native_config_test_failed") as failure:
                self.session.test_native_config()
        self.assertEqual(failure.exception.exit_code, 1)
        output = self.session.path / "native-config-test.output"
        self.assertEqual(output.stat().st_mode & 0o777, 0o600)
        evidence = next((self.session.path / "evidence").glob("*-native-config-test.json")).read_text()
        self.assertNotIn("withheld-value", evidence)
        self.assertEqual(json.loads(evidence)["result"], {"category": "auth_config_rejected", "line": 7, "exit_code": 1})

    def test_native_exit_metadata_and_private_output_do_not_expose_argv(self):
        process = mock.MagicMock()
        process.__enter__.return_value = process
        process.poll.return_value = 17
        process.returncode = 17

        def spawn(_command, **kwargs):
            kwargs["stdout"].write(b"private stdout value")
            kwargs["stderr"].write(b"private stderr value")
            return process

        output = self.session.path / "native-config-test.output"
        with mock.patch.object(controller.subprocess, "Popen", side_effect=spawn):
            with self.assertRaisesRegex(controller.Refused, "native_command_failed") as failure:
                controller.run(["private-program", "private-argument"], command_class="nats_config_test", private_output=output)
        self.assertEqual(failure.exception.command_class, "nats_config_test")
        self.assertEqual(failure.exception.exit_code, 17)
        self.assertNotIn("private", str(failure.exception))
        self.assertEqual(output.stat().st_mode & 0o777, 0o600)
        self.assertIn(b"private stderr value", output.read_bytes())

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
