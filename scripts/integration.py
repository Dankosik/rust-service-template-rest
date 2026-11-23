#!/usr/bin/env python3
"""Run throwaway Docker Compose integration suites and SQLx offline metadata checks."""

from __future__ import annotations

import argparse
import os
import shutil
import subprocess
import sys
import tempfile
import tomllib
from collections.abc import Sequence
from pathlib import Path

from scripts import lib


_INBOUND_LIFECYCLE_TEST = (
    "restarting_with_an_old_inbound_receipt_acknowledges_redelivery_without_another_job"
)


def _db_test_args(root: Path = lib.ROOT) -> list[str]:
    manifest_path = root / "test/Cargo.toml"
    if not manifest_path.is_file():
        return []
    manifest = tomllib.loads(manifest_path.read_text(encoding="utf-8"))
    args: list[str] = []
    for target in manifest.get("test", ()):
        # template:begin temporal:integration-db-skip-temporal
        if target["name"] == "temporal":
            continue
        # template:end temporal:integration-db-skip-temporal
        if "integration" in target.get("required-features", ()):
            args.extend(["--test", target["name"]])
    return args


def _has_explicit_target_filter(extra: Sequence[str]) -> bool:
    for arg in extra:
        if arg == "--":
            return False
        if arg in {"--test", "--tests", "--lib", "--bin", "--bins", "--doc", "--all-targets"} or arg.startswith(("--test=", "--bin=")):
            return True
    return False


def _db_test_threads() -> str:
    if raw := os.environ.get("INTEGRATION_DB_TEST_THREADS"):
        return raw
    return str(min(4, max(2, os.cpu_count() or 2)))


def _is_no_run(extra_args: Sequence[str]) -> bool:
    for arg in extra_args:
        if arg == "--":
            return False
        if arg == "--no-run":
            return True
    return False


def cmd_compile(suite: str) -> None:
    if suite == "retained" and (lib.ROOT / "crates/infra-postgres").is_dir():
        lib.run(
            ["cargo", "test", "--locked", "-p", "integration-tests", "--features", "integration", *_db_test_args(), "--no-run"],
            env=lib.clean_pg_env({"DATABASE_URL": "compile-only", "PGBOUNCER_DATABASE_URL": "compile-only"}),
        )
    if (lib.ROOT / "crates/infra-messaging").is_dir():
        lib.run(
            [
                "cargo", "test", "--locked", "-p", "infra-messaging", "--features", "integration",
                "--test", "jetstream", "--test", "idle_pull", "--test", "credential_rotation",
                "--example", "dlq_recovery", "--no-run",
            ]
        )
        if (lib.ROOT / "crates/infra-messaging/tests/wire_compat.rs").is_file():
            lib.run(["cargo", "test", "--locked", "-p", "infra-messaging", "--test", "wire_compat", "--no-run"])
    if (lib.ROOT / "crates/infra-cache").is_dir():
        lib.run(
            ["cargo", "test", "--locked", "-p", "infra-cache", "--features", "integration", "--test", "valkey", "--no-run"],
            env=lib.clean_pg_env({"CACHE_URL": "compile-only"}),
        )
    if (lib.ROOT / "crates/infra-object-storage").is_dir():
        lib.run(
            [
                "cargo", "test", "--locked", "-p", "infra-object-storage", "--features", "integration",
                "--test", "emulator", "--no-run",
            ],
            env=lib.clean_pg_env(
                {
                    "OBJECT_STORAGE_TEST_ENDPOINT": "compile-only",
                    "OBJECT_STORAGE_TEST_ACCESS_KEY_ID": "template",
                    "OBJECT_STORAGE_TEST_SECRET_ACCESS_KEY": "template-secret",
                }
            ),
        )
    if suite == "retained" and (lib.ROOT / "crates/infra-oauth2-client-credentials").is_dir():
        lib.run(
            [
                "cargo", "test", "--locked", "-p", "infra-oauth2-client-credentials", "--features", "integration",
                "tests::keycloak", "--no-run",
            ],
            env=lib.clean_pg_env({"OAUTH_TEST_KEYCLOAK_URL": "compile-only"}),
        )
    # template:begin temporal:integration-compile-temporal
    if (lib.ROOT / "crates/infra-temporal").is_dir():
        temporal_env = lib.clean_pg_env({"TEMPORAL_ADDRESS": "compile-only"})
        lib.run(
            ["cargo", "test", "--locked", "-p", "infra-temporal", "--features", "integration", "--test", "temporal", "--no-run"],
            env=temporal_env,
        )
        lib.run(
            ["cargo", "test", "--locked", "-p", "integration-tests", "--features", "integration", "--test", "temporal", "--no-run"],
            env=temporal_env,
        )
    # template:end temporal:integration-compile-temporal


