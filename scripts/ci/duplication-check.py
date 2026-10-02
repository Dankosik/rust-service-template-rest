#!/usr/bin/env python3
"""Run pinned jscpd; admit only reviewed, path-bound source envelopes.

jscpd owns detection. This adapter interprets native byte coordinates, never
Rust syntax or tokens. Reports are disposable; policy is never rewritten.
"""
import argparse
import json
import os
from pathlib import Path, PurePosixPath
import re
import subprocess
import sys
import tempfile


class CheckError(ValueError):
    pass


def run(command, root):
    result = subprocess.run(command, cwd=root, capture_output=True, text=True)
    if result.returncode:
        raise CheckError(f"{command[0]} failed ({result.returncode}): {result.stderr or result.stdout}")
    return result.stdout


def relative_path(value):
    if not isinstance(value, str) or not value or "\\" in value:
        raise CheckError(f"invalid repository-relative path: {value!r}")
    path = PurePosixPath(value)
    if path.is_absolute() or ".." in path.parts or str(path) != value:
        raise CheckError(f"invalid repository-relative path: {value!r}")
    return path


def metadata(root):
    return json.loads(run(["cargo", "metadata", "--locked", "--offline", "--no-deps", "--format-version", "1"], root))


def discover(root, meta):
    members = set(meta["workspace_members"])
    packages = [p for p in meta["packages"] if p["id"] in members]
    if len(packages) != len(members) or not members:
        raise CheckError("Cargo metadata did not describe every workspace member")
    # Git is the ignore authority; --others includes pending nonignored source.
    tracked = run(["git", "ls-files", "-z", "--cached", "--others", "--exclude-standard"], root).split("\0")
    visible = set(tracked)
    source, tests = set(), set()
    for package in packages:
        directory = Path(package["manifest_path"]).parent
        member = directory.relative_to(root)
        if directory.is_symlink() or not directory.is_dir():
            raise CheckError(f"cannot inspect required member {member}")
        def walk_error(error):
            raise error
        for current, directories, files in os.walk(directory, followlinks=False, onerror=walk_error):
            directories[:] = sorted(d for d in directories if d not in {"target", "vendor", "vendored", ".git"}
                                    and not (Path(current) / d).is_symlink())
            for filename in sorted(files):
                path = Path(current) / filename
                rel = path.relative_to(root).as_posix()
                if path.suffix != ".rs" or path.is_symlink() or rel not in visible:
                    continue
                if rel.startswith("crates/grpc-contracts/src/generated/"):
                    continue
                local = path.relative_to(directory).parts
                dedicated = (member.parts[0] == "test" or
                             any(part in {"tests", "benches", "examples", "fixtures"} for part in local[:-1]) or
                             local == ("src", "tests.rs"))
                (tests if dedicated else source).add(rel)
    if not source:
        raise CheckError("unexpectedly empty production Rust scope")
    return sorted(source), sorted(tests)


def native_command(root):
    versions = (root / "tools/versions.env").read_text()
    matches = re.findall(r"^JSCPD_VERSION=([0-9]+\.[0-9]+\.[0-9]+)$", versions, re.M)
    if len(matches) != 1:
        raise CheckError("tools/versions.env must declare one JSCPD_VERSION")
    command = ["npx", "--yes", f"jscpd@{matches[0]}"]
    actual = run(command + ["--version"], root).strip()
    if actual not in {matches[0], f"jscpd {matches[0]}"}:
        raise CheckError(f"jscpd version mismatch: expected {matches[0]}, got {actual!r}")
    return command


def detector_config(root):
    config = json.loads((root / ".jscpd.json").read_text())
    # Scope and admission are adapter-owned. Additional detector modes/ignores
    # would silently change which handwritten source reaches this gate.
    required = {"format": ["rust"], "mode": "mild", "minTokens": 100,
                "minLines": 15, "reporters": ["console", "json"], "failOnEmpty": True}
    if (not isinstance(config, dict) or set(config) != set(required)
            or any(config[key] != value for key, value in required.items()
                   if key not in {"minTokens", "minLines"})
            or any(type(config[key]) is not int or config[key] < 1 for key in ("minTokens", "minLines"))):
        raise CheckError(".jscpd.json requires Rust mild exact detection, positive token/line minima, console/json reports and failOnEmpty")
    return config


def scan(root, paths, destination, command, config):
    destination.mkdir(parents=True, exist_ok=True)
    report = destination / "jscpd-report.json"
    report.unlink(missing_ok=True)
    # JSON path lists avoid command-line length limits without changing scope.
    effective = dict(config, path=[str(root / path) for path in paths], absolute=True)
    config_path = destination / "scan-config.json"
    config_path.write_text(json.dumps(effective))
    output = run(command + ["--config", str(config_path), "--output", str(destination)], root)
    print(output, end="")
    if not report.is_file():
        raise CheckError(f"jscpd did not write {report}")
    data = json.loads(report.read_text())
    validate_report(data, root, paths)
    return data


