#!/usr/bin/env python3
"""Plan the CI initializer matrix: which runtime graphs a diff can affect, and
the part that runs each graph.

    initializer-matrix.py --all            every graph (push, tag, schedule)
    initializer-matrix.py < changed-paths  graphs the diff can affect
    initializer-matrix.py --self-test

Prints `initializer_matrix=<json>`, `artifact_graphs=<ids>` and
`artifact_matrix=<json>` for GITHUB_OUTPUT. The initializer matrix always holds
the `source` part; runtime parts appear when they have a selected graph. The
artifact matrix runs each selected release representative in its own job,
restoring the initializer cache of the graph's family.

The classifier selects runtime graphs with initializer_runtime and the four
release representatives with initializer_artifacts; --artifacts-only emits the
latter selection for make verify.
Such a path under a narrowable prefix affects a graph only when the initializer
keeps it for the graph's profile tuple: a path the graph removes cannot reach
its build. Any other one (manifests, the lockfile, the initializer, Make,
workflows) selects every graph.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.dont_write_bytecode = True
sys.path.insert(0, str(ROOT / "scripts/lib"))

import template_init  # noqa: E402

FIELDS = (
    "database", "authn", "outbound_http", "outbound_auth", "http_idempotency", "jobs",
    "messaging", "outbox", "webhooks", "inbound_webhooks", "grpc", "cache", "object_storage", "service_auth",
)
ARTIFACT_GRAPHS = (1, 7, 47, 65)
NARROWABLE = ("crates/", "migrations/", "test/", "api/proto/")

# Parts balance the measured warm seconds per graph into legs of about ten
# minutes (docs/ci-cd-production-ready.md), longest first. Every graph belongs
# to one part. A part restores the Cargo cache of its family, the name before
# the last dash: `core` graphs retain no idempotency, jobs, messaging, outbox
# or webhooks, `full` graphs do. Only the family's saving part, which holds its
# heaviest dependency variants, writes that cache, so two caches serve every
# part inside the repository's 10 GB budget.
PARTS: dict[str, tuple[int, ...]] = {
    "full-1": (38, 45, 54, 55, 63, 68),
    "full-2": (20, 42, 49, 56, 61),
    "full-3": (22, 31, 32, 53),
    "full-4": (13, 21, 25, 34, 44),
    "full-5": (14, 19, 26, 36, 46),
    "full-6": (16, 17, 40, 41, 48),
    "full-7": (15, 18, 24, 33, 39),
    "full-8": (27, 30, 35, 37, 65),
    "full-9": (23, 28, 29, 43, 47),
    "core-1": (3, 5, 9, 52, 57, 60, 62, 64, 67),
    "core-2": (4, 7, 8, 10, 51, 58, 59, 66),
    "core-3": (1, 2, 6, 11, 12, 50),
}
SAVING_PARTS = ("full-1", "core-1")


def family(part: str) -> str:
    return part.rsplit("-", 1)[0]


def graphs() -> dict[int, dict[str, str]]:
    listing = subprocess.run(
        ["bash", str(ROOT / "scripts/ci/template-init-check.sh"), "--repo", str(ROOT), "--list-graphs"],
        check=True, capture_output=True, text=True,
    ).stdout
    inventory = {}
    for line in listing.splitlines():
        number, *values = line.split()
        inventory[int(number)] = dict(zip(FIELDS, values, strict=True))
    return inventory


def removed_paths(profiles: template_init.ProfileData, graph: dict[str, str]) -> tuple[str, ...]:
    inputs = template_init.InitInputs(
        service_name="matrix", repository="https://github.com/example/matrix", description="matrix",
        codeowner="@example/platform", agent_harness="core", **graph,
    )
    selected = template_init._selected_marker_profiles(inputs)
    removals = [path for profile, paths in profiles.removals.items() if profile not in selected for path in paths]
    return (*profiles.source_only, *removals)


def kept(path: str, removed: tuple[str, ...]) -> bool:
    return not any(path == entry.rstrip("/") or path.startswith(entry.rstrip("/") + "/") for entry in removed)


def selects_runtime(paths: list[str], surface: str = "initializer_runtime") -> bool:
    """The classifier's initializer_runtime verdict for these paths together."""

    if not paths:
        return False
    surfaces = subprocess.run(
        ["bash", str(ROOT / "scripts/ci/changed-surfaces.sh")],
        input="".join(f"{path}\n" for path in paths), capture_output=True, text=True, cwd=ROOT,
    ).stdout
    return f"{surface}=true" in surfaces.splitlines()