def cmd_db(extra: Sequence[str], *, with_sqlx_check: bool = False) -> None:
    if not (lib.ROOT / "crates/infra-postgres").is_dir():
        raise lib.ScriptError("postgres profile not present", exit_code=2)
    target_args = [] if _has_explicit_target_filter(extra) else _db_test_args()
    if _is_no_run(extra):
        lib.run(
            ["cargo", "test", "--locked", "-p", "integration-tests", "--features", "integration", *target_args, *extra],
            env=lib.clean_pg_env(
                {
                    "DATABASE_URL": os.environ.get("DATABASE_URL") or "compile-only",
                    "PGBOUNCER_DATABASE_URL": os.environ.get("PGBOUNCER_DATABASE_URL") or "compile-only",
                }
            ),
        )
        return
    run_sqlx = with_sqlx_check or os.environ.get("INTEGRATION_DB_SQLX_CHECK") == "1"
    if run_sqlx:
        _verify_sqlx_cli()
    lib.require_docker(allow_refusal=True)
    include_lifecycle = (not extra) and (lib.ROOT / "crates/service/src/bootstrap/inbound_webhooks.rs").is_file()
    use_nextest = (not extra) and (shutil.which("cargo-nextest") is not None)
    compile_env = lib.clean_pg_env({"DATABASE_URL": "compile-only", "PGBOUNCER_DATABASE_URL": "compile-only"})
    with lib.ComposeStack("service-db") as stack:
        pg_port = stack.start("postgres", 5432)
        nats_port = stack.start("nats", 4222) if (lib.ROOT / "crates/infra-messaging").is_dir() else None
        if use_nextest:
            base = ["cargo", "nextest", "run", "--locked", "--no-tests=pass", "-p", "integration-tests"]
            service_pkg = ""
            if include_lifecycle:
                service_pkg, _ = lib.read_service_meta()
                base.extend(["-p", service_pkg])
            base.extend(["--features", "integration-tests/integration", *target_args])
            if include_lifecycle:
                base.extend(["--test", "lifecycle"])
            lib.run([*base, "--no-run"], env=compile_env)
        else:
            lib.run(
                ["cargo", "test", "--locked", "-p", "integration-tests", "--features", "integration", *target_args, "--no-run"],
                env=compile_env,
            )
        stack.wait("postgres", pg_port)
        pgb_port = stack.up("pgbouncer", 6432)
        env_extra = {
            "DATABASE_URL": f"postgres://app:app@127.0.0.1:{pg_port}/app?sslmode=disable",
            "PGBOUNCER_DATABASE_URL": f"postgres://app:app@127.0.0.1:{pgb_port}/app?sslmode=disable",
        }
        if nats_port is not None:
            stack.wait("nats", nats_port)
            env_extra["NATS_URL"] = f"nats://127.0.0.1:{nats_port}"
        env = lib.clean_pg_env(env_extra)
        if use_nextest:
            lib.run([*base, "--test-threads", _db_test_threads(), "-E", "package(integration-tests)"], env=env)
            if include_lifecycle:
                lib.run(
                    [
                        *base, "--test-threads", "1", "--run-ignored", "only",
                        "-E", f"package({service_pkg}) & test(={_INBOUND_LIFECYCLE_TEST})",
                    ],
                    env=env,
                )
        else:
            lib.run(
                ["cargo", "test", "--locked", "-p", "integration-tests", "--features", "integration", *target_args, *extra],
                env=env,
            )
            if include_lifecycle:
                lib.run(
                    [
                        "cargo", "test", "--locked", "--manifest-path", "crates/service/Cargo.toml",
                        "--test", "lifecycle", _INBOUND_LIFECYCLE_TEST, "--", "--ignored", "--exact",
                    ],
                    env=env,
                )
        if run_sqlx:
            _run_sqlx_prepare_against_port(pg_port, check_only=True)