def integer(value, label, minimum=0):
    if type(value) is not int or value < minimum:
        raise CheckError(f"invalid {label}: {value!r}")
    return value


def report_interval(file, root, paths):
    if not isinstance(file, dict) or not isinstance(file.get("name"), str):
        raise CheckError("unknown jscpd file schema")
    name = Path(file["name"])
    if name.is_absolute():
        try:
            name = name.relative_to(root)
        except ValueError as error:
            raise CheckError(f"report path outside repository: {name}") from error
    path = str(relative_path(name.as_posix()))
    if path not in paths:
        raise CheckError(f"reported unselected path: {path}")
    data = (root / path).read_bytes()
    offsets = []
    for endpoint in ("start", "end"):
        loc = file.get(endpoint + "Loc")
        if not isinstance(loc, dict):
            raise CheckError(f"unknown jscpd coordinates for {path}")
        offset = integer(loc.get("position"), "byte position")
        line = integer(loc.get("line"), "line", 1)
        column = integer(loc.get("column"), "column")
        if offset > len(data) or file.get(endpoint) != line:
            raise CheckError(f"out-of-bounds/inconsistent coordinates for {path}")
        prefix = data[:offset]
        if prefix.count(b"\n") + 1 != line or len(prefix.rsplit(b"\n", 1)[-1]) != column:
            raise CheckError(f"byte/line coordinates disagree for {path}:{line}")
        prefix.decode("utf-8")  # Endpoints must not bisect a UTF-8 code point.
        offsets.append(offset)
    if offsets[0] >= offsets[1]:
        raise CheckError(f"empty or reversed reported interval in {path}")
    return path, offsets[0], offsets[1]


def validate_report(report, root, paths):
    if not isinstance(report, dict) or not isinstance(report.get("duplicates"), list):
        raise CheckError("unknown jscpd report schema")
    statistics = report.get("statistics")
    if not isinstance(statistics, dict) or not isinstance(statistics.get("total"), dict):
        raise CheckError("unknown jscpd statistics schema")
    total = statistics["total"]
    sources = integer(total.get("sources"), "source count", 1)
    integer(total.get("tokens"), "production token count", 1)
    # Native statistics omit files below minTokens. Do not invent a tokenizer
    # to reproduce that eligibility decision, but reject impossible totals.
    if sources > len(paths):
        raise CheckError(f"jscpd counted {sources} sources outside the {len(paths)} selected files")
    if integer(total.get("clones"), "clone count") != len(report["duplicates"]):
        raise CheckError("jscpd clone count disagrees with report")
    for pair in report["duplicates"]:
        if not isinstance(pair, dict) or pair.get("format") != "rust" or pair.get("kind") != "exact":
            raise CheckError("unknown jscpd clone schema/mode")
        integer(pair.get("tokens"), "clone token count", 1)
        for side in ("firstFile", "secondFile"):
            report_interval(pair.get(side), root, paths)


def lines(text):
    return [line.lstrip(" \t") for line in text.replace("\r\n", "\n").split("\n") if line.strip(" \t")]


def reduced(current, original):
    remaining = iter(lines(original))
    return all(any(candidate == line for candidate in remaining) for line in lines(current))


def anchor_offset(data, anchor, before):
    text = anchor["text"].replace("\r\n", "\n").encode()
    if anchor["boundary"]:
        if before and data.startswith(text):
            return len(text)
        if not before and data.endswith(text):
            return len(data) - len(text)
    elif text and data.count(text) == 1:
        return data.index(text) + (len(text) if before else 0)
    return None