def select(paths: list[str] | None, inventory: dict[int, dict[str, str]],
           surface: str = "initializer_runtime") -> set[int]:
    if paths is None:
        return set(inventory)
    if selects_runtime([path for path in paths if not path.startswith(NARROWABLE)], surface):
        return set(inventory)
    narrowable = [path for path in paths if path.startswith(NARROWABLE) and selects_runtime([path], surface)]
    profiles = template_init._profile_data(ROOT)
    return {
        number for number, graph in inventory.items()
        if any(kept(path, removed_paths(profiles, graph)) for path in narrowable)
    }


def select_artifacts(paths: list[str] | None, inventory: dict[int, dict[str, str]]) -> list[int]:
    """The retained-binary seams, narrowed with the runtime planner's removal policy."""
    representatives = {number: inventory[number] for number in ARTIFACT_GRAPHS}
    return sorted(select(paths, representatives, "initializer_artifacts"))


def matrix(selected: set[int], inventory: dict[int, dict[str, str]]) -> dict[str, list[dict[str, str | bool]]]:
    assigned = [number for numbers in PARTS.values() for number in numbers]
    if sorted(assigned) != sorted(inventory):
        missing = sorted(set(inventory) - set(assigned))
        extra = sorted(set(assigned) - set(inventory))
        duplicate = sorted({number for number in assigned if assigned.count(number) > 1})
        raise SystemExit(f"initializer parts must hold every graph once: missing={missing} unknown={extra} duplicate={duplicate}")
    include: list[dict[str, str | bool]] = [{"part": "source", "cache": "source", "save": True}]
    for part, numbers in PARTS.items():
        chosen = [number for number in numbers if number in selected]
        if chosen:
            # A saving part that ran a narrowed subset would claim the family
            # key with an incomplete target until the lockfile changes.
            save = part in SAVING_PARTS and len(chosen) == len(numbers)
            include.append({"part": part, "graphs": ",".join(map(str, chosen)), "cache": family(part), "save": save})
    return {"include": include}


def artifact_matrix(artifacts: list[int]) -> dict[str, list[dict[str, int | str]]]:
    parts = {number: part for part, numbers in PARTS.items() for number in numbers}
    return {"include": [{"graph": number, "cache": family(parts[number])} for number in artifacts]}


