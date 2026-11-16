#!/usr/bin/env python3
"""Verify that .railway/railway.ts watchPatterns cover all Docker build context inputs."""

from __future__ import annotations

import argparse
import json
import re
import shlex
import sys
from collections.abc import Sequence
from pathlib import Path

sys.dont_write_bytecode = True

IAC = ".railway/railway.ts"
DOCKERFILE = "build/docker/Dockerfile"
_LITERAL = re.compile(r"[A-Za-z0-9_.-]+(?:/[A-Za-z0-9_.-]+)*")
_FORBIDDEN_CONTEXT_KEYS = (
    "rootDirectory",
    "context",
    "buildContext",
    "additionalContexts",
    "dockerContext",
    "buildCommand",
)


class Refusal(ValueError):
    pass


def literal(path: str) -> bool:
    return bool(_LITERAL.fullmatch(path)) and all(part not in {".", ".."} for part in path.split("/"))


def strip_comments_and_strings(program: str) -> tuple[str, str]:
    tokens = r""""(?:[^"\\]|\\.)*"|'(?:[^'\\]|\\.)*'|`(?:[^`\\]|\\.)*`|//[^\n]*|/\*[\s\S]*?\*/"""
    uncommented = re.sub(tokens, lambda m: " " if m[0].startswith("/") else m[0], program)

    def mask_value(match: re.Match[str]) -> str:
        key = match[0][1:-1]
        if key.isidentifier() and re.match(r"\s*:", uncommented[match.end() :]):
            return key
        return '""'

    return uncommented, re.sub(tokens, mask_value, uncommented)


def policy_watches(root: Path) -> set[str] | None:
    iac_path = root / IAC
    if not iac_path.is_file():
        raise Refusal(f"{IAC}: missing Railway IaC configuration")
    uncommented, masked = strip_comments_and_strings(iac_path.read_text(encoding="utf-8"))

    if "..." in masked or re.search(r"\[[^\]]+\]\s*:", masked):
        raise Refusal(f"{IAC}: unsupported computed/spread IaC policy; use explicit build settings")

    for key in _FORBIDDEN_CONTEXT_KEYS:
        if re.search(r"[{,]\s*" + key + r"\s*[:,}]", masked):
            raise Refusal(f"{IAC}: unsupported {key}; coverage requires the repository root build context")

    has_build = any(
        re.search(r"[{,]\s*" + key + r"\s*[:,}]", masked)
        for key in ("build", "builder", "dockerfilePath", "watchPatterns")
    )
    if not has_build:
        if re.search(r"\bsource\s*:\s*image\s*\([^)]+\)", masked) and not re.search(r"\bgithub\s*\(", masked):
            return None
        raise Refusal(f"{IAC}: expected explicit build.watchPatterns or a published-image service")

    for key, expected in (("builder", "DOCKERFILE"), ("dockerfilePath", DOCKERFILE)):
        matches = re.findall(r"\b" + key + r'\s*:\s*"([^"\n]+)"', uncommented)
        if len(matches) != 1 or matches[0] != expected:
            raise Refusal(f"{IAC}: build.{key} must appear once and equal {expected!r}")

    patterns = re.findall(r"\bwatchPatterns\s*:\s*\[([^\]]*)\]", uncommented, re.S)
    if len(patterns) != 1:
        raise Refusal(f"{IAC}: expected exactly one literal watchPatterns array")

    try:
        values = json.loads("[" + re.sub(r",\s*$", "", patterns[0]) + "]")
    except json.JSONDecodeError as error:
        raise Refusal(f"{IAC}: watchPatterns must be a literal string array; {error.msg}")

    if not values or not all(isinstance(value, str) for value in values) or len(set(values)) != len(values):
        raise Refusal(f"{IAC}: watchPatterns must be non-empty, unique literal strings")

    for value in values:
        base = value[:-3] if value.endswith("/**") else value
        if not literal(base):
            raise Refusal(f"{IAC}: unsupported watch syntax {value!r}; use a literal file or directory/**")

    return set(values)


def input_families(root: Path) -> set[str]:
    rules = [
        (number, line.strip())
        for number, line in enumerate((root / ".dockerignore").read_text(encoding="utf-8").splitlines(), 1)
        if line.strip() and not line.startswith("#")
    ]
    if not rules or rules[0][1] != "*":
        raise Refusal(".dockerignore: supported coverage starts with '*' and explicit literal !file or !directory/ inclusions")
    inputs = {DOCKERFILE, ".dockerignore"}
    if (root / "rust-toolchain.toml").is_file():
        inputs.add("rust-toolchain.toml")
    for number, rule in rules[1:]:
        if not rule.startswith("!"):
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


def remote(arguments: list[str]) -> bool:
    operands = [argument for argument in arguments if not argument.startswith("--")]
    return len(operands) > 1 and all(re.match(r"https?://", source) for source in operands[:-1])


def check_dockerfile(root: Path) -> None:
    override = root / (DOCKERFILE + ".dockerignore")
    if override.exists() or override.is_symlink():
        raise Refusal(
            f"{DOCKERFILE}.dockerignore: Dockerfile-specific ignore overrides are unsupported; use the root .dockerignore"
        )
    text = (root / DOCKERFILE).read_text(encoding="utf-8")
    if re.search(r"^[ \t]*#\s*escape\s*=", text, re.M | re.I):
        raise Refusal(f"{DOCKERFILE}: unsupported escape directive")
    stripped = re.sub(r"(?m)^[ \t]*#.*(?:\r?\n|$)", "", text)
    stages: set[str] = set()
    stage_count = 0
    for line in re.sub(r"\\[ \t]*\r?\n", " ", stripped).splitlines():
        if not line.strip():
            continue
        try:
            parts = shlex.split(line)
        except ValueError as error:
            raise Refusal(f"{DOCKERFILE}: unsupported instruction {line.strip()!r}: {error}")
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
            elif operation == "ADD" and remote(parts[1:]):
                continue
            elif operation != "COPY" or len(parts) < 3 or any(
                source != "." and not literal(source.rstrip("/")) for source in parts[1:-1]
            ):
                raise Refusal(f"{DOCKERFILE}: unsupported context instruction {line.strip()!r}")
        elif operation == "RUN" and "--mount=" in line:
            mounts = [part.removeprefix("--mount=") for part in parts[1:] if part.startswith("--mount=")]
            if not mounts or any(
                [field for field in mount.split(",") if field.startswith("type=")] != ["type=secret"]
                for mount in mounts
            ):
                raise Refusal(f"{DOCKERFILE}: unsupported RUN mount context")


def check(root: Path) -> str:
    watches = policy_watches(root)
    if watches is None:
        return f"{IAC} deploys a published image; Railway builds no repository context"
    check_dockerfile(root)
    for family in sorted(input_families(root)):
        path = family[:-3] if family.endswith("/**") else family
        covered = family in watches or any(
            watch.endswith("/**") and path.startswith(watch[:-3] + "/") for watch in watches
        )
        if not covered:
            raise Refusal(f"{family}: image input is uncovered; add it to build.watchPatterns in {IAC}")
    return f"{IAC} covers admitted image input families"


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", type=Path, default=Path(__file__).resolve().parents[2])
    args = parser.parse_args(argv)
    try:
        verdict = check(args.repo)
    except (OSError, ValueError) as error:
        print(f"image-inputs-check: {error}", file=sys.stderr)
        return 1
    print(f"image-inputs-check: {verdict}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
