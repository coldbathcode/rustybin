#!/usr/bin/env python3
"""Check local links, fragments and assets in the built mdBook (no dependencies).

Run after `mdbook build docs`. External links are deliberately not requested.
"""
from html.parser import HTMLParser
from pathlib import Path
import sys
from urllib.parse import unquote, urlsplit


class Page(HTMLParser):
    def __init__(self, source):
        super().__init__(convert_charrefs=True)
        self.ids = set()
        self.links = []
        self.feed(source)

    def handle_starttag(self, tag, attrs):
        attrs = dict(attrs)
        if attrs.get("id"):
            self.ids.add(attrs["id"])
        for attr in ("href", "src"):
            value = attrs.get(attr)
            if value:
                self.links.append(value)


def check(book):
    book = book.resolve()
    pages = {p: Page(p.read_text()) for p in sorted(book.rglob("*.html"))}
    problems = set()
    count = 0
    for source, page in pages.items():
        for link in page.links:
            url = urlsplit(link)
            if url.scheme or url.netloc:
                continue
            count += 1
            target = (
                book / unquote(url.path).lstrip("/")
                if url.path.startswith("/")
                else source.parent / unquote(url.path) if url.path else source
            ).resolve()
            if target.is_dir():
                target /= "index.html"
            if not target.is_file():
                problems.add(f"{source.relative_to(book)}: missing target {link}")
            elif url.fragment and target in pages:
                if unquote(url.fragment) not in pages[target].ids:
                    problems.add(f"{source.relative_to(book)}: missing fragment {link}")
    for problem in sorted(problems):
        print(problem)
    print(f"{len(pages)} pages, {count} local links and assets, {len(problems)} problems")
    return bool(problems)


if __name__ == "__main__":
    book = Path(sys.argv[1]).resolve() if len(sys.argv) > 1 else Path(__file__).resolve().parents[1] / "book"
    if not (book / "index.html").is_file():
        sys.exit("Build the documentation first: mdbook build docs")
    sys.exit(check(book))
