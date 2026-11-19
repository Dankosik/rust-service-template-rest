#!/usr/bin/env python3
"""Synchronize and verify canonical .agents roles, skills, and harness adapters (source-template tool)."""

from __future__ import annotations

import argparse
import difflib
import json
import os
import re
import sys
import tempfile
import tomllib
from collections.abc import Sequence
from dataclasses import dataclass
from pathlib import Path
from typing import Any

HARNESS_CHOICES = ("core", "codex", "claude", "qwen", "cursor", "grok", "opencode", "antigravity", "all")


class HarnessError(ValueError):
    pass


@dataclass(frozen=True)
class AdapterPack:
    canonical: tuple[str, ...]
    generated: tuple[str, ...]
    settings: tuple[str, ...] = ()
    static_copies: tuple[tuple[str, str], ...] = ()
    setting_spec: tuple[str, str, str, str] | None = None
    role_dirs: tuple[tuple[str, str], ...] = ()


ADAPTERS: dict[str, AdapterPack] = {
    "codex": AdapterPack(
        canonical=(".agents/codex-project.toml",),
        generated=(".codex/config.toml", ".codex/agents/"),
        role_dirs=((".codex/agents", "toml"),),
    ),
    "claude": AdapterPack(
        canonical=(".agents/adapters/claude/CLAUDE.md", ".agents/adapters/claude/settings.json"),
        generated=("CLAUDE.md", ".claude/agents/", ".claude/skills/"),
        settings=(".claude/settings.json",),
        static_copies=((".agents/adapters/claude/CLAUDE.md", "CLAUDE.md"),),
        setting_spec=(".agents/adapters/claude/settings.json", ".claude/settings.json", "env", "CLAUDE_CODE_MAX_SUBAGENT_SPAWN_DEPTH"),
        role_dirs=((".claude/agents", "md"),),
    ),
    "qwen": AdapterPack(
        canonical=(".agents/adapters/qwen/QWEN.md", ".agents/adapters/qwen/settings.json"),
        generated=("QWEN.md", ".qwen/agents/", ".qwen/skills/"),
        settings=(".qwen/settings.json",),
        static_copies=((".agents/adapters/qwen/QWEN.md", "QWEN.md"),),
        setting_spec=(".agents/adapters/qwen/settings.json", ".qwen/settings.json", "model", "maxSubagentDepth"),
        role_dirs=((".qwen/agents", "md"),),
    ),
    "cursor": AdapterPack(
        canonical=(".agents/adapters/cursor/rules/agent-harness.mdc",),
        generated=(".cursor/rules/agent-harness.mdc", ".cursor/agents/"),
        static_copies=((".agents/adapters/cursor/rules/agent-harness.mdc", ".cursor/rules/agent-harness.mdc"),),
        role_dirs=((".cursor/agents", "md"),),
    ),
    "grok": AdapterPack(
        canonical=(".agents/adapters/grok/Grok.md", ".agents/adapters/grok/rules/harness.md"),
        generated=("Grok.md", ".grok/rules/harness.md", ".grok/agents/", ".grok/roles/"),
        static_copies=(
            (".agents/adapters/grok/Grok.md", "Grok.md"),
            (".agents/adapters/grok/rules/harness.md", ".grok/rules/harness.md"),
        ),
        role_dirs=((".grok/agents", "md"), (".grok/roles", "toml")),
    ),
    "opencode": AdapterPack(
        canonical=(
            ".agents/adapters/opencode/opencode.json",
            ".agents/adapters/opencode/.gitignore",
            ".agents/adapters/opencode/rules/harness.md",
            ".agents/adapters/opencode/commands/orchestrator.md",
            ".agents/adapters/opencode/plugins/task-subagents.js",
        ),
        generated=(
            "opencode.json",
            ".opencode/.gitignore",
            ".opencode/rules/harness.md",
            ".opencode/commands/orchestrator.md",
            ".opencode/plugins/task-subagents.js",
            ".opencode/agents/",
        ),
        static_copies=(
            (".agents/adapters/opencode/opencode.json", "opencode.json"),
            (".agents/adapters/opencode/.gitignore", ".opencode/.gitignore"),
            (".agents/adapters/opencode/rules/harness.md", ".opencode/rules/harness.md"),
            (".agents/adapters/opencode/commands/orchestrator.md", ".opencode/commands/orchestrator.md"),
            (".agents/adapters/opencode/plugins/task-subagents.js", ".opencode/plugins/task-subagents.js"),
        ),
        role_dirs=((".opencode/agents", "md"),),
    ),
    "antigravity": AdapterPack(
        canonical=(".agents/adapters/antigravity/rules/antigravity-harness.md",),
        generated=(".agents/rules/antigravity-harness.md", ".agents/agents/"),
        static_copies=((".agents/adapters/antigravity/rules/antigravity-harness.md", ".agents/rules/antigravity-harness.md"),),
        role_dirs=((".agents/agents", "md"),),
    ),
}

