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

    def test_compose_session_file_owns_recovery_inputs_and_retains_docker_context(self):
        inherited = {
            "RECOVERY_SESSION": "/invocation/rehearsal-parent",
            "RECOVERY_ARTIFACTS": "/invocation/artifacts",
            "RECOVERY_GENERATION": "parent-generation",
            "RECOVERY_PASSWORD": "fixture",
            "DOCKER_CONTEXT": "fixture-context",
            "DOCKER_HOST": "unix:///fixture/docker.sock",
            "PATH": "/fixture/bin",
            "UNRELATED_INPUT": "fixture-value",
        }
        retained = {"DOCKER_CONTEXT": "fixture-context", "DOCKER_HOST": "unix:///fixture/docker.sock",
                    "PATH": "/fixture/bin", "UNRELATED_INPUT": "fixture-value"}
        with mock.patch.dict(controller.os.environ, inherited, clear=True), \
                mock.patch.object(controller, "run", return_value=(0, b"{}")) as native:
            self.session.compose("config", "--format", "json", timeout=10)
            self.assertEqual(dict(controller.os.environ), inherited)
        command = native.call_args.args[0]
        self.assertEqual(command[:4], ["docker", "compose", "--env-file", str(self.session.path / "compose.env")])
        self.assertEqual(command[-3:], ["config", "--format", "json"])
        self.assertEqual(native.call_args.kwargs, {"env": retained, "timeout": 10})

    def test_config_lexer_and_include_variable_forms_keep_location_without_values(self):
        controller.private_text(self.session.path / "nats1.conf",
                                'server_name: fixture\ntls { cert_file: "withheld-certificate" }\n')
        (self.session.path / "auth").mkdir(mode=0o700)
        controller.private_text(self.session.path / "auth/server.conf",
                                'operator: "/auth/operator.jwt"\nresolver: MEMORY\nresolver_preload: {\n"private-key": "withheld-secret"\n}\n')
        cases = (
            ("nats-server: Parse error on line 2: 'Expected a block-level value to end with a new line, but got withheld-secret instead.'\n",
             "node", 2, "tls", "block_value_terminator"),
            ("nats-server: error parsing include file '../auth/server.conf', variable reference for 'withheld-secret' on line 2 can not be found\n",
             "auth", 2, "resolver", "variable_not_found"),
            ("nats-server: error parsing include file '/auth/server.conf', variable reference for 'withheld-secret' on line 2 could not be parsed: private-value\n",
             "auth", 2, "resolver", "variable_parse_failed"),
            ("nats-server: error parsing include file '../auth/server.conf', Parse error on line 4: 'Unexpected key separator withheld-secret'\n",
             "auth", 4, "unknown", "unexpected_key_separator"),
        )
        for message, role, line, field, code in cases:
            with self.subTest(code=code):
                result = controller.config_diagnostic(message, self.session.path)
                self.assertEqual((result["config_role"], result["line"], result["column"], result["field"], result["parser_code"]),
                                 (role, line, None, field, code))
                self.assertNotIn("withheld", json.dumps(result))
                self.assertNotIn("private-value", json.dumps(result))

    def test_config_column_selects_only_an_owned_static_field(self):
        line = 'tls { cert_file: "withheld-certificate", key_file: "withheld-key" }'
        controller.private_text(self.session.path / "nats1.conf", line + "\n")
        column = line.index("key_file") + 1
        result = controller.config_diagnostic(
            f"nats-server: /session/node.conf:1:{column}: error parsing X509 certificate/key pair: withheld-secret\n",
            self.session.path)
        self.assertEqual(result, {"config_role": "node", "line": 1, "column": column, "field": "key_file",
                                  "parser_code": "tls_key_pair", "path_role": "node_config"})
        self.assertNotIn("withheld", json.dumps(result))

    def test_known_config_io_paths_export_roles_and_errno_codes_only(self):
        cases = (
            ("nats-server: open /tls/server.key: permission denied\n", "node", "server_key", "io_permission_denied"),
            ("nats-server: error parsing include file '../auth/server.conf', open /auth/server.conf: no such file or directory\n",
             "auth", "auth_config", "io_missing"),
            ("nats-server: open /auth/operator.jwt: read-only file system\n", "auth", "operator_jwt", "io_read_only"),
        )
        for message, role, path_role, code in cases:
            with self.subTest(code=code):
                result = controller.config_diagnostic(message, self.session.path)
                self.assertEqual(result, {"config_role": role, "line": None, "column": None, "field": "unknown",
                                          "parser_code": code, "path_role": path_role})
                self.assertNotIn("/", json.dumps(result))

    def test_unknown_include_and_symlink_do_not_read_or_export_untrusted_fields(self):
        for source in ("/outside/withheld-secret", "/auth/operator.jwt", "/tls/server.key"):
            with self.subTest(source=source), mock.patch.object(Path, "open") as read:
                result = controller.config_diagnostic(
                    f"nats-server: error parsing include file '{source}', Parse error on line 3: 'Unexpected EOF.'\n",
                    self.session.path)
            read.assert_not_called()
            self.assertEqual((result["config_role"], result["line"], result["field"], result["parser_code"]),
                             ("other", 3, "unknown", "unexpected_eof"))
            self.assertNotIn("withheld", json.dumps(result))
        with tempfile.TemporaryDirectory() as external:
            other = Path(external) / "private.conf"
            controller.private_text(other, "operator: withheld-secret\n")
            (self.session.path / "nats1.conf").symlink_to(other)
            with mock.patch.object(Path, "open") as read:
                result = controller.config_diagnostic("nats-server: Parse error on line 1: 'Unexpected EOF.'\n", self.session.path)
            read.assert_not_called()
            self.assertEqual(result["field"], "unknown")

    def test_startup_failure_retains_first_operation_and_only_safe_owned_state(self):
        self.session.data["containers"] = {"nats1": {"id": "owned-container"}}
        observed = [{"Id": "owned-container", "Config": {"Env": ["PRIVATE=withheld-value"]},
                     "State": {"Status": "exited", "Running": False, "ExitCode": 1, "OOMKilled": False,
                               "Error": "withheld-value", "Health": {"Status": "unhealthy",
                                                                       "Log": [{"Output": "withheld-value"}]}}}]
        with mock.patch.object(controller, "native_json", return_value=observed) as inspect, \
                mock.patch.object(self.session, "capture_broker_fatals", return_value={}):
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
            "broker_fatals": {},
        })

    def test_fatal_categories_require_native_fatal_prefixes_and_never_export_text(self):
        known = b"[1] 2026/10/07 03:00:00.123456 [FTL] Can't start JetStream: withheld-secret\n"
        report = controller.broker_fatal(known)
        self.assertEqual(report, {"category": "jetstream_startup", "line": 1})
        self.assertNotIn("withheld-secret", json.dumps(report))
        self.assertEqual(controller.broker_fatal(b"[1] [FTL] unfamiliar withheld-secret\n"),
                         {"category": "unclassified", "line": 1})
        for raw in (b"[1] [ERR] Can't start JetStream: withheld-secret\n",
                    b"[1] [INF] echoed [FTL] Can't start JetStream: withheld-secret\n"):
            self.assertEqual(controller.broker_fatal(raw), {"category": "unclassified", "line": None})

    def test_constructor_error_frames_export_only_known_categories_and_line_numbers(self):
        cases = (
            (b"nats-server: Error processing trusted operator keys\n", "constructor_operator_keys", 1),
            (b'\nnats-server: preload account error for "withheld-account": withheld-secret\n', "constructor_account_preload", 2),
            (b"nats-server: error resolving system account: withheld-secret\n", "constructor_system_account_resolution", 1),
            (b"nats-server: system_account in config and operator JWT must be identical\n", "constructor_system_account_mismatch", 1),
            (b"nats-server: operator withheld-key expected minor version 99 > server minor version 15\n", "constructor_operator_version", 1),
            (b"nats-server: undocumented constructor refusal withheld-secret\n", "unclassified", 1),
        )
        for raw, category, line in cases:
            with self.subTest(category=category):
                result = controller.broker_fatal(raw)
                self.assertEqual(result, {"category": category, "line": line})
                self.assertNotIn("withheld", json.dumps(result))

    def test_constructor_error_marker_must_be_anchored(self):
        for raw in (b"wrapper: nats-server: Error processing trusted operator keys\n",
                    b"[1] [INF] nats-server: error resolving system account: withheld-secret\n"):
            self.assertEqual(controller.broker_fatal(raw), {"category": "unclassified", "line": None})

    def test_runtime_log_capture_reads_only_exact_owned_brokers_before_reporting_failure(self):
        self.session.data["containers"] = {name: {"id": name + "-id"} for name in ("nats1", "nats2", "client")}
        observed = []
        for name, original in self.session.data["containers"].items():
            observed.append({"Id": original["id"], "Config": {"Labels": {
                controller.LABEL: "foreign" if name == "nats2" else self.session.data["generation"],
                "com.docker.compose.project": self.session.data["project"], "com.docker.compose.service": name}},
                "State": {"Status": "exited", "Running": False, "ExitCode": 1, "OOMKilled": False}})

        def capture(command, **kwargs):
            self.assertEqual(command, ["docker", "logs", "--tail", "80", "nats1-id"])
            self.assertLessEqual(kwargs["timeout"], 5)
            self.assertEqual(kwargs["private_output_limit"], 128 * 1024)
            controller.private_text(kwargs["private_output"], "[1] [FTL] Can't set system account: withheld-secret\n")
            return 0, b""

        with mock.patch.object(controller, "native_json", return_value=observed), \
                mock.patch.object(controller, "run", side_effect=capture) as logs:
            with self.assertRaisesRegex(controller.Refused, "native_command_failed"):
                with self.session.startup_diagnostics():
                    self.session.startup_phase("broker_up")
                    raise controller.Refused("native_command_failed", command_class="docker_compose", exit_code=1)
        logs.assert_called_once()
        evidence = next((self.session.path / "evidence").glob("*-startup-failure.json")).read_text()
        self.assertNotIn("withheld-secret", evidence)
        reports = json.loads(evidence)["result"]["broker_fatals"]
        self.assertEqual(reports, {
            "nats1": {"category": "system_account", "line": 1, "capture_exit_code": 0, "broker_exit_code": 1},
            "nats2": {"category": "unclassified", "line": None, "capture_exit_code": None, "broker_exit_code": None},
        })
        self.assertEqual((self.session.path / "broker-nats1.output").stat().st_mode & 0o777, 0o600)

    def test_runtime_log_capture_failure_preserves_original_failure(self):
        self.session.data["containers"] = {"nats1": {"id": "nats1-id"}}
        observed = [{"Id": "nats1-id", "Config": {"Labels": {
            controller.LABEL: self.session.data["generation"],
            "com.docker.compose.project": self.session.data["project"], "com.docker.compose.service": "nats1"}},
            "State": {"Status": "exited", "Running": False, "ExitCode": 1, "OOMKilled": False}}]
        with mock.patch.object(controller, "native_json", return_value=observed), \
                mock.patch.object(controller, "run", side_effect=controller.Refused(
                    "native_command_deadline", command_class="broker_logs", exit_code=-15)):
            with self.assertRaisesRegex(controller.Refused, "original_failure"):
                with self.session.startup_diagnostics():
                    self.session.startup_phase("broker_up")
                    raise controller.Refused("original_failure", command_class="docker_compose", exit_code=1)
        report = controller.load(next((self.session.path / "evidence").glob("*-startup-failure.json")))["result"]
        self.assertEqual((report["phase"], report["command_class"], report["exit_code"]), ("broker_up", "docker_compose", 1))
        self.assertEqual(report["broker_fatals"]["nats1"], {
            "category": "unclassified", "line": None, "capture_exit_code": -15, "broker_exit_code": 1,
        })

    def test_runtime_logs_cannot_extend_an_expired_session(self):
        self.session.data["expires_at"] = 100
        self.session.data["containers"] = {"nats1": {"id": "nats1-id"}}
        observed = [{"Id": "nats1-id", "Config": {"Labels": {
            controller.LABEL: self.session.data["generation"],
            "com.docker.compose.project": self.session.data["project"], "com.docker.compose.service": "nats1"}},
            "State": {"ExitCode": 1}}]
        with mock.patch.object(controller.time, "time", return_value=100), mock.patch.object(controller, "run") as logs:
            report = self.session.capture_broker_fatals(observed)
        logs.assert_not_called()
        self.assertEqual(report["nats1"], {
            "category": "unclassified", "line": None, "capture_exit_code": None, "broker_exit_code": 1,
        })

    def test_native_config_receipt_withholds_parser_text_and_keeps_image_entrypoint(self):
        def parser(*args, **kwargs):
            self.assertNotIn("--entrypoint", args)
            self.assertEqual(args[-9:], ("nats1", "timeout", "-k", "5", "20", "nats-server",
                                        "-t", "-c", "/session/node.conf"))
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
        self.assertEqual(json.loads(evidence)["result"], {
            "category": "auth_config_rejected", "line": 7, "exit_code": 1, "config_success": False,
            "unknown_flag_error": False, "usage_printed": False, "native_refusal": True,
            "config_role": "auth", "column": 3, "field": "unknown", "parser_code": "unknown", "path_role": "auth_config",
        })

    def test_native_config_requires_explicit_success_and_no_refusal_even_at_exit_zero(self):
        success = "nats-server: configuration file /session/node.conf is valid (sha256:0123456789abcdef)\n"
        cases = (
            (0, "flag provided but not defined: -test\nUsage: nats-server [options]\n", "unsupported_config_test_flag"),
            (0, "Usage: nats-server [options]\n", "config_test_usage"),
            (0, "", "config_success_unobserved"),
            (0, success + "nats-server: /session/node.conf:7:3: withheld-secret\n", "node_config_rejected"),
            (1, success, "native_test_failed"),
            (0, success, "valid"),
        )
        for code, text, category in cases:
            with self.subTest(category=category):
                def parser(*_args, **kwargs):
                    kwargs["private_output"].write_text(text)
                    kwargs["private_output"].chmod(0o600)
                    return code, b""

                with mock.patch.object(self.session, "compose", side_effect=parser), \
                        mock.patch.object(self.session, "evidence") as evidence:
                    if category == "valid":
                        self.session.test_native_config()
                    else:
                        with self.assertRaisesRegex(controller.Refused, "native_config_test_failed"):
                            self.session.test_native_config()
                operation, report = evidence.call_args.args
                self.assertEqual(operation, "native-config-test")
                self.assertEqual(report["category"], category)
                self.assertNotIn("withheld-secret", json.dumps(report))

    def test_native_exit_metadata_and_private_output_do_not_expose_argv(self):
        process = mock.MagicMock()
        process.__enter__.return_value = process
        process.poll.return_value = 17
        process.returncode = 17

        def spawn(_command, **kwargs):
            kwargs["stdout"].write(b"private stdout value")
            kwargs["stderr"].write(b"private stderr value" + b"x" * 256)
            return process

        output = self.session.path / "native-config-test.output"
        with mock.patch.object(controller.subprocess, "Popen", side_effect=spawn):
            with self.assertRaisesRegex(controller.Refused, "native_command_failed") as failure:
                controller.run(["private-program", "private-argument"], command_class="nats_config_test",
                               private_output=output, private_output_limit=64)
        self.assertEqual(failure.exception.command_class, "nats_config_test")
        self.assertEqual(failure.exception.exit_code, 17)
        self.assertNotIn("private", str(failure.exception))
        self.assertEqual(output.stat().st_mode & 0o777, 0o600)
        self.assertEqual(output.stat().st_size, 64)
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
import messaging_recovery_scenarios as scenarios