def cmd_cache(extra: Sequence[str]) -> None:
    if not (lib.ROOT / "crates/infra-cache").is_dir():
        raise lib.ScriptError("cache profile not present", exit_code=2)
    cmd = ["cargo", "test", "--locked", "-p", "infra-cache", "--features", "integration", "--test", "valkey", *extra]
    if _is_no_run(extra):
        lib.run(cmd, env=lib.clean_pg_env({"CACHE_URL": os.environ.get("CACHE_URL") or "compile-only"}))
        return
    if existing := os.environ.get("CACHE_URL"):
        lib.run(cmd, env=lib.clean_pg_env({"CACHE_URL": existing}))
        return
    lib.require_docker(allow_refusal=True)
    with lib.ComposeStack("service-cache") as stack:
        port = stack.up("valkey", 6379)
        lib.run(cmd, env=lib.clean_pg_env({"CACHE_URL": f"redis://127.0.0.1:{port}"}))


def cmd_object_storage(extra: Sequence[str]) -> None:
    if not (lib.ROOT / "crates/infra-object-storage").is_dir():
        raise lib.ScriptError("object-storage profile not present", exit_code=2)
    cmd = [
        "cargo", "test", "--locked", "-p", "infra-object-storage", "--features", "integration",
        "--test", "emulator", *extra,
    ]
    creds = {
        "OBJECT_STORAGE_TEST_ACCESS_KEY_ID": os.environ.get("OBJECT_STORAGE_TEST_ACCESS_KEY_ID") or "template",
        "OBJECT_STORAGE_TEST_SECRET_ACCESS_KEY": os.environ.get("OBJECT_STORAGE_TEST_SECRET_ACCESS_KEY") or "template-secret",
    }
    if _is_no_run(extra):
        lib.run(cmd, env=lib.clean_pg_env({**creds, "OBJECT_STORAGE_TEST_ENDPOINT": os.environ.get("OBJECT_STORAGE_TEST_ENDPOINT") or "compile-only"}))
        return
    if existing := os.environ.get("OBJECT_STORAGE_TEST_ENDPOINT"):
        lib.run(cmd, env=lib.clean_pg_env({**creds, "OBJECT_STORAGE_TEST_ENDPOINT": existing}))
        return
    lib.require_docker(allow_refusal=True)
    with lib.ComposeStack("service-s3") as stack:
        port = stack.up("versitygw", 7070)
        lib.run(cmd, env=lib.clean_pg_env({**creds, "OBJECT_STORAGE_TEST_ENDPOINT": f"http://127.0.0.1:{port}"}))