ALLOWED_ROLE_KEYS = frozenset({
    "name", "description", "class", "claude_model", "claude_effort",
    "qwen_model", "cursor_model", "grok_model", "grok_effort", "output_schema", "instructions",
})

OUTPUT_SCHEMA_LINES = {
    "lane-result-v1": (
        "Return fields: conclusion, evidence (primary locator and result), "
        "uncertainty_or_conflict, disposition (accept | reject | repair | "
        "carry_as_proof_or_risk | reopen), destination."
    ),
    "specialist-result-v1": (
        "Return Decision Result V1 (disposition: decision | constraint_only | "
        "proof_only | no_new_decision_required | blocked, decision_or_constraint, "
        "forced_consequences, proof_or_gap, blocker, "
        "strongest_rejected_alternative, rejection_reason, "
        "reopen_owner_or_condition), or Candidate Proposal V1 when dispatched "
        "as a generative candidate lane (pressure, proposal, caller_sketch, "
        "hidden_complexity, dependencies_and_cost, thin_leverage_and_risks, "
        "blocker_or_gap, anchors)."
    ),
    "review-result-v1": "Return `docs/spec-first-workflow/interfaces/review-result-v1.md`.",
    "delegated-result-v1": (
        "Return fields: result, changed_paths, proof (checks actually run "
        "with their results, or none), candidate, gap."
    ),
}

CLAUDE_MUTABLE_TOOLS = "Read, Grep, Glob, Bash, Edit, Write, Agent, SendMessage, TaskOutput, TaskStop"
CLAUDE_READONLY_TOOLS = "Read, Grep, Glob, Bash"
QWEN_MUTABLE_TOOLS = (
    "  - read_file\n  - grep_search\n  - glob\n  - list_directory\n  - run_shell_command\n"
    "  - write_file\n  - edit\n  - agent\n  - list_agents\n  - send_message\n  - task_stop"
)
QWEN_READONLY_TOOLS = "  - read_file\n  - grep_search\n  - glob\n  - list_directory\n  - run_shell_command"
ANTIGRAVITY_MUTABLE_TOOLS = "[view_file, grep_search, run_command, replace_file_content, write_to_file, invoke_subagent, send_message]"
ANTIGRAVITY_READONLY_TOOLS = "[view_file, grep_search, run_command]"

LEAD_DESC = (
    "Unit: Use when bound ACCEPTANCE_UNIT_LEAD for an implementation unit or "
    "final delivery validation. Own the assigned boundary; Skip sibling scheduling."
)
ORCH_DESC = (
    "Ledger: Use when this session must route a ready persisted Implementation "
    "ledger as LEDGER_ORCHESTRATOR. Own routing; Skip unit work."
)

RUNTIME_START = "# template-owned-codex-runtime:start"
RUNTIME_END = "# template-owned-codex-runtime:end"
REGISTRY_START = "# template-owned-codex-agents:start"
REGISTRY_END = "# template-owned-codex-agents:end"

