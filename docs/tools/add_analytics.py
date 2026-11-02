#!/usr/bin/env python3
"""Add the Cloudflare Web Analytics beacon to every page of the built book.

Usage: python3 docs/tools/add_analytics.py docs/book <token>

The docs workflow runs it only when the repository variable
DOCS_ANALYTICS_TOKEN is set (the site token of a Web Analytics site in the
Cloudflare dashboard). The beacon sets no cookies and keeps no personal data,
so the site needs no consent banner. Without the variable the book is
published as built.
"""
import pathlib
import re
import sys


def main() -> int:
    if len(sys.argv) != 3:
        print(__doc__.strip(), file=sys.stderr)
        return 2
    root, token = pathlib.Path(sys.argv[1]), sys.argv[2].strip()
    if not re.fullmatch(r"[0-9a-f]{32}", token):
        print("the token is 32 hexadecimal characters (the site token of Web Analytics)", file=sys.stderr)
        return 1
    beacon = (
        '<script defer src="https://static.cloudflareinsights.com/beacon.min.js" '
        f"data-cf-beacon='{{\"token\": \"{token}\"}}'></script>\n"
    )
    pages = 0
    for page in root.rglob("*.html"):
        text = page.read_text(encoding="utf-8")
        if "static.cloudflareinsights.com" in text or "</head>" not in text:
            continue
        page.write_text(text.replace("</head>", beacon + "</head>", 1), encoding="utf-8")
        pages += 1
    print(f"analytics beacon added to {pages} page(s)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