def self_test(inventory: dict[int, dict[str, str]]) -> None:
    everything = set(inventory)
    cases = {
        "crates/service/src/main.rs": everything,
        "crates/infra-http/src/observe.rs": set(),
        "crates/infra-cache/src/lib.rs": {n for n, g in inventory.items() if g["cache"] != "none"},
        "crates/infra-messaging/src/outbox.rs": {n for n, g in inventory.items() if g["outbox"] != "none"},
        "crates/infra-bearerauthn/src/jwt.rs": {n for n, g in inventory.items() if g["authn"] == "oidc-jwt"},
        "crates/infra-serviceauth/src/verify.rs": {n for n, g in inventory.items() if g["service_auth"] != "none"},
        "Cargo.lock": everything,
        "scripts/lib/template_init.py": everything,
    }
    for path, expected in cases.items():
        actual = select([path], inventory)
        if actual != expected:
            raise SystemExit(f"{path}: selected {sorted(actual)}, expected {sorted(expected)}")
    if select(["README.md"], inventory):
        raise SystemExit("README.md selected a runtime graph")
    if select(["README.md", "crates/infra-cache/src/lib.rs"], inventory) != cases["crates/infra-cache/src/lib.rs"]:
        raise SystemExit("projected text widened a narrowed selection")
    full = matrix(everything, inventory)["include"]
    if [row["part"] for row in full] != ["source", *PARTS]:
        raise SystemExit("the full matrix must list every part")
    if sorted(row["part"] for row in full if row["save"]) != sorted(["source", *SAVING_PARTS]):
        raise SystemExit("a full matrix must save the source cache and each family's cache once")
    if {row["cache"] for row in full} != {"source", *map(family, SAVING_PARTS)}:
        raise SystemExit("every part must restore a cache that a saving part writes")
    narrowed = matrix(everything - {PARTS[SAVING_PARTS[0]][0]}, inventory)["include"]
    if any(row["save"] for row in narrowed if row["part"] == SAVING_PARTS[0]):
        raise SystemExit("a saving part that ran a subset of its graphs must not save")
    if matrix(set(), inventory) != {"include": [{"part": "source", "cache": "source", "save": True}]}:
        raise SystemExit("an empty selection must keep only the source part")
    # Independent artifact seams: pruning a provider must not run its release
    # graph; shared image/initializer controls must observe every binary set.
    artifact_cases = {
        "README.md": [],
        ".agents/roles/worker-agent.toml": [],
        "crates/infra-cache/src/lib.rs": [65],
        "crates/infra-postgres/src/lib.rs": [7, 65],
        "crates/infra-messaging/src/lib.rs": [47, 65],
        "crates/infra-messaging/src/outbox.rs": [65],
        "crates/jobs-worker/src/main.rs": [47, 65],
        "crates/infra-bearerauthn/src/jwt.rs": [],
        "crates/infra-serviceauth/src/verify.rs": [],
        "crates/service/src/main.rs": [1, 7, 47, 65],
        "build/docker/Dockerfile": [1, 7, 47, 65],
        "scripts/ci/runtime-image-inventory.py": [1, 7, 47, 65],
        "Cargo.lock": [1, 7, 47, 65],
        "scripts/lib/template_init.py": [1, 7, 47, 65],
    }
    for path, expected in artifact_cases.items():
        actual = select_artifacts([path], inventory)
        if actual != expected:
            raise SystemExit(f"{path}: artifact graphs {actual}, expected {expected}")
    if select_artifacts(["README.md", "crates/infra-cache/src/lib.rs"], inventory) != [65]:
        raise SystemExit("prose widened the artifact selection")
    rows = artifact_matrix(list(ARTIFACT_GRAPHS))["include"]
    if [row["graph"] for row in rows] != list(ARTIFACT_GRAPHS) or not {row["cache"] for row in rows} <= set(map(family, SAVING_PARTS)):
        raise SystemExit("every release representative must restore a cache that a saving part writes")
    print("initializer matrix self-test: pass")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--all", action="store_true")
    mode.add_argument("--self-test", action="store_true")
    parser.add_argument("--artifacts-only", action="store_true", help="emit only the shared artifact graph selection")
    arguments = parser.parse_args()
    inventory = graphs()
    if arguments.self_test:
        self_test(inventory)
        return 0
    paths = None if arguments.all else [line.strip() for line in sys.stdin if line.strip()]
    artifacts = select_artifacts(paths, inventory)
    print(f"artifact_graphs={','.join(map(str, artifacts))}")
    if arguments.artifacts_only:
        return 0
    print(f"artifact_matrix={json.dumps(artifact_matrix(artifacts), separators=(',', ':'))}")
    selected = select(paths, inventory)
    print(f"initializer_matrix={json.dumps(matrix(selected, inventory), separators=(',', ':'))}")
    print(f"initializer_graphs={len(selected)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
