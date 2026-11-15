#!/usr/bin/env python3

import os
from pathlib import Path
import subprocess
import tempfile
import unittest


ENTRY = Path(__file__).resolve().parents[1] / "ci/test-integration-oauth.sh"

LOCK = """case "$1" in
    --resource-register) echo resource ;;
    --resource-run) echo container ;;
    --resource-cleanup) echo cleanup >> events ;;
    --assert-held|--resource-bind) ;;
    *) exit 1 ;;
esac
"""

CURL = """echo curl >> events
case "$SCENARIO" in
    ready) exit 0 ;;
    retry) if [ -f first-attempt ]; then exit 0; fi; touch first-attempt ;;
    stuck-http) echo $$ > probe.pid; exec sleep 8 ;;
    late)
        observer=$PPID
        kill -STOP "$observer"
        (sleep 3; kill -CONT "$observer") </dev/null >/dev/null 2>&1 &
        exit 0 ;;
    shared-budget) sleep 1.25 ;;
esac
exit 22
"""

DOCKER = """case "$1" in
    info) ;;
    port) echo 127.0.0.1:32100 ;;
    inspect)
        echo inspect >> events
        case "$SCENARIO" in
            stopped) echo false ;;
            uninspectable) exit 1 ;;
            stuck-inspect) echo $$ > probe.pid; exec sleep 8 ;;
            shared-budget) sleep 1.25; echo inspect-finished >> events; echo true ;;
            *) echo true ;;
        esac ;;
    logs) echo logs >> events ;;
    *) exit 1 ;;
esac
"""


class Readiness(unittest.TestCase):
    def check_scenario(self, name, expected_status, expected_events, budget=180):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            ci = root / "scripts/ci"
            ci.mkdir(parents=True)
            script = ci / ENTRY.name
            script.write_text(ENTRY.read_text().replace("READY_TIMEOUT_SECONDS=180",
                                                        f"READY_TIMEOUT_SECONDS={budget}", 1))
            (ci / "validation-lock.sh").write_text(LOCK)
            for binary, body in (("curl", CURL), ("docker", DOCKER),
                                 ("cargo", 'echo "cargo $OAUTH_TEST_KEYCLOAK_URL" >> events')):
                executable = root / binary
                executable.write_text("#!/bin/sh\n" + body + "\n")
                executable.chmod(0o755)
            result = subprocess.run(
                ["bash", str(script)], capture_output=True, text=True, timeout=6,
                env={**os.environ, "PATH": f"{root}:{os.environ['PATH']}",
                     "SCENARIO": name, "VALIDATION_LOCK_TOKEN": "fixture-token",
                     "OAUTH_TEST_KEYCLOAK_URL": "http://external" if name == "external" else ""},
            )
            self.assertEqual(result.returncode, expected_status, result.stderr)
            self.assertEqual((root / "events").read_text().splitlines(), expected_events)
            if name.startswith("stuck-"):
                with self.assertRaises(ProcessLookupError):
                    os.kill(int((root / "probe.pid").read_text()), 0)

    def test_stuck_probes_and_late_success_cannot_start_tests(self):
        cases = {
            "stuck-http": ["curl"],
            "stuck-inspect": ["curl", "inspect"],
            "shared-budget": ["curl", "inspect"],
            "late": ["curl"],
        }
        for name, probes in cases.items():
            with self.subTest(scenario=name):
                self.check_scenario(name, 1, [*probes, "logs", "cleanup"], budget=2)

    def test_stopped_or_uninspectable_container_fails_early(self):
        for name in ("stopped", "uninspectable"):
            with self.subTest(scenario=name):
                self.check_scenario(name, 1, ["curl", "inspect", "logs", "cleanup"])

    def test_tests_receive_ready_managed_or_external_url(self):
        self.check_scenario("ready", 0, ["curl", "cargo http://127.0.0.1:32100", "cleanup"])
        self.check_scenario("retry", 0, ["curl", "inspect", "curl", "cargo http://127.0.0.1:32100", "cleanup"])
        self.check_scenario("external", 0, ["cargo http://external"])


if __name__ == "__main__":
    unittest.main()
