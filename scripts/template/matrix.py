#!/usr/bin/env python3
"""Plan the CI initializer matrix: which runtime graphs a diff can affect, and
the part that runs each graph.

    matrix.py --all            every graph (push, tag, schedule)
    matrix.py --union BASE < changed-paths  diff classified at base and head
    matrix.py < changed-paths  graphs the diff can affect
    matrix.py --self-test

Prints `initializer_matrix=<json>`, `artifact_graphs=<ids>` and
`artifact_matrix=<json>` for GITHUB_OUTPUT. The initializer matrix always holds
the `source` part; runtime parts appear when they have a selected graph. The
artifact matrix runs the selected release representatives in the legs of
ARTIFACT_LEGS, each restoring the initializer cache of its graphs' family.

The classifier selects the source part with initializer_source, runtime graphs
with initializer_runtime and the four release representatives with
initializer_artifacts; --artifacts-only emits the latter selection for make
verify.
Such a path under a narrowable prefix affects a graph only when the initializer
keeps it for the graph's profile tuple: a path the graph removes cannot reach
its build. Any other one (manifests, the lockfile, the initializer, Make,
workflows) selects every graph.
"""

from __future__ import annotations

import argparse
from collections import Counter
from functools import cache
import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.dont_write_bytecode = True
for _subdir in ("scripts/template", "scripts/verify"):
    _entry = str(ROOT / _subdir)
    if _entry not in sys.path:
        sys.path.insert(0, _entry)

import init as template_init  # noqa: E402
import surfaces as surfaces_mod  # noqa: E402
from surfaces import classify_files  # noqa: E402

