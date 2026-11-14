"""Exercise the OpenAPI Make recipe against per-invocation Cargo messages."""

import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]


class OpenApiGeneration(unittest.TestCase):
    def test_publish_only_the_current_successful_artifact(self):
        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory)
            messages = work / "messages"
            output = work / "committed contract.yaml"
            generator = work / "custom target" / "release" / "openapi"
            generator.parent.mkdir(parents=True)
            generator.write_text('#!/bin/sh\ntouch "$GENERATOR_RAN"\nprintf "generated\\n"\nexit "$GENERATOR_STATUS"\n')
            generator.chmod(0o755)
            ran = work / "ran"
            artifact = {
                "reason": "compiler-artifact",
                "manifest_path": str(ROOT / "crates/service/Cargo.toml"),
                "target": {"name": "openapi", "kind": ["bin"]},
                "profile": {"test": False},
                "executable": str(generator),
                "fresh": True,
            }
            unrelated = [
                {**artifact, "manifest_path": str(ROOT / "test/Cargo.toml")},
                {**artifact, "profile": {"test": True}},
                {**artifact, "target": {"name": "openapi", "kind": ["example"]}},
            ]
            for label, records, build_status, generator_status, invoked, published in [
                ("warm build", [*unrelated, artifact], 0, 0, True, True),
                ("duplicate", [artifact, artifact], 0, 0, False, False),
                ("stale executable on disk", unrelated, 0, 0, False, False),
                ("build failed after artifact", [artifact], 1, 0, False, False),
                ("generator failed after output", [artifact], 0, 1, True, False),
            ]:
                with self.subTest(label):
                    output.write_bytes(b"committed\n")
                    ran.unlink(missing_ok=True)
                    messages.write_text("".join(json.dumps(record) + "\n" for record in records))
                    result = subprocess.run(
                        ["make", "openapi-generate", f"OPENAPI_FILE={output}",
                         """BUILD_CARGO=sh -c 'cat "$$CARGO_MESSAGES"; exit "$$CARGO_STATUS"'"""],
                        cwd=ROOT, capture_output=True, text=True,
                        env=dict(os.environ, CARGO_MESSAGES=str(messages), CARGO_STATUS=str(build_status),
                                 GENERATOR_RAN=str(ran), GENERATOR_STATUS=str(generator_status)),
                    )
                    self.assertEqual(result.returncode == 0, published, result.stderr)
                    self.assertEqual(ran.exists(), invoked)
                    self.assertEqual(output.read_bytes(), b"generated\n" if published else b"committed\n")
                    self.assertEqual(list(work.glob(".openapi.*")), [])


if __name__ == "__main__":
    unittest.main()
