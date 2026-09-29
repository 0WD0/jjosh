#!/usr/bin/env bash
set -eu -o pipefail

# In ordinary and bare repositories, the discovery candidate is also the git directory.
# Ownership tests must check that path once, plus the checkout for a non-bare repository.
git init -q worktree
git init -q --bare bare