class WorkerPermissionProbe(unittest.TestCase):
    def test_permission_requires_exact_native_denial_and_never_accepts_missing_or_actual_reply(self):
        marker = ('12:34:56 >>> Unexpected NATS error: nats: permissions violation: '
                  'Permissions Violation for Publish to "$JS.API.STREAM.UPDATE.RECOVERY"\n')
        cases = (
            (0, b"", marker, None),
            (0, b"", "No responders are available\n", "worker_permission_denial_unresolved"),
            (1, b"", "connection failed\n", "worker_permission_denial_unresolved"),
            (0, b"", marker.replace("UPDATE.RECOVERY", "UPDATE.OTHER"), "worker_permission_denial_unresolved"),
            (0, b"", marker.replace("Publish to", "Subscription to"), "worker_permission_denial_unresolved"),
            (0, b'{"type":"io.nats.jetstream.api.v1.stream_update_response"}', "", "worker_can_mutate_stream_topology"),
            (0, b'{"error":{"code":400}}', marker, "worker_can_mutate_stream_topology"),
        )
        for code, reply, diagnostic, refusal in cases:
            with self.subTest(code=code, refusal=refusal), tempfile.TemporaryDirectory() as directory:
                session = mock.Mock(path=Path(directory), data={"source_stream": "RECOVERY"})
                before = {"name": "RECOVERY", "max_bytes": 1024}
                session.request.return_value = {"config": before}

                def client(*_args, **kwargs):
                    if "private_output" not in kwargs:
                        return 0, json.dumps({"config": before}).encode()
                    controller.private_text(kwargs["private_output"], diagnostic)
                    return code, reply

                session.client_exec.side_effect = client
                rehearsal = scenarios.Rehearsal.__new__(scenarios.Rehearsal)
                rehearsal.c = controller
                rehearsal.session = session
                rehearsal.info = mock.Mock(return_value={"config": before})
                if refusal:
                    with self.assertRaisesRegex(controller.Refused, refusal):
                        rehearsal.worker_permissions()
                else:
                    rehearsal.worker_permissions()
                report = session.evidence.call_args.args[1]
                self.assertEqual(report["worker_stream_update_refused"], refusal is None)
                self.assertNotIn("Permissions Violation", json.dumps(report))
                self.assertNotIn("$JS.API", json.dumps(report))


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
