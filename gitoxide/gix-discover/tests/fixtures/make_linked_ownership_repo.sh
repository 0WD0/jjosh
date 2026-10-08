#!/usr/bin/env bash
set -eu -o pipefail

# Each of the checkout, gitfile, private git directory, and common git directory must limit trust.
# Run in the final fixture directory: Git writes absolute paths into linked-worktree metadata.
git init -q main
git -C main commit -q --allow-empty -m initial
git -C main worktree add -q --detach ../linked
