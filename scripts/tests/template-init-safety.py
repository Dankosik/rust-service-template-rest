#!/usr/bin/env python3
"""Refusal and one-shot preservation checks against an isolated candidate."""

from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path


sys.dont_write_bytecode = True


_LEGACY_B206_PROFILE_SHA256 = "75e68f9c7defd4031f5d7a0bc2866f337a6c79f69a69f56c79a268d48d8d6530"
_LEGACY_B206_REVISION = "b2060279370713f05be81b1ad44a31e3e960bccc"
_HISTORICAL_OUTBOUND_REVISION = "43b7588edbdb1ebfbc478fb28e0e3d2e77417960"
_HISTORICAL_HTTP_IDEMPOTENCY_REVISION = "4819113b21c110e69f3f1d4d26f3bf9337c83b72"
_LEGACY_PROFILE_KEYS = ("schema_version", "source_only", "postgres", "identity", "cargo_lock")
_AUTH_ONLY_PROFILE_KEYS = (
    "schema_version", "source_only", "postgres", "authn", "oidc-jwt", "oidc-introspection", "identity", "cargo_lock",
)
_OUTBOUND_ONLY_PROFILE_KEYS = (
    "schema_version", "source_only", "postgres", "authn", "oidc-jwt", "oidc-introspection",
    "identity", "cargo_lock", "outbound-http", "egress-dns", "request-budget",
)
_HTTP_IDEMPOTENCY_ONLY_PROFILE_KEYS = (
    "schema_version", "source_only", "postgres", "authn", "oidc-jwt", "oidc-introspection",
    "identity", "cargo_lock", "outbound-http", "egress-dns", "request-budget", "http-idempotency",
    "http-idempotency-mounted",
)
# Historical DNS pack is replay input only; never part of current selection.
_HISTORICAL_EGRESS_PACK = {
    "remove_when_unselected": [
        "crates/infra-egress-dns/"
    ],
    "markers": [
        {
            "path": "Cargo.toml",
            "ids": [
                "workspace-egress-dns-crate",
                "workspace-egress-dns",
                "workspace-egress-tls-fixtures"
            ]
        },
        {
            "path": "docs/architecture/boundaries.md",
            "ids": [
                "docs-boundaries-egress-owner",
                "docs-boundaries-egress-edges"
            ]
        },
        {
            "path": "docs/project-structure-and-module-organization.md",
            "ids": [
                "docs-structure-egress-placement"
            ]
        },
        {
            "path": "docs/build-test-and-development-commands.md",
            "ids": [
                "docs-commands-egress"
            ]
        }
    ]
}
_NEW_PROJECTION_CHECKER = "scripts/tests/template-profile-projections.py"


def run(args: list[str], *, cwd: Path, env: dict[str, str] | None = None) -> subprocess.CompletedProcess[str]:
    return subprocess.run(args, cwd=cwd, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False, env=env)


def state(root: Path) -> str:
    records: list[str] = []
    for path in sorted(root.rglob("*")):
        if ".git" in path.parts:
            continue
        relative = path.relative_to(root).as_posix()
        mode = path.lstat().st_mode
        if path.is_symlink():
            payload = os.readlink(path).encode()
        elif path.is_file():
            payload = path.read_bytes()
        else:
            records.append(f"dir {relative} {mode:o}")
            continue
        records.append(f"file {relative} {mode:o} {hashlib.sha256(payload).hexdigest()}")
    status = run(["git", "status", "--porcelain=v1", "--ignored"], cwd=root)
    records.append(f"git {status.stdout}")
    return "\n".join(records)


def clone(source: Path, destination: Path) -> None:
    result = run(["git", "clone", "--quiet", "--no-local", os.fspath(source), os.fspath(destination)], cwd=source)
    if result.returncode:
        raise AssertionError(result.stderr)


