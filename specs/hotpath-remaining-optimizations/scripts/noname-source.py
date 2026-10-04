#!/usr/bin/env python3
"""Remote-only attribution source: restore just the old eager span name."""
import pathlib

path = pathlib.Path('/root/remaining/baseline/crates/infra-http/src/observe.rs')
source = path.read_text()
bindings = '    let name_route = route.unwrap_or_default().trim_end();\n    let separator = if name_route.is_empty() { "" } else { " " };\n'
field = '        otel.name = %format_args!("{method}{separator}{name_route}"),'
assert source.count(bindings) == source.count(field) == 1
source = source.replace(bindings, '').replace(field,
    '        otel.name = format!("{method} {}", route.unwrap_or_default()).trim(),')
path.write_text(source)
