"""Shared helpers for the mock LLM conformance scripts."""

import os
import sys

BASE = os.environ.get("RUSTYBIN_URL", "http://127.0.0.1:18300").rstrip("/")

# Optional second instance started with RUSTYBIN_AI_API_KEY=conformance-secret
# (credential checks against an expected key); empty skips those checks.
AUTH_BASE = os.environ.get("RUSTYBIN_AUTH_URL", "").rstrip("/")

_failures = []
_passed = 0


def check(name, cond, detail=""):
    """Record one assertion (keeps going so every failure is reported)."""
    global _passed
    if cond:
        _passed += 1
        print(f"  ok   {name}")
    else:
        _failures.append(name)
        print(f"  FAIL {name} {detail}")


def section(title):
    print(f"\n== {title}")


def run(title, fn):
    """Run one test function; an exception counts as a failure."""
    section(title)
    try:
        fn()
    except Exception as e:  # noqa: BLE001 - report and continue
        check(f"{title} raised no exception", False, repr(e))


def finish():
    print(f"\n{_passed} passed, {len(_failures)} failed")
    if _failures:
        for f in _failures:
            print(f"  - {f}")
        sys.exit(1)
