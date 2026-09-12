#!/usr/bin/env bash
set -eu -o pipefail

# A SHA-256 remote, created explicitly with `--object-format=sha256` so it stays SHA-256 regardless of
# `GIX_TEST_FIXTURE_HASH`. Its HEAD is direct so clone doesn't rewrite configuration as a side
# effect of branch setup, which lets tests verify that object-format adoption preserves the
# configured remote by itself.

git init --object-format=sha256 -q remote
(
  cd remote
  echo hello >file
  git add file
  git commit -q -m "initial"
  git checkout --detach -q HEAD
)
