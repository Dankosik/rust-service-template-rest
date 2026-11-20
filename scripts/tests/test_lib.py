"""Unit tests for shared script primitives (Docker refusal and ComposeStack signal unwinding)."""

from __future__ import annotations

import os
import signal
import unittest
from unittest import mock

from scripts import lib


class LibTests(unittest.TestCase):
    def test_require_docker_missing_binary_refuses_cleanly(self) -> None:
        with mock.patch("subprocess.run", side_effect=FileNotFoundError("docker")):
            with mock.patch.dict(os.environ, {}, clear=False):
                os.environ.pop("REQUIRE_DOCKER", None)
                with self.assertRaises(lib.ScriptError) as ctx:
                    lib.require_docker(allow_refusal=True)
                self.assertEqual(ctx.exception.exit_code, 2)

            with mock.patch.dict(os.environ, {"REQUIRE_DOCKER": "1"}, clear=False):
                with self.assertRaises(lib.ScriptError) as ctx:
                    lib.require_docker(allow_refusal=True)
                self.assertEqual(ctx.exception.exit_code, 1)

    def test_compose_stack_signal_unwinds_inner_finally_before_down(self) -> None:
        events: list[str] = []
        stack = lib.ComposeStack("test-signal")
        stack.active = True

        def fake_down(*, strict: bool = False) -> None:
            events.append(f"down:{strict}")
            stack.active = False

        with mock.patch.object(stack, "down", side_effect=fake_down):
            with self.assertRaises(SystemExit) as ctx:
                with stack:
                    try:
                        os.kill(os.getpid(), signal.SIGTERM)
                    finally:
                        events.append("inner-cleanup")
            self.assertEqual(ctx.exception.code, 128 + signal.SIGTERM)
        self.assertEqual(events, ["inner-cleanup", "down:False"])

    def test_compose_stack_start_and_wait_overlap_readiness(self) -> None:
        stack = lib.ComposeStack("test-overlap")
        calls: list[list[str]] = []

        def fake_run(cmd: list[str], **_kwargs: object) -> mock.MagicMock:
            calls.append(list(cmd))
            out = "127.0.0.1:49152\n" if "port" in cmd else "cid123\n"
            return mock.MagicMock(returncode=0, stdout=out, stderr="")

        with mock.patch.object(lib, "run", side_effect=fake_run), mock.patch.object(lib, "wait_http_ready") as wait_http:
            port = stack.start("keycloak", 8080, profile="oauth")
            self.assertEqual(port, 49152)
            self.assertTrue(stack.active)
            stack.wait("keycloak", port, profile="oauth", ready_url="/realms/master", ready_timeout_s=30)
            wait_http.assert_called_once_with(
                "http://127.0.0.1:49152/realms/master",
                container="cid123",
                timeout_s=30,
            )
            stack.wait("postgres", 5432, timeout_s=20)
        self.assertIn("up", calls[0])
        self.assertNotIn("--wait", calls[0])
        self.assertIn("--no-recreate", calls[-1])
        self.assertIn("--wait", calls[-1])
        stack.active = False


if __name__ == "__main__":
    unittest.main()
