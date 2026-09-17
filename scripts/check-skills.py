#!/usr/bin/env python3
"""Structural check for the skills under .agents/skills.

Enforces the rust-cli-skills shape: frontmatter with exactly `name` and
`description`, a description that states its trigger in at most two
sentences, one heading, prose paragraphs without links or lists, an opening
bold concept, a body inside the word budget, and a LICENSE beside SKILL.md.
Proves shape, not model behavior. Standard library only.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SKILLS = ROOT / ".agents" / "skills"
ROOT_LICENSE = ROOT / "LICENSE"
MIN_WORDS, MAX_WORDS = 250, 500
ALLOWED_FILES = {"SKILL.md", "LICENSE"}
FRONTMATTER_KEYS = {"name", "description"}
NAME = re.compile(r"^[a-z0-9]+(-[a-z0-9]+)*$")


def parse_frontmatter(text: str) -> tuple[dict[str, str], str]:
    if not text.startswith("---\n"):
        raise ValueError("missing frontmatter")
    end = text.find("\n---\n", 4)
    if end < 0:
        raise ValueError("unterminated frontmatter")
    fields: dict[str, str] = {}
    for line in text[4:end].splitlines():
        if not line.strip():
            continue
        key, sep, value = line.partition(":")
        if not sep or line.startswith((" ", "\t")):
            raise ValueError(f"frontmatter must be flat `key: value` lines, got {line!r}")
        fields[key.strip()] = value.strip().strip('"')
    return fields, text[end + 5 :]


def check(skill_dir: Path) -> list[str]:
    name = skill_dir.name
    problems: list[str] = []
    skill = skill_dir / "SKILL.md"
    if not skill.is_file():
        return [f"{name}: missing SKILL.md"]
    try:
        fields, body = parse_frontmatter(skill.read_text())
    except ValueError as err:
        return [f"{name}: {err}"]

    if set(fields) != FRONTMATTER_KEYS:
        problems.append(f"{name}: frontmatter keys must be exactly {sorted(FRONTMATTER_KEYS)}, got {sorted(fields)}")
    if fields.get("name") != name or not NAME.match(name):
        problems.append(f"{name}: frontmatter name must equal the lowercase-hyphen directory name")
    description = fields.get("description", "")
    if not description:
        problems.append(f"{name}: empty description")
    else:
        if "Use " not in description:
            problems.append(f"{name}: description must state its trigger with `Use when`, `Use for`, or `Use to`")
        if len(re.findall(r"[.!?](\s|$)", description)) > 2:
            problems.append(f"{name}: description longer than two sentences")
        if len(description) > 1024:
            problems.append(f"{name}: description longer than 1024 characters")

    paragraphs = [p.strip() for p in re.split(r"\n\s*\n", body.strip()) if p.strip()]
    headings = [p for p in paragraphs if p.startswith("#")]
    if len(headings) != 1 or not paragraphs or not paragraphs[0].startswith("# "):
        problems.append(f"{name}: body must start with one H1 heading and contain no other headings")
    prose = paragraphs[1:] if paragraphs and paragraphs[0].startswith("# ") else paragraphs
    if not prose or not prose[0].startswith("**"):
        problems.append(f"{name}: first paragraph must open with the bold leading concept")
    if len(prose) < 4:
        problems.append(f"{name}: body has {len(prose)} paragraphs, expected at least 4")
    for paragraph in prose:
        if re.search(r"\]\(", paragraph):
            problems.append(f"{name}: body contains a link; skills are self-contained prose")
            break
        if re.match(r"^\s*([-*]|\d+\.)\s", paragraph, re.M):
            problems.append(f"{name}: body contains a list; write paragraphs")
            break
        if paragraph.startswith("```"):
            problems.append(f"{name}: body contains a code block; name APIs in prose")
            break
    words = len(" ".join(prose).split())
    if not MIN_WORDS <= words <= MAX_WORDS:
        problems.append(f"{name}: body has {words} words, budget is {MIN_WORDS}-{MAX_WORDS}")

    license_file = skill_dir / "LICENSE"
    if not license_file.is_file():
        problems.append(f"{name}: missing LICENSE beside SKILL.md")
    elif ROOT_LICENSE.is_file() and license_file.read_bytes() != ROOT_LICENSE.read_bytes():
        problems.append(f"{name}: LICENSE differs from the repository LICENSE")
    for stray in skill_dir.iterdir():
        if stray.name not in ALLOWED_FILES:
            problems.append(f"{name}: unexpected entry {stray.name}; a skill is SKILL.md and LICENSE only")
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