FIELDS = (
    "database", "authn", "outbound_http", "outbound_auth", "http_idempotency", "jobs",
    "messaging", "outbox", "webhooks", "inbound_webhooks", "grpc", "cache", "object_storage", "service_auth",
)
GRAPHS: dict[int, tuple[str, ...]] = {
    1: ("none", "none", "none", "none", "none", "none", "none", "none", "none", "none", "none", "none", "none", "none"),
    2: ("none", "none", "bounded", "none", "none", "none", "none", "none", "none", "none", "none", "none", "none", "none"),
    3: ("none", "oidc-jwt", "none", "none", "none", "none", "none", "none", "none", "none", "none", "none", "none", "none"),
    4: ("none", "oidc-jwt", "bounded", "none", "none", "none", "none", "none", "none", "none", "none", "none", "none", "none"),
    5: ("none", "oidc-introspection", "none", "none", "none", "none", "none", "none", "none", "none", "none", "none", "none", "none"),
    6: ("none", "oidc-introspection", "bounded", "none", "none", "none", "none", "none", "none", "none", "none", "none", "none", "none"),
    7: ("postgres", "none", "none", "none", "none", "none", "none", "none", "none", "none", "none", "none", "none", "none"),
    8: ("postgres", "none", "bounded", "none", "none", "none", "none", "none", "none", "none", "none", "none", "none", "none"),
    9: ("postgres", "oidc-jwt", "none", "none", "none", "none", "none", "none", "none", "none", "none", "none", "none", "none"),
    10: ("postgres", "oidc-jwt", "bounded", "none", "none", "none", "none", "none", "none", "none", "none", "none", "none", "none"),
    11: ("postgres", "oidc-introspection", "none", "none", "none", "none", "none", "none", "none", "none", "none", "none", "none", "none"),
    12: ("postgres", "oidc-introspection", "bounded", "none", "none", "none", "none", "none", "none", "none", "none", "none", "none", "none"),
    13: ("postgres", "oidc-jwt", "none", "none", "postgres", "none", "none", "none", "none", "none", "none", "none", "none", "none"),
    14: ("postgres", "oidc-jwt", "bounded", "none", "postgres", "none", "none", "none", "none", "none", "none", "none", "none", "none"),
    15: ("postgres", "oidc-introspection", "none", "none", "postgres", "none", "none", "none", "none", "none", "none", "none", "none", "none"),
    16: ("postgres", "oidc-introspection", "bounded", "none", "postgres", "none", "none", "none", "none", "none", "none", "none", "none", "none"),
    17: ("postgres", "none", "none", "none", "none", "postgres", "none", "none", "none", "none", "none", "none", "none", "none"),
    18: ("postgres", "none", "bounded", "none", "none", "postgres", "none", "none", "none", "none", "none", "none", "none", "none"),
    19: ("postgres", "oidc-jwt", "none", "none", "none", "postgres", "none", "none", "none", "none", "none", "none", "none", "none"),
    20: ("postgres", "oidc-jwt", "bounded", "none", "none", "postgres", "none", "none", "none", "none", "none", "none", "none", "none"),
    21: ("postgres", "oidc-introspection", "none", "none", "none", "postgres", "none", "none", "none", "none", "none", "none", "none", "none"),
    22: ("postgres", "oidc-introspection", "bounded", "none", "none", "postgres", "none", "none", "none", "none", "none", "none", "none", "none"),
    23: ("postgres", "oidc-jwt", "none", "none", "postgres", "postgres", "none", "none", "none", "none", "none", "none", "none", "none"),
    24: ("postgres", "oidc-jwt", "bounded", "none", "postgres", "postgres", "none", "none", "none", "none", "none", "none", "none", "none"),
    25: ("postgres", "oidc-introspection", "none", "none", "postgres", "postgres", "none", "none", "none", "none", "none", "none", "none", "none"),
    26: ("postgres", "oidc-introspection", "bounded", "none", "postgres", "postgres", "none", "none", "none", "none", "none", "none", "none", "none"),
    27: ("postgres", "none", "none", "none", "none", "postgres", "none", "none", "none", "standard-webhooks", "none", "none", "none", "none"),
    28: ("postgres", "none", "bounded", "none", "none", "postgres", "none", "none", "none", "standard-webhooks", "none", "none", "none", "none"),
    29: ("postgres", "none", "bounded", "none", "none", "postgres", "none", "none", "durable", "none", "none", "none", "none", "none"),
    30: ("postgres", "none", "bounded", "none", "none", "postgres", "none", "none", "durable", "standard-webhooks", "none", "none", "none", "none"),
    31: ("postgres", "oidc-jwt", "none", "none", "none", "postgres", "none", "none", "none", "standard-webhooks", "none", "none", "none", "none"),
    32: ("postgres", "oidc-jwt", "bounded", "none", "none", "postgres", "none", "none", "none", "standard-webhooks", "none", "none", "none", "none"),
    33: ("postgres", "oidc-jwt", "bounded", "none", "none", "postgres", "none", "none", "durable", "none", "none", "none", "none", "none"),
    34: ("postgres", "oidc-jwt", "bounded", "none", "none", "postgres", "none", "none", "durable", "standard-webhooks", "none", "none", "none", "none"),
    35: ("postgres", "oidc-introspection", "none", "none", "none", "postgres", "none", "none", "none", "standard-webhooks", "none", "none", "none", "none"),
    36: ("postgres", "oidc-introspection", "bounded", "none", "none", "postgres", "none", "none", "none", "standard-webhooks", "none", "none", "none", "none"),
    37: ("postgres", "oidc-introspection", "bounded", "none", "none", "postgres", "none", "none", "durable", "none", "none", "none", "none", "none"),
    38: ("postgres", "oidc-introspection", "bounded", "none", "none", "postgres", "none", "none", "durable", "standard-webhooks", "none", "none", "none", "none"),
    39: ("postgres", "oidc-jwt", "none", "none", "postgres", "postgres", "none", "none", "none", "standard-webhooks", "none", "none", "none", "none"),
    40: ("postgres", "oidc-jwt", "bounded", "none", "postgres", "postgres", "none", "none", "none", "standard-webhooks", "none", "none", "none", "none"),
    41: ("postgres", "oidc-jwt", "bounded", "none", "postgres", "postgres", "none", "none", "durable", "none", "none", "none", "none", "none"),
    42: ("postgres", "oidc-jwt", "bounded", "none", "postgres", "postgres", "none", "none", "durable", "standard-webhooks", "none", "none", "none", "none"),
    43: ("postgres", "oidc-introspection", "none", "none", "postgres", "postgres", "none", "none", "none", "standard-webhooks", "none", "none", "none", "none"),
    44: ("postgres", "oidc-introspection", "bounded", "none", "postgres", "postgres", "none", "none", "none", "standard-webhooks", "none", "none", "none", "none"),
    45: ("postgres", "oidc-introspection", "bounded", "none", "postgres", "postgres", "none", "none", "durable", "none", "none", "none", "none", "none"),
    46: ("postgres", "oidc-introspection", "bounded", "none", "postgres", "postgres", "none", "none", "durable", "standard-webhooks", "none", "none", "none", "none"),
    47: ("none", "none", "none", "none", "none", "none", "nats-jetstream", "none", "none", "none", "none", "none", "none", "none"),
    48: ("postgres", "none", "none", "none", "none", "postgres", "nats-jetstream", "postgres", "none", "none", "none", "none", "none", "none"),
    49: ("postgres", "oidc-introspection", "bounded", "none", "postgres", "postgres", "nats-jetstream", "postgres", "durable", "standard-webhooks", "none", "none", "none", "none"),
    50: ("none", "none", "bounded", "oauth2-client-credentials", "none", "none", "none", "none", "none", "none", "none", "none", "none", "none"),
    51: ("none", "oidc-jwt", "bounded", "oauth2-client-credentials", "none", "none", "none", "none", "none", "none", "none", "none", "none", "none"),
    52: ("none", "oidc-introspection", "bounded", "oauth2-client-credentials", "none", "none", "none", "none", "none", "none", "none", "none", "none", "none"),
    53: ("postgres", "oidc-introspection", "bounded", "oauth2-client-credentials", "postgres", "postgres", "none", "none", "durable", "standard-webhooks", "none", "none", "none", "none"),
    54: ("none", "none", "bounded", "oauth2-client-credentials", "none", "none", "nats-jetstream", "none", "none", "none", "none", "none", "none", "none"),
    55: ("postgres", "oidc-introspection", "bounded", "oauth2-client-credentials", "postgres", "postgres", "nats-jetstream", "postgres", "durable", "standard-webhooks", "none", "none", "none", "none"),
    56: ("postgres", "none", "none", "none", "none", "postgres", "nats-jetstream", "none", "none", "none", "none", "none", "none", "none"),
    57: ("none", "none", "none", "none", "none", "none", "none", "none", "none", "none", "enabled", "none", "none", "none"),
    58: ("none", "oidc-jwt", "none", "none", "none", "none", "none", "none", "none", "none", "enabled", "none", "none", "none"),
    59: ("none", "oidc-introspection", "none", "none", "none", "none", "none", "none", "none", "none", "enabled", "none", "none", "none"),
    60: ("none", "none", "bounded", "oauth2-client-credentials", "none", "none", "none", "none", "none", "none", "enabled", "none", "none", "none"),
    61: ("postgres", "oidc-introspection", "bounded", "oauth2-client-credentials", "postgres", "postgres", "nats-jetstream", "postgres", "durable", "standard-webhooks", "enabled", "none", "none", "none"),
    62: ("none", "none", "none", "none", "none", "none", "none", "none", "none", "none", "none", "redis", "none", "none"),
    63: ("postgres", "oidc-introspection", "bounded", "oauth2-client-credentials", "postgres", "postgres", "nats-jetstream", "postgres", "durable", "standard-webhooks", "enabled", "redis", "none", "none"),
    64: ("none", "none", "none", "none", "none", "none", "none", "none", "none", "none", "none", "none", "s3", "none"),
    65: ("postgres", "oidc-introspection", "bounded", "oauth2-client-credentials", "postgres", "postgres", "nats-jetstream", "postgres", "durable", "standard-webhooks", "enabled", "redis", "s3", "none"),
    66: ("none", "none", "none", "none", "none", "none", "none", "none", "none", "none", "enabled", "none", "none", "jwt"),
    67: ("none", "oidc-jwt", "none", "none", "none", "none", "none", "none", "none", "none", "enabled", "none", "none", "jwt"),
    68: ("postgres", "oidc-introspection", "bounded", "oauth2-client-credentials", "postgres", "postgres", "nats-jetstream", "postgres", "durable", "standard-webhooks", "enabled", "redis", "s3", "jwt"),
}

