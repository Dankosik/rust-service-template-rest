#!/usr/bin/env python3
"""Admit the documented root-context image watch policy, without executing IaC."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import re
import shlex
import sys

POLICY = "docs/railway-deployment-profile.md"
DOCKERFILE = "build/docker/Dockerfile"
_LITERAL = re.compile(r"[A-Za-z0-9_.-]+(?:/[A-Za-z0-9_.-]+)*")


class Refusal(ValueError):
    pass


def literal(path: str) -> bool:
    return bool(_LITERAL.fullmatch(path)) and all(part not in {".", ".."} for part in path.split("/"))


def one(values: list[str], owner: str) -> str:
    if len(values) != 1:
        raise Refusal(f"{owner}: expected exactly one literal policy form, found {len(values)}")
    return values[0]


def watch_set(values: list[str], owner: str) -> set[str]:
    for value in values:
        base = value[:-3] if value.endswith("/**") else value
        if not literal(base):
            raise Refusal(f"{owner}: unsupported watch syntax {value!r}; use a literal file or directory/**")
    if not values or len(set(values)) != len(values):
        raise Refusal(f"{owner}: empty or duplicate watch patterns")
    return set(values)


def policy_watches(text: str) -> set[str]:
    def table(key: str) -> str:
        return one(re.findall(r"^\| `" + re.escape(key) + r"` \| ([^|]+) \|", text, re.M), f"{POLICY} table {key}")

    table_patterns = table("build.watchPatterns")
    if not re.fullmatch(r"\s*`[^`]+`(?:\s*,\s*`[^`]+`)*\s*", table_patterns):
        raise Refusal(f"{POLICY}: watch table must contain only comma-separated literal patterns")
    table_set = watch_set(re.findall(r"`([^`]+)`", table_patterns), f"{POLICY} table")
    snippet = one([block for block in re.findall(r"```ts\n(.*?)\n```", text, re.S) if "watchPatterns" in block], f"{POLICY} IaC")
    # Comments are explanatory prose, never configuration. Strings are admitted below.
    snippet = re.sub(r"//[^\n]*", "", snippet)
    if "..." in snippet or re.search(r"\[[^\]]+\]\s*:", snippet):
        raise Refusal(f"{POLICY}: unsupported computed/spread IaC policy; use explicit build settings")
    for key in ("builder", "dockerfilePath", "watchPatterns"):
        if len(re.findall(r"\b" + key + r"\s*:", snippet)) != 1:
            raise Refusal(f"{POLICY}: expected exactly one IaC {key} setting")
    for key in ("rootDirectory", "context", "buildContext", "additionalContexts", "dockerContext", "buildCommand"):
        if re.search(r"\b" + key + r"\s*:", snippet) or re.search(r"^\|[^|]*\b" + key + r"\b", text, re.M):
            raise Refusal(f"{POLICY}: unsupported {key}; coverage requires the repository root build context")
    for key, expected in (("builder", "DOCKERFILE"), ("dockerfilePath", DOCKERFILE)):
        row = table("build." + key).strip()
        actual = one(re.findall(r"\b" + key + r'\s*:\s*"([^"\n]+)"\s*,', snippet), f"{POLICY} IaC {key}")
        if row != f"`{expected}`" or actual != expected:
            raise Refusal(f"{POLICY}: build.{key} must select {expected!r} in both forms")
    patterns = one(re.findall(r"\bwatchPatterns\s*:\s*\[([^\]]*)\]", snippet, re.S), f"{POLICY} IaC watchPatterns")
    # JSON strings plus an optional trailing comma are the supported data subset.
    try:
        values = json.loads("[" + re.sub(r",\s*$", "", patterns) + "]")
    except json.JSONDecodeError as error:
        raise Refusal(f"{POLICY}: watchPatterns must be a literal string array; {error.msg}") from error
    if not all(isinstance(value, str) for value in values):
        raise Refusal(f"{POLICY}: watchPatterns must contain literal strings only")
    iac_set = watch_set(values, f"{POLICY} IaC")
    if table_set != iac_set:
        raise Refusal(f"{POLICY}: watch forms disagree; table-only={sorted(table_set - iac_set)}, IaC-only={sorted(iac_set - table_set)}")
    return table_set


def input_families(root: Path) -> set[str]:
    rules = [(number, line.strip()) for number, line in enumerate((root / ".dockerignore").read_text().splitlines(), 1)
             if line.strip() and not line.startswith("#")]
    if not rules or rules[0][1] != "*":
        raise Refusal(".dockerignore: supported coverage starts with '*' and explicit literal !file or !directory/ inclusions")
    inputs = {DOCKERFILE, ".dockerignore"}
    for number, rule in rules[1:]:
        if not rule.startswith("!"):
            # Exclusions only narrow the conservative obligations from inclusions.
            continue
        path = rule[1:]
        directory = path.endswith("/")
        path = path[:-1] if directory else path
        if not literal(path):
            raise Refusal(f".dockerignore:{number}: unsupported inclusion {rule!r}; use literal !file or !directory/")
        if not directory and (root / path).is_dir():
            raise Refusal(f".dockerignore:{number}: ambiguous directory {rule!r}; spell it !{path}/")
        inputs.add(path + "/**" if directory else path)
    return inputs


def check_dockerfile(root: Path) -> None:
    override = root / (DOCKERFILE + ".dockerignore")
    if override.exists() or override.is_symlink():
        raise Refusal(f"{DOCKERFILE}.dockerignore: Dockerfile-specific ignore overrides are unsupported; use the root .dockerignore")
    text = (root / DOCKERFILE).read_text()
    if re.search(r"^#\s*escape\s*=", text, re.M | re.I):
        raise Refusal(f"{DOCKERFILE}: unsupported escape directive; coverage uses the current backslash continuation grammar")
    # Only current local context COPY and previously declared stages are supported.
    # External build contexts must not be mistaken for an already covered stage.
    stages: set[str] = set()
    stage_count = 0
    for line in re.sub(r"\\\n", " ", text).splitlines():
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        try:
            parts = shlex.split(line)
        except ValueError as error:
            raise Refusal(f"{DOCKERFILE}: unsupported instruction {line.strip()!r}: {error}") from error
        operation = parts[0].upper()
        if operation == "FROM":
            if len(parts) >= 4 and parts[-2].upper() == "AS":
                stages.add(parts[-1])
            stage_count += 1
        elif operation in {"COPY", "ADD"}:
            if len(parts) > 1 and parts[1].startswith("--from="):
                source = parts[1].split("=", 1)[1]
                if source not in stages and not (source.isdigit() and int(source) < stage_count - 1):
                    raise Refusal(f"{DOCKERFILE}: unsupported external/named COPY context {source!r}")
            elif operation != "COPY" or len(parts) < 3 or any(source != "." and not literal(source.rstrip("/")) for source in parts[1:-1]):
                raise Refusal(f"{DOCKERFILE}: unsupported context instruction {line.strip()!r}; admit its inputs before changing {POLICY}")
        elif operation == "RUN" and "--mount=" in line:
            raise Refusal(f"{DOCKERFILE}: unsupported RUN mount context; admit its inputs before changing {POLICY}")


def check(root: Path) -> None:
    watches = policy_watches((root / POLICY).read_text())
    check_dockerfile(root)
    for family in sorted(input_families(root)):
        path = family[:-3] if family.endswith("/**") else family
        covered = family in watches or any(watch.endswith("/**") and path.startswith(watch[:-3] + "/") for watch in watches)
        if not covered:
            raise Refusal(f"{family}: image input is uncovered; add it to both build.watchPatterns forms in {POLICY}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", type=Path, default=Path(__file__).resolve().parents[2])
    args = parser.parse_args()
    try:
        check(args.repo)
    except (OSError, ValueError) as error:
        print(f"image-inputs-check: {error}", file=sys.stderr)
        return 1
    print("image-inputs-check: documented source deployment covers admitted image input families")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
