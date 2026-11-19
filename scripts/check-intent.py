#!/usr/bin/env python3
"""Static structural verifier for Intent V1 (specs/<task>/intent.md)."""

from __future__ import annotations

import argparse
import re
import sys
from dataclasses import dataclass, field
from pathlib import Path

sys.dont_write_bytecode = True

VALID_STATUSES = frozenset({"draft", "ready"})
VALID_BINDINGS = frozenset({"hard", "default"})
REQUIRED_SECTIONS = (
    "Problem",
    "Desired outcome",
    "Affected actors and surfaces",
    "Constraints & boundaries",
    "Success signal",
)
OPTIONAL_SECTIONS = (
    "Settled decisions",
    "Concrete scenarios",
    "Preferences & delegated choices",
    "Assumptions & reopen triggers",
    "Terminology",
    "Open questions",
)
PLACEHOLDER_VALUES = frozenset({"n/a", "na", "tbd", "todo", "none", "tba", "..."})
REQUESTER_OWNER_PHRASES = frozenset(
    {"user", "end user", "requester", "product", "product owner", "human", "pm", "owner"}
)

TITLE_RE = re.compile(r"^#\s+Intent:\s*(.+?)\s*$")
H2_RE = re.compile(r"^##\s+(.+?)\s*$")
H3_DECISION_RE = re.compile(r"^###\s+(I\d+)\b(?:\s*(?:\u2014|--|-)\s*(.+?))?\s*$")
STATUS_RE = re.compile(r"^status:\s*(\S+)\s*$")
FIELD_RE = re.compile(
    r"^(?:-\s+)?\*\*(Binding|Decision|Rejected|Why|Status):?\*\*:?\s*(.*)$",
    re.IGNORECASE,
)
ASSUMPTION_RE = re.compile(r"^\*\*(A\d+)(?:\s*(?:\u2014|--|-)\s*[^*]+?)?:?\*\*:?\s*(.*)$")
REOPEN_IF_RE = re.compile(r"(?:\*\*)?Reopen if(?:\*\*)?:(?:\*\*)?\s*(.+)", re.IGNORECASE)
SUPERSEDED_LINE_RE = re.compile(
    r"^(?:-\s+)?(?:\*\*(?:Status|Decision):?\*\*:?\s*)?superseded\s+by\s+(I\d+)\b",
    re.IGNORECASE,
)
_OQ_STOP = r"(?=\s*(?:\||(?:\*\*)?(?:Owner|Fallback|Reopen if)(?:\*\*)?:)|\s*$)"
OWNER_FIELD_RE = re.compile(
    rf"(?:\*\*)?Owner(?:\*\*)?:(?:\*\*)?\s*(.*?){_OQ_STOP}", re.IGNORECASE
)
FALLBACK_FIELD_RE = re.compile(
    rf"(?:\*\*)?Fallback(?:\*\*)?:(?:\*\*)?\s*(.*?){_OQ_STOP}", re.IGNORECASE
)
OQ_REOPEN_IF_RE = re.compile(
    rf"(?:\*\*)?Reopen if(?:\*\*)?:(?:\*\*)?\s*(.*?){_OQ_STOP}", re.IGNORECASE
)
TEMPLATE_PLACEHOLDER_RE = re.compile(r"^\s*<[^<>]+>\s*$")
MD_LINK_RE = re.compile(r"\[[^\]]*\]\(([^)]+)\)")
BACKTICK_REPO_PATH_RE = re.compile(
    r"`((?:crates|api|docs|migrations|scripts|template|quality|build|config)/[A-Za-z0-9._/\-]+(?::\d+)?|Cargo\.toml|Makefile)`"
)


@dataclass
class IntentDecision:
    decision_id: str
    title: str
    binding: str | None
    decision: str
    rejected: str
    why: str
    superseded_by: str | None = None


@dataclass
class IntentReport:
    intent_path: Path
    status: str | None
    decisions: list[IntentDecision] = field(default_factory=list)
    assumptions_count: int = 0
    scenarios_count: int = 0
    open_questions_count: int = 0
    has_remaining_frontier: bool = False
    errors: list[str] = field(default_factory=list)
    warnings: list[str] = field(default_factory=list)


def _resolve_intent_path(target: Path) -> Path:
    if target.is_file():
        return target
    if target.suffix == ".md":
        return target
    return target / "intent.md"


