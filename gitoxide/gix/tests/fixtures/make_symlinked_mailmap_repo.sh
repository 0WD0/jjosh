#!/usr/bin/env bash
set -eu -o pipefail

gix_testtools_require_symlinks

# Keep the target outside the repository but inside this disposable fixture.
echo "External Name <external@example.com>" >external-mailmap
mkdir repo
(
  cd repo
  git init -q
  git checkout -b main
  echo "Proper Name <proper@example.com>" >.mailmap
  git add .mailmap
  git commit -q -m "initial mailmap"

  # Replace the tracked regular file only in the worktree.
  rm .mailmap
  ln -s ../external-mailmap .mailmap
)
