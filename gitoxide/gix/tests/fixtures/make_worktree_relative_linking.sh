#!/usr/bin/env bash
set -eu -o pipefail

make_repo_with_relative_worktree_links() {
  local base=$1

  mkdir -p "$base"
  git -C "$base" init -q main
  (
    cd "$base/main"
    git commit -q --allow-empty -m init
    git worktree add -q --detach --relative-paths ../linked HEAD
  )
}

make_repo_with_relative_worktree_links .
make_repo_with_relative_worktree_links actual

# The symlinked main-repository variant is only used by Unix tests. On platforms
# where creating symlinks is unavailable, keep the rest of the fixture useful.
ln -s actual/main main-symlink 2>/dev/null || true
