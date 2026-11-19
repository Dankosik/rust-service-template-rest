"""Tests for image publication metadata and published image rescanning."""

from __future__ import annotations

import hashlib
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.dont_write_bytecode = True

ROOT = Path(__file__).resolve().parents[2]


def _write_executable(path: Path, body: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("#!/bin/sh\n" + body + "\n", encoding="utf-8")
    path.chmod(0o755)


class PublishMetadataTests(unittest.TestCase):
    SHA = "0123456789abcdef0123456789abcdef01234567"
    CONFIG_ID = "sha256:" + "a" * 64
    OTHER_ID = "sha256:" + "b" * 64
    RUN_IMAGE = "ghcr.io/owner/service:build-42-1"

    def setUp(self) -> None:
        self.temp = Path(self.enterContext(tempfile.TemporaryDirectory(prefix="publish-image-")))
        self.bin_dir = self.temp / "bin"
        self.bin_dir.mkdir()
        self.summary = self.temp / "summary.md"
        self.github_output = self.temp / "github_output"
        self.export_json = self.temp / "export.json"
        self.manifest = self.temp / "manifest"
        self.state_file = self.temp / "docker_state.json"

        docker_stub = self.bin_dir / "docker"
        docker_stub.write_text(
            f'''#!/usr/bin/env python3
import json, pathlib, sys
state = json.loads(pathlib.Path({str(self.state_file)!r}).read_text(encoding="utf-8"))
args = sys.argv[1:]
if args[0] == "image":
    print(state["loaded_id"])
elif args[0] == "tag":
    sys.exit(0 if args[1] == state["expected_tag_source"] else 1)
elif args[0] == "push":
    if args[1] == state["run_image"] and state.get("push_lines"):
        print(state["push_lines"])
    sys.exit(1 if args[1] == state.get("fail_tag") else 0)
elif args[0] == "buildx":
    if args[3] == "--raw":
        if args[4] != state["run_image"]:
            sys.exit(1)
        sys.stdout.buffer.write(pathlib.Path(state["manifest_path"]).read_bytes())
    else:
        print(state["confirmed_digest"])
    sys.exit(1 if state.get("readback_failed") else 0)
else:
    sys.exit(1)
''',
            encoding="utf-8",
        )
        docker_stub.chmod(0o755)
        self.env = {
            **os.environ,
            "PATH": f"{self.bin_dir}{os.pathsep}{os.environ['PATH']}",
        }

    def run_publish(self, *args: str, extra_env: dict[str, str] | None = None) -> subprocess.CompletedProcess[str]:
        merged = dict(self.env)
        if extra_env:
            merged.update(extra_env)
        return subprocess.run(
            [sys.executable, str(ROOT / "scripts/image/publish_metadata.py"), *args],
            cwd=self.temp,
            env=merged,
            capture_output=True,
            text=True,
        )

    def write_docker_state(self, **overrides: object) -> None:
        base = {
            "loaded_id": self.CONFIG_ID,
            "expected_tag_source": self.CONFIG_ID,
            "run_image": self.RUN_IMAGE,
            "manifest_path": str(self.manifest),
            "push_lines": "",
            "confirmed_digest": "",
            "fail_tag": "",
            "readback_failed": False,
        }
        base.update(overrides)
        self.state_file.write_text(json.dumps(base), encoding="utf-8")

    def test_prepare_and_metadata_modes_and_refusals(self) -> None:
        main_res = self.run_publish(
            "prepare", "main", self.SHA, "Owner/Service", "main", "main", "refs/heads/main", "42", "3", "0.1.0"
        )
        self.assertEqual(main_res.returncode, 0, main_res.stderr)
        expected_main = (
            "image_repo=ghcr.io/owner/service\n"
            "primary_image=ghcr.io/owner/service:build-42-3\n"
            "version=0.1.0\n"
            "promote_tags=sha-0123456789ab main\n"
            "artifact_name=sbom-main-0123456789ab-42-3\n"
            "identity=https://github.com/Owner/Service/.github/workflows/cd.yml@refs/heads/main\n"
        )
        self.assertEqual(main_res.stdout, expected_main)

        (self.temp / "Cargo.toml").write_text('[workspace.package]\nversion = "1.2.3"\n', encoding="utf-8")
        svc_toml = self.temp / "crates/service/Cargo.toml"
        svc_toml.parent.mkdir(parents=True, exist_ok=True)
        svc_toml.write_text('[package]\nname = "service"\nversion.workspace = true\n', encoding="utf-8")

        prep_res = self.run_publish(
            "prepare",
            extra_env={
                "MODE": "release",
                "CANDIDATE_SHA": self.SHA,
                "GITHUB_REPOSITORY": "Owner/Service",
                "DEFAULT_BRANCH": "main",
                "GITHUB_REF_NAME": "v1.2.3",
                "GITHUB_REF": "refs/tags/v1.2.3",
                "GITHUB_RUN_ID": "42",
                "GITHUB_RUN_ATTEMPT": "4",
                "GITHUB_OUTPUT": str(self.github_output),
                "PUBLISH_REPO_ROOT": str(self.temp),
            },
        )
        self.assertEqual(prep_res.returncode, 0, prep_res.stderr)
        expected_release = (
            "image_repo=ghcr.io/owner/service\n"
            "primary_image=ghcr.io/owner/service:build-42-4\n"
            "version=1.2.3\n"
            "promote_tags=v1.2.3 latest\n"
            "artifact_name=sbom-v1.2.3-42-4\n"
            "identity=https://github.com/Owner/Service/.github/workflows/cd.yml@refs/tags/v1.2.3\n"
        )
        self.assertEqual(self.github_output.read_text(encoding="utf-8"), expected_release)

        for bad_args in (
            ("prepare", "main", self.SHA, "Owner/Service", "main", "main", "refs/tags/main", "42", "1", "0.1.0"),
            ("prepare", "main", "invalid", "Owner/Service", "main", "", "", "42", "1", "0.1.0"),
            ("prepare", "release", self.SHA, "Owner/Service", "main", "v1/bad", "refs/tags/v1/bad", "42", "1", "1.0.0"),
            ("prepare", "release", self.SHA, "Owner/Service", "main", "v1.2.4", "refs/tags/v1.2.4", "42", "1", "1.2.3"),
            ("prepare", "main", self.SHA, "Owner/Service", "main", "", "", "42", "1", "not-a-version"),
            ("prepare", "unknown", self.SHA, "Owner/Service", "main", "", "", "42", "1", "0.1.0"),
        ):
            with self.subTest(bad_args=bad_args):
                self.assertNotEqual(self.run_publish(*bad_args).returncode, 0)

    def test_identify_push_and_promote_verification(self) -> None:
        manifest_bytes = f'{{"schemaVersion":2,"config":{{"digest":"{self.CONFIG_ID}"}},"layers":[]}}\n\n'.encode()
        self.manifest.write_bytes(manifest_bytes)
        digest = "sha256:" + hashlib.sha256(manifest_bytes).hexdigest()
        self.export_json.write_text(
            json.dumps(
                {
                    "containerimage.digest": digest,
                    "containerimage.descriptor": {
                        "digest": digest,
                        "mediaType": "application/vnd.oci.image.manifest.v1+json",
                    },
                    "containerimage.config.digest": self.CONFIG_ID,
                }
            ),
            encoding="utf-8",
        )
        push_line = f"build-42-1: digest: {digest} size: 123"

        for kind, loaded in (("config", self.CONFIG_ID), ("descriptor", digest)):
            with self.subTest(kind=kind):
                self.write_docker_state(loaded_id=loaded, push_lines=push_line, confirmed_digest=digest)
                ident = self.run_publish("identify", self.RUN_IMAGE, str(self.export_json))
                self.assertEqual(ident.returncode, 0, ident.stderr)
                self.assertEqual(ident.stdout, f"image_id={loaded}\nid_kind={kind}\n")
                pushed = self.run_publish("push", self.RUN_IMAGE, loaded, kind)
                self.assertEqual(pushed.returncode, 0, pushed.stderr)
                self.assertEqual(pushed.stdout.strip(), digest)

        # Replaced run tag or mismatched subject refuses.
        self.write_docker_state(loaded_id=self.OTHER_ID, push_lines=push_line, confirmed_digest=digest)
        self.assertNotEqual(self.run_publish("identify", self.RUN_IMAGE, str(self.export_json)).returncode, 0)
        self.assertNotEqual(self.run_publish("push", self.RUN_IMAGE, self.CONFIG_ID, "config").returncode, 0)
        self.assertNotEqual(self.run_publish("push", self.RUN_IMAGE, self.OTHER_ID, "config").returncode, 0)
        self.assertNotEqual(self.run_publish("push", self.RUN_IMAGE, self.OTHER_ID, "descriptor").returncode, 0)

        # Failed push / readback / duplicate or wrong-tag push line refuses.
        self.write_docker_state(
            loaded_id=self.CONFIG_ID,
            push_lines=push_line,
            confirmed_digest=digest,
            fail_tag=self.RUN_IMAGE,
        )
        self.assertNotEqual(self.run_publish("push", self.RUN_IMAGE, self.CONFIG_ID, "config").returncode, 0)
        self.write_docker_state(
            loaded_id=self.CONFIG_ID,
            push_lines=push_line,
            confirmed_digest=digest,
            readback_failed=True,
        )
        self.assertNotEqual(self.run_publish("push", self.RUN_IMAGE, self.CONFIG_ID, "config").returncode, 0)
        self.write_docker_state(
            loaded_id=self.CONFIG_ID,
            push_lines=f"{push_line}\n{push_line}",
            confirmed_digest=digest,
        )
        self.assertNotEqual(self.run_publish("push", self.RUN_IMAGE, self.CONFIG_ID, "config").returncode, 0)
        self.write_docker_state(
            loaded_id=self.CONFIG_ID,
            push_lines=f"another-tag: digest: {digest} size: 123",
            confirmed_digest=digest,
        )
        self.assertNotEqual(self.run_publish("push", self.RUN_IMAGE, self.CONFIG_ID, "config").returncode, 0)

        # Altered registry bytes refuse.
        self.manifest.write_bytes(manifest_bytes + b"\n")
        self.write_docker_state(loaded_id=self.CONFIG_ID, push_lines=push_line, confirmed_digest=digest)
        self.assertNotEqual(self.run_publish("push", self.RUN_IMAGE, self.CONFIG_ID, "config").returncode, 0)

        # OCI image index refuses config ID, admits descriptor ID.
        index_bytes = (
            f'{{"schemaVersion":2,"mediaType":"application/vnd.oci.image.index.v1+json",'
            f'"manifests":[{{"digest":"{digest}"}}]}}'
        ).encode()
        self.manifest.write_bytes(index_bytes)
        index_digest = "sha256:" + hashlib.sha256(index_bytes).hexdigest()
        index_push_line = f"build-42-1: digest: {index_digest} size: 155"
        self.export_json.write_text(
            json.dumps(
                {
                    "containerimage.digest": index_digest,
                    "containerimage.descriptor": {
                        "digest": index_digest,
                        "mediaType": "application/vnd.oci.image.index.v1+json",
                    },
                    "containerimage.config.digest": self.CONFIG_ID,
                }
            ),
            encoding="utf-8",
        )
        self.write_docker_state(loaded_id=self.CONFIG_ID, push_lines=index_push_line, confirmed_digest=index_digest)
        self.assertNotEqual(self.run_publish("identify", self.RUN_IMAGE, str(self.export_json)).returncode, 0)
        self.assertNotEqual(self.run_publish("push", self.RUN_IMAGE, self.CONFIG_ID, "config").returncode, 0)

        self.write_docker_state(loaded_id=index_digest, push_lines=index_push_line, confirmed_digest=index_digest)
        self.assertEqual(self.run_publish("identify", self.RUN_IMAGE, str(self.export_json)).returncode, 0)
        self.assertEqual(self.run_publish("push", self.RUN_IMAGE, index_digest, "descriptor").returncode, 0)

        # Promote and promote-summary verification & partial failure receipts.
        self.write_docker_state(confirmed_digest=index_digest)
        ok_promote = self.run_publish(
            "promote-summary",
            extra_env={
                "IMAGE_REPO": "ghcr.io/owner/service",
                "IMAGE_ID": self.CONFIG_ID,
                "DIGEST": index_digest,
                "PROMOTE_TAGS": "sha-0123456789ab main",
                "GITHUB_STEP_SUMMARY": str(self.summary),
            },
        )
        self.assertEqual(ok_promote.returncode, 0, ok_promote.stderr)
        self.assertIn(f"Confirmed tag: ghcr.io/owner/service:main at {index_digest}", self.summary.read_text())

        self.summary.write_text("", encoding="utf-8")
        self.write_docker_state(confirmed_digest=index_digest, fail_tag="ghcr.io/owner/service:main")
        second_fail = self.run_publish(
            "promote-summary",
            "ghcr.io/owner/service",
            self.CONFIG_ID,
            index_digest,
            "sha-0123456789ab main",
            str(self.summary),
        )
        self.assertNotEqual(second_fail.returncode, 0)
        self.assertIn("Promoted tags before failure: sha-0123456789ab", self.summary.read_text(encoding="utf-8"))

        self.summary.write_text("", encoding="utf-8")
        self.write_docker_state(confirmed_digest=index_digest, fail_tag="ghcr.io/owner/service:sha-0123456789ab")
        first_fail = self.run_publish(
            "promote-summary",
            "ghcr.io/owner/service",
            self.CONFIG_ID,
            index_digest,
            "sha-0123456789ab main",
            str(self.summary),
        )
        self.assertNotEqual(first_fail.returncode, 0)
        self.assertIn("Promoted tags before failure: none", self.summary.read_text(encoding="utf-8"))

        self.summary.write_text("", encoding="utf-8")
        self.write_docker_state(confirmed_digest=self.OTHER_ID)
        wrong_digest = self.run_publish(
            "promote-summary",
            "ghcr.io/owner/service",
            self.CONFIG_ID,
            index_digest,
            "sha-0123456789ab main",
            str(self.summary),
        )
        self.assertNotEqual(wrong_digest.returncode, 0)


class PublishedImageScanTests(unittest.TestCase):
    @unittest.skipUnless(shutil.which("jq") and shutil.which("openssl"), "published image scan requires jq and openssl")
    def test_scans_each_deployable_digest_across_pages_despite_findings(self) -> None:
        root = Path(self.enterContext(tempfile.TemporaryDirectory(prefix="published-image-scan-")))
        first, other, cache = ("sha256:" + digit * 64 for digit in ("1", "2", "3"))

        def version(digest: str, *tags: str) -> dict[str, object]:
            return {"name": digest, "metadata": {"container": {"tags": tags}}}

        pages = [[version(first, "main")], [version(first, "latest"), version(other, "v1.2.0"), version(cache, "buildcache")]]
        (root / "versions.json").write_text(json.dumps(pages), encoding="utf-8")
        _write_executable(
            root / "gh",
            '''case "$*" in
  "api repos/owner/service "*) echo Organization ;;
  "api --paginate --slurp orgs/owner/packages/container/service/versions"*) cat "$FIXTURE/versions.json" ;;
  *) exit 1 ;;
esac''',
        )
        _write_executable(
            root / "docker",
            '''for arg do image=$arg; done
case "$image" in
  ghcr.io) cat >/dev/null ;;
  --download-db-only) exit 0 ;;
  ghcr.io/owner/service@*)
    printf '%s\\n' "$image" >>"$FIXTURE/scans"
    [ "$image" != "$FINDING_IMAGE" ] || exit 42 ;;
  *) exit 1 ;;
esac''',
        )
        finding_image = f"ghcr.io/owner/service@{first}"
        result = subprocess.run(
            ["bash", str(ROOT / "scripts/image/rescan.sh")],
            env={
                **os.environ,
                "PATH": f"{root}{os.pathsep}{os.environ['PATH']}",
                "FIXTURE": str(root),
                "FINDING_IMAGE": finding_image,
                "GH_TOKEN": "registry-fixture",
                "GITHUB_ACTOR": "owner",
                "GITHUB_REPOSITORY": "owner/service",
                "GITHUB_STEP_SUMMARY": str(root / "summary"),
            },
            capture_output=True,
            text=True,
        )
        self.assertEqual(result.returncode, 1, result.stderr)
        scans = (root / "scans").read_text(encoding="utf-8").splitlines()
        self.assertCountEqual(scans, [finding_image, f"ghcr.io/owner/service@{other}"])
        summary = (root / "summary").read_text(encoding="utf-8")
        self.assertIn(f"{first}` | latest, main | HIGH or CRITICAL findings require attention", summary)
        self.assertIn(f"{other}` | v1.2.0 | No HIGH or CRITICAL findings", summary)