def _find_repo_root(start: Path) -> Path | None:
    current = start.resolve()
    if current.is_file():
        current = current.parent
    for candidate in (current, *current.parents):
        if (candidate / ".git").exists() or (
            (candidate / "Cargo.toml").is_file() and (candidate / "AGENTS.md").is_file()
        ):
            return candidate
    return None


def _is_placeholder(text: str) -> bool:
    cleaned = text.strip(" \t\r\n`*_.-\u2014")
    if not cleaned:
        return True
    if cleaned.lower() in PLACEHOLDER_VALUES:
        return True
    return bool(TEMPLATE_PLACEHOLDER_RE.fullmatch(cleaned))


def _section_is_empty_or_placeholder(lines: list[str]) -> bool:
    items = _strip_bullet_items(lines)
    if not items:
        return True
    return all(_is_placeholder(item) for item in items)


def _strip_bullet_items(lines: list[str]) -> list[str]:
    items: list[str] = []
    in_fence = False
    base_indent: int | None = None
    for raw in lines:
        stripped = raw.strip()
        if stripped.startswith("```"):
            in_fence = not in_fence
            continue
        if in_fence or not stripped:
            continue
        indent = len(raw) - len(raw.lstrip(" \t"))
        if stripped.startswith("- "):
            if base_indent is None or indent <= base_indent:
                base_indent = indent
                items.append(stripped[2:].strip())
            elif items:
                items[-1] = f"{items[-1]} {stripped}"
            else:
                items.append(stripped[2:].strip())
        elif items:
            items[-1] = f"{items[-1]} {stripped}"
        else:
            items.append(stripped)
    return items


def _parse_sections(raw_text: str) -> tuple[str | None, str | None, dict[str, list[str]]]:
    title: str | None = None
    status: str | None = None
    sections: dict[str, list[str]] = {}
    current_section: str | None = None
    in_fence = False

    for line in raw_text.splitlines():
        stripped = line.strip()
        if stripped.startswith("```"):
            in_fence = not in_fence
            if current_section is not None:
                sections[current_section].append(line)
            continue

        if not in_fence and current_section is None:
            if title is None:
                title_match = TITLE_RE.match(stripped)
                if title_match:
                    title = title_match.group(1).strip()
                    continue
            if status is None:
                status_match = STATUS_RE.match(stripped)
                if status_match:
                    status = status_match.group(1).strip().lower()
                    continue

        if not in_fence:
            h2_match = H2_RE.match(stripped)
            if h2_match:
                current_section = h2_match.group(1).strip()
                sections.setdefault(current_section, [])
                continue

        if current_section is not None:
            sections[current_section].append(line)

    return title, status, sections


