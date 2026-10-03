#!/usr/bin/env python3
"""Check the documentation's example includes.

Every `{{#include path:anchor}}` in docs/src must point at an existing file and
anchor (mdBook silently renders a missing anchor as an empty block), and every
anchor defined under docs/examples must be shown on some page. Exit code 1 on
problems.
"""
import pathlib
import re
import sys

DOCS = pathlib.Path(__file__).resolve().parent.parent
INCLUDE = re.compile(r"(?<!\\)\{\{#include ([^:}]+)(?::([A-Za-z0-9_]+))?\}\}")
ANCHOR = re.compile(r"ANCHOR: ([A-Za-z0-9_]+)")

problems = []
used = set()
for page in sorted((DOCS / "src").rglob("*.md")):
    for m in INCLUDE.finditer(page.read_text()):
        target = (page.parent / m.group(1)).resolve()
        if not target.is_file():
            problems.append(f"{page.relative_to(DOCS)}: missing file {m.group(1)}")
            continue
        anchor = m.group(2)
        if anchor:
            if anchor not in ANCHOR.findall(target.read_text()):
                problems.append(f"{page.relative_to(DOCS)}: no anchor {anchor} in {m.group(1)}")
            used.add((target, anchor))

for example in sorted((DOCS / "examples").rglob("*")):
    if example.suffix not in (".hurl", ".sh"):
        continue
    for anchor in ANCHOR.findall(example.read_text()):
        if (example.resolve(), anchor) not in used:
            problems.append(f"{example.relative_to(DOCS)}: anchor {anchor} is not shown on any page")

for p in problems:
    print(p)
print(f"{len(used)} included examples, {len(problems)} problems")
sys.exit(1 if problems else 0)