def init(
    source: Path, target: Path, *extra: str, environment: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    baseline = [
        ("--service-name", "safety-api"), ("--repository", "https://github.com/example/safety-api"),
        ("--description", "Safety API 👩‍💻"), ("--codeowner", "@example/platform"),
    ]
    replacements = set(extra[::2])
    identity = [value for pair in baseline if pair[0] not in replacements for value in pair]
    return run(
        [
            "bash", os.fspath(source / "scripts/init-module.sh"), "--repo", os.fspath(target),
            *identity, *extra,
        ],
        cwd=source,
        env=None if environment is None else {**os.environ, **environment},
    )


def legacy_profile_inventory_bytes(source: Path) -> bytes:
    profile = json.loads((source / "scripts/lib/template_profiles.json").read_text(encoding="utf-8"))
    legacy = {key: profile[key] for key in _LEGACY_PROFILE_KEYS}
    source_only = legacy["source_only"]
    if not isinstance(source_only, list) or source_only.count(_NEW_PROJECTION_CHECKER) != 1:
        raise AssertionError("current profile inventory lacks exactly one projection checker entry")
    legacy["source_only"] = [item for item in source_only if item != _NEW_PROJECTION_CHECKER]
    # The shared PostgreSQL proxy did not exist in the pinned b206 inventory.
    legacy["postgres"]["remove_when_none"].remove("test/tests/support/commit_proxy.rs")
    rendered = (json.dumps(legacy, indent=2) + "\n").encode("utf-8")
    if hashlib.sha256(rendered).hexdigest() != _LEGACY_B206_PROFILE_SHA256:
        raise AssertionError("legacy b206 profile inventory bytes changed")
    return rendered


def install_historical_none(source: Path, target: Path) -> None:
    (target / "scripts/lib/template_profiles.json").write_bytes(legacy_profile_inventory_bytes(source))
    lock = json.loads((target / "template.lock").read_text(encoding="utf-8"))
    lock["profiles"].pop("authn")
    lock["profiles"].pop("outbound_http", None)
    lock["profiles"].pop("outbound_auth", None)
    lock["profiles"].pop("http_idempotency", None)
    lock["profiles"].pop("jobs", None)
    lock["profiles"].pop("messaging", None)
    lock["profiles"].pop("outbox", None)
    lock["profiles"].pop("webhooks", None)
    lock["profiles"].pop("inbound_webhooks", None)
    lock["source"]["checkout_revision"] = _LEGACY_B206_REVISION
    (target / "template.lock").write_text(json.dumps(lock, indent=2) + "\n", encoding="utf-8")


def install_derived_auth_only_none(source: Path, target: Path) -> None:
    profile = json.loads((source / "scripts/lib/template_profiles.json").read_text(encoding="utf-8"))
    auth_only = {key: profile[key] for key in _AUTH_ONLY_PROFILE_KEYS}
    (target / "scripts/lib/template_profiles.json").write_text(
        json.dumps(auth_only, indent=2) + "\n", encoding="utf-8"
    )
    lock = json.loads((target / "template.lock").read_text(encoding="utf-8"))
    lock["profiles"].pop("outbound_http", None)
    lock["profiles"].pop("outbound_auth", None)
    lock["profiles"].pop("http_idempotency", None)
    lock["profiles"].pop("jobs", None)
    lock["profiles"].pop("messaging", None)
    lock["profiles"].pop("outbox", None)
    lock["profiles"].pop("webhooks", None)
    lock["profiles"].pop("inbound_webhooks", None)
    (target / "template.lock").write_text(json.dumps(lock, indent=2) + "\n", encoding="utf-8")


def install_derived_outbound_only_none(source: Path, target: Path) -> None:
    profile = json.loads((source / "scripts/lib/template_profiles.json").read_text(encoding="utf-8"))
    profile["egress-dns"] = _HISTORICAL_EGRESS_PACK
    outbound_only = {key: profile[key] for key in _OUTBOUND_ONLY_PROFILE_KEYS}
    (target / "scripts/lib/template_profiles.json").write_text(
        json.dumps(outbound_only, indent=2) + "\n", encoding="utf-8"
    )
    lock = json.loads((target / "template.lock").read_text(encoding="utf-8"))
    # The DNS-bearing outbound generation's four-field shape: a missing
    # later selection means `none` during replay.
    profiles = lock["profiles"]
    lock["profiles"] = {
        "database": profiles["database"],
        "authn": profiles.get("authn", "none"),
        "outbound_http": profiles.get("outbound_http", "none"),
        "agent_harness": profiles["agent_harness"],
    }
    lock["source"]["checkout_revision"] = _HISTORICAL_OUTBOUND_REVISION
    (target / "template.lock").write_text(json.dumps(lock, indent=2) + "\n", encoding="utf-8")


def install_derived_http_idempotency_only_none(source: Path, target: Path) -> None:
    profile = json.loads((source / "scripts/lib/template_profiles.json").read_text(encoding="utf-8"))
    profile["egress-dns"] = _HISTORICAL_EGRESS_PACK
    http_idempotency_only = {key: profile[key] for key in _HTTP_IDEMPOTENCY_ONLY_PROFILE_KEYS}
    (target / "scripts/lib/template_profiles.json").write_text(
        json.dumps(http_idempotency_only, indent=2) + "\n", encoding="utf-8"
    )
    lock = json.loads((target / "template.lock").read_text(encoding="utf-8"))
    # The http-idempotency generation's five-field shape, whatever an earlier
    # fixture left in the lock: a missing selection there means `none`.
    profiles = lock["profiles"]
    lock["profiles"] = {
        "database": profiles["database"],
        "authn": profiles.get("authn", "none"),
        "outbound_http": profiles.get("outbound_http", "none"),
        "http_idempotency": profiles.get("http_idempotency", "none"),
        "agent_harness": profiles["agent_harness"],
    }
    lock["source"]["checkout_revision"] = _HISTORICAL_HTTP_IDEMPOTENCY_REVISION
    (target / "template.lock").write_text(json.dumps(lock, indent=2) + "\n", encoding="utf-8")


def load_marker_modules(source: Path):
    library = source / "scripts/lib"
    sys.path.insert(0, os.fspath(library))
    try:
        module_path = library / "template_init.py"
        spec = importlib.util.spec_from_file_location("template_init_safety_probe", module_path)
        if spec is None or spec.loader is None:
            raise AssertionError("template_init.py cannot be imported")
        module = importlib.util.module_from_spec(spec)
        sys.modules[spec.name] = module
        spec.loader.exec_module(module)
        state = sys.modules.get("template_state")
        if state is None:
            raise AssertionError("template_state.py was not imported")
        return module, state
    finally:
        sys.path.remove(os.fspath(library))


def assert_marker_syntax(source: Path, work: Path) -> None:
    initializer, state_module = load_marker_modules(source)
    table = (
        ("\t#\ttemplate:begin authn:tabbed", ("begin", "authn", "tabbed"), True),
        ("  //  template:end oidc-jwt:spaced", ("end", "oidc-jwt", "spaced"), True),
        ("\t<!--\ttemplate:begin oidc-introspection:html\t-->\t", ("begin", "oidc-introspection", "html"), True),
        ("t// template:begin authn:literal-prefix", None, False),
    )
    for line, expected, executable in table:
        if initializer._marker(line) != expected:
            raise AssertionError(f"marker parser mismatch for {line!r}")
        if state_module._contains_profile_marker((line + "\n").encode("utf-8")) != executable:
            raise AssertionError(f"marker state mismatch for {line!r}")

    profile = initializer.ProfileData(
        source_only=(),
        removals={},
        markers=(("authn", "marker.txt", "one"), ("authn", "marker.txt", "two")),
        identity=(),
        cargo_lock={"schema_version": 1},
    )
    inputs = initializer.InitInputs(
        service_name="marker-api",
        repository="https://github.com/example/marker-api",
        description="Marker API",
        codeowner="@example/platform",
        database="none",
        authn="none",
        outbound_http="none",
        outbound_auth="none",
        http_idempotency="none",
        jobs="none",
        messaging="none",
        outbox="none",
        webhooks="none",
        inbound_webhooks="none",
        agent_harness="core",
    )
    for label, contents in (
        ("duplicate", "# template:begin authn:one\n# template:end authn:one\n# template:begin authn:one\n# template:end authn:one\n"),
        ("nested", "# template:begin authn:one\n# template:begin authn:two\n# template:end authn:two\n# template:end authn:one\n"),
        ("missing", "# ordinary content\n"),
    ):
        marker_root = work / f"marker-{label}"
        marker_root.mkdir()
        (marker_root / "marker.txt").write_text(contents, encoding="utf-8")
        try:
            initializer._apply_markers(marker_root, profile, inputs)
        except initializer.Refusal:
            continue
        raise AssertionError(f"{label} marker structure was accepted")


def assert_preflight_extraction(source: Path, work: Path) -> None:
    initializer, _state_module = load_marker_modules(source)
    calls: list[str] = []
    original_project = initializer._project_staged
    original_runtime = initializer._validate_staged_runtime
    try:
        initializer._project_staged = lambda *_args: calls.append("project")
        initializer._validate_staged_runtime = lambda *_args: calls.append("runtime")
        initializer._preflight_staged(Path("unused"), object(), object())
    finally:
        initializer._project_staged = original_project
        initializer._validate_staged_runtime = original_runtime
    if calls != ["project", "runtime"]:
        raise AssertionError(f"preflight phase order changed: {calls}")

    def arguments(target: Path) -> argparse.Namespace:
        return argparse.Namespace(
            repo=target,
            service_name="preflight-api",
            repository="https://github.com/example/preflight-api",
            description="Preflight API",
            codeowner="@example/platform",
            database="none",
            authn="none",
            outbound_http="none",
            outbound_auth="none",
            http_idempotency="none",
            jobs="none",
            messaging="none",
            outbox="none",
            webhooks="none",
            inbound_webhooks="none",
            agent_harness="core",
        )

    for phase in ("project", "runtime"):
        target = work / f"preflight-{phase}"
        clone(source, target)
        before = state(target)
        injected: list[str] = []
        original_project = initializer._project_staged
        original_runtime = initializer._validate_staged_runtime
        try:
            if phase == "project":
                def fail_project(*_args):
                    injected.append("project")
                    raise initializer.Refusal("forced projection failure")

                initializer._project_staged = fail_project
            else:
                def fail_runtime(*_args):
                    injected.append("runtime")
                    raise initializer.Refusal("forced runtime validation failure")

                initializer._project_staged = lambda *_args: None
                initializer._validate_staged_runtime = fail_runtime
            try:
                initializer.initialize(arguments(target))
            except initializer.Refusal as error:
                expected = f"forced {phase}{'ion' if phase == 'project' else ' validation'} failure"
                if str(error) != expected:
                    raise AssertionError(f"{phase} preflight raised the wrong refusal: {error}") from error
            else:
                raise AssertionError(f"{phase} preflight failure was accepted")
        finally:
            initializer._project_staged = original_project
            initializer._validate_staged_runtime = original_runtime
        if injected != [phase]:
            raise AssertionError(f"{phase} preflight did not run the injected failure: {injected}")
        if state(target) != before:
            raise AssertionError(f"{phase} preflight failure mutated its target")


def must_refuse(
    source: Path, work: Path, label: str, *extra: str,
    environment: dict[str, str] | None = None, expected: str | None = None,
) -> None:
    target = work / label
    clone(source, target)
    before = state(target)
    result = init(source, target, *extra, environment=environment)
    if result.returncode == 0:
        raise AssertionError(f"{label}: initializer unexpectedly succeeded")
    if (
        label not in {
            "duplicate-db", "duplicate-authn", "duplicate-outbound-http", "duplicate-outbound-auth",
            "duplicate-http-idempotency", "duplicate-jobs", "jobs-flag-and-environment", "duplicate-messaging",
            "messaging-flag-and-environment", "duplicate-outbox", "outbox-flag-and-environment",
            "duplicate-webhooks", "webhooks-flag-and-environment", "duplicate-inbound-webhooks",
        }
        and "may be supplied once" in result.stderr
    ):
        raise AssertionError(f"{label}: fixture accidentally exercised duplicate-option refusal")
    if expected is not None and expected not in result.stderr:
        raise AssertionError(f"{label}: refusal did not name {expected!r}: {result.stderr}")
    if state(target) != before:
        raise AssertionError(f"{label}: refusal changed target bytes or Git state")


def must_refuse_raw_description(source: Path, work: Path) -> None:
    target = work / "non-utf8-description"
    clone(source, target)
    before = state(target)
    result = subprocess.run(
        [
            b"bash", os.fsencode(source / "scripts/init-module.sh"), b"--repo", os.fsencode(target),
            b"--service-name", b"safety-api", b"--repository", b"https://github.com/example/safety-api",
            b"--description", b"bad\xed\xa0\x80description", b"--codeowner", b"@example/platform",
        ],
        cwd=os.fspath(source), stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False,
    )
    if result.returncode == 0 or state(target) != before:
        raise AssertionError("non-UTF-8 description was not a preserving refusal")


def assert_profile_pack(source: Path, target: Path, profile_name: str, selected: bool) -> None:
    profile = json.loads((source / "scripts/lib/template_profiles.json").read_text(encoding="utf-8"))
    key = "remove_when_none" if profile_name == "postgres" else "remove_when_unselected"
    for relative in profile[profile_name][key]:
        path = target / relative.rstrip("/")
        if not selected:
            if path.exists() or path.is_symlink():
                raise AssertionError(f"unselected {profile_name} output remains {relative}")
            continue
        if relative.endswith("/"):
            if path.is_symlink() or not path.is_dir():
                raise AssertionError(f"selected {profile_name} output lacks retained directory {relative}")
        elif path.is_symlink() or not path.is_file():
            raise AssertionError(f"selected {profile_name} output lacks retained file {relative}")


def assert_profile_packs(
    source: Path, target: Path, *, database: str, authn: str, outbound_http: str, http_idempotency: str,
    jobs: str, outbound_auth: str = "none", outbox: str = "none", webhooks: str = "none",
    inbound_webhooks: str = "none",
) -> None:
    assert_profile_pack(source, target, "postgres", database == "postgres")
    assert_profile_pack(source, target, "authn", authn != "none")
    assert_profile_pack(source, target, "oidc-jwt", authn == "oidc-jwt")
    assert_profile_pack(source, target, "oidc-introspection", authn == "oidc-introspection")
    assert_profile_pack(source, target, "outbound-http", outbound_http == "bounded")
    assert_profile_pack(source, target, "outbound-auth", outbound_auth == "oauth2-client-credentials")
    shared_selected = authn != "none" or outbound_http == "bounded"
    assert_profile_pack(source, target, "tls-fixtures", shared_selected)
    assert_profile_pack(
        source, target, "request-budget", outbound_http == "bounded" or http_idempotency == "postgres"
    )
    assert_profile_pack(source, target, "http-idempotency", http_idempotency == "postgres")
    assert_profile_pack(
        source, target, "http-idempotency-mounted", http_idempotency == "postgres" and authn == "oidc-introspection"
    )
    assert_profile_pack(source, target, "jobs", jobs == "postgres")
    assert_profile_pack(
        source, target, "jobs-http-idempotency", jobs == "postgres" and http_idempotency == "postgres"
    )
    assert_profile_pack(source, target, "outbox", outbox == "postgres")
    assert_profile_pack(source, target, "webhooks-common", webhooks == "durable" or inbound_webhooks == "standard-webhooks")
    assert_profile_pack(source, target, "webhooks", webhooks == "durable")
    assert_profile_pack(source, target, "inbound-webhooks", inbound_webhooks == "standard-webhooks")


def assert_lock_authn(target: Path, expected: str) -> None:
    lock = json.loads((target / "template.lock").read_text(encoding="utf-8"))
    if lock["profiles"].get("authn") != expected:
        raise AssertionError(f"template.lock did not record authn={expected}")


def assert_lock_outbound_http(target: Path, expected: str) -> None:
    lock = json.loads((target / "template.lock").read_text(encoding="utf-8"))
    if lock["profiles"].get("outbound_http") != expected:
        raise AssertionError(f"template.lock did not record outbound_http={expected}")


def assert_lock_outbound_auth(target: Path, expected: str) -> None:
    lock = json.loads((target / "template.lock").read_text(encoding="utf-8"))
    if lock["profiles"].get("outbound_auth") != expected:
        raise AssertionError(f"template.lock did not record outbound_auth={expected}")


def assert_lock_http_idempotency(target: Path, expected: str) -> None:
    lock = json.loads((target / "template.lock").read_text(encoding="utf-8"))
    if lock["profiles"].get("http_idempotency") != expected:
        raise AssertionError(f"template.lock did not record http_idempotency={expected}")


def assert_lock_jobs(target: Path, expected: str) -> None:
    lock = json.loads((target / "template.lock").read_text(encoding="utf-8"))
    if lock["profiles"].get("jobs") != expected:
        raise AssertionError(f"template.lock did not record jobs={expected}")


def assert_lock_outbox(target: Path, expected: str) -> None:
    lock = json.loads((target / "template.lock").read_text(encoding="utf-8"))
    if lock["profiles"].get("outbox") != expected:
        raise AssertionError(f"template.lock did not record outbox={expected}")


def assert_lock_webhooks(target: Path, webhooks: str, inbound_webhooks: str) -> None:
    lock = json.loads((target / "template.lock").read_text(encoding="utf-8"))
    if lock["profiles"].get("webhooks") != webhooks:
        raise AssertionError(f"template.lock did not record webhooks={webhooks}")
    if lock["profiles"].get("inbound_webhooks") != inbound_webhooks:
        raise AssertionError(f"template.lock did not record inbound_webhooks={inbound_webhooks}")


def assert_outbound_lock_refusals(source: Path, target: Path) -> None:
    lock_path = target / "template.lock"
    original = lock_path.read_bytes()
    cases = (
        ("missing-authn", lambda profiles: profiles.pop("authn")),
        ("missing-database", lambda profiles: profiles.pop("database")),
        ("unknown-profile-field", lambda profiles: profiles.update(unexpected="value")),
        ("unknown-outbound-value", lambda profiles: profiles.update(outbound_http="unbounded")),
    )
    for label, mutate in cases:
        lock = json.loads(original)
        profiles = lock["profiles"]
        mutate(profiles)
        lock_path.write_text(json.dumps(lock, indent=2) + "\n", encoding="utf-8")
        before = state(target)
        result = init(source, target, "--database", "none", "--authn", "none", "--agent-harness", "claude")
        if result.returncode == 0 or state(target) != before:
            raise AssertionError(f"{label} outbound lock shape was not a preserving refusal")
        lock_path.write_bytes(original)


def assert_outbound_auth_lock_refusals(source: Path, target: Path) -> None:
    lock_path = target / "template.lock"
    original = lock_path.read_bytes()
    cases = (
        ("unknown-outbound-auth-value", lambda profiles: profiles.update(outbound_auth="oauth1")),
        (
            "oauth-without-bounded-http",
            lambda profiles: profiles.update(outbound_auth="oauth2-client-credentials", outbound_http="none"),
        ),
    )
    for label, mutate in cases:
        lock = json.loads(original)
        profiles = lock["profiles"]
        mutate(profiles)
        lock_path.write_text(json.dumps(lock, indent=2) + "\n", encoding="utf-8")
        before = state(target)
        result = init(source, target, "--database", "none", "--authn", "none", "--agent-harness", "claude")
        if result.returncode == 0 or state(target) != before:
            raise AssertionError(f"{label} outbound auth lock shape was not a preserving refusal")
        lock_path.write_bytes(original)


def assert_http_idempotency_lock_refusals(source: Path, target: Path) -> None:
    lock_path = target / "template.lock"
    original = lock_path.read_bytes()
    cases = (
        ("http-idempotency-without-database", lambda profiles: profiles.update(http_idempotency="postgres")),
        (
            "http-idempotency-without-authn",
            lambda profiles: profiles.update(http_idempotency="postgres", database="postgres"),
        ),
        ("missing-outbound-http-with-http-idempotency", lambda profiles: profiles.pop("outbound_http")),
        ("unknown-http-idempotency-value", lambda profiles: profiles.update(http_idempotency="mysql")),
    )
    for label, mutate in cases:
        lock = json.loads(original)
        profiles = lock["profiles"]
        mutate(profiles)
        lock_path.write_text(json.dumps(lock, indent=2) + "\n", encoding="utf-8")
        before = state(target)
        result = init(source, target, "--database", "none", "--authn", "none", "--agent-harness", "claude")
        if result.returncode == 0 or state(target) != before:
            raise AssertionError(f"{label} http_idempotency lock shape was not a preserving refusal")
        lock_path.write_bytes(original)


def assert_jobs_lock_refusals(source: Path, target: Path) -> None:
    lock_path = target / "template.lock"
    original = lock_path.read_bytes()
    cases = (
        ("jobs-without-database", lambda profiles: profiles.update(jobs="postgres")),
        ("unknown-jobs-value", lambda profiles: profiles.update(jobs="redis")),
        ("null-jobs-value", lambda profiles: profiles.update(jobs=None)),
        ("missing-http-idempotency-with-jobs", lambda profiles: profiles.pop("http_idempotency")),
    )
    for label, mutate in cases:
        lock = json.loads(original)
        profiles = lock["profiles"]
        mutate(profiles)
        lock_path.write_text(json.dumps(lock, indent=2) + "\n", encoding="utf-8")
        before = state(target)
        result = init(source, target, "--database", "none", "--authn", "none", "--agent-harness", "claude")
        if result.returncode == 0 or state(target) != before:
            raise AssertionError(f"{label} jobs lock shape was not a preserving refusal")
        lock_path.write_bytes(original)


def assert_outbox_lock_refusals(source: Path, target: Path) -> None:
    lock_path = target / "template.lock"
    original = lock_path.read_bytes()
    cases = (
        ("outbox-without-database", lambda profiles: profiles.update(outbox="postgres")),
        ("outbox-without-jobs", lambda profiles: profiles.update(outbox="postgres", database="postgres")),
        (
            "outbox-without-messaging",
            lambda profiles: profiles.update(outbox="postgres", database="postgres", jobs="postgres"),
        ),
        ("unknown-outbox-value", lambda profiles: profiles.update(outbox="sqlite")),
    )
    for label, mutate in cases:
        lock = json.loads(original)
        profiles = lock["profiles"]
        mutate(profiles)
        lock_path.write_text(json.dumps(lock, indent=2) + "\n", encoding="utf-8")
        before = state(target)
        result = init(source, target, "--database", "none", "--authn", "none", "--agent-harness", "claude")
        if result.returncode == 0 or state(target) != before:
            raise AssertionError(f"{label} outbox lock shape was not a preserving refusal")
        lock_path.write_bytes(original)


def assert_webhook_lock_refusals(source: Path, target: Path) -> None:
    lock_path = target / "template.lock"
    original = lock_path.read_bytes()
    cases = (
        ("unknown-webhooks-value", lambda profiles: profiles.update(webhooks="streaming")),
        ("unknown-inbound-webhooks-value", lambda profiles: profiles.update(inbound_webhooks="signed")),
        ("webhooks-without-outbound-http", lambda profiles: profiles.update(webhooks="durable", database="postgres", jobs="postgres")),
        ("inbound-webhooks-without-jobs", lambda profiles: profiles.update(inbound_webhooks="standard-webhooks", database="postgres")),
        ("missing-webhooks", lambda profiles: profiles.pop("webhooks")),
    )
    for label, mutate in cases:
        lock = json.loads(original)
        profiles = lock["profiles"]
        mutate(profiles)
        lock_path.write_text(json.dumps(lock, indent=2) + "\n", encoding="utf-8")
        before = state(target)
        result = init(source, target, "--database", "none", "--authn", "none", "--agent-harness", "claude")
        if result.returncode == 0 or state(target) != before:
            raise AssertionError(f"{label} webhook lock shape was not a preserving refusal")
        lock_path.write_bytes(original)


def check(source: Path) -> None:
    with tempfile.TemporaryDirectory(prefix="template-init-safety-") as temp:
        work = Path(temp)
        assert_marker_syntax(source, work)
        assert_preflight_extraction(source, work)
        # Invalid identity inputs refuse before a plan is written. Cc and the
        # explicit Unicode separators are refused; permitted emoji/ZWJ text is
        # exercised by the successful initialization below.
        must_refuse(source, work, "unknown-db", "--database", "sqlite")
        must_refuse(source, work, "duplicate-db", "--database", "none", "--database", "postgres")
        must_refuse(source, work, "unknown-authn", "--authn", "mtls")
        must_refuse(source, work, "duplicate-authn", "--authn", "none", "--authn", "oidc-jwt")
        must_refuse(source, work, "unknown-outbound-http", "--outbound-http", "unbounded")
        must_refuse(source, work, "duplicate-outbound-http", "--outbound-http", "none", "--outbound-http", "bounded")
        must_refuse(source, work, "unknown-outbound-auth", "--outbound-auth", "oauth1")
        must_refuse(
            source, work, "duplicate-outbound-auth", "--outbound-auth", "none", "--outbound-auth", "oauth2-client-credentials"
        )
        must_refuse(source, work, "unknown-http-idempotency", "--http-idempotency", "mysql")
        must_refuse(
            source, work, "duplicate-http-idempotency", "--http-idempotency", "none", "--http-idempotency", "postgres"
        )
        must_refuse(source, work, "http-idempotency-requires-database", "--http-idempotency", "postgres")
        must_refuse(
            source, work, "http-idempotency-requires-authn", "--database", "postgres", "--http-idempotency", "postgres"
        )
        must_refuse(source, work, "unknown-jobs", "--jobs", "redis", expected="JOBS is unsupported")
        must_refuse(
            source, work, "duplicate-jobs", "--jobs", "none", "--jobs", "postgres",
            expected="--jobs may be supplied once",
        )
        must_refuse(
            source, work, "jobs-flag-and-environment", "--jobs", "none",
            environment={"JOBS": "none"}, expected="JOBS may be supplied once, by flag or environment",
        )
        must_refuse(
            source, work, "jobs-requires-database", "--database", "none", "--jobs", "postgres",
            expected="JOBS=postgres requires DATABASE=postgres",
        )
        must_refuse(source, work, "unknown-messaging", "--messaging", "amqp", expected="MESSAGING is unsupported")
        must_refuse(
            source, work, "duplicate-messaging", "--messaging", "none", "--messaging", "nats-jetstream",
            expected="--messaging may be supplied once",
        )
        must_refuse(
            source, work, "messaging-flag-and-environment", "--messaging", "none",
            environment={"MESSAGING": "none"}, expected="MESSAGING may be supplied once, by flag or environment",
        )
        must_refuse(source, work, "unknown-outbox", "--outbox", "sqlite", expected="OUTBOX is unsupported")
        must_refuse(
            source, work, "duplicate-outbox", "--outbox", "none", "--outbox", "postgres",
            expected="--outbox may be supplied once",
        )
        must_refuse(
            source, work, "outbox-flag-and-environment", "--outbox", "none",
            environment={"OUTBOX": "none"}, expected="OUTBOX may be supplied once, by flag or environment",
        )
        must_refuse(
            source, work, "outbox-requires-database", "--outbox", "postgres",
            expected="OUTBOX=postgres requires DATABASE=postgres",
        )
        must_refuse(
            source, work, "outbox-requires-jobs", "--database", "postgres", "--outbox", "postgres",
            expected="OUTBOX=postgres requires JOBS=postgres",
        )
        must_refuse(
            source, work, "outbox-requires-messaging", "--database", "postgres", "--jobs", "postgres",
            "--outbox", "postgres", expected="OUTBOX=postgres requires MESSAGING=nats-jetstream",
        )
        must_refuse(source, work, "unknown-webhooks", "--webhooks", "ephemeral", expected="WEBHOOKS is unsupported")
        must_refuse(
            source, work, "duplicate-webhooks", "--webhooks", "none", "--webhooks", "durable",
            expected="--webhooks may be supplied once",
        )
        must_refuse(
            source, work, "webhooks-flag-and-environment", "--webhooks", "none",
            environment={"WEBHOOKS": "none"}, expected="WEBHOOKS may be supplied once, by flag or environment",
        )
        must_refuse(
            source, work, "webhooks-requires-database", "--webhooks", "durable",
            expected="WEBHOOKS=durable requires DATABASE=postgres",
        )
        must_refuse(
            source, work, "webhooks-requires-jobs", "--database", "postgres", "--outbound-http", "bounded",
            "--webhooks", "durable", expected="WEBHOOKS=durable requires JOBS=postgres",
        )
        must_refuse(
            source, work, "webhooks-requires-outbound-http", "--database", "postgres", "--jobs", "postgres",
            "--webhooks", "durable", expected="WEBHOOKS=durable requires OUTBOUND_HTTP=bounded",
        )
        must_refuse(
            source, work, "unknown-inbound-webhooks", "--inbound-webhooks", "legacy",
            expected="INBOUND_WEBHOOKS is unsupported",
        )
        must_refuse(
            source, work, "duplicate-inbound-webhooks", "--inbound-webhooks", "none", "--inbound-webhooks", "standard-webhooks",
            expected="--inbound-webhooks may be supplied once",
        )
        must_refuse(
            source, work, "inbound-webhooks-requires-database", "--inbound-webhooks", "standard-webhooks",
            expected="INBOUND_WEBHOOKS=standard-webhooks requires DATABASE=postgres",
        )
        must_refuse(
            source, work, "inbound-webhooks-requires-jobs", "--database", "postgres",
            "--inbound-webhooks", "standard-webhooks", expected="INBOUND_WEBHOOKS=standard-webhooks requires JOBS=postgres",
        )
        must_refuse(source, work, "codeowner-depth", "--codeowner", "@a/b/c")
        must_refuse(source, work, "reserved-abstract", "--service-name", "abstract")
        must_refuse(source, work, "reserved-gen", "--service-name", "gen")
        must_refuse(source, work, "c1-description", "--description", "bad\u0085description")
        must_refuse(source, work, "line-separator", "--description", "bad\u2028description")
        must_refuse(source, work, "paragraph-separator", "--description", "bad\u2029description")
        must_refuse_raw_description(source, work)

        target = work / "success"
        clone(source, target)
        local_adapter_note = target / ".codex/service-note.txt"
        local_adapter_note.write_text("service-owned sibling\n", encoding="utf-8")
        result = init(source, target, "--database", "none", "--agent-harness", "claude")
        if result.returncode:
            raise AssertionError(f"successful initialization failed: {result.stderr}")
        assert_profile_packs(
            source, target, database="none", authn="none", outbound_http="none", http_idempotency="none", jobs="none"
        )
        assert_lock_authn(target, "none")
        assert_lock_outbound_http(target, "none")
        assert_lock_outbound_auth(target, "none")
        assert_lock_http_idempotency(target, "none")
        assert_lock_jobs(target, "none")
        assert_lock_outbox(target, "none")
        assert_lock_webhooks(target, "none", "none")
        for removed in [
            "make/source.mk", "scripts/ci/template-init-check.sh", "scripts/tests/template-init-safety.py",
            "crates/infra-postgres", "crates/migrate", "migrations", "env/docker-compose.yml",
            "Grok.md", "QWEN.md", ".agents/codex-project.toml",
            ".codex/config.toml", ".codex/agents", ".cursor/rules/agent-harness.mdc", ".cursor/agents",
            ".grok/rules/harness.md", ".grok/agents", ".grok/roles", "opencode.json",
            ".opencode/rules/harness.md", ".opencode/agents", ".opencode/commands/orchestrator.md",
            ".opencode/plugins/task-subagents.js", ".qwen/agents", ".qwen/skills", ".qwen/settings.json",
        ]:
            if (target / removed).exists() or (target / removed).is_symlink():
                raise AssertionError(f"none/claude output retained {removed}")
        if local_adapter_note.read_text(encoding="utf-8") != "service-owned sibling\n":
            raise AssertionError("initializer changed unrelated product-root content")
        for empty_parent in (".cursor", ".grok", ".qwen", ".opencode/commands", ".opencode/plugins", ".opencode/rules"):
            if (target / empty_parent).exists():
                raise AssertionError(f"initializer retained an empty removed-pack parent: {empty_parent}")
        for parent in (".codex", ".cursor", ".grok", ".opencode", ".qwen"):
            path = target / parent
            if path.exists():
                print(f"preserved product parent {parent}: directory={path.is_dir()} "
                      f"symlink={path.is_symlink()} children={sorted(p.name for p in path.iterdir())}")
        if not (target / "template.lock").is_file() or not (target / "CLAUDE.md").is_file():
            raise AssertionError("initialized output omitted retained local state or selected carrier")
        lifecycle_source = (target / "crates/service/tests/lifecycle.rs").read_text(encoding="utf-8")
        if 'env!("CARGO_BIN_EXE_safety-api")' not in lifecycle_source:
            raise AssertionError("lifecycle test lost the exact hyphenated Cargo binary environment key")
        if not any((target / ".claude/skills").iterdir()):
            raise AssertionError("selected Claude skill links were not retained")
        service_skill = target / ".agents/skills/service-local"
        service_skill.mkdir()
        (service_skill / "SKILL.md").write_text(
            "# Service-local method\nRepository: https://github.com/example/safety-api\n",
            encoding="utf-8",
        )
        (service_skill / ".service-owned").write_text("", encoding="utf-8")
        (target / ".claude/skills/service-local").symlink_to("../../.agents/skills/service-local")
        service_spec = target / "specs/service-feature/spec.md"
        service_spec.parent.mkdir(parents=True)
        service_spec.write_text("Service-owned decision after initialization.\n", encoding="utf-8")
        with (target / "README.md").open("a", encoding="utf-8") as readme:
            readme.write("\nService-owned onboarding update.\n")
        after = state(target)
        repeat = init(source, target, "--database", "none", "--agent-harness", "claude")
        if repeat.returncode or state(target) != after:
            raise AssertionError("matching complete-lock initialization was not a byte-preserving no-op")
        # A complete DNS-generation inventory is accepted only for no-op
        # replay. Current projection must never select or emit its DNS pack.
        inventory_path = target / "scripts/lib/template_profiles.json"
        lock_path = target / "template.lock"
        current_inventory = inventory_path.read_bytes()
        current_lock = lock_path.read_bytes()
        install_derived_outbound_only_none(source, target)
        historical_before = state(target)
        historical_repeat = init(source, target, "--database", "none", "--agent-harness", "claude")
        if historical_repeat.returncode or state(target) != historical_before:
            raise AssertionError("previous DNS inventory replay was not byte-preserving")
        historical_inventory = json.loads(inventory_path.read_text(encoding="utf-8"))
        historical_inventory["unknown-pack"] = {"remove_when_unselected": [], "markers": []}
        inventory_path.write_text(json.dumps(historical_inventory, indent=2) + "\n", encoding="utf-8")
        unknown_before = state(target)
        unknown_repeat = init(source, target, "--database", "none", "--agent-harness", "claude")
        if unknown_repeat.returncode == 0 or state(target) != unknown_before:
            raise AssertionError("unknown historical inventory was accepted or mutated")
        inventory_path.write_bytes(current_inventory)
        lock_path.write_bytes(current_lock)
        assert_outbound_lock_refusals(source, target)
        assert_outbound_auth_lock_refusals(source, target)
        assert_http_idempotency_lock_refusals(source, target)
        assert_jobs_lock_refusals(source, target)
        assert_outbox_lock_refusals(source, target)
        assert_webhook_lock_refusals(source, target)
        install_derived_auth_only_none(source, target)
        auth_only_before = state(target)
        auth_only_repeat = init(source, target, "--database", "none", "--authn", "none", "--agent-harness", "claude")
        if auth_only_repeat.returncode or state(target) != auth_only_before:
            raise AssertionError("derived auth-only none lock replay was not byte-preserving")
        auth_only_mismatch = init(
            source, target, "--database", "none", "--authn", "none", "--outbound-http", "bounded", "--agent-harness", "claude"
        )
        if auth_only_mismatch.returncode == 0 or state(target) != auth_only_before:
            raise AssertionError("auth-only none lock accepted an outbound profile migration")
        install_derived_outbound_only_none(source, target)
        outbound_only_before = state(target)
        outbound_only_repeat = init(source, target, "--database", "none", "--authn", "none", "--agent-harness", "claude")
        if outbound_only_repeat.returncode or state(target) != outbound_only_before:
            raise AssertionError("derived outbound-only none lock replay was not byte-preserving")
        outbound_only_mismatch = init(
            source, target, "--database", "postgres", "--authn", "oidc-jwt", "--http-idempotency", "postgres",
            "--agent-harness", "claude",
        )
        if outbound_only_mismatch.returncode == 0 or state(target) != outbound_only_before:
            raise AssertionError("outbound-only none lock accepted an http idempotency profile migration")
        (target / "scripts/lib/template_profiles.json").write_bytes(
            (source / "scripts/lib/template_profiles.json").read_bytes()
        )
        lock = json.loads((target / "template.lock").read_text(encoding="utf-8"))
        # A five-field lock against the current inventory: a missing selection
        # means `none`, and the matching replay keeps its bytes.
        profiles = lock["profiles"]
        lock["profiles"] = {
            "database": profiles["database"],
            "authn": profiles.get("authn", "none"),
            "outbound_http": profiles.get("outbound_http", "none"),
            "http_idempotency": profiles.get("http_idempotency", "none"),
            "agent_harness": profiles["agent_harness"],
        }
        (target / "template.lock").write_text(json.dumps(lock, indent=2) + "\n", encoding="utf-8")
        five_field_before = state(target)
        five_field_repeat = init(source, target, "--database", "none", "--authn", "none", "--agent-harness", "claude")
        if five_field_repeat.returncode or state(target) != five_field_before:
            raise AssertionError("five-field none lock replay was not byte-preserving")
        install_derived_http_idempotency_only_none(source, target)
        http_idempotency_only_before = state(target)
        http_idempotency_only_repeat = init(
            source, target, "--database", "none", "--authn", "none", "--agent-harness", "claude"
        )
        if http_idempotency_only_repeat.returncode or state(target) != http_idempotency_only_before:
            raise AssertionError("derived http-idempotency-only none lock replay was not byte-preserving")
        jobs_migration = init(
            source, target, "--database", "postgres", "--jobs", "postgres", "--agent-harness", "claude"
        )
        if jobs_migration.returncode == 0 or state(target) != http_idempotency_only_before:
            raise AssertionError("http-idempotency-only none lock accepted a jobs profile migration")
        if "template initialization choices differ from the complete template.lock" not in jobs_migration.stderr:
            raise AssertionError(
                "jobs profile migration refusal did not name "
                "'template initialization choices differ from the complete template.lock': "
                f"{jobs_migration.stderr}"
            )
        install_historical_none(source, target)
        historical_before = state(target)
        historical_repeat = init(source, target, "--database", "none", "--authn", "none", "--agent-harness", "claude")
        if historical_repeat.returncode or state(target) != historical_before:
            raise AssertionError("actual historical none lock replay was not byte-preserving")
        historical_mismatch = init(source, target, "--database", "none", "--authn", "oidc-jwt", "--agent-harness", "claude")
        if historical_mismatch.returncode == 0 or state(target) != historical_before:
            raise AssertionError("historical none lock accepted an authn profile migration")
        (target / "template.lock").write_text("{\"state\": []}\n", encoding="utf-8")
        malformed_before = state(target)
        malformed = init(source, target, "--database", "none", "--agent-harness", "claude")
        if malformed.returncode == 0 or state(target) != malformed_before:
            raise AssertionError("malformed lock was not a preserving refusal")
        postgres_target = work / "postgres-replay"
        clone(source, postgres_target)
        postgres = init(source, postgres_target, "--database", "postgres", "--agent-harness", "core")
        if postgres.returncode:
            raise AssertionError(f"postgres initialization failed: {postgres.stderr}")
        assert_profile_packs(
            source, postgres_target, database="postgres", authn="none", outbound_http="none", http_idempotency="none",
            jobs="none",
        )
        (postgres_target / "specs/service-feature").mkdir(parents=True)
        (postgres_target / "specs/service-feature/decision.md").write_text(
            "Service-owned PostgreSQL decision.\n", encoding="utf-8"
        )
        postgres_before = state(postgres_target)
        postgres_repeat = init(source, postgres_target, "--database", "postgres", "--agent-harness", "core")
        if postgres_repeat.returncode or state(postgres_target) != postgres_before:
            raise AssertionError("complete postgres lock replay changed service-owned content")
        postgres_jobs_mismatch = init(
            source, postgres_target, "--database", "postgres", "--jobs", "postgres", "--agent-harness", "core"
        )
        if postgres_jobs_mismatch.returncode == 0 or state(postgres_target) != postgres_before:
            raise AssertionError("complete postgres lock accepted a jobs profile migration")
        if "template initialization choices differ from the complete template.lock" not in postgres_jobs_mismatch.stderr:
            raise AssertionError(
                "jobs profile migration refusal did not name "
                "'template initialization choices differ from the complete template.lock': "
                f"{postgres_jobs_mismatch.stderr}"
            )
        missing_pack = postgres_target / "make/profile-postgres.mk"
        missing_pack.unlink()
        missing_before = state(postgres_target)
        missing_replay = init(source, postgres_target, "--database", "postgres", "--agent-harness", "core")
        if missing_replay.returncode == 0 or state(postgres_target) != missing_before:
            raise AssertionError("complete postgres lock accepted a missing retained pack")
        for authn in ("oidc-jwt", "oidc-introspection"):
            authn_target = work / f"{authn}-replay"
            clone(source, authn_target)
            initialized = init(source, authn_target, "--database", "none", "--authn", authn, "--agent-harness", "core")
            if initialized.returncode:
                raise AssertionError(f"{authn} initialization failed: {initialized.stderr}")
            assert_profile_packs(
                source, authn_target, database="none", authn=authn, outbound_http="none", http_idempotency="none",
                jobs="none",
            )
            assert_lock_authn(authn_target, authn)
            authn_before = state(authn_target)
            replay = init(source, authn_target, "--database", "none", "--authn", authn, "--agent-harness", "core")
            if replay.returncode or state(authn_target) != authn_before:
                raise AssertionError(f"complete {authn} lock replay changed target bytes")
        outbound_target = work / "outbound-replay"
        clone(source, outbound_target)
        outbound = init(source, outbound_target, "--database", "none", "--outbound-http", "bounded", "--agent-harness", "core")
        if outbound.returncode:
            raise AssertionError(f"bounded outbound initialization failed: {outbound.stderr}")
        assert_profile_packs(
            source, outbound_target, database="none", authn="none", outbound_http="bounded", http_idempotency="none",
            jobs="none",
        )
        assert_lock_outbound_http(outbound_target, "bounded")
        outbound_before = state(outbound_target)
        outbound_replay = init(source, outbound_target, "--database", "none", "--outbound-http", "bounded", "--agent-harness", "core")
        if outbound_replay.returncode or state(outbound_target) != outbound_before:
            raise AssertionError("complete bounded outbound lock replay changed target bytes")
        oauth_target = work / "oauth-replay"
        clone(source, oauth_target)
        oauth = init(source, oauth_target, "--database", "none", "--outbound-auth", "oauth2-client-credentials", "--agent-harness", "core")
        if oauth.returncode:
            raise AssertionError(f"OAuth initialization failed: {oauth.stderr}")
        assert_profile_packs(
            source, oauth_target, database="none", authn="none", outbound_http="bounded", outbound_auth="oauth2-client-credentials",
            http_idempotency="none", jobs="none",
        )
        assert_lock_outbound_http(oauth_target, "bounded")
        assert_lock_outbound_auth(oauth_target, "oauth2-client-credentials")
        oauth_before = state(oauth_target)
        oauth_replay = init(
            source, oauth_target, "--database", "none", "--outbound-auth", "oauth2-client-credentials", "--agent-harness", "core"
        )
        if oauth_replay.returncode or state(oauth_target) != oauth_before:
            raise AssertionError("complete OAuth lock replay changed target bytes")
        http_idempotency_target = work / "http-idempotency-replay"
        clone(source, http_idempotency_target)
        http_idempotency_result = init(
            source, http_idempotency_target, "--database", "postgres", "--authn", "oidc-introspection",
            "--http-idempotency", "postgres", "--agent-harness", "core",
        )
        if http_idempotency_result.returncode:
            raise AssertionError(f"postgres http idempotency initialization failed: {http_idempotency_result.stderr}")
        assert_profile_packs(
            source, http_idempotency_target, database="postgres", authn="oidc-introspection",
            outbound_http="none", http_idempotency="postgres", jobs="none",
        )
        assert_lock_http_idempotency(http_idempotency_target, "postgres")
        http_idempotency_before = state(http_idempotency_target)
        http_idempotency_replay = init(
            source, http_idempotency_target, "--database", "postgres", "--authn", "oidc-introspection",
            "--http-idempotency", "postgres", "--agent-harness", "core",
        )
        if http_idempotency_replay.returncode or state(http_idempotency_target) != http_idempotency_before:
            raise AssertionError("complete postgres http idempotency lock replay changed target bytes")
        http_idempotency_revert_mismatch = init(
            source, http_idempotency_target, "--database", "postgres", "--authn", "oidc-introspection",
            "--http-idempotency", "none", "--agent-harness", "core",
        )
        if http_idempotency_revert_mismatch.returncode == 0 or state(http_idempotency_target) != http_idempotency_before:
            raise AssertionError("postgres http idempotency lock accepted a profile migration back to none")
        jobs_target = work / "jobs-replay"
        clone(source, jobs_target)
        jobs_result = init(
            source, jobs_target, "--database", "postgres", "--jobs", "postgres", "--agent-harness", "core"
        )
        if jobs_result.returncode:
            raise AssertionError(f"postgres jobs initialization failed: {jobs_result.stderr}")
        assert_profile_packs(
            source, jobs_target, database="postgres", authn="none", outbound_http="none",
            http_idempotency="none", jobs="postgres",
        )
        assert_lock_jobs(jobs_target, "postgres")
        jobs_before = state(jobs_target)
        jobs_replay = init(
            source, jobs_target, "--database", "postgres", "--jobs", "postgres", "--agent-harness", "core"
        )
        if jobs_replay.returncode or state(jobs_target) != jobs_before:
            raise AssertionError("complete postgres jobs lock replay changed target bytes")
        jobs_revert_mismatch = init(
            source, jobs_target, "--database", "postgres", "--jobs", "none", "--agent-harness", "core"
        )
        if jobs_revert_mismatch.returncode == 0 or state(jobs_target) != jobs_before:
            raise AssertionError("postgres jobs lock accepted a profile migration back to none")
        if "template initialization choices differ from the complete template.lock" not in jobs_revert_mismatch.stderr:
            raise AssertionError(
                "jobs profile migration refusal did not name "
                "'template initialization choices differ from the complete template.lock': "
                f"{jobs_revert_mismatch.stderr}"
            )
        messaging_target = work / "messaging-replay"
        clone(source, messaging_target)
        messaging_result = init(
            source, messaging_target, "--database", "none", "--jobs", "none",
            "--messaging", "nats-jetstream", "--agent-harness", "core",
        )
        if messaging_result.returncode:
            raise AssertionError(f"messaging-only initialization failed: {messaging_result.stderr}")
        assert_profile_pack(source, messaging_target, "messaging", True)
        assert_profile_pack(source, messaging_target, "worker", True)
        assert_profile_pack(source, messaging_target, "integration", True)
        lock = json.loads((messaging_target / "template.lock").read_text(encoding="utf-8"))
        if lock["profiles"].get("messaging") != "nats-jetstream":
            raise AssertionError("template.lock did not record messaging=nats-jetstream")
        messaging_before = state(messaging_target)
        messaging_replay = init(
            source, messaging_target, "--database", "none", "--jobs", "none",
            "--messaging", "nats-jetstream", "--agent-harness", "core",
        )
        if messaging_replay.returncode or state(messaging_target) != messaging_before:
            raise AssertionError("complete messaging lock replay changed target bytes")
        outbox_target = work / "outbox-replay"
        clone(source, outbox_target)
        outbox_result = init(
            source, outbox_target, "--database", "postgres", "--jobs", "postgres",
            "--messaging", "nats-jetstream", "--outbox", "postgres", "--agent-harness", "core",
        )
        if outbox_result.returncode:
            raise AssertionError(f"outbox initialization failed: {outbox_result.stderr}")
        assert_profile_packs(
            source, outbox_target, database="postgres", authn="none", outbound_http="none",
            http_idempotency="none", jobs="postgres", outbox="postgres",
        )
        assert_lock_outbox(outbox_target, "postgres")
        outbox_before = state(outbox_target)
        outbox_replay = init(
            source, outbox_target, "--database", "postgres", "--jobs", "postgres",
            "--messaging", "nats-jetstream", "--outbox", "postgres", "--agent-harness", "core",
        )
        if outbox_replay.returncode or state(outbox_target) != outbox_before:
            raise AssertionError("complete outbox lock replay changed target bytes")
        outbox_revert_mismatch = init(
            source, outbox_target, "--database", "postgres", "--jobs", "postgres",
            "--messaging", "nats-jetstream", "--outbox", "none", "--agent-harness", "core",
        )
        if outbox_revert_mismatch.returncode == 0 or state(outbox_target) != outbox_before:
            raise AssertionError("complete outbox lock accepted a profile migration back to none")
        webhooks_target = work / "webhooks-replay"
        clone(source, webhooks_target)
        webhooks_result = init(
            source, webhooks_target, "--database", "postgres", "--jobs", "postgres", "--outbound-http", "bounded",
            "--webhooks", "durable", "--inbound-webhooks", "standard-webhooks", "--agent-harness", "core",
        )
        if webhooks_result.returncode:
            raise AssertionError(f"webhooks initialization failed: {webhooks_result.stderr}")
        assert_profile_packs(
            source, webhooks_target, database="postgres", authn="none", outbound_http="bounded",
            http_idempotency="none", jobs="postgres", webhooks="durable", inbound_webhooks="standard-webhooks",
        )
        assert_lock_webhooks(webhooks_target, "durable", "standard-webhooks")
        webhooks_before = state(webhooks_target)
        webhooks_replay = init(
            source, webhooks_target, "--database", "postgres", "--jobs", "postgres", "--outbound-http", "bounded",
            "--webhooks", "durable", "--inbound-webhooks", "standard-webhooks", "--agent-harness", "core",
        )
        if webhooks_replay.returncode or state(webhooks_target) != webhooks_before:
            raise AssertionError("complete webhook lock replay changed target bytes")
        for permitted in ("union", "raw"):
            allowed = work / f"permitted-{permitted}"
            clone(source, allowed)
            result = init(source, allowed, "--service-name", permitted, "--database", "none")
            if result.returncode:
                raise AssertionError(f"permitted Rust-adjacent name {permitted} was refused: {result.stderr}")
    print("template initializer safety: pass")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--source", required=True, type=Path)
    args = parser.parse_args()
    check(args.source.resolve())
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
