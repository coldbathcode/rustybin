#!/usr/bin/env bash
# Fail when a tracked text file contains an em dash (U+2014) or an en dash
# (U+2013). Project rule: use commas, colons, parentheses or " - " instead.
# Binary files are skipped (-I). The pattern matches the UTF-8 bytes, so this
# script does not contain the characters itself.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"
if LC_ALL=C git grep -n -I -P '\xe2\x80[\x93\x94]' -- . ; then
  echo "error: em or en dash found (lines above); use , : ( ) or ' - ' instead" >&2
  exit 1
fi
echo "no em or en dashes in tracked text files"