def cmd_messaging(extra: Sequence[str]) -> None:
    if not (lib.ROOT / "crates/infra-messaging").is_dir():
        raise lib.ScriptError("messaging profile not present", exit_code=2)
    if _is_no_run(extra):
        lib.run(
            [
                "cargo", "test", "--locked", "-p", "infra-messaging", "--features", "integration",
                "--test", "jetstream", "--test", "idle_pull", "--test", "credential_rotation",
                "--example", "dlq_recovery", *extra,
            ]
        )
        return
    lib.require_docker(allow_refusal=True)
    base_cmd = ["cargo", "test", "--locked", "-p", "infra-messaging", "--features", "integration"]
    lib.run([*base_cmd, "--test", "jetstream", "--test", "idle_pull", "--example", "dlq_recovery", "--no-run"])

    failures: list[str] = []
    with tempfile.TemporaryDirectory(prefix="service-nats-") as tmp:
        work = Path(tmp)
        ordinary_url = os.environ.get("NATS_URL")
        ordinary_cfg = str(lib.ROOT / "env/nats/nats-server.conf")

        with lib.ComposeStack("service-nats-ordinary") as stack:
            nats_env = {"NATS_PORT": "127.0.0.1:0", "NATS_CONFIG_FILE": ordinary_cfg}
            if not ordinary_url:
                port = stack.up("nats", 4222, timeout_s=30, extra_env=nats_env)
                ordinary_url = f"nats://127.0.0.1:{port}"

            idle_log = work / "idle.log"
            with idle_log.open("wb") as log_handle:
                idle_proc = subprocess.Popen(
                    [*base_cmd, "--test", "idle_pull", *extra],
                    cwd=lib.ROOT,
                    env=lib.clean_pg_env({"NATS_URL": ordinary_url}),
                    stdout=log_handle,
                    stderr=subprocess.STDOUT,
                )
                try:
                    main_res = lib.run(
                        [*base_cmd, "--test", "jetstream", "--example", "dlq_recovery", *extra],
                        env=lib.clean_pg_env({"NATS_URL": ordinary_url}),
                        check=False,
                    )
                    idle_rc = idle_proc.wait()
                except BaseException:
                    idle_proc.terminate()
                    idle_proc.wait()
                    raise
            sys.stdout.write(idle_log.read_text(encoding="utf-8", errors="replace"))
            if main_res.returncode != 0:
                failures.append(f"jetstream/dlq_recovery exited {main_res.returncode}")
            if idle_rc != 0:
                failures.append(f"idle_pull exited {idle_rc}")

            if not failures:
                if not stack.active:
                    port = stack.up("nats", 4222, timeout_s=30, extra_env=nats_env)
                    ordinary_url = f"nats://127.0.0.1:{port}"
                receipt = str(work / "cursor.json")
                lib.run(
                    [*base_cmd, "--test", "jetstream", "connection::save_ack_floor_before_restart", "--", "--ignored", "--exact"],
                    env=lib.clean_pg_env({"NATS_URL": ordinary_url, "NATS_CURSOR_RECEIPT": receipt}),
                )
                new_port = stack.restart("nats", 4222, extra_env=nats_env, timeout_s=30)
                lib.run(
                    [*base_cmd, "--test", "jetstream", "connection::read_ack_floor_after_restart", "--", "--ignored", "--exact"],
                    env=lib.clean_pg_env({"NATS_URL": f"nats://127.0.0.1:{new_port}", "NATS_CURSOR_RECEIPT": receipt}),
                )

        fixture_dir = lib.ROOT / "env/nats/auth-rotation"
        with lib.ComposeStack("service-nats-auth") as stack:
            port = stack.up(
                "nats",
                4222,
                timeout_s=30,
                extra_env={"NATS_PORT": "127.0.0.1:0", "NATS_CONFIG_FILE": str(fixture_dir / "nats-server.conf")},
            )
            res = lib.run(
                [
                    *base_cmd, "--test", "jetstream",
                    "credentials::credentials_file_rotation_is_authenticated_by_the_broker",
                    "--", "--ignored", "--exact", "--no-capture",
                ],
                env=lib.clean_pg_env(
                    {
                        "NATS_AUTH_URL": f"nats://127.0.0.1:{port}",
                        "NATS_AUTH_CREDS_A": str(fixture_dir / "user-a.creds"),
                        "NATS_AUTH_CREDS_B": str(fixture_dir / "user-b.creds"),
                    }
                ),
                check=False,
            )
            if res.returncode != 0:
                failures.append(f"auth-rotation exited {res.returncode}")

        with lib.ComposeStack("service-nats-cred-rot") as stack:
            port = stack.up(
                "nats",
                4222,
                timeout_s=30,
                extra_env={"NATS_PORT": "127.0.0.1:0", "NATS_CONFIG_FILE": str(lib.ROOT / "env/nats/credential-rotation.conf")},
            )
            res = lib.run(
                [
                    *base_cmd, "--test", "credential_rotation",
                    "expired_old_credentials_are_refused_and_file_replacement_recovers_the_client",
                    "--", "--exact", "--no-capture",
                ],
                env=lib.clean_pg_env({"NATS_AUTH_URL": f"nats://127.0.0.1:{port}"}),
                check=False,
            )
            if res.returncode != 0:
                failures.append(f"credential-rotation exited {res.returncode}")

    if failures:
        raise lib.ScriptError("; ".join(failures))


