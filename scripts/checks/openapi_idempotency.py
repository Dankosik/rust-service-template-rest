#!/usr/bin/env python3
"""Enforce replay compatibility between two OpenAPI documents in JSON form."""

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
    normalized = CAPTURE.sub(lambda match: match[0] if match[1] is None else "{" + match[1] + "}", path[1:])
    return tuple(normalized.split("/"))


def literal_prefix(part: str) -> str:
    return part.removesuffix("{*}").removesuffix("{}")


def specificity(part: str) -> tuple[int, bool, bool]:
    prefix = literal_prefix(part)
    return len(prefix), part == prefix, not part.endswith("{*}")


def route_priority(route: Route) -> tuple[tuple[int, bool, bool], ...]:
    return tuple(map(specificity, route))


def intersection(left: Route, right: Route) -> Route | None:
    common = []
    for index, (old, new) in enumerate(zip(left, right), 1):
        wider, narrower = sorted((old, new), key=specificity)
        if wider != narrower:
            prefix = literal_prefix(wider)
            if wider == prefix or not literal_prefix(narrower).startswith(prefix):
                return None
            if narrower == prefix and index == max(len(left), len(right)):
                return None
        common.append(narrower)
        if old.endswith("{*}"):
            return (*common, *right[index:])
        if new.endswith("{*}"):
            return (*common, *left[index:])
    return tuple(common) if len(left) == len(right) else None


def resolve(document: dict, item: dict) -> dict:
    references = set()
    while "$ref" in item:
        reference = item["$ref"]
        if not reference.startswith("#/") or reference in references:
            raise ValueError(f"cannot resolve OpenAPI reference {reference!r}")
        references.add(reference)
        item = document
        for part in unquote(reference[2:]).split("/"):
            key = part.replace("~1", "/").replace("~0", "~")
            item = item[int(key) if isinstance(item, list) else key]
    return item


def document_routes(document: dict) -> dict[Route, tuple[str, dict[str, set[str] | None]]]:
    routes = {}
    for path, declaration in document.get("paths", {}).items():
        if not path.startswith("/"):
            continue
        item = resolve(document, declaration)
        fingerprints = {}
        for method in METHODS:
            if method not in item:
                continue
            operation = item[method]
            fingerprints[method] = None
            headers = {}
            for parameter in item.get("parameters", []) + operation.get("parameters", []):
                parameter = resolve(document, parameter)
                if parameter["in"] == "header":
                    headers[parameter["name"].lower()] = parameter.get("required", False)
            if not headers.get(KEY):
                continue
            if not any(
                name.lower() == REPLAYED
                for status, response in operation.get("responses", {}).items()
                if status.startswith("2")
                for name in resolve(document, response).get("headers", {})
            ):
                continue
            fingerprints[method] = set(headers) - {KEY, "content-type"}
        routes[route_pattern(path)] = path, fingerprints
    return routes


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("base", type=Path)
    parser.add_argument("candidate", type=Path)
    args = parser.parse_args()
    try:
        before = document_routes(json.loads(args.base.read_text(encoding="utf-8")))
        after = document_routes(json.loads(args.candidate.read_text(encoding="utf-8")))
    except (OSError, ValueError, LookupError, TypeError) as exc:
        print(f"openapi-idempotency: {exc}", file=sys.stderr)
        return 2

    result = 0
    added_routes = sorted(after.keys() - before.keys(), key=lambda route: after[route][0])
    before_priorities = {route: route_priority(route) for route in before}
    for route, (path, before_operations) in before.items():
        protected = [method for method, headers in before_operations.items() if headers is not None]
        if not protected:
            continue
        priority = before_priorities[route]
        existing_shadows = [previous for previous, rank in before_priorities.items() if rank > priority]
        for added_route in added_routes:
            if route_priority(added_route) <= priority:
                continue
            overlap = intersection(route, added_route)
            if overlap is None or any(intersection(overlap, previous) == overlap for previous in existing_shadows):
                continue
            affected = ", ".join(method.upper() for method in protected)
            print(
                f"openapi-idempotency: {after[added_route][0]} would take precedence over {affected} {path}. "
                "Serve the new route at a URL outside the existing idempotent route.",
                file=sys.stderr,
            )
            result = 1
        after_operations = after.get(route, ("", {}))[1]
        for method in protected:
            old_headers = before_operations[method]
            if method not in after_operations or old_headers == after_operations[method]:
                continue
            new_headers = after_operations[method]
            if new_headers is None:
                change = "removed the idempotent replay contract"
            else:
                added = ", ".join(sorted(new_headers - old_headers)) or "none"
                removed = ", ".join(sorted(old_headers - new_headers)) or "none"
                change = f"changed fingerprint headers (added: {added}; removed: {removed})"
            print(
                f"openapi-idempotency: {method.upper()} {path} {change}. "
                "Serve the changed contract at a versioned URL alongside the old route.",
                file=sys.stderr,
            )
            result = 1
    return result


if __name__ == "__main__":
    raise SystemExit(main())