def _parse_settled_decisions(
    lines: list[str], errors: list[str]
) -> list[IntentDecision]:
    decisions: list[IntentDecision] = []
    blocks: list[tuple[str, str, list[str]]] = []
    in_fence = False

    for line in lines:
        stripped = line.strip()
        if stripped.startswith("```"):
            in_fence = not in_fence
            if blocks:
                blocks[-1][2].append(line)
            continue
        if not in_fence:
            h3_match = H3_DECISION_RE.match(stripped)
            if h3_match:
                blocks.append((h3_match.group(1).upper(), (h3_match.group(2) or "").strip(), []))
                continue
        if blocks:
            blocks[-1][2].append(line)

    if not blocks:
        errors.append(
            "Settled decisions: section is present but contains no '### I<n> — <topic>' entries"
        )
        return decisions

    seen_ids: set[str] = set()
    for dec_id, dec_title, body_lines in blocks:
        if dec_id in seen_ids:
            errors.append(f"Settled decisions: duplicate decision ID {dec_id}")
        seen_ids.add(dec_id)
        if not dec_title or _is_placeholder(dec_title):
            errors.append(f"Settled decisions {dec_id}: missing concise decision title")

        fields: dict[str, str] = {}
        active_field: str | None = None
        body_in_fence = False
        superseded_by: str | None = None
        for raw in body_lines:
            stripped = raw.strip()
            if stripped.startswith("```"):
                body_in_fence = not body_in_fence
                continue
            if body_in_fence or not stripped:
                continue
            field_match = FIELD_RE.match(stripped)
            if field_match:
                key = field_match.group(1).lower()
                fields[key] = field_match.group(2).strip()
                active_field = key
                if key in {"status", "decision"} and superseded_by is None:
                    sup_match = SUPERSEDED_LINE_RE.match(stripped)
                    if sup_match:
                        superseded_by = sup_match.group(1).upper()
            elif active_field is None or (
                not raw.startswith((" ", "\t")) and stripped.startswith("- ")
            ):
                sup_match = SUPERSEDED_LINE_RE.match(stripped)
                if sup_match and superseded_by is None:
                    superseded_by = sup_match.group(1).upper()
                elif active_field is not None:
                    fields[active_field] = f"{fields[active_field]} {stripped}".strip()
            else:
                fields[active_field] = f"{fields[active_field]} {stripped}".strip()

        raw_binding = fields.get("binding", "").strip()
        binding: str | None = None
        if raw_binding:
            first_token = raw_binding.split()[0].strip("`*.,;:").lower()
            if first_token in VALID_BINDINGS:
                binding = first_token
            else:
                errors.append(
                    f"Settled decisions {dec_id}: invalid Binding {raw_binding!r} "
                    f"(expected 'hard' or 'default')"
                )

        dec_text = fields.get("decision", "")
        rej_text = fields.get("rejected", "")
        why_text = fields.get("why", "")

        if superseded_by is None:
            if binding is None and not raw_binding:
                errors.append(
                    f"Settled decisions {dec_id}: missing '**Binding:** hard | default'"
                )
            if _is_placeholder(dec_text):
                errors.append(f"Settled decisions {dec_id}: missing non-empty '**Decision:**'")
            if _is_placeholder(why_text):
                errors.append(f"Settled decisions {dec_id}: missing non-empty '**Why:**'")

        decisions.append(
            IntentDecision(
                decision_id=dec_id,
                title=dec_title,
                binding=binding,
                decision=dec_text,
                rejected=rej_text,
                why=why_text,
                superseded_by=superseded_by,
            )
        )

    for dec in decisions:
        if dec.superseded_by == dec.decision_id:
            errors.append(
                f"Settled decisions {dec.decision_id}: decision cannot supersede itself"
            )
        elif dec.superseded_by and dec.superseded_by not in seen_ids:
            errors.append(
                f"Settled decisions {dec.decision_id}: Superseded by unknown ID {dec.superseded_by}"
            )

    return decisions


def _check_constraints(lines: list[str], errors: list[str]) -> None:
    entries: list[tuple[str, list[str]]] = []
    in_fence = False
    base_indent: int | None = None
    for raw in lines:
        stripped = raw.strip()
        if stripped.startswith("```"):
            in_fence = not in_fence
            continue
        if in_fence or not stripped:
            continue
        indent = len(raw) - len(raw.lstrip(" \t"))
        if stripped.startswith("- "):
            if base_indent is None or indent <= base_indent:
                base_indent = indent
                entries.append((stripped[2:].strip(), []))
            elif entries:
                entries[-1][1].append(stripped[2:].strip())
            else:
                entries.append((stripped[2:].strip(), []))
        elif entries:
            entries[-1][1].append(stripped)
        else:
            entries.append((stripped, []))

    if not entries:
        errors.append("Constraints & boundaries: section is empty")
        return

    has_hard = False
    has_scope = False
    for head, sub_items in entries:
        if ":" not in head:
            continue
        label, _, inline_val = head.partition(":")
        label_lower = label.strip(" \t\r\n`*_").lower()
        has_content = any(not _is_placeholder(val) for val in (inline_val, *sub_items))
        if not has_content:
            continue
        if "hard constraints" in label_lower or "must not" in label_lower:
            has_hard = True
        if (
            "scope & non-goals" in label_lower
            or "scope and non-goals" in label_lower
            or "non-goals" in label_lower
        ):
            has_scope = True

    if not has_hard:
        errors.append(
            "Constraints & boundaries: missing non-empty 'Hard constraints & `Must not`:' entry"
        )
    if not has_scope:
        errors.append(
            "Constraints & boundaries: missing non-empty 'Scope & non-goals:' entry"
        )


def _check_assumptions(lines: list[str], errors: list[str]) -> int:
    items = _strip_bullet_items(lines)
    if not items:
        errors.append(
            "Assumptions & reopen triggers: omit section when empty instead of leaving it blank"
        )
        return 0

    seen_ids: set[str] = set()
    count = 0
    for item in items:
        match = ASSUMPTION_RE.match(item)
        if not match:
            errors.append(
                f"Assumptions & reopen triggers: entry must start with '**A<n> — <name>:**' ({item[:60]!r})"
            )
            continue
        count += 1
        aid = match.group(1).upper()
        if aid in seen_ids:
            errors.append(f"Assumptions & reopen triggers: duplicate assumption ID {aid}")
        seen_ids.add(aid)
        reopen_match = REOPEN_IF_RE.search(item)
        if not reopen_match or _is_placeholder(reopen_match.group(1)):
            errors.append(
                f"Assumptions & reopen triggers {aid}: missing objective 'Reopen if:' trigger"
            )
    return count