def cmd_oauth(extra: Sequence[str]) -> None:
    if not (lib.ROOT / "crates/infra-oauth2-client-credentials").is_dir():
        raise lib.ScriptError("oauth profile not present", exit_code=2)
    cmd = [
        "cargo", "test", "--locked", "-p", "infra-oauth2-client-credentials",
        "--features", "integration", "tests::keycloak", *extra,
    ]
    if _is_no_run(extra):
        lib.run(cmd, env=lib.clean_pg_env({"OAUTH_TEST_KEYCLOAK_URL": os.environ.get("OAUTH_TEST_KEYCLOAK_URL") or "compile-only"}))
        return
    if existing := os.environ.get("OAUTH_TEST_KEYCLOAK_URL"):
        lib.run(cmd, env=lib.clean_pg_env({"OAUTH_TEST_KEYCLOAK_URL": existing}))
        return
    lib.require_docker(allow_refusal=True)
    with lib.ComposeStack("service-oauth") as stack:
        port = stack.start("keycloak", 8080, profile="oauth")
        env = lib.clean_pg_env({"OAUTH_TEST_KEYCLOAK_URL": f"http://127.0.0.1:{port}"})
        compile_cmd = [
            "cargo", "test", "--locked", "-p", "infra-oauth2-client-credentials",
            "--features", "integration", "--no-run",
        ]
        lib.run(compile_cmd, env=env)
        stack.wait("keycloak", port, profile="oauth", ready_url="/realms/master", ready_timeout_s=180)
        lib.run(cmd, env=env)


# template:begin temporal:integration-cmd-temporal
def cmd_temporal(extra: Sequence[str]) -> None:
    if not (lib.ROOT / "crates/infra-temporal").is_dir():
        raise lib.ScriptError("temporal profile not present", exit_code=2)
    adapter_base = ["cargo", "test", "--locked", "-p", "infra-temporal", "--features", "integration", "--test", "temporal"]
    fixture_base = ["cargo", "test", "--locked", "-p", "integration-tests", "--features", "integration", "--test", "temporal"]
    adapter_cmd = [*adapter_base, *extra]
    fixture_cmd = [*fixture_base, *extra]
    if _is_no_run(extra):
        compile_env = lib.clean_pg_env({"TEMPORAL_ADDRESS": os.environ.get("TEMPORAL_ADDRESS") or "compile-only"})
        lib.run(adapter_cmd, env=compile_env)
        lib.run(fixture_cmd, env=compile_env)
        return
    if existing := os.environ.get("TEMPORAL_ADDRESS"):
        run_env = lib.clean_pg_env({"TEMPORAL_ADDRESS": existing})
        lib.run(adapter_cmd, env=run_env)
        lib.run(fixture_cmd, env=run_env)
        return
    lib.require_docker(allow_refusal=True)
    with lib.ComposeStack("service-temporal") as stack:
        port = stack.start("temporal", 7233)
        compile_env = lib.clean_pg_env({"TEMPORAL_ADDRESS": "compile-only"})
        lib.run([*adapter_base, "--no-run"], env=compile_env)
        lib.run([*fixture_base, "--no-run"], env=compile_env)
        stack.wait("temporal", port, timeout_s=60)
        run_env = lib.clean_pg_env({"TEMPORAL_ADDRESS": f"127.0.0.1:{port}"})
        lib.run(adapter_cmd, env=run_env)
        lib.run(fixture_cmd, env=run_env)
