#!/usr/bin/env bash
set -eu -o pipefail

# Generate on macOS and preserve the archive: Git precomposes index paths differently elsewhere.
git init
git config core.precomposeUnicode true
mkdir Teaching

# Each umlaut is decomposed (U + U+0308). The emoji and umlaut separately work;
# their combination makes Git's UTF-8-MAC conversion fail and leaves the filename decomposed.
printf 'content' >"📹Ü.md"
printf 'content' >"Ü📹.md"
printf 'content' >"Ü.md"
printf 'content' >"📹.md"
printf 'content' >"❤Ü.md"
printf 'content' >"Teaching/🎥 Überwachung im digitalen Zeitalter.md"

git add --all
git commit -m initial
