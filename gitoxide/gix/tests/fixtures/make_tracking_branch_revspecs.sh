#!/usr/bin/env bash
set -eu -o pipefail

# Explicit HEAD and main selectors must use the current branch tracking configuration.
# Record Git results here so the read-only test never needs to invoke Git.
git init -q
git commit -q --allow-empty -m initial
git remote add origin .
git config branch.main.remote origin
git config branch.main.merge refs/heads/main
git fetch -q origin

for op in upstream u push; do
  for branch in '' HEAD main; do
    revspec="$branch@{$op}"
    printf '%s\n' "$revspec" >>baseline.git
    git rev-parse --verify "$revspec" >>baseline.git
  done
done
