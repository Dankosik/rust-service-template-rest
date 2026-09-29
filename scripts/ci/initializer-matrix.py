#!/usr/bin/env python3
"""Plan the CI initializer matrix: which runtime graphs a diff can affect, and
the part that runs each graph.

    initializer-matrix.py --all            every graph (push, tag, schedule)
    initializer-matrix.py < changed-paths  graphs the diff can affect
    initializer-matrix.py --self-test

Prints `initializer_matrix=<json>` for GITHUB_OUTPUT. The matrix always holds
the `source` part; runtime parts appear when they have a selected graph.

Only initializer_runtime paths (scripts/ci/changed-surfaces.sh) select graphs.
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
    "messaging", "outbox", "webhooks", "inbound_webhooks", "grpc", "cache",
)
NARROWABLE = ("crates/", "migrations/", "test/", "api/proto/")

# Parts group graphs that share dependency features, so one part's Cargo
# target and cache serve all of them, and balance the measured per-graph
# minutes (docs/ci-cd-production-ready.md). Every graph belongs to one part.
PARTS: dict[str, tuple[int, ...]] = {
    "baseline": (1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 47, 50, 51, 52, 54, 57, 58, 59, 60, 62),
    "idempotency": (13, 14, 15, 16, 23, 24),
    "jobs": (17, 18, 19, 20, 21, 22, 56),
    "jobs-webhooks": (25, 26, 27, 28, 29),
    "webhooks": (30, 31, 32, 33, 34, 35, 36, 37, 38),
    "webhooks-messaging": (39, 40, 41, 42, 43, 44, 45, 46, 48, 61, 63),
    "messaging-oauth": (49, 53, 55),
}


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


def selects_runtime(paths: list[str]) -> bool:
    """The classifier's initializer_runtime verdict for these paths together."""

    if not paths:
        return False
    surfaces = subprocess.run(
        ["bash", str(ROOT / "scripts/ci/changed-surfaces.sh")],
        input="".join(f"{path}\n" for path in paths), capture_output=True, text=True, cwd=ROOT,
    ).stdout
    return "initializer_runtime=true" in surfaces.splitlines()


def select(paths: list[str] | None, inventory: dict[int, dict[str, str]]) -> set[int]:
    if paths is None:
        return set(inventory)
    if selects_runtime([path for path in paths if not path.startswith(NARROWABLE)]):
        return set(inventory)
    narrowable = [path for path in paths if path.startswith(NARROWABLE) and selects_runtime([path])]
    profiles = template_init._profile_data(ROOT)
    return {
        number for number, graph in inventory.items()
        if any(kept(path, removed_paths(profiles, graph)) for path in narrowable)
    }


def matrix(selected: set[int], inventory: dict[int, dict[str, str]]) -> dict[str, list[dict[str, str]]]:
    assigned = [number for numbers in PARTS.values() for number in numbers]
    if sorted(assigned) != sorted(inventory):
        missing = sorted(set(inventory) - set(assigned))
        extra = sorted(set(assigned) - set(inventory))
        duplicate = sorted({number for number in assigned if assigned.count(number) > 1})
        raise SystemExit(f"initializer parts must hold every graph once: missing={missing} unknown={extra} duplicate={duplicate}")
    include = [{"part": "source"}]
    for part, numbers in PARTS.items():
        chosen = [number for number in numbers if number in selected]
        if chosen:
            include.append({"part": part, "graphs": ",".join(map(str, chosen))})
    return {"include": include}


def self_test(inventory: dict[int, dict[str, str]]) -> None:
    everything = set(inventory)
    cases = {
        "crates/service/src/main.rs": everything,
        "crates/infra-http/src/observe.rs": set(),
        "crates/infra-cache/src/lib.rs": {n for n, g in inventory.items() if g["cache"] != "none"},
        "crates/infra-messaging/src/outbox.rs": {n for n, g in inventory.items() if g["outbox"] != "none"},
        "crates/infra-bearerauthn/src/jwt.rs": {n for n, g in inventory.items() if g["authn"] == "oidc-jwt"},
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
    if matrix(set(), inventory) != {"include": [{"part": "source"}]}:
        raise SystemExit("an empty selection must keep only the source part")
    print("initializer matrix self-test: pass")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--all", action="store_true")
    mode.add_argument("--self-test", action="store_true")
    arguments = parser.parse_args()
    inventory = graphs()
    if arguments.self_test:
        self_test(inventory)
        return 0
    paths = None if arguments.all else [line.strip() for line in sys.stdin if line.strip()]
    selected = select(paths, inventory)
    print(f"initializer_matrix={json.dumps(matrix(selected, inventory), separators=(',', ':'))}")
    print(f"initializer_graphs={len(selected)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
