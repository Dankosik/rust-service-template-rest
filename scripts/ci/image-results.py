#!/usr/bin/env python3
"""Admit selected native image jobs; never consume image receipts as authority."""
from __future__ import annotations

import copy
import json
import os
from pathlib import Path
import sys


def admit(needs: dict, draft: str) -> list[str]:
    changes = needs.get("changes", {})
    if changes.get("result") != "success":
        raise ValueError("image selection did not succeed")
    outputs = changes.get("outputs", {})
    runtime = outputs.get("runtime_image")
    graphs = outputs.get("artifact_graphs")
    if runtime not in ("true", "false") or not isinstance(graphs, str):
        raise ValueError("missing image selection outputs")
    if draft not in ("true", "false"):
        raise ValueError("missing draft policy")
    if draft == "true":
        return ["Image proof deferred by draft PR policy."]
    lines = []
    # The selected derived command validates and executes the distinct graph
    # set. Native job success is its result; this aggregate does not re-parse it.
    for name, selected in (("image-source", runtime == "true"), ("image-derived", bool(graphs))):
        if not selected:
            continue
        lane = needs.get(name, {})
        if lane.get("result") != "success":
            raise ValueError(f"{name}: selected lane did not succeed")
        proof = lane.get("outputs", {})
        for key in ("artifact-id", "artifact-digest"):
            if not isinstance(proof.get(key), str) or not proof[key].strip():
                raise ValueError(f"{name}: missing required {key}")
        attempt = proof.get("producing-attempt", "")
        if not isinstance(attempt, str) or not attempt.isdecimal() or int(attempt) < 1:
            raise ValueError(f"{name}: missing producing attempt")
        # needs belongs to this run/candidate/selection. A successful lane from
        # an earlier attempt survives rerun-failed-jobs; do not relabel it.
        lines.append(f"{name}: artifact {proof['artifact-id']}, archive digest "
                     f"{proof['artifact-digest']}, producing attempt {attempt}, "
                     f"{proof.get('artifact-url', '')}")
    return lines or ["No image lane selected."]


def self_test() -> None:
    lane = {"result": "success", "outputs": {
        "artifact-id": "42", "artifact-digest": "fixture-archive-digest",
        "artifact-url": "https://example.invalid/artifacts/42", "producing-attempt": "1",
    }}
    baseline = {"changes": {"result": "success", "outputs": {
        "runtime_image": "true", "artifact_graphs": "1,7,47,65",
    }}, "image-source": copy.deepcopy(lane), "image-derived": copy.deepcopy(lane)}
    # Native prior-attempt output is retained verbatim, even when the aggregate
    # runs in attempt 2. No artifact content or independent receipt is supplied.
    previous = os.environ.get("GITHUB_RUN_ATTEMPT")
    os.environ["GITHUB_RUN_ATTEMPT"] = "2"
    try:
        assert len(admit(baseline, "false")) == 2
        assert all("producing attempt 1" in line for line in admit(baseline, "false"))
    finally:
        if previous is None:
            os.environ.pop("GITHUB_RUN_ATTEMPT")
        else:
            os.environ["GITHUB_RUN_ATTEMPT"] = previous
    for name in ("changes", "image-source", "image-derived"):
        for result in ("failure", "cancelled", "skipped", ""):
            case = copy.deepcopy(baseline)
            case[name]["result"] = result
            try:
                admit(case, "false")
            except ValueError:
                pass
            else:
                raise AssertionError(f"accepted {name} result {result!r}")
    for name in ("image-source", "image-derived"):
        for key in ("artifact-id", "artifact-digest", "producing-attempt"):
            case = copy.deepcopy(baseline)
            del case[name]["outputs"][key]
            try:
                admit(case, "false")
            except ValueError as error:
                assert key in str(error) or key == "producing-attempt"
            else:
                raise AssertionError(f"accepted missing {name} {key}")
    unselected = copy.deepcopy(baseline)
    unselected["changes"]["outputs"] = {"runtime_image": "false", "artifact_graphs": ""}
    del unselected["image-source"]
    del unselected["image-derived"]
    assert admit(unselected, "false") == ["No image lane selected."]
    deferred = copy.deepcopy(baseline)
    deferred["image-source"] = deferred["image-derived"] = {"result": "skipped"}
    assert "draft" in admit(deferred, "true")[0]
    print("native image aggregate self-test: pass")


def main() -> int:
    if sys.argv[1:] == ["--self-test"]:
        self_test()
        return 0
    if sys.argv[1:]:
        raise ValueError("usage: image-results.py [--self-test]")
    lines = admit(json.loads(os.environ["IMAGE_NEEDS"]), os.environ["IMAGE_DRAFT"])
    summary = "\n".join(lines) + "\n"
    print(summary, end="")
    if path := os.environ.get("GITHUB_STEP_SUMMARY"):
        with Path(path).open("a") as stream:
            stream.write(summary)
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (ValueError, KeyError, TypeError, AttributeError) as error:
        print(f"image admission failed: {error}", file=sys.stderr)
        sys.exit(1)