def envelopes(root, baseline):
    if not isinstance(baseline, dict) or baseline.get("version") != 1 or not isinstance(baseline.get("cases"), list):
        raise CheckError("unknown duplication admission schema")
    resolved = []
    ids = set()
    for case in baseline["cases"]:
        if not isinstance(case, dict):
            raise CheckError("unknown duplication admission case schema")
        ident = case.get("id")
        if (not isinstance(ident, str) or not ident or ident in ids
                or case.get("kind") not in {"test", "production"}
                or not isinstance(case.get("reason"), str) or not case["reason"].strip()):
            raise CheckError("admission needs unique ID, kind and reason")
        ids.add(ident)
        occurrences = case.get("occurrences")
        if not isinstance(occurrences, list) or len(occurrences) < 2:
            raise CheckError(f"{ident}: expected at least two diagnosed occurrences")
        for index, occurrence in enumerate(occurrences):
            if not isinstance(occurrence, dict):
                raise CheckError(f"{ident}: unknown occurrence schema")
            path = str(relative_path(occurrence.get("path")))
            ceiling = integer(occurrence.get("max_tokens"), "admitted token ceiling", 1)
            original = occurrence.get("source")
            if not isinstance(original, str) or not original:
                raise CheckError(f"{ident}: missing admitted source")
            for side in ("before", "after"):
                anchor = occurrence.get(side)
                if not isinstance(anchor, dict) or not isinstance(anchor.get("text"), str) or type(anchor.get("boundary")) is not bool:
                    raise CheckError(f"{ident}: invalid {side} anchor")
                if not anchor["text"] and not anchor["boundary"]:
                    raise CheckError(f"{ident}: empty nonboundary anchor")
            file = root / path
            if not file.exists():
                continue  # Removed profile; no finding can consume this slot.
            if file.is_symlink():
                raise CheckError(f"{ident}: symbolic-link admission path {path}")
            data = file.read_bytes()
            normalized = data.replace(b"\r\n", b"\n")
            a = anchor_offset(normalized, occurrence["before"], True)
            b = anchor_offset(normalized, occurrence["after"], False)
            if a is None or b is None or a >= b:
                continue
            if not reduced(normalized[a:b].decode(), original):
                continue
            # Convert normalized offsets back to raw report byte positions.
            def raw_offset(offset):
                # Mixed LF/CRLF files also preserve native byte coordinates.
                raw = 0
                for _ in range(offset):
                    raw += 2 if data[raw:raw + 2] == b"\r\n" else 1
                return raw
            raw_a, raw_b = raw_offset(a), raw_offset(b)
            resolved.append((ident, index, path, raw_a, raw_b, ceiling))
    return resolved


def admit(report, root, paths, baseline):
    validate_report(report, root, paths)
    admitted = envelopes(root, baseline)
    used, charged_tokens, exact, failures = {}, {}, set(), []
    for pair in report["duplicates"]:
        intervals = [report_interval(pair[side], root, paths) for side in ("firstFile", "secondFile")]
        matches = [[entry for entry in admitted if entry[2] == path and entry[3] <= a < b <= entry[4]]
                   for path, a, b in intervals]
        label = " <-> ".join(f"{pair[side]['name']}:{pair[side]['start']}-{pair[side]['end']}" for side in ("firstFile", "secondFile"))
        compatible = [(a, b) for a in matches[0] for b in matches[1] if a[0] == b[0] and a[1] != b[1]]
        if len(compatible) != 1:
            failures.append(f"{label}: no unique reviewed family pair (new, moved, grown, changed source or invalid anchors)")
            continue
        left, right = compatible[0]
        if pair["tokens"] > min(left[5], right[5]):
            failures.append(f"{label}: {left[0]} token ceiling exceeded ({pair['tokens']})")
            continue
        if left[1] > right[1]:
            left, right = right, left
            intervals.reverse()
        slot = left[0], left[1], right[1]
        identity = slot, tuple(intervals)
        if identity in exact:
            continue
        # Split shrinking reports may share a pair slot only without reusing
        # either side of the original source. Identical records are harmless.
        if any(any(max(old[i][1], intervals[i][1]) < min(old[i][2], intervals[i][2]) for i in (0, 1)) for old in used.get(slot, [])):
            failures.append(f"{label}: {left[0]} overlapping pair-slot reuse")
            continue
        tokens = charged_tokens.get(slot, 0) + pair["tokens"]
        if tokens > min(left[5], right[5]):
            failures.append(f"{label}: {left[0]} split pair token ceiling exceeded ({tokens})")
            continue
        exact.add(identity)
        used.setdefault(slot, []).append(intervals)
        charged_tokens[slot] = tokens
    return failures


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=("check", "report"), nargs="?", default="check")
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    root = args.root.resolve()
    try:
        config = detector_config(root)
        baseline = json.loads((root / "quality/duplication-baseline.json").read_text())
        source, tests = discover(root, metadata(root))
        command = native_command(root)
        with tempfile.TemporaryDirectory(prefix="duplication-check-") as temporary:
            destination = (args.output or root / "target/quality-reports").resolve() if args.mode == "report" else Path(temporary)
            report = scan(root, source, destination / "source", command, config)
            failures = admit(report, root, source, baseline)
            if tests:
                scan(root, tests, destination / "tests", command, config)
            else:
                # A persistent report directory may predate profile removal.
                # Never leave its old test scan looking like current evidence.
                for name in ("jscpd-report.json", "scan-config.json"):
                    (destination / "tests" / name).unlink(missing_ok=True)
                print("Dedicated-test scope absent; no detector scan selected.")
            for failure in failures:
                print(f"duplication: {failure}", file=sys.stderr)
            if args.mode == "report":
                print(f"Reports: {destination}; admission unchanged ({len(failures)} unadmitted pairs).")
            elif failures:
                return 1
            print(f"Duplication evaluated: {len(source)} gated files; {len(tests)} report-only test files.")
        return 0
    except (OSError, ValueError, KeyError, TypeError) as error:
        print(f"duplication: cannot evaluate: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