def _is_requester_owner(raw: str) -> bool:
    cleaned = re.sub(r"\[([^\]]+)\]\([^)]+\)", r"\1", raw).strip()
    prefix, sep, _ = cleaned.partition("(")
    if sep and prefix.strip(" \t`*_\u2014-"):
        cleaned = prefix
    else:
        cleaned = cleaned.strip("()")
    for part in re.split(r"[/,]", cleaned):
        phrase = " ".join(re.findall(r"[A-Za-z0-9]+", part.lower()))
        if phrase in REQUESTER_OWNER_PHRASES:
            return True
    return False


def _check_open_questions(lines: list[str], status: str | None, errors: list[str]) -> int:
    items = _strip_bullet_items(lines)
    if not items:
        errors.append("Open questions: omit section when empty instead of leaving it blank")
        return 0
    if len(items) == 1 and _is_placeholder(items[0]):
        errors.append("Open questions: omit section when empty instead of writing placeholder text")
        return 0

    if status == "ready":
        for idx, item in enumerate(items, start=1):
            owner_match = OWNER_FIELD_RE.search(item)
            fallback_match = FALLBACK_FIELD_RE.search(item)
            reopen_match = OQ_REOPEN_IF_RE.search(item)

            if not owner_match or _is_placeholder(owner_match.group(1)):
                errors.append(
                    f"Open questions #{idx}: ready intent.md requires non-requester 'Owner:' "
                    "(e.g. Research or external source)"
                )
            else:
                owner_raw = owner_match.group(1).strip()
                if _is_requester_owner(owner_raw):
                    errors.append(
                        f"Open questions #{idx}: ready intent.md cannot keep an unanswered "
                        f"requester-owned question (Owner: {owner_raw!r}); "
                        "close it in I<n>, convert to A<n>, or delegate it"
                    )

            if not fallback_match or _is_placeholder(fallback_match.group(1)):
                errors.append(
                    f"Open questions #{idx}: ready intent.md requires an active 'Fallback:' "
                    "(A<n> or delegated default)"
                )

            if not reopen_match or _is_placeholder(reopen_match.group(1)):
                errors.append(
                    f"Open questions #{idx}: ready intent.md requires an objective 'Reopen if:' trigger"
                )

    return len(items)


def _check_cited_paths(
    raw_text: str, intent_path: Path, repo_root: Path | None, warnings: list[str]
) -> None:
    if repo_root is None:
        return

    seen_missing: set[str] = set()
    in_fence = False
    for line in raw_text.splitlines():
        if line.strip().startswith("```"):
            in_fence = not in_fence
            continue
        if in_fence:
            continue

        for match in MD_LINK_RE.finditer(line):
            href = match.group(1).strip()
            if not href or href.startswith(("#", "http://", "https://", "mailto:")):
                continue
            target_part = href.split("#", 1)[0].strip()
            if not target_part or target_part.endswith("intent.md"):
                continue
            resolved = (intent_path.parent / target_part).resolve()
            repo_rel = (repo_root / target_part).resolve()
            if not resolved.exists() and not repo_rel.exists() and target_part not in seen_missing:
                seen_missing.add(target_part)
                warnings.append(
                    f"cited link target not found on disk (verify if planned path or typo): {target_part}"
                )

        for match in BACKTICK_REPO_PATH_RE.finditer(line):
            raw_path = match.group(1).split(":", 1)[0]
            if not (repo_root / raw_path).exists() and raw_path not in seen_missing:
                seen_missing.add(raw_path)
                warnings.append(
                    f"cited repo path not found on disk (verify if planned path or typo): {raw_path}"
                )


