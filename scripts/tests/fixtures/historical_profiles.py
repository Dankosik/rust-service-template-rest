from __future__ import annotations

import hashlib
import json
from pathlib import Path


_LEGACY_B206_PROFILE_SHA256 = "75e68f9c7defd4031f5d7a0bc2866f337a6c79f69a69f56c79a268d48d8d6530"
_LEGACY_B206_REVISION = "b2060279370713f05be81b1ad44a31e3e960bccc"
_HISTORICAL_OUTBOUND_REVISION = "43b7588edbdb1ebfbc478fb28e0e3d2e77417960"
_LEGACY_B206_PROFILE_PATH = "scripts/tests/fixtures/template-profiles-b206.json"
_AUTH_ONLY_PROFILE_KEYS = (
    "schema_version", "source_only", "postgres", "authn", "oidc-jwt", "oidc-introspection", "identity", "cargo_lock",
)
_OUTBOUND_ONLY_PROFILE_KEYS = (
    "schema_version", "source_only", "postgres", "authn", "oidc-jwt", "oidc-introspection",
    "identity", "cargo_lock", "outbound-http", "egress-dns", "request-budget",
)
# Historical DNS pack is replay input only; never part of current selection.
HISTORICAL_EGRESS_PACK = {
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


def legacy_profile_inventory_bytes(source: Path) -> bytes:
    # Frozen bytes from _LEGACY_B206_REVISION, independent of current profiles.
    rendered = (source / _LEGACY_B206_PROFILE_PATH).read_bytes()
    if hashlib.sha256(rendered).hexdigest() != _LEGACY_B206_PROFILE_SHA256:
        raise AssertionError("legacy b206 profile inventory bytes changed")
    return rendered


def install_historical_none(source: Path, target: Path) -> None:
    (target / "scripts/lib/template_profiles.json").write_bytes(legacy_profile_inventory_bytes(source))
    lock = json.loads((target / "template.lock").read_text(encoding="utf-8"))
    lock["profiles"].pop("authn")
    lock["profiles"].pop("outbound_http", None)
    lock["profiles"].pop("outbound_auth", None)
    lock["profiles"].pop("grpc", None)
    lock["profiles"].pop("http_idempotency", None)
    lock["profiles"].pop("jobs", None)
    lock["profiles"].pop("messaging", None)
    lock["profiles"].pop("outbox", None)
    lock["profiles"].pop("webhooks", None)
    lock["profiles"].pop("inbound_webhooks", None)
    lock["profiles"].pop("cache", None)
    lock["profiles"].pop("object_storage", None)
    lock["profiles"].pop("service_auth", None)
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
    lock["profiles"].pop("grpc", None)
    lock["profiles"].pop("http_idempotency", None)
    lock["profiles"].pop("jobs", None)
    lock["profiles"].pop("messaging", None)
    lock["profiles"].pop("outbox", None)
    lock["profiles"].pop("webhooks", None)
    lock["profiles"].pop("inbound_webhooks", None)
    lock["profiles"].pop("cache", None)
    lock["profiles"].pop("object_storage", None)
    lock["profiles"].pop("service_auth", None)
    (target / "template.lock").write_text(json.dumps(lock, indent=2) + "\n", encoding="utf-8")


def install_derived_outbound_only_none(source: Path, target: Path) -> None:
    profile = json.loads((source / "scripts/lib/template_profiles.json").read_text(encoding="utf-8"))
    profile["egress-dns"] = HISTORICAL_EGRESS_PACK
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