_ROLE_NAME_RE = re.compile(r"^[a-z0-9]+(?:-[a-z0-9]+)*$")
_MODEL_HEAD_RE = re.compile(r"^[A-Za-z0-9]")
_MODEL_BODY_RE = re.compile(r"^[A-Za-z0-9._/:=-]+$")
_WORKSPACE_CRATES_REF = ".agents/skills/rust-dependencies/references/workspace-crates.md"
_WORKSPACE_CRATES_BULLET_RE = re.compile(r"^- \*\*[^*]+\*\* \u2014 (?P<crates>`[^`]+`(?:, `[^`]+`)*):")
_BACKTICK_TOKEN_RE = re.compile(r"`([^`]+)`")


def selected_adapters(harness: str) -> tuple[str, ...]:
    if harness == "core":
        return ()
    if harness == "all":
        return tuple(ADAPTERS)
    if harness in ADAPTERS:
        return (harness,)
    raise HarnessError(f"unsupported harness: {harness}")


def _parse_json_no_dups(raw: bytes, label: str) -> Any:
    def _no_dups(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
        out: dict[str, Any] = {}
        for k, v in pairs:
            if k in out:
                raise HarnessError(f"{label}: duplicate JSON key {k!r}")
            out[k] = v
        return out

    try:
        return json.loads(raw.decode("utf-8"), object_pairs_hook=_no_dups)
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise HarnessError(f"{label}: invalid JSON") from exc


def _atomic_write(path: Path, data: bytes, mode: int = 0o644) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, tmp_name = tempfile.mkstemp(dir=path.parent, prefix=f".{path.name}.")
    try:
        with os.fdopen(fd, "wb") as handle:
            handle.write(data)
        os.chmod(tmp_name, mode)
        os.replace(tmp_name, path)
    except BaseException:
        try:
            os.unlink(tmp_name)
        except OSError:
            pass
        raise


def _read_regular(repo: Path, rel: str) -> str:
    path = repo / rel
    if path.is_symlink() or not path.is_file():
        raise HarnessError(f"{rel} is missing or a symlink")
    return path.read_text(encoding="utf-8")


def _validate_model(key: str, value: str, rel: str) -> None:
    if not value:
        return
    sanitized = value.replace("[", "").replace("]", "")
    if not _MODEL_HEAD_RE.match(value) or not _MODEL_BODY_RE.fullmatch(sanitized):
        raise HarnessError(f"{rel} has an unsafe {key} value")


def _render_roles(repo: Path) -> tuple[list[str], dict[str, dict[str, bytes]]]:
    roles_dir = repo / ".agents/roles"
    if roles_dir.is_symlink() or not roles_dir.is_dir():
        raise HarnessError(".agents/roles is missing")

    rendered: dict[str, dict[str, bytes]] = {
        ".codex/agents": {}, ".claude/agents": {}, ".qwen/agents": {},
        ".grok/agents": {}, ".grok/roles": {}, ".cursor/agents": {},
        ".opencode/agents": {}, ".agents/agents": {},
    }
    role_names: list[str] = []

    for source_file in sorted(roles_dir.glob("*.toml")):
        rel = source_file.relative_to(repo).as_posix()
        if source_file.is_symlink():
            raise HarnessError(f"{rel} is a symlink")
        raw = source_file.read_bytes()
        if b"\r" in raw:
            raise HarnessError(f"{rel} contains CRLF")
        data = tomllib.loads(raw.decode("utf-8"))
        if unknown := sorted(set(data) - ALLOWED_ROLE_KEYS):
            raise HarnessError(f"{rel} has unsupported keys: {', '.join(unknown)}")
        for k, v in data.items():
            if not isinstance(v, str) or "\0" in v:
                raise HarnessError(f"{rel} key {k} must be a non-NUL string")

        name = data.get("name", "")
        description = data.get("description", "")
        body = data.get("instructions", "").rstrip("\n")
        if not _ROLE_NAME_RE.fullmatch(name) or name != source_file.stem:
            raise HarnessError(f"{rel} name must match filename")
        if not description or any(ch in description for ch in ("\n", '"', "\\")):
            raise HarnessError(f"{rel} invalid description")
        if not body:
            raise HarnessError(f"{rel} empty instructions")

        role_class = data.get("class", "")
        if role_class == "read-only-specialist":
            common = _read_regular(repo, ".agents/role-classes/read-only-specialist.md").rstrip("\n")
            fallback = _read_regular(repo, ".agents/role-classes/read-only-specialist-fallback.md").rstrip("\n")
            sandbox_mode, cursor_ro, grok_cap = "read-only", "true", "read-only"
            claude_tools, qwen_tools, ag_tools = CLAUDE_READONLY_TOOLS, QWEN_READONLY_TOOLS, ANTIGRAVITY_READONLY_TOOLS
        elif role_class == "mutable-worker":
            common = _read_regular(repo, ".agents/role-classes/mutable-worker.md").rstrip("\n")
            fallback = _read_regular(repo, ".agents/role-classes/mutable-worker-fallback.md").rstrip("\n")
            sandbox_mode, cursor_ro, grok_cap = "workspace-write", "false", "all"
            claude_tools, qwen_tools, ag_tools = CLAUDE_MUTABLE_TOOLS, QWEN_MUTABLE_TOOLS, ANTIGRAVITY_MUTABLE_TOOLS
        else:
            raise HarnessError(f"{rel} unsupported class {role_class!r}")

        claude_model = data.get("claude_model", "")
        claude_effort = data.get("claude_effort", "")
        qwen_model = data.get("qwen_model", "")
        cursor_model = data.get("cursor_model", "")
        grok_model = data.get("grok_model", "")
        grok_effort = data.get("grok_effort", "")
        for k, v in (("claude_model", claude_model), ("qwen_model", qwen_model), ("cursor_model", cursor_model), ("grok_model", grok_model)):
            _validate_model(k, v, rel)
        if not claude_model or not cursor_model or not grok_model:
            raise HarnessError(f"{rel} missing required model field")
        if claude_effort not in ("", "low", "medium", "high", "xhigh", "max"):
            raise HarnessError(f"{rel} unsupported claude_effort {claude_effort!r}")
        if grok_effort not in ("inherit", "low", "medium", "high", "xhigh"):
            raise HarnessError(f"{rel} unsupported grok_effort {grok_effort!r}")
        schema_line = OUTPUT_SCHEMA_LINES.get(data.get("output_schema", ""))
        if schema_line is None:
            raise HarnessError(f"{rel} unsupported output_schema {data.get('output_schema')!r}")

        role_names.append(name)
        rendered[".codex/agents"][f"{name}.toml"] = (
            f'name = "{name}"\ndescription = "{description}"\nsandbox_mode = "{sandbox_mode}"\n\n'
            f'developer_instructions = """\n{common}\n\n{schema_line}\n\n{body}\n"""\n'
        ).encode("utf-8")

        effort_line = f"effort: {claude_effort}\n" if claude_effort else ""
        rendered[".claude/agents"][f"{name}.md"] = (
            f'---\nname: {name}\ndescription: "{description}"\ntools: {claude_tools}\n'
            f"model: {claude_model}\n{effort_line}---\n\n{common}\n\n{fallback}\n\n{schema_line}\n\n{body}\n"
        ).encode("utf-8")

        qwen_model_line = f"model: {qwen_model}\n" if qwen_model else ""
        rendered[".qwen/agents"][f"{name}.md"] = (
            f'---\nname: {name}\ndescription: "{description}"\n{qwen_model_line}'
            f"tools:\n{qwen_tools}\n---\n\n{common}\n\n{fallback}\n\n{schema_line}\n\n{body}\n"
        ).encode("utf-8")

        grok_model_line = "" if grok_model == "inherit" else f"model: {grok_model}\n"
        grok_fallback = f"\n{fallback}\n" if role_class == "mutable-worker" else ""
        rendered[".grok/agents"][f"{name}.md"] = (
            f'---\nname: {name}\ndescription: "{description}"\n{grok_model_line}'
            f"permission_mode: bypassPermissions\nagents_md: true\n---\n\n{common}\n{grok_fallback}\n{schema_line}\n\n{body}\n"
        ).encode("utf-8")

        grok_toml = f'description = "{description}"\ndefault_capability_mode = "{grok_cap}"\n'
        if grok_model != "inherit":
            grok_toml += f'model = "{grok_model}"\n'
        if grok_effort != "inherit":
            grok_toml += f'reasoning_effort = "{grok_effort}"\n'
        rendered[".grok/roles"][f"{name}.toml"] = grok_toml.encode("utf-8")

        rendered[".cursor/agents"][f"{name}.md"] = (
            f'---\nname: {name}\ndescription: "{description}"\nmodel: {cursor_model}\n'
            f"readonly: {cursor_ro}\n---\n\n{common}\n\n{fallback}\n\n{schema_line}\n\n{body}\n"
        ).encode("utf-8")

        op_lines = ["---", f'description: "{description}"', "mode: subagent", "hidden: true"]
        if grok_model != "inherit":
            op_lines.append(f"model: xai/{grok_model}")
        elif grok_effort != "inherit":
            op_lines.append("model: xai/grok-4.6")
        if grok_effort != "inherit":
            op_lines.append(f"variant: {grok_effort}")
        op_lines.append("permission:")
        if role_class == "read-only-specialist":
            op_lines.append("  edit: deny")
        if role_class == "mutable-worker":
            op_lines.extend(["  task:", '    "*": deny', "    worker-agent: allow", "    evidence-agent: allow"])
        else:
            op_lines.append("  task: deny")
        op_lines.extend(["  question: deny", "---", ""])
        rendered[".opencode/agents"][f"{name}.md"] = ("\n".join(op_lines) + f"\n{common}\n\n{fallback}\n\n{schema_line}\n\n{body}\n").encode("utf-8")

        rendered[".agents/agents"][f"{name}.md"] = (
            f'---\nname: {name}\ndescription: "{description}"\ntools: {ag_tools}\n'
            f"mainAgent: false\nsubagent: true\n---\n\n{common}\n\n{fallback}\n\n{schema_line}\n\n{body}\n"
        ).encode("utf-8")

    if not role_names:
        raise HarnessError(".agents/roles contains no canonical role files")

    impl = "../../docs/spec-first-workflow/phases/implementation.md"
    lead_phase = impl.removesuffix(".md")
    lead_method = (
        f"Apply [Unit Implementation]({lead_phase}/unit.md) for an assigned unit,\n"
        f"[Repair Assignment]({lead_phase}/repair.md) for a Final Validation repair, or\n"
        f"[Final Validation]({lead_phase}/final-validation.md) as Delivery Owner.\n"
    )
    orch_method = f"Apply [Implementation as Ledger Orchestrator]({impl}#carrier)\n"
    lead_tail = (
        "Apply the assigned packet, repair brief, or ledger scope and its named Method. Return\n"
        "`docs/spec-first-workflow/interfaces/acceptance-result-v1.md`.\n"
    )

    lead_specs = (
        (".claude/agents", f'name: acceptance-unit-lead\ndescription: "{LEAD_DESC}"\ntools: {CLAUDE_MUTABLE_TOOLS}\nmodel: inherit\neffort: high', "Claude Code", "claude-code.md"),
        (".qwen/agents", f'name: acceptance-unit-lead\ndescription: "{LEAD_DESC}"\nmodel: inherit\ntools:\n{QWEN_MUTABLE_TOOLS}', "Qwen Code", "qwen-code.md"),
        (".cursor/agents", f'name: acceptance-unit-lead\ndescription: "{LEAD_DESC}"\nmodel: grok-4.6[effort=xhigh]\nreadonly: false', "Cursor", "cursor.md"),
        (".grok/agents", f'name: acceptance-unit-lead\ndescription: "{LEAD_DESC}"\nagents_md: true\npermission_mode: bypassPermissions\nprompt_mode: full', "Grok Build", "grok-build.md"),
        (".opencode/agents", f'description: "{LEAD_DESC}"\nmode: all\nmodel: xai/grok-4.6\nvariant: xhigh\npermission:\n  task:\n    "*": deny\n    worker-agent: allow\n    specialist-agent: allow\n    evidence-agent: allow\n    reviewer-agent: allow\n    adjudicator-agent: allow\n  question: deny', "OpenCode", "opencode.md"),
        (".agents/agents", f'name: acceptance-unit-lead\ndescription: "{LEAD_DESC}"\ntools: {ANTIGRAVITY_MUTABLE_TOOLS}\nmainAgent: false\nsubagent: true', "Antigravity", "antigravity.md"),
    )
    for rel_dir, fm, label, doc in lead_specs:
        rendered[rel_dir]["acceptance-unit-lead.md"] = (
            f"---\n{fm}\n---\n\n{lead_method}Load [{label} adapter](../../docs/agent-harness/{doc}) "
            f"only when spawning sub-lanes or independent review.\n{lead_tail}"
        ).encode("utf-8")

    orch_specs = (
        (".grok/agents", f'name: orchestrator\ndescription: "{ORCH_DESC}"\nagents_md: true\npermission_mode: bypassPermissions\nprompt_mode: full', "Grok Build", "grok-build.md"),
        (".opencode/agents", f'description: "{ORCH_DESC}"\nmode: primary\nmodel: xai/grok-4.6\nvariant: high\npermission:\n  task:\n    "*": deny\n    acceptance-unit-lead: allow\n    worker-agent: allow\n    specialist-agent: allow\n    evidence-agent: allow\n    reviewer-agent: allow\n    adjudicator-agent: allow', "OpenCode", "opencode.md"),
        (".agents/agents", f'name: orchestrator\ndescription: "{ORCH_DESC}"\ntools: {ANTIGRAVITY_MUTABLE_TOOLS}\nmainAgent: true\nsubagent: false', "Antigravity", "antigravity.md"),
    )
    for rel_dir, fm, label, doc in orch_specs:
        rendered[rel_dir]["orchestrator.md"] = (
            f"---\n{fm}\n---\n\n{orch_method}and [{label} adapter](../../docs/agent-harness/{doc}).\n"
        ).encode("utf-8")

    return role_names, rendered


def _render_codex_config(repo: Path, role_names: list[str]) -> bytes:
    text = _read_regular(repo, ".agents/codex-project.toml")
    if not text.endswith("\n"):
        text += "\n"
    registry = [REGISTRY_START, "# Registry compatibility: agents.<name>.config_file entries are required by Codex."]
    for role in role_names:
        registry.extend([f"[agents.{role}]", f'config_file = "agents/{role}.toml"', ""])
    registry.append(REGISTRY_END)
    return (f"{RUNTIME_START}\n{text}{RUNTIME_END}\n\n" + "\n".join(registry) + "\n").encode("utf-8")


def _parse_frontmatter(text: str) -> dict[str, str]:
    if not text.startswith("---\n"):
        raise HarnessError("missing frontmatter")
    end = text.find("\n---\n", 4)
    if end < 0:
        raise HarnessError("unterminated frontmatter")
    fields: dict[str, str] = {}
    for line in text[4:end].splitlines():
        if not line.strip() or line.startswith((" ", "\t")):
            continue
        key, sep, value = line.partition(":")
        if sep:
            fields[key.strip()] = value.strip().strip('"')
    return fields


def check_workspace_crates_reference(repo: Path, *, check_manifest: bool = True) -> list[str]:
    ref_path = repo / _WORKSPACE_CRATES_REF
    if not ref_path.is_file():
        return []
    declared: frozenset[str] | None = None
    if check_manifest:
        manifest = tomllib.loads((repo / "Cargo.toml").read_text(encoding="utf-8"))
        declared = frozenset(manifest.get("workspace", {}).get("dependencies", {}))
    problems: list[str] = []
    entries = 0
    for lineno, line in enumerate(ref_path.read_text(encoding="utf-8").splitlines(), start=1):
        if not line.startswith("- "):
            continue
        match = _WORKSPACE_CRATES_BULLET_RE.match(line)
        if match is None:
            problems.append(f"{_WORKSPACE_CRATES_REF}:{lineno}: bullet must match '- **Task** — `crate`: note'")
            continue
        entries += 1
        if declared is None:
            continue
        for crate in _BACKTICK_TOKEN_RE.findall(match.group("crates")):
            if not crate.startswith("std::") and crate not in declared:
                problems.append(f"{_WORKSPACE_CRATES_REF}:{lineno}: crate `{crate}` is not in [workspace.dependencies]")
    if entries == 0:
        problems.append(f"{_WORKSPACE_CRATES_REF}: contains no crate entries")
    return problems


def _validate_skills(repo: Path) -> list[str]:
    skills_root = repo / ".agents/skills"
    if skills_root.is_symlink() or not skills_root.is_dir():
        raise HarnessError(".agents/skills is missing")
    names: list[str] = []
    problems: list[str] = []
    for entry in sorted(skills_root.iterdir()):
        if entry.is_symlink() or not entry.is_dir():
            problems.append(f".agents/skills/{entry.name}: must be a regular directory")
            continue
        skill = entry / "SKILL.md"
        if skill.is_symlink() or not skill.is_file():
            problems.append(f"{entry.name}: missing SKILL.md")
            continue
        try:
            fields = _parse_frontmatter(skill.read_text(encoding="utf-8"))
        except (OSError, UnicodeDecodeError, HarnessError) as err:
            problems.append(f"{entry.name}: {err}")
            continue
        if fields.get("name") != entry.name or not _ROLE_NAME_RE.fullmatch(entry.name):
            problems.append(f"{entry.name}: frontmatter name must match directory name")
        if not fields.get("description"):
            problems.append(f"{entry.name}: empty description")
        names.append(entry.name)
    problems.extend(check_workspace_crates_reference(repo, check_manifest=(repo / "template/init.py").is_file()))
    if not names:
        raise HarnessError(".agents/skills contains no skills")
    if problems:
        raise HarnessError("\n".join(problems))
    return names


def _read_setting_leaf(repo: Path, adapter: str) -> Any:
    spec = ADAPTERS[adapter].setting_spec
    assert spec is not None
    src_rel, _, parent_key, leaf_key = spec
    data = _parse_json_no_dups((repo / src_rel).read_bytes(), src_rel)
    parent = data.get(parent_key) if isinstance(data, dict) else None
    if not isinstance(parent, dict) or leaf_key not in parent:
        raise HarnessError(f"{src_rel} has no managed setting {parent_key}.{leaf_key}")
    return parent[leaf_key]


def _check_file_bytes(repo: Path, rel: str, expected: bytes) -> None:
    path = repo / rel
    if path.is_symlink() or not path.is_file():
        raise HarnessError(f"{rel} is missing; run `just agent-harness-sync`")
    actual = path.read_bytes()
    if actual != expected:
        diff = difflib.unified_diff(
            actual.decode("utf-8", errors="replace").splitlines(keepends=True),
            expected.decode("utf-8", errors="replace").splitlines(keepends=True),
            fromfile=rel,
            tofile=f"{rel} (expected)",
        )
        sys.stderr.writelines(diff)
        raise HarnessError(f"{rel} is stale; run `just agent-harness-sync`")


def sync_or_check(repo: Path, mode: str, harness: str = "all", *, require_materialized: bool = False) -> None:
    selected = selected_adapters(harness)
    role_names, rendered_roles = _render_roles(repo)
    skill_names = _validate_skills(repo)
    codex_config = _render_codex_config(repo, role_names) if "codex" in selected else b""

    static_payloads: dict[str, bytes] = {}
    setting_values: dict[str, Any] = {}
    for adapter in selected:
        pack = ADAPTERS[adapter]
        for src_rel, dst_rel in pack.static_copies:
            _read_regular(repo, src_rel)
            static_payloads[dst_rel] = (repo / src_rel).read_bytes()
        if pack.setting_spec is not None:
            setting_values[adapter] = _read_setting_leaf(repo, adapter)

    if mode == "apply":
        for adapter in selected:
            pack = ADAPTERS[adapter]
            for rel_dir, ext in pack.role_dirs:
                target_dir = repo / rel_dir
                target_dir.mkdir(parents=True, exist_ok=True)
                expected_map = rendered_roles[rel_dir]
                for existing in sorted(target_dir.glob(f"*.{ext}")):
                    if existing.name not in expected_map:
                        existing.unlink()
                for filename, payload in sorted(expected_map.items()):
                    _atomic_write(target_dir / filename, payload)
            if adapter == "codex":
                _atomic_write(repo / ".codex/config.toml", codex_config)
            if adapter in ("claude", "qwen"):
                links_root = repo / f".{adapter}/skills"
                links_root.mkdir(parents=True, exist_ok=True)
                for entry in list(links_root.iterdir()):
                    if entry.is_symlink():
                        entry.unlink()
                for name in skill_names:
                    (links_root / name).symlink_to(f"../../.agents/skills/{name}")
            for _, dst_rel in pack.static_copies:
                _atomic_write(repo / dst_rel, static_payloads[dst_rel])
            if pack.setting_spec is not None:
                _, dst_rel, parent_key, leaf_key = pack.setting_spec
                dst = repo / dst_rel
                data = _parse_json_no_dups(dst.read_bytes(), dst_rel) if dst.is_file() else {}
                data.setdefault(parent_key, {})[leaf_key] = setting_values[adapter]
                _atomic_write(dst, (json.dumps(data, indent=2, ensure_ascii=False) + "\n").encode("utf-8"))
        print(f"agent harness: {len(role_names)} roles and {len(skill_names)} skills synced for {harness}")
        return

    checked = 0
    for adapter in selected:
        pack = ADAPTERS[adapter]
        candidates = [d for d, _ in pack.role_dirs] + [dst for _, dst in pack.static_copies]
        if not require_materialized and not any((repo / rel).exists() for rel in candidates):
            continue
        checked += 1
        for rel_dir, ext in pack.role_dirs:
            target_dir = repo / rel_dir
            if not target_dir.is_dir():
                raise HarnessError(f"{rel_dir} is missing")
            expected_map = rendered_roles[rel_dir]
            for existing in sorted(target_dir.glob(f"*.{ext}")):
                if existing.name not in expected_map:
                    raise HarnessError(f"{existing.relative_to(repo).as_posix()} has no canonical role source")
            for filename, payload in sorted(expected_map.items()):
                _check_file_bytes(repo, f"{rel_dir}/{filename}", payload)
        if adapter == "codex":
            _check_file_bytes(repo, ".codex/config.toml", codex_config)
        if adapter in ("claude", "qwen"):
            links_root = repo / f".{adapter}/skills"
            if not links_root.is_dir():
                raise HarnessError(f".{adapter}/skills is missing")
            for name in skill_names:
                link = links_root / name
                if not link.is_symlink() or os.readlink(link) != f"../../.agents/skills/{name}":
                    raise HarnessError(f".{adapter}/skills/{name} symlink is missing or stale")
        for _, dst_rel in pack.static_copies:
            _check_file_bytes(repo, dst_rel, static_payloads[dst_rel])
        if pack.setting_spec is not None:
            _, dst_rel, parent_key, leaf_key = pack.setting_spec
            dst = repo / dst_rel
            if not dst.is_file():
                raise HarnessError(f"{dst_rel} is missing")
            data = _parse_json_no_dups(dst.read_bytes(), dst_rel)
            if data.get(parent_key, {}).get(leaf_key) != setting_values[adapter]:
                raise HarnessError(f"{dst_rel} managed setting is stale; run `just agent-harness-sync`")

    note = f"{checked} materialized adapters verified" if checked else "canonical sources verified"
    print(f"agent harness: {len(role_names)} roles and {len(skill_names)} skills current for {harness} ({note})")


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    modes = parser.add_mutually_exclusive_group(required=True)
    modes.add_argument("--apply", dest="mode", action="store_const", const="apply")
    modes.add_argument("--check", dest="mode", action="store_const", const="check")
    parser.add_argument("--repo", type=Path, default=Path("."))
    parser.add_argument("--harness", choices=HARNESS_CHOICES, default="all")
    parser.add_argument("--require-materialized", action="store_true")
    args = parser.parse_args(argv)
    try:
        sync_or_check(args.repo.resolve(strict=True), args.mode, args.harness, require_materialized=args.require_materialized)
    except (HarnessError, OSError, tomllib.TOMLDecodeError) as exc:
        print(f"agent harness: {exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
