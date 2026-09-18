#!/usr/bin/env python3
"""Structural check for the skills under .agents/skills.

Two classes share the directory (docs/skill-authoring.md):

- decision skills (`rust-*`): the rust-cli-skills shape; frontmatter with
  exactly `name` and `description`, a description that states its trigger in
  at most two sentences, one heading, prose paragraphs without links or
  lists, an opening bold concept, a body inside the word budget, and a
  LICENSE beside SKILL.md;
- workflow skills (ported from the Go template, harness-neutral): the same
  name and description rules plus `metadata.invocation` (`role` or `user`),
  `metadata.kind` (`carrier` or `workflow`), and
  `disable-model-invocation: true`; the body is a short carrier that may link
  to the workflow documents and hold lists; `references/*.md` and the Codex
  carrier `agents/openai.yaml` are the only other entries.

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
WORKFLOW_MAX_WORDS = 600
DECISION_KEYS = {"name", "description"}
WORKFLOW_KEYS = {"name", "description", "metadata", "disable-model-invocation"}
WORKFLOW_METADATA = {"invocation": {"role", "user"}, "kind": {"carrier", "workflow"}}
NAME = re.compile(r"^[a-z0-9]+(-[a-z0-9]+)*$")


def parse_frontmatter(text: str) -> tuple[dict[str, object], str]:
    """Flat `key: value` lines, plus one level of indented keys under `metadata`."""
    if not text.startswith("---\n"):
        raise ValueError("missing frontmatter")
    end = text.find("\n---\n", 4)
    if end < 0:
        raise ValueError("unterminated frontmatter")
    fields: dict[str, object] = {}
    parent: str | None = None
    for line in text[4:end].splitlines():
        if not line.strip():
            continue
        indented = line.startswith((" ", "\t"))
        key, sep, value = line.strip().partition(":")
        if not sep:
            raise ValueError(f"frontmatter line is not `key: value`: {line!r}")
        key, value = key.strip(), value.strip().strip('"')
        if indented:
            if parent != "metadata":
                raise ValueError(f"only `metadata` may hold nested keys, got {line!r}")
            fields["metadata"][key] = value  # type: ignore[index]
            continue
        if value:
            fields[key] = value
            parent = None
        else:
            fields[key] = {}
            parent = key
    return fields, text[end + 5 :]


def check_description(name: str, fields: dict[str, object]) -> list[str]:
    problems: list[str] = []
    if fields.get("name") != name or not NAME.match(name):
        problems.append(f"{name}: frontmatter name must equal the lowercase-hyphen directory name")
    description = str(fields.get("description", ""))
    if not description:
        problems.append(f"{name}: empty description")
        return problems
    if "Use " not in description:
        problems.append(f"{name}: description must state its trigger with `Use when`, `Use for`, `Use to`, or `Use only`")
    if len(re.findall(r"[.!?](\s|$)", description)) > 2:
        problems.append(f"{name}: description longer than two sentences")
    if len(description) > 1024:
        problems.append(f"{name}: description longer than 1024 characters")
    return problems


def check_decision_body(name: str, body: str) -> list[str]:
    problems: list[str] = []
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
            problems.append(f"{name}: body contains a link; decision skills are self-contained prose")
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
    return problems


def check_workflow_body(name: str, body: str) -> list[str]:
    problems: list[str] = []
    paragraphs = [p.strip() for p in re.split(r"\n\s*\n", body.strip()) if p.strip()]
    if not paragraphs or not paragraphs[0].startswith("# "):
        problems.append(f"{name}: body must start with one H1 heading")
    if sum(1 for p in paragraphs if p.startswith("# ")) != 1:
        problems.append(f"{name}: body must have exactly one H1 heading")
    words = len(" ".join(paragraphs[1:]).split())
    if not 0 < words <= WORKFLOW_MAX_WORDS:
        problems.append(f"{name}: body has {words} words, workflow budget is 1-{WORKFLOW_MAX_WORDS}")
    return problems


def check_entries(name: str, skill_dir: Path, workflow: bool) -> list[str]:
    problems: list[str] = []
    license_file = skill_dir / "LICENSE"
    if not license_file.is_file():
        problems.append(f"{name}: missing LICENSE beside SKILL.md")
    elif ROOT_LICENSE.is_file() and license_file.read_bytes() != ROOT_LICENSE.read_bytes():
        problems.append(f"{name}: LICENSE differs from the repository LICENSE")
    for entry in sorted(skill_dir.iterdir()):
        if entry.name in {"SKILL.md", "LICENSE"}:
            continue
        if workflow and entry.name == "references" and entry.is_dir():
            for stray in entry.rglob("*"):
                if stray.is_file() and stray.suffix != ".md":
                    problems.append(f"{name}: references/ holds {stray.relative_to(skill_dir)}; only Markdown belongs there")
            continue
        if workflow and entry.name == "agents" and entry.is_dir():
            if sorted(p.name for p in entry.iterdir()) != ["openai.yaml"]:
                problems.append(f"{name}: agents/ may hold only the Codex carrier openai.yaml")
            continue
        allowed = "SKILL.md, LICENSE, references/*.md, agents/openai.yaml" if workflow else "SKILL.md and LICENSE"
        problems.append(f"{name}: unexpected entry {entry.name}; a skill is {allowed} only")
    return problems


def check(skill_dir: Path) -> list[str]:
    name = skill_dir.name
    skill = skill_dir / "SKILL.md"
    if not skill.is_file():
        return [f"{name}: missing SKILL.md"]
    try:
        fields, body = parse_frontmatter(skill.read_text())
    except ValueError as err:
        return [f"{name}: {err}"]

    workflow = "metadata" in fields
    problems = check_description(name, fields)
    if workflow:
        if set(fields) != WORKFLOW_KEYS:
            problems.append(f"{name}: workflow frontmatter keys must be exactly {sorted(WORKFLOW_KEYS)}, got {sorted(fields)}")
        metadata = fields.get("metadata")
        if not isinstance(metadata, dict) or set(metadata) != set(WORKFLOW_METADATA):
            problems.append(f"{name}: metadata must hold exactly {sorted(WORKFLOW_METADATA)}")
        else:
            for key, allowed in WORKFLOW_METADATA.items():
                if metadata.get(key) not in allowed:
                    problems.append(f"{name}: metadata.{key} must be one of {sorted(allowed)}, got {metadata.get(key)!r}")
        if fields.get("disable-model-invocation") != "true":
            problems.append(f"{name}: workflow skills set `disable-model-invocation: true`")
        problems += check_workflow_body(name, body)
    else:
        if set(fields) != DECISION_KEYS:
            problems.append(f"{name}: frontmatter keys must be exactly {sorted(DECISION_KEYS)}, got {sorted(fields)}")
        problems += check_decision_body(name, body)
    problems += check_entries(name, skill_dir, workflow)
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
    workflow = sum(1 for d in dirs if "metadata:" in (d / "SKILL.md").read_text().split("\n---\n", 1)[0])
    print(f"{len(dirs)} skills ok ({len(dirs) - workflow} decision, {workflow} workflow)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
