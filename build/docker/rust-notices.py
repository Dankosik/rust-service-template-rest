import json
import re
import subprocess
import sys
import tomllib
from collections import defaultdict
from pathlib import Path
from tempfile import TemporaryDirectory

ABOUT_CONFIG = Path(__file__).with_suffix(".toml")
NOTICE_FILE = re.compile(r"(?:LICENSE|LICENCE|NOTICE|COPYING|COPYRIGHT)(?:[._-].+)?")

def main(target: str, output: Path) -> None:
    with Path("deny.toml").open("rb") as handle:
        policy = tomllib.load(handle)["licenses"]
    with TemporaryDirectory() as temporary:
        config = Path(temporary) / "about.toml"
        config.write_text(
            f'accepted = {json.dumps(policy["allow"])}\n' + ABOUT_CONFIG.read_text(encoding="utf-8"),
            encoding="utf-8",
        )
        report = json.loads(
            subprocess.check_output(
                [
                    "cargo", "about", "generate", "--locked", "--fail", "--workspace",
                    "--target", target, "--config", str(config),
                    "--threshold", str(policy["confidence-threshold"]), "--format", "json",
                ]
            )
        )

    package_roots = {Path(entry["package"]["manifest_path"]).parent.resolve() for entry in report["crates"]}
    notices: dict[str, set[str]] = defaultdict(set)
    for license in report["licenses"]:
        if source := license["source_path"]:
            path = Path(source)
            if path.is_absolute() and not any(path.resolve().is_relative_to(root) for root in package_roots):
                sys.exit(f"cargo-about selected a license outside the scanned sources: {source}")
        text = license["text"].rstrip()
        for used_by in license["used_by"]:
            crate = used_by["crate"]
            notices[text].add(f'{crate["name"]} {crate["version"]} [{license["id"]}]')

    for entry in report["crates"]:
        package = entry["package"]
        directory = Path(package["manifest_path"]).parent
        for path in directory.rglob("*"):
            source = path.relative_to(directory)
            if not path.is_symlink() and path.is_file() and (
                NOTICE_FILE.fullmatch(path.name) or "LICENSES" in source.parts[:-1]
            ):
                text = path.read_text(encoding="utf-8").rstrip()
                if text:
                    notices[text].add(f'{package["name"]} {package["version"]} ({source.as_posix()})')

    if not report["licenses"]:
        sys.exit("cargo-about returned no Rust dependency licenses")
    with output.open("w", encoding="utf-8") as handle:
        print(f"Third-party Rust dependency notices for {target}\n", file=handle)
        print(
            "The default-feature workspace graph covers the shipped binaries and can include additional crates.\n"
            "Build-only and dev-only dependency edges are excluded. Upstream packaged notice files are included.\n",
            file=handle,
        )
        for sources, text in sorted((sorted(sources), text) for text, sources in notices.items()):
            print("---", *sources, "", text, "", sep="\n", file=handle)


if __name__ == "__main__":
    main(sys.argv[1], Path(sys.argv[2]))
