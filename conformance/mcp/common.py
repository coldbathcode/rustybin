"""Shared helpers for the MCP conformance scripts (official Python MCP SDK)."""

import os
import sys
import traceback

BASE = os.environ.get("MCP_BASE", "http://127.0.0.1:18400").rstrip("/")

_failures: list[str] = []
_passed = 0


def check(cond: bool, what: str) -> None:
    global _passed
    if cond:
        _passed += 1
        print(f"  ok   {what}")
    else:
        _failures.append(what)
        print(f"  FAIL {what}")


def section(name: str) -> None:
    print(f"\n== {name}")


def text_of(result) -> str:
    return "\n".join(getattr(c, "text", "") or "" for c in result.content)


def run(main) -> None:
    """Run an async main, print a summary and exit non-zero on failures."""
    import asyncio
    import warnings

    warnings.simplefilter("ignore", DeprecationWarning)
    try:
        asyncio.run(main())
    except Exception:  # noqa: BLE001 - report any SDK rejection as a failure
        traceback.print_exc()
        _failures.append("unhandled exception")
    print(f"\n{_passed} passed, {len(_failures)} failed")
    if _failures:
        for f in _failures:
            print(f"  - {f}")
        sys.exit(1)
