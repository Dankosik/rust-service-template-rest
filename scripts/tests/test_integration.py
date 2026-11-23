"""Unit tests for integration runner target filtering, nextest parallelism, and shared Postgres sqlx-check."""

from __future__ import annotations

import os
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from scripts import integration, lib


class _FakeStack:
    def __init__(self, events: list[str]) -> None:
        self.events = events

    def __enter__(self) -> _FakeStack:
        return self

    def __exit__(self, *_exc: object) -> None:
        self.events.append("stack:down")

    def start(self, service: str, container_port: int, **_kwargs: object) -> int:
        self.events.append(f"start:{service}")
        return 10000 + container_port

    def wait(self, service: str, _port: int, **_kwargs: object) -> None:
        self.events.append(f"wait:{service}")

    def up(self, service: str, container_port: int, **_kwargs: object) -> int:
        self.events.append(f"up:{service}")
        return 10000 + container_port


class IntegrationRunnerTests(unittest.TestCase):
    def test_db_test_args_selects_only_integration_manifest_targets(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "test").mkdir(parents=True)
            (root / "test/Cargo.toml").write_text(
                """
                [[test]]
                name = "postgres"
                path = "tests/postgres.rs"
                required-features = ["integration"]

                [[test]]
                name = "orders"
                path = "tests/orders.rs"
                required-features = ["integration"]

                [[test]]
                name = "outbound_http"
                path = "tests/outbound_http.rs"
                """,
                encoding="utf-8",
            )
            self.assertEqual(integration._db_test_args(root), ["--test", "postgres", "--test", "orders"])

    def test_cmd_db_uses_unified_nextest_overlaps_startup_and_reuses_postgres_for_sqlx_check(self) -> None:
        if not (lib.ROOT / "crates/infra-postgres").is_dir():
            self.skipTest("postgres profile not present")
        events: list[str] = []
        commands: list[list[str]] = []

        def fake_run(cmd: list[str], **_kwargs: object) -> mock.MagicMock:
            commands.append(list(cmd))
            events.append("run:" + " ".join(cmd[:4]))
            return mock.MagicMock(returncode=0)

        with (
            mock.patch.object(lib, "require_docker"),
            mock.patch.object(lib, "ComposeStack", return_value=_FakeStack(events)),
            mock.patch.object(lib, "run", side_effect=fake_run),
            mock.patch.object(integration.shutil, "which", return_value="/usr/bin/tool"),
            mock.patch.object(integration, "_verify_sqlx_cli"),
            mock.patch.dict(os.environ, {"INTEGRATION_DB_TEST_THREADS": "4"}, clear=False),
        ):
            integration.cmd_db([], with_sqlx_check=True)

        self.assertEqual(events[0], "start:postgres")
        compile_idx = events.index("run:cargo nextest run --locked")
        wait_idx = events.index("wait:postgres")
        self.assertLess(compile_idx, wait_idx, "compilation must overlap with container startup")
        self.assertIn("--no-run", commands[0])
        self.assertIn("integration-tests/integration", commands[0])
        self.assertNotIn("utility_values", " ".join(commands[0]))
        self.assertTrue(any(cmd[:3] == ["sqlx", "database", "create"] for cmd in commands))
        self.assertTrue(any(cmd[:4] == ["cargo", "sqlx", "prepare", "--workspace"] for cmd in commands))
        self.assertEqual(events.count("start:postgres"), 1, "sqlx-check must reuse the running Postgres container")

    def test_cmd_oauth_overlaps_keycloak_startup_with_compilation(self) -> None:
        if not (lib.ROOT / "crates/infra-oauth2-client-credentials").is_dir():
            self.skipTest("outbound-auth profile not present")
        events: list[str] = []

        def fake_run(cmd: list[str], **_kwargs: object) -> mock.MagicMock:
            events.append("compile" if "--no-run" in cmd else "test")
            return mock.MagicMock(returncode=0)

        with (
            mock.patch.object(lib, "require_docker"),
            mock.patch.object(lib, "ComposeStack", return_value=_FakeStack(events)),
            mock.patch.object(lib, "run", side_effect=fake_run),
            mock.patch.dict(os.environ, {}, clear=False),
        ):
            os.environ.pop("OAUTH_TEST_KEYCLOAK_URL", None)
            integration.cmd_oauth([])

        self.assertEqual(events, ["start:keycloak", "compile", "wait:keycloak", "test", "stack:down"])

    def test_cmd_clickhouse_overlaps_startup_with_compilation_and_injects_env(self) -> None:
        if not (lib.ROOT / "crates/infra-clickhouse").is_dir():
            self.skipTest("clickhouse profile not present")
        events: list[str] = []
        test_envs: list[dict[str, str]] = []

        def fake_run(cmd: list[str], **kwargs: object) -> mock.MagicMock:
            events.append("compile" if "--no-run" in cmd else "test")
            if "--no-run" not in cmd:
                env = kwargs.get("env")
                assert isinstance(env, dict)
                test_envs.append(env)
            return mock.MagicMock(returncode=0)

        with (
            mock.patch.object(lib, "require_docker"),
            mock.patch.object(lib, "ComposeStack", return_value=_FakeStack(events)),
            mock.patch.object(lib, "run", side_effect=fake_run),
            mock.patch.dict(os.environ, {}, clear=False),
        ):
            os.environ.pop("CLICKHOUSE_URL", None)
            integration.cmd_clickhouse([])

        self.assertEqual(events, ["start:clickhouse", "compile", "wait:clickhouse", "test", "stack:down"])
        self.assertEqual(len(test_envs), 1)
        env = test_envs[0]
        self.assertEqual(env["CLICKHOUSE_URL"], "http://127.0.0.1:18123")
        self.assertEqual(env["CLICKHOUSE_DATABASE"], "app")
        self.assertEqual(env["CLICKHOUSE_USER"], "app")
        self.assertEqual(env["CLICKHOUSE_PASSWORD"], "app")
        self.assertEqual(env["CLICKHOUSE_READONLY_USER"], "app_ro")
        self.assertEqual(env["CLICKHOUSE_READONLY_PASSWORD"], "app_ro")

    def test_cmd_compile_retained_compiles_clickhouse_integration_target(self) -> None:
        if not (lib.ROOT / "crates/infra-clickhouse").is_dir():
            self.skipTest("clickhouse profile not present")
        commands: list[list[str]] = []

        def fake_run(cmd: list[str], **_kwargs: object) -> mock.MagicMock:
            commands.append(list(cmd))
            return mock.MagicMock(returncode=0)

        with mock.patch.object(lib, "run", side_effect=fake_run):
            integration.cmd_compile("retained")

        self.assertTrue(
            any(
                "-p" in cmd and "infra-clickhouse" in cmd and "--test" in cmd and "clickhouse" in cmd and "--no-run" in cmd
                for cmd in commands
            ),
            commands,
        )


if __name__ == "__main__":
    unittest.main()
