#!/usr/bin/env python3
"""Rebase the realm's exact-count assertions after new intrinsics are installed.

The realm graph's own totals live in `realm.rs`; every test that pins a count,
a per-test budget derived from one, or an atomic limit-failure pair has to move
by the same deltas. Usage:

    python3 scripts/rebase_realm_counts.py --functions +3 --properties +9 --atoms +2 --code-units +18
"""

import argparse
import re
import sys
from pathlib import Path

FILES = [
    "crates/quickjs-runtime/src/runtime/tests.rs",
    "crates/quickjs-runtime/src/vm_tests.rs",
    "crates/quickjs-runtime/tests/error_runtime.rs",
    "crates/quickjs-runtime/tests/symbol_values.rs",
    "crates/quickjs-runtime/tests/vm_execution.rs",
    "crates/quickjs-runtime/tests/vm_installation.rs",
    "crates/quickjs-runtime/tests/vm_objects.rs",
    "crates/quickjs-runtime/tests/vm_realm_globals.rs",
]


def shift_calls(text, pattern, delta):
    return re.sub(pattern, lambda m: m.group(1) + str(int(m.group(2)) + delta), text)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--functions", type=int, default=0)
    parser.add_argument("--properties", type=int, default=0)
    parser.add_argument("--objects", type=int, default=0)
    parser.add_argument("--atoms", type=int, default=0)
    parser.add_argument("--code-units", type=int, default=0)
    parser.add_argument("--root", type=Path, default=Path("."))
    args = parser.parse_args()

    # A single-realm count and a two-realm limit both move by the same delta per
    # realm, so the two-realm tables in error_runtime.rs use twice the delta.
    for name in FILES:
        path = args.root / name
        if not path.exists():
            print(f"missing {path}", file=sys.stderr)
            continue
        text = original = path.read_text()
        per_realm = 2 if name.endswith("error_runtime.rs") else 1
        for pattern, delta in [
            (r"(object_properties\(\),\s*)(\d+)", args.properties),
            (r"(with_max_object_properties\()(\d+)", args.properties * per_realm),
            (r"(heap_functions\(\),\s*)(\d+)", args.functions),
            (r"(with_max_heap_functions\()(\d+)", args.functions * per_realm),
            (r"(heap_objects\(\),\s*)(\d+)", args.objects),
            (r"(with_max_heap_objects\()(\d+)", args.objects * per_realm),
        ]:
            if delta:
                text = shift_calls(text, pattern, delta)
        for constant, delta in [
            ("REALM_ERROR_GRAPH_FUNCTIONS", args.functions),
            ("REALM_ERROR_GRAPH_PROPERTIES", args.properties),
            ("REALM_ERROR_GRAPH_OBJECTS", args.objects),
            ("REALM_DYNAMIC_ATOMS", args.atoms),
            ("REALM_DYNAMIC_ATOM_CODE_UNITS", args.code_units),
            ("REALM_DYNAMIC_INTERNER_SLOTS", args.atoms),
        ]:
            if delta:
                text = re.sub(
                    rf"(const {constant}: u\d+ = )(\d+)",
                    lambda m: m.group(1) + str(int(m.group(2)) + delta),
                    text,
                )
        for prefix, delta in [
            ("PREDEFINED_ATOM_COUNT + ", args.atoms),
            ("PREDEFINED_DESCRIPTION_CODE_UNITS + ", args.code_units),
            ("PREDEFINED_INTERNER_SLOTS + ", args.atoms),
        ]:
            if delta:
                text = re.sub(
                    rf"({re.escape(prefix)})(\d+)",
                    lambda m: m.group(1) + str(int(m.group(2)) + delta),
                    text,
                )
        # An atomic limit-failure table reports the configured maximum as the
        # limit, so mirror it rather than shifting it independently.
        text = re.sub(
            r"(RuntimeLimits::default\(\)\.with_max_(?:object_properties|heap_functions|heap_objects)"
            r"\((\d+)\),\n\s*RuntimeResource::(?:ObjectProperties|HeapFunctions|HeapObjects),\n(\s*))"
            r"\d+,\n\s*\d+,",
            lambda m: f"{m.group(1)}{m.group(2)},\n{m.group(3)}{int(m.group(2)) + 1},",
            text,
        )
        # A `matches!` arm on the same limit does too, so mirror the nearest
        # preceding configured maximum into its `limit`/`observed` pair — but
        # only when the arm names the matching resource, since one test can
        # configure several limits and assert on a different one.
        resources = {
            "object_properties": "ObjectProperties",
            "heap_functions": "HeapFunctions",
            "heap_objects": "HeapObjects",
        }
        configured = None
        lines = text.splitlines(keepends=True)
        for index, line in enumerate(lines):
            found = re.search(
                r"with_max_(object_properties|heap_functions|heap_objects)\((\d+)\)", line
            )
            if found:
                configured = (resources[found.group(1)], int(found.group(2)))
                continue
            if configured is None:
                continue
            if re.search(r"with_max_\w+\(", line):
                # Another limit follows, so the arm's resource is ambiguous.
                configured = None
                continue
            resource = re.match(r"\s*resource: RuntimeResource::(\w+),\n", line)
            if resource:
                if resource.group(1) != configured[0]:
                    configured = None
                continue
            limit = re.match(r"(\s*)limit: (\d+),\n", line)
            if limit and index + 1 < len(lines):
                observed = re.match(r"(\s*)observed: (\d+),\n", lines[index + 1])
                if observed:
                    lines[index] = f"{limit.group(1)}limit: {configured[1]},\n"
                    lines[index + 1] = f"{observed.group(1)}observed: {configured[1] + 1},\n"
                    configured = None
        text = "".join(lines)
        if text != original:
            path.write_text(text)
            print(f"updated {name}")


if __name__ == "__main__":
    main()