# One image-derived job per leg; the two core representatives share one, so a
# full run's long jobs fit the runners CodeQL leaves (docs/ci-cd-production-ready.md).
ARTIFACT_LEGS = ((1, 7), (47,), (65,))
ARTIFACT_GRAPHS = tuple(number for leg in ARTIFACT_LEGS for number in leg)
NARROWABLE = ("crates/", "migrations/", "test/", "api/proto/")

# Parts balance the measured warm seconds per graph into legs of about
# fourteen minutes, no longer than the longest other job, so that a full run's
# long jobs together hold no more runners than the account's twenty concurrent
# jobs leave beside CodeQL (docs/ci-cd-production-ready.md). A graph after one
# that built the same dependency variants costs less (61 took 40 s after 49 and
# 56, 103 s without them), so such neighbors share a part. Every graph belongs
# to one part. A part restores the Cargo cache of its family, the name before
# the last dash: `core` graphs retain no idempotency, jobs, messaging, outbox
# or webhooks, `full` graphs do. Only the family's saving part, which holds its
# heaviest dependency variants, writes that cache, so two caches serve every
# part inside the repository's 10 GB budget.
PARTS: dict[str, tuple[int, ...]] = {
    "full-1": (35, 38, 39, 41, 42, 45, 54, 55, 63, 65, 68),
    "full-2": (13, 15, 25, 26, 36, 44, 49, 56, 61),
    "full-3": (17, 19, 43, 46, 47, 53),
    "full-4": (18, 21, 27, 34, 48),
    "full-5": (14, 22, 29, 30, 31, 33, 37),
    "full-6": (16, 20, 23, 24, 28, 32, 40),
    "core-1": (3, 4, 5, 9, 50, 51, 52, 57, 58, 60, 62, 64, 67),
    "core-2": (1, 2, 6, 7, 8, 10, 11, 12, 59, 66),
}
SAVING_PARTS = ("full-1", "core-1")
_TARGET_SURFACES = ("initializer_runtime", "initializer_artifacts")


