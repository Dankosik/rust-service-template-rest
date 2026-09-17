#!/usr/bin/env python3
"""Structural check for the canonical skills under .agents/skills.

Proves shape, not model behavior: frontmatter fields, name/directory
agreement, the machine contract (invocation and kind), the body word budget
from docs/skill-authoring.md, and that relative links resolve. Standard
library only, so it runs anywhere CI has Python 3.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SKILLS = ROOT / ".agents" / "skills"
INVOCATIONS = {"model", "user", "role"}
KINDS = {"method", "workflow", "carrier"}
MIN_WORDS, MAX_WORDS = 100, 600
LINK = re.compile(r"\[[^\]]*\]\(([^)\s]+)\)")
HEADING = re.compile(r"^#+\s+(.*)$", re.M)


def parse_frontmatter(text: str) -> tuple[dict, str]:
    if not text.startswith("---\n"):
        raise ValueError("missing frontmatter")
    end = text.find("\n---\n", 4)
    if end < 0:
        raise ValueError("unterminated frontmatter")
    fields: dict = {}
    current: dict | None = None
    for line in text[4:end].splitlines():
        if not line.strip():
            continue
        if line.startswith("  "):
            if current is None:
                raise ValueError(f"indented line without a parent: {line!r}")
            key, _, value = line.strip().partition(":")
            current[key.strip()] = value.strip().strip('"')
            continue
        key, _, value = line.partition(":")
        value = value.strip()
        if value:
            fields[key.strip()] = value.strip('"')
            current = None
        else:
            current = fields.setdefault(key.strip(), {})
    return fields, text[end + 5 :]


def anchors(path: Path) -> set[str]:
    return {
        re.sub(r"[^\w\- ]", "", h.strip().lower()).replace(" ", "-")
        for h in HEADING.findall(path.read_text())
    }


def check(skill_dir: Path) -> list[str]:
    problems: list[str] = []
    skill = skill_dir / "SKILL.md"
    if not skill.is_file():
        return [f"{skill_dir.name}: missing SKILL.md"]
    text = skill.read_text()
    try:
        fields, body = parse_frontmatter(text)
    except ValueError as err:
        return [f"{skill_dir.name}: {err}"]

    if fields.get("name") != skill_dir.name:
        problems.append(f"{skill_dir.name}: frontmatter name {fields.get('name')!r} != directory")
    description = fields.get("description", "")
    if not description:
        problems.append(f"{skill_dir.name}: empty description")
    elif not description.startswith("Use "):
        problems.append(f"{skill_dir.name}: description must state its trigger, starting with 'Use'")
    elif len(re.findall(r"[.!?](\s|$)", description)) > 2:
        problems.append(f"{skill_dir.name}: description longer than two sentences")

    metadata = fields.get("metadata")
    if not isinstance(metadata, dict):
        problems.append(f"{skill_dir.name}: missing metadata block")
    else:
        if metadata.get("invocation") not in INVOCATIONS:
            problems.append(f"{skill_dir.name}: metadata.invocation must be one of {sorted(INVOCATIONS)}")
        if metadata.get("kind") not in KINDS:
            problems.append(f"{skill_dir.name}: metadata.kind must be one of {sorted(KINDS)}")

    words = len(body.split())
    if not MIN_WORDS <= words <= MAX_WORDS:
        problems.append(f"{skill_dir.name}: body has {words} words, budget is {MIN_WORDS}-{MAX_WORDS}")
    if not HEADING.search(body):
        problems.append(f"{skill_dir.name}: body has no heading")

    for link in LINK.findall(body):
        if link.startswith(("http://", "https://", "mailto:")):
            continue
        target_path, _, anchor = link.partition("#")
        target = (skill.parent / target_path).resolve() if target_path else skill
        if not target.exists():
            problems.append(f"{skill_dir.name}: broken link {link}")
        elif anchor and target.suffix == ".md" and anchor not in anchors(target):
            problems.append(f"{skill_dir.name}: missing anchor in link {link}")

    for stray in skill_dir.iterdir():
        if stray.name not in {"SKILL.md", "references", "LICENSE"}:
            problems.append(f"{skill_dir.name}: unexpected file {stray.name}")
    return problems


def main() -> int:
    if not SKILLS.is_dir():
        print(f"no skills directory at {SKILLS}", file=sys.stderr)
        return 1
    dirs = sorted(p for p in SKILLS.iterdir() if p.is_dir())
    problems = [problem for skill_dir in dirs for problem in check(skill_dir)]
    for problem in problems:
        print(problem, file=sys.stderr)
    if problems:
        return 1
    print(f"{len(dirs)} skills ok")
    return 0


if __name__ == "__main__":
    sys.exit(main())