# template:end temporal:integration-cmd-temporal


def _verify_sqlx_cli() -> None:
    if shutil.which("cargo-sqlx") is None or shutil.which("sqlx") is None:
        raise lib.ScriptError("sqlx-cli is required; run `mise install cargo:sqlx-cli`", exit_code=2)
    cargo_toml = tomllib.loads((lib.ROOT / "Cargo.toml").read_text(encoding="utf-8"))
    sqlx_dep = cargo_toml.get("workspace", {}).get("dependencies", {}).get("sqlx")
    driver_ver = sqlx_dep if isinstance(sqlx_dep, str) else (sqlx_dep.get("version", "") if isinstance(sqlx_dep, dict) else "")
    mise_sqlx = lib.read_mise_config().get("tools", {}).get("cargo:sqlx-cli")
    cli_ver = mise_sqlx if isinstance(mise_sqlx, str) else (mise_sqlx.get("version", "") if isinstance(mise_sqlx, dict) else "")
    if cli_ver and driver_ver != cli_ver:
        raise lib.ScriptError(f"Cargo.toml pins sqlx {driver_ver or '<none>'}, mise.toml pins sqlx-cli {cli_ver}", exit_code=2)


def _run_sqlx_prepare_against_port(pg_port: int, *, check_only: bool) -> None:
    env = lib.clean_pg_env({"DATABASE_URL": f"postgres://app:app@127.0.0.1:{pg_port}/sqlx_prepare_{os.getpid()}?sslmode=disable"})
    lib.run(["sqlx", "database", "create"], env=env)
    lib.run(["sqlx", "migrate", "run", "--source", "migrations"], env=env)
    prepare_cmd = ["cargo", "sqlx", "prepare", "--workspace"]
    if check_only:
        prepare_cmd.append("--check")
    prepare_cmd.extend(["--", "--locked"])
    lib.run(prepare_cmd, env={**env, "SQLX_OFFLINE": "false"})


def cmd_sqlx_prepare(*, check_only: bool) -> None:
    _verify_sqlx_cli()
    lib.require_docker()
    with lib.ComposeStack("sqlx-prepare") as stack:
        port = stack.up("postgres", 5432)
        _run_sqlx_prepare_against_port(port, check_only=check_only)


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    comp = sub.add_parser("compile")
    comp.add_argument("--suite", choices=("retained", "services"), default="retained")
    db_p = sub.add_parser("db")
    db_p.add_argument("--with-sqlx-check", action="store_true")
    db_p.add_argument("cargo_args", nargs=argparse.REMAINDER)
    for name in (
        "cache",
        "object-storage",
        "messaging",
        "oauth",
        # template:begin temporal:integration-subparser-temporal
        "temporal",
        # template:end temporal:integration-subparser-temporal
    ):
        p = sub.add_parser(name)
        p.add_argument("cargo_args", nargs=argparse.REMAINDER)
    sqlx_p = sub.add_parser("sqlx-prepare")
    sqlx_p.add_argument("--check", action="store_true")

    args = parser.parse_args(argv)

    def _dispatch() -> None:
        if args.command == "compile":
            cmd_compile(args.suite)
        elif args.command == "sqlx-prepare":
            cmd_sqlx_prepare(check_only=args.check)
        else:
            extra = list(args.cargo_args)
            if extra and extra[0] == "--":
                extra = extra[1:]
            if args.command == "db":
                cmd_db(extra, with_sqlx_check=args.with_sqlx_check)
            else:
                {
                    "cache": cmd_cache,
                    "object-storage": cmd_object_storage,
                    "messaging": cmd_messaging,
                    "oauth": cmd_oauth,
                    # template:begin temporal:integration-dispatch-temporal
                    "temporal": cmd_temporal,
                    # template:end temporal:integration-dispatch-temporal
                }[args.command](extra)

    return lib.run_cli(_dispatch)


if __name__ == "__main__":
    raise SystemExit(main())
