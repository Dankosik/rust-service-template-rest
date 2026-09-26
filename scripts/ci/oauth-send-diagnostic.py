#!/usr/bin/env python3
"""Temporary, compile-only OAuth Send discriminators for the branch CI job."""

import argparse
import difflib
import hashlib
import json
import os
from pathlib import Path
import signal
import subprocess
import time


INITIALIZER = """        // Give Moka a Send initializer without propagating nested opaque futures.
        // It borrows this owner and remains lazy until elected by the cache.
        let init: Pin<
            Box<dyn Future<Output = Result<Arc<CachedCredential>, FillError>> + Send + '_>,
        > = Box::pin(self.0.fetch(deadline));
        let result = tokio::time::timeout_at(deadline, self.0.cache.try_get_with((), init))
"""

HOOK = """struct TokenHttpClient<'owner> {
    owner: &'owner Owner,
    deadline: Instant,
}

impl<'client, 'owner: 'client> oauth2::AsyncHttpClient<'client> for TokenHttpClient<'owner> {
    type Error = AcquisitionError;
    type Future =
        Pin<Box<dyn Future<Output = Result<oauth2::HttpResponse, Self::Error>> + Send + 'client>>;

    fn call(&'client self, request: oauth2::HttpRequest) -> Self::Future {
        self.owner.exchange(request, self.deadline)
    }
}

"""


def replace_once(source, old, new):
    count = source.count(old)
    if count != 1:
        raise ValueError(f"expected exactly one source anchor, found {count}: {old!r}")
    return source.replace(old, new, 1)


def variants(pristine):
    concrete = replace_once(
        pristine,
        "        let hook = |request| self.exchange(request, deadline);\n",
        "        let hook = TokenHttpClient { owner: self, deadline };\n",
    )
    concrete = replace_once(concrete, "struct CachedCredential {\n", HOOK + "struct CachedCredential {\n")
    direct = replace_once(
        concrete,
        INITIALIZER,
        """        let result = tokio::time::timeout_at(
            deadline,
            self.0.cache.try_get_with((), self.0.fetch(deadline)),
        )
""",
    )
    owned = replace_once(
        pristine,
        INITIALIZER,
        """        // The elected initializer owns the same private credential owner.
        let init: Pin<
            Box<dyn Future<Output = Result<Arc<CachedCredential>, FillError>> + Send>,
        > = Box::pin(Arc::clone(&self.0).fetch(deadline));
        let result = tokio::time::timeout_at(deadline, self.0.cache.try_get_with((), init))
""",
    )
    owned = replace_once(
        owned,
        "    async fn fetch(&self, caller_deadline: Instant) -> Result<Arc<CachedCredential>, FillError> {\n",
        "    async fn fetch(self: Arc<Self>, caller_deadline: Instant) -> Result<Arc<CachedCredential>, FillError> {\n",
    )
    return [
        ("00-baseline", "Unchanged typed closure and borrowed boxed initializer.", pristine),
        ("01-concrete-hook", "Explicit AsyncHttpClient lifetime; retain borrowed boxed initializer.", concrete),
        ("02-concrete-hook-direct-initializer", "Same concrete hook; remove only the initializer box.", direct),
        ("03-owned-initializer", "Original typed closure; change only initializer ownership to Arc.", owned),
    ]


def write_json(path, value):
    path.write_text(json.dumps(value, indent=2) + "\n", encoding="utf-8")


def stop(process):
    if process.poll() is None:
        os.killpg(process.pid, signal.SIGTERM)
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGKILL)
            process.wait()


def check(root, directory, command, timeout):
    started = time.monotonic()
    result = {"command": command, "timeout_seconds": timeout, "timed_out": False}
    process = None
    try:
        with (directory / "stdout.log").open("wb") as stdout, (directory / "stderr.log").open("wb") as stderr:
            process = subprocess.Popen(
                command, cwd=root, stdout=stdout, stderr=stderr, start_new_session=True,
            )
            try:
                result["exit_code"] = process.wait(timeout=timeout)
            except subprocess.TimeoutExpired:
                result.update(timed_out=True, exit_code=124)
            finally:
                stop(process)
    except BaseException as error:
        result["runner_error"] = f"{type(error).__name__}: {error}"
        raise
    finally:
        result["process_exit_code"] = None if process is None else process.returncode
        result["elapsed_seconds"] = round(time.monotonic() - started, 3)
        write_json(directory / "result.json", result)
    return result


def interrupt(signum, _frame):
    raise KeyboardInterrupt(f"received signal {signum}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output_dir", type=Path)
    parser.add_argument("--timeout-seconds", type=int, default=240)
    args = parser.parse_args()
    if args.timeout_seconds <= 0:
        parser.error("--timeout-seconds must be positive")

    root = Path(__file__).resolve().parents[2]
    source_path = root / "crates/infra-oauth2-client-credentials/src/lib.rs"
    pristine_bytes = source_path.read_bytes()
    pristine = pristine_bytes.decode("utf-8")
    candidates = variants(pristine)
    output = args.output_dir.resolve()
    output.mkdir(parents=True, exist_ok=False)
    (output / "pristine.rs").write_bytes(pristine_bytes)
    command = ["cargo", "check", "--locked", "-p", "infra-oauth2-client-credentials", "--all-features", "--lib"]
    summary = {
        "source": str(source_path.relative_to(root)),
        "pristine_sha256": hashlib.sha256(pristine_bytes).hexdigest(),
        "command": command,
        "results": [],
        "compiled_candidates": [],
    }
    signal.signal(signal.SIGTERM, interrupt)
    try:
        for name, meaning, source in candidates:
            directory = output / name
            directory.mkdir()
            (directory / "lib.rs").write_text(source, encoding="utf-8")
            (directory / "patch.diff").write_text(
                "".join(difflib.unified_diff(
                    pristine.splitlines(keepends=True), source.splitlines(keepends=True),
                    fromfile="a/" + summary["source"], tofile="b/" + summary["source"],
                )), encoding="utf-8",
            )
            source_path.write_text(source, encoding="utf-8")
            print(f"{name}: {meaning}", flush=True)
            result = check(root, directory, command, args.timeout_seconds)
            summary["results"].append({"name": name, "meaning": meaning, **result})
            if name != "00-baseline" and result["exit_code"] == 0:
                summary["compiled_candidates"].append(name)
            print(f"{name}: exit={result['exit_code']} elapsed={result['elapsed_seconds']}s", flush=True)
    except BaseException as error:
        summary["runner_error"] = f"{type(error).__name__}: {error}"
        raise
    finally:
        source_path.write_bytes(pristine_bytes)
        summary["source_restored"] = source_path.read_bytes() == pristine_bytes
        write_json(output / "summary.json", summary)

    print(json.dumps(summary, indent=2), flush=True)
    return 0 if summary["compiled_candidates"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
