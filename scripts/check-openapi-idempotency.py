#!/usr/bin/env python3
"""Enforce Idempotency-Key replay compatibility between two OpenAPI JSON documents."""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path
from urllib.parse import unquote

METHODS = ("post", "put", "patch", "delete")
KEY = "idempotency-key"
REPLAYED = "idempotent-replayed"
CAPTURE = re.compile(r"\{\{|\}\}|\{(\*?)(?:[^{}]|\{\{|\}\})+\}")
Route = tuple[str, ...]


def route_pattern(path: str) -> Route:
    normalized = CAPTURE.sub(lambda m: m[0] if m[1] is None else "{" + m[1] + "}", path[1:])
    return tuple(normalized.split("/"))


def literal_prefix(part: str) -> str:
    return part.removesuffix("{*}").removesuffix("{}")


def specificity(part: str) -> tuple[int, bool, bool]:
    prefix = literal_prefix(part)
    return len(prefix), part == prefix, not part.endswith("{*}")


def route_priority(route: Route) -> tuple[tuple[int, bool, bool], ...]:
    return tuple(map(specificity, route))


def intersection(left: Route, right: Route) -> Route | None:
    common: list[str] = []
    for index, (old, new) in enumerate(zip(left, right), 1):
        wider, narrower = sorted((old, new), key=specificity)
        if wider != narrower:
            prefix = literal_prefix(wider)
            if wider == prefix or not literal_prefix(narrower).startswith(prefix):
                return None
            if narrower == prefix and index == max(len(left), len(right)):
                return None
        if narrower.endswith("{*}") and wider.endswith("{}"):
            narrower = literal_prefix(narrower) + "{}"
        common.append(narrower)
        if old.endswith("{*}"):
            return (*common, *right[index:])
        if new.endswith("{*}"):
            return (*common, *left[index:])
    return tuple(common) if len(left) == len(right) else None


def exposed_overlap(left: Route, right: Route, shadows: list[Route]) -> bool:
    overlap = intersection(left, right)
    if overlap is None:
        return False
    coverings = [part for shadow in shadows if (part := intersection(overlap, shadow)) is not None]
    if overlap in coverings:
        return False
    if not coverings or not overlap[-1].endswith("{*}"):
        return True

    def exposed(shape: Route) -> bool:
        return all(intersection(shape, cov) != shape for cov in coverings)

    shape = (*overlap[:-1], literal_prefix(overlap[-1]) + "{}")
    if exposed(shape):
        return True
    deepest = max(map(len, coverings))
    while len(shape) <= deepest:
        if exposed((*shape, "")):
            return True
        shape = (*shape, "{}")
        if exposed(shape):
            return True
    return False


def resolve(document: dict, item: dict) -> dict:
    seen: set[str] = set()
    while "$ref" in item:
        ref = item["$ref"]
        if not ref.startswith("#/") or ref in seen:
            raise ValueError(f"cannot resolve OpenAPI reference {ref!r}")
        seen.add(ref)
        item = document
        for part in unquote(ref[2:]).split("/"):
            key = part.replace("~1", "/").replace("~0", "~")
            item = item[int(key) if isinstance(item, list) else key]
    return item


def document_routes(document: dict) -> dict[Route, tuple[str, dict[str, set[str] | None]]]:
    routes: dict[Route, tuple[str, dict[str, set[str] | None]]] = {}
    for path, declaration in document.get("paths", {}).items():
        if not path.startswith("/"):
            continue
        item = resolve(document, declaration)
        fingerprints: dict[str, set[str] | None] = {}
        for method in METHODS:
            if method not in item:
                continue
            op = item[method]
            fingerprints[method] = None
            headers = {
                p["name"].lower(): p.get("required", False)
                for p in (resolve(document, raw) for raw in item.get("parameters", []) + op.get("parameters", []))
                if p["in"] == "header"
            }
            if not headers.get(KEY):
                continue
            if not any(
                name.lower() == REPLAYED
                for status, resp in op.get("responses", {}).items()
                if status.startswith("2")
                for name in resolve(document, resp).get("headers", {})
            ):
                continue
            fingerprints[method] = set(headers) - {KEY, "content-type"}
        routes[route_pattern(path)] = (path, fingerprints)
    return routes


def check_compatibility(before_doc: dict, after_doc: dict) -> list[str]:
    before = document_routes(before_doc)
    after = document_routes(after_doc)
    errors: list[str] = []
    added_routes = sorted(after.keys() - before.keys(), key=lambda r: after[r][0])
    before_prio = {r: route_priority(r) for r in before}
    after_prio = {r: route_priority(r) for r in after}

    for route, (path, before_ops) in before.items():
        protected = [m for m, hdrs in before_ops.items() if hdrs is not None]
        if not protected:
            continue
        prio = before_prio[route]
        shadows = [prev for prev, rank in before_prio.items() if rank > prio]
        for added in added_routes:
            if after_prio[added] > prio and exposed_overlap(route, added, shadows):
                affected = ", ".join(m.upper() for m in protected)
                errors.append(f"{after[added][0]} would take precedence over {affected} {path}")
        if route not in after:
            for fb, (fb_path, ops) in after.items():
                methods = [m.upper() for m in protected if m in ops]
                fb_prio = after_prio[fb]
                if methods and fb_prio < prio:
                    cand_shadows = [o for o, rank in after_prio.items() if rank > fb_prio]
                    if exposed_overlap(route, fb, shadows + cand_shadows):
                        errors.append(f"removing {', '.join(methods)} {path} would route retries to {fb_path}")
            continue
        after_ops = after[route][1]
        for method in protected:
            old_headers = before_ops[method]
            if method not in after_ops or old_headers == after_ops[method]:
                continue
            new_headers = after_ops[method]
            if new_headers is None:
                change = "removed the idempotent replay contract"
            else:
                added_h = ", ".join(sorted(new_headers - old_headers)) or "none"
                removed_h = ", ".join(sorted(old_headers - new_headers)) or "none"
                change = f"changed fingerprint headers (added: {added_h}; removed: {removed_h})"
            errors.append(f"{method.upper()} {path} {change}")
    return errors


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("base", type=Path)
    parser.add_argument("candidate", type=Path)
    args = parser.parse_args()
    try:
        errors = check_compatibility(
            json.loads(args.base.read_text(encoding="utf-8")),
            json.loads(args.candidate.read_text(encoding="utf-8")),
        )
    except (OSError, ValueError, LookupError, TypeError) as exc:
        print(f"openapi-idempotency: {exc}", file=sys.stderr)
        return 2
    for err in errors:
        print(f"openapi-idempotency: {err}", file=sys.stderr)
    return 1 if errors else 0


if __name__ == "__main__":
    raise SystemExit(main())
