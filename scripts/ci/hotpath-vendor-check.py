#!/usr/bin/env python3
"""Remote-only provenance check of the deliberately patched published source."""
import hashlib
import io
import pathlib
import tarfile
import tomllib
import urllib.request

url = 'https://static.crates.io/crates/hotpath/hotpath-0.28.4.crate'
with urllib.request.urlopen(url, timeout=30) as response:
    archive = response.read()
assert hashlib.sha256(archive).hexdigest() == '92f8d56370b4cf47b04cf873315d2d0a226a3766ca74f5131521b7aa8bc3d41f'
root = pathlib.Path('vendor/hotpath')
omitted = {'Cargo.lock', 'Cargo.toml.orig'}
checked = 0
published_paths = set()
with tarfile.open(fileobj=io.BytesIO(archive), mode='r:gz') as source:
    for entry in source.getmembers():
        if not entry.isfile():
            continue
        relative = pathlib.PurePosixPath(entry.name).relative_to('hotpath-0.28.4')
        published_paths.add(str(relative))
        if str(relative) in omitted:
            continue
        pristine = source.extractfile(entry).read()
        actual = (root / relative).read_bytes()
        if str(relative) == 'Cargo.toml':
            expected = tomllib.loads(pristine.decode())
            expected['dependencies']['rmcp']['version'] = '=2.1.0'
            assert tomllib.loads(actual.decode()) == expected
        elif str(relative) == 'src/mcp_server.rs':
            assert pristine.count(b'    model::*,\n') == 1
            assert actual == pristine.replace(b'    model::*,\n', b'    model::{ContentBlock as Content, *},\n')
        else:
            assert actual == pristine, str(relative)
        checked += 1
actual_paths = {str(path.relative_to(root)) for path in root.rglob('*') if path.is_file()}
assert actual_paths == (published_paths - omitted) | {'PATCHES.md'}
print(f'Published hotpath0.28.4 verified: {checked} files; only manifest and MCP import changed')