def family(part: str) -> str:
    return part.rsplit("-", 1)[0]


def removed_paths(profiles: template_init.ProfileData, graph: tuple[str, ...]) -> tuple[str, ...]:
    inputs = template_init.InitInputs(
        service_name="matrix", repository="https://github.com/example/matrix", description="matrix",
        codeowner="@example/platform", agent_harness="core", **dict(zip(FIELDS, graph, strict=True)),
    )
    selected = template_init._selected_marker_profiles(inputs)
    removals = [path for profile, paths in profiles.removals.items() if profile not in selected for path in paths]
    return (*profiles.source_only, *removals)


def kept(path: str, removed: tuple[str, ...]) -> bool:
    return not any(path == entry.rstrip("/") or path.startswith(entry.rstrip("/") + "/") for entry in removed)


def initializer_surfaces(paths: list[str], base: str | None) -> set[str]:
    if not paths:
        return set()
    if base is None:
        try:
            verdicts = classify_files(ROOT, paths)
        except (ValueError, RuntimeError) as error:
            sys.stderr.write(f"{error}\n")
            raise SystemExit(1)
        return {surface for surface in _TARGET_SURFACES if verdicts.get(surface)}
    try:
        report = surfaces_mod.classify_union_report(ROOT, base, paths)
    except surfaces_mod.SurfaceConfigError as error:
        sys.stderr.write(f"{error}\n")
        raise SystemExit(error.exit_code)
    if not report.classified:
        sys.stderr.write(surfaces_mod.format_report(report))
        raise SystemExit(1)
    return {surface for surface in _TARGET_SURFACES if report.surfaces.get(surface)}


def _narrow_surfaces(path: str, base: str | None) -> set[str]:
    try:
        verdicts = classify_files(ROOT, [path])
    except (ValueError, RuntimeError):
        if base is not None:
            return initializer_surfaces([path], base)
        raise
    return {surface for surface in _TARGET_SURFACES if verdicts.get(surface)}


