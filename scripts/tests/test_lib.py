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


if __name__ == "__main__":
    unittest.main()