def check_intent(target: Path, repo_root: Path | None = None) -> IntentReport:
    intent_path = _resolve_intent_path(target)
    report = IntentReport(intent_path=intent_path, status=None)

    if not intent_path.is_file():
        report.errors.append(f"intent not found: {intent_path}")
        return report

    raw_text = intent_path.read_text(encoding="utf-8")
    title, status, sections = _parse_sections(raw_text)
    report.status = status

    if not title or _is_placeholder(title):
        report.errors.append("intent.md: missing non-empty '# Intent: <short desired outcome>' title")

    if status not in VALID_STATUSES:
        report.errors.append(
            f"intent.md: invalid or missing status {status!r} "
            f"(expected one of {sorted(VALID_STATUSES)})"
        )

    section_lookup = {name.lower(): (name, lines) for name, lines in sections.items()}

    for req in REQUIRED_SECTIONS:
        entry = section_lookup.get(req.lower())
        if entry is None:
            report.errors.append(f"intent.md: missing required section '## {req}'")
            continue
        _, lines = entry
        if _section_is_empty_or_placeholder(lines):
            report.errors.append(f"intent.md: required section '## {req}' is empty or placeholder")

    if "constraints & boundaries" in section_lookup:
        _check_constraints(section_lookup["constraints & boundaries"][1], report.errors)

    for opt in OPTIONAL_SECTIONS:
        entry = section_lookup.get(opt.lower())
        if entry is None:
            continue
        orig_name, lines = entry
        if _section_is_empty_or_placeholder(lines):
            report.errors.append(
                f"intent.md: optional section '## {orig_name}' is empty or placeholder "
                "(omit when empty)"
            )

    for lower_name, (orig_name, _lines) in section_lookup.items():
        if lower_name.startswith("remaining frontier"):
            report.has_remaining_frontier = True
            if status == "ready":
                report.errors.append(
                    f"intent.md: draft-only section '## {orig_name}' must be removed before status: ready"
                )

    if "settled decisions" in section_lookup and not _section_is_empty_or_placeholder(
        section_lookup["settled decisions"][1]
    ):
        report.decisions = _parse_settled_decisions(
            section_lookup["settled decisions"][1], report.errors
        )

    if "concrete scenarios" in section_lookup and not _section_is_empty_or_placeholder(
        section_lookup["concrete scenarios"][1]
    ):
        report.scenarios_count = len(_strip_bullet_items(section_lookup["concrete scenarios"][1]))

    if "assumptions & reopen triggers" in section_lookup and not _section_is_empty_or_placeholder(
        section_lookup["assumptions & reopen triggers"][1]
    ):
        report.assumptions_count = _check_assumptions(
            section_lookup["assumptions & reopen triggers"][1], report.errors
        )

    if "open questions" in section_lookup and not _section_is_empty_or_placeholder(
        section_lookup["open questions"][1]
    ):
        report.open_questions_count = _check_open_questions(
            section_lookup["open questions"][1], status, report.errors
        )

    effective_root = repo_root if repo_root is not None else _find_repo_root(intent_path)
    _check_cited_paths(raw_text, intent_path, effective_root, report.warnings)

    return report


def format_report(report: IntentReport) -> str:
    hard = sum(1 for d in report.decisions if d.superseded_by is None and d.binding == "hard")
    default = sum(1 for d in report.decisions if d.superseded_by is None and d.binding == "default")
    superseded = sum(1 for d in report.decisions if d.superseded_by is not None)
    dec_summary = f"{len(report.decisions)} ({hard} hard, {default} default"
    if superseded:
        dec_summary += f", {superseded} superseded"
    dec_summary += ")"

    out: list[str] = [
        f"Intent: {report.intent_path} (status: {report.status or 'unknown'})",
        (
            f"Summary: decisions={dec_summary}, scenarios={report.scenarios_count}, "
            f"assumptions={report.assumptions_count}, open_questions={report.open_questions_count}"
            + (", remaining_frontier=present" if report.has_remaining_frontier else "")
        ),
    ]
    if report.warnings:
        out.append("Warnings:")
        for warn in report.warnings:
            out.append(f"  - {warn}")
    return "\n".join(out)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Verify Intent V1 (specs/<task>/intent.md) mechanical invariants."
    )
    parser.add_argument(
        "target",
        type=Path,
        help="Path to specs/<task> directory or specs/<task>/intent.md",
    )
    parser.add_argument(
        "--repo",
        type=Path,
        default=None,
        help="Optional repository root for verifying cited relative paths",
    )
    args = parser.parse_args(argv)
    report = check_intent(args.target, repo_root=args.repo)
    print(format_report(report))
    if report.errors:
        print("Errors:", file=sys.stderr)
        for err in report.errors:
            print(f"  - {err}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