def select(paths: list[str] | None, base: str | None = None,
           artifacts_only: bool = False) -> tuple[set[int], list[int]]:
    candidates = {"initializer_artifacts": set(ARTIFACT_GRAPHS)}
    missing = candidates["initializer_artifacts"] - GRAPHS.keys()
    if missing:
        raise SystemExit(f"initializer inventory lacks release representatives: {sorted(missing)}")
    if not artifacts_only:
        candidates["initializer_runtime"] = set(GRAPHS)
    broad = (candidates.keys() if paths is None else
             initializer_surfaces([path for path in paths if not path.startswith(NARROWABLE)], base))
    selected = {surface: numbers if surface in broad else set() for surface, numbers in candidates.items()}
    narrow = candidates.keys() - broad
    narrow_paths = list(dict.fromkeys(path for path in (paths or ()) if path.startswith(NARROWABLE)))
    if narrow and narrow_paths:
        profiles = template_init._profile_data(ROOT)

        @cache
        def removals(number: int) -> tuple[str, ...]:
            return removed_paths(profiles, GRAPHS[number])

        for path in narrow_paths:
            for surface in _narrow_surfaces(path, base) & narrow:
                selected[surface].update(number for number in candidates[surface] if kept(path, removals(number)))
    return selected.get("initializer_runtime", set()), sorted(selected["initializer_artifacts"])


def matrix(selected: set[int]) -> dict[str, list[dict[str, str | bool]]]:
    assigned = Counter(number for numbers in PARTS.values() for number in numbers)
    if assigned != Counter(GRAPHS.keys()):
        missing = sorted(GRAPHS.keys() - assigned.keys())
        extra = sorted(assigned.keys() - GRAPHS.keys())
        duplicate = sorted(number for number, count in assigned.items() if count > 1)
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


def artifact_matrix(artifacts: list[int]) -> dict[str, list[dict[str, str]]]:
    parts = {number: part for part, numbers in PARTS.items() for number in numbers}
    include = []
    for leg in ARTIFACT_LEGS:
        chosen = [number for number in leg if number in artifacts]
        if chosen:
            include.append({"graphs": ",".join(map(str, chosen)), "cache": family(parts[chosen[0]])})
    return {"include": include}


def self_test() -> None:
    everything = set(GRAPHS)
    profiles_by_graph = {
        number: dict(zip(FIELDS, graph, strict=True)) for number, graph in GRAPHS.items()
    }
    cases = {
        "crates/service/src/main.rs": everything,
        "crates/infra-http/src/observe.rs": set(),
        "crates/infra-cache/src/lib.rs": {62, 63, 65, 68},
        "crates/infra-messaging/src/outbox.rs": {n for n, g in profiles_by_graph.items() if g["outbox"] != "none"},
        "crates/infra-bearerauthn/src/jwt.rs": {n for n, g in profiles_by_graph.items() if g["authn"] == "oidc-jwt"},
        "crates/infra-serviceauth/src/verify.rs": {n for n, g in profiles_by_graph.items() if g["service_auth"] != "none"},
        "Cargo.lock": everything,
        "scripts/template/init.py": everything,
        "scripts/template/matrix.py": everything,
        "scripts/template/state.py": everything,
        "scripts/template/sync.py": set(),
        "build/docker/runtime-progress.Dockerfile": set(),
    }
    for path, expected in cases.items():
        actual, _ = select([path])
        if actual != expected:
            raise SystemExit(f"{path}: selected {sorted(actual)}, expected {sorted(expected)}")
    if select(["README.md"])[0]:
        raise SystemExit("README.md selected a runtime graph")
    if select(["README.md", "crates/infra-cache/src/lib.rs"])[0] != cases["crates/infra-cache/src/lib.rs"]:
        raise SystemExit("projected text widened a narrowed selection")
    full = matrix(everything)["include"]
    if [row["part"] for row in full] != ["source", *PARTS]:
        raise SystemExit("the full matrix must list every part")
    if sorted(row["part"] for row in full if row["save"]) != sorted(["source", *SAVING_PARTS]):
        raise SystemExit("a full matrix must save the source cache and each family's cache once")
    if {row["cache"] for row in full} != {"source", *map(family, SAVING_PARTS)}:
        raise SystemExit("every part must restore a cache that a saving part writes")
    narrowed = matrix(everything - {PARTS[SAVING_PARTS[0]][0]})["include"]
    if any(row["save"] for row in narrowed if row["part"] == SAVING_PARTS[0]):
        raise SystemExit("a saving part that ran a subset of its graphs must not save")
    if matrix(set()) != {"include": [{"part": "source", "cache": "source", "save": True}]}:
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
        "build/docker/pgo-train.sh": [1, 7, 47, 65],
        "build/docker/runtime-progress.Dockerfile": [],
        "scripts/image/inventory.py": [1, 7, 47, 65],
        "Cargo.lock": [1, 7, 47, 65],
        "scripts/template/init.py": [1, 7, 47, 65],
        "scripts/template/matrix.py": [1, 7, 47, 65],
        "scripts/template/state.py": [1, 7, 47, 65],
        "scripts/template/sync.py": [],
    }
    for path, expected in artifact_cases.items():
        _, actual = select([path], artifacts_only=True)
        if actual != expected:
            raise SystemExit(f"{path}: artifact graphs {actual}, expected {expected}")
    if select(["README.md", "crates/infra-cache/src/lib.rs"], artifacts_only=True)[1] != [65]:
        raise SystemExit("prose widened the artifact selection")
    parts = {number: part for part, numbers in PARTS.items() for number in numbers}
    if sorted(ARTIFACT_GRAPHS) != [1, 7, 47, 65]:
        raise SystemExit("the artifact legs must hold each required release representative once")
    if any(len({family(parts[number]) for number in leg}) != 1 for leg in ARTIFACT_LEGS):
        raise SystemExit("an artifact leg must hold one cache family")
    rows = artifact_matrix(list(ARTIFACT_GRAPHS))["include"]
    if [row["graphs"] for row in rows] != [",".join(map(str, leg)) for leg in ARTIFACT_LEGS] or not {row["cache"] for row in rows} <= set(map(family, SAVING_PARTS)):
        raise SystemExit("every release representative must restore a cache that a saving part writes")
    if artifact_matrix([7, 65]) != {"include": [{"graphs": "7", "cache": "core"}, {"graphs": "65", "cache": "full"}]}:
        raise SystemExit("a narrowed artifact selection must keep only its selected graphs")
    print("initializer matrix self-test: pass")


def print_graph_rows(selection: str | None, *, artifacts: bool = False) -> int:
    selected = set(map(str, ARTIFACT_GRAPHS if artifacts else GRAPHS))
    if selection is not None:
        requested = selection.split(",")
        distinct = set(requested)
        if len(distinct) != len(requested) or not distinct <= set(map(str, GRAPHS)):
            print(f"--runtime-graphs requires distinct comma-separated graph IDs {min(GRAPHS)}..{max(GRAPHS)}",
                  file=sys.stderr)
            return 2
        if artifacts and not distinct <= selected:
            print(f"--artifact-graphs accepts only distinct IDs {','.join(map(str, ARTIFACT_GRAPHS))}",
                  file=sys.stderr)
            return 2
        selected = distinct
    for number, graph in GRAPHS.items():
        if str(number) in selected:
            print(number, *graph)
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--all", action="store_true")
    mode.add_argument("--union", metavar="BASE", help="use the same base and head classifiers as the diff")
    mode.add_argument("--self-test", action="store_true")
    mode.add_argument("--list-graphs", action="store_true")
    mode.add_argument("--runtime-graphs", metavar="IDS")
    mode.add_argument("--artifact-graphs", nargs="?", const=",".join(map(str, ARTIFACT_GRAPHS)), metavar="IDS")
    parser.add_argument("--files", nargs="*", help="changed paths (defaults to stdin)")
    parser.add_argument("--artifacts-only", action="store_true", help="emit only the shared artifact graph selection")
    arguments = parser.parse_args()
    if arguments.self_test:
        self_test()
        return 0
    if arguments.list_graphs or arguments.runtime_graphs is not None:
        return print_graph_rows(arguments.runtime_graphs)
    if arguments.artifact_graphs is not None:
        return print_graph_rows(arguments.artifact_graphs, artifacts=True)
    if arguments.all:
        paths = None
    elif arguments.files is not None:
        paths = list(arguments.files)
    else:
        paths = [path for line in sys.stdin if (path := line.rstrip("\n"))]
    selected, artifacts = select(paths, arguments.union, arguments.artifacts_only)
    print(f"artifact_graphs={','.join(map(str, artifacts))}")
    if arguments.artifacts_only:
        return 0
    print(f"artifact_matrix={json.dumps(artifact_matrix(artifacts), separators=(',', ':'))}")
    print(f"initializer_matrix={json.dumps(matrix(selected), separators=(',', ':'))}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
