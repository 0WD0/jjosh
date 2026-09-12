#!/usr/bin/env bash
set -eu -o pipefail

git init -q

git checkout -b main
touch this
git add this
git commit -q -m c1
echo hello >> this
git commit -q -am c2

mkdir -p some/very/deeply/nested/subdir
mkdir -p some-with-file/very/deeply/nested/subdir
touch some-with-file/very/deeply/nested/subdir/empty-file

git init --bare bare.git

git init --bare bare-repo-with-index.git
(cd bare-repo-with-index.git
  touch index
)

git init -q repo.git
(cd repo.git
  touch this
  git add this
  git commit -q -m "init non-bare repo with git suffix"
)

git init non-bare-repo-without-index
(cd non-bare-repo-without-index
  touch this
  git add this && git commit -m "init"
  rm .git/index
)

git init all-untracked
(cd all-untracked
  >a
  mkdir d
  >d/a
)

git init empty-core-excludes
(cd empty-core-excludes
  echo $'[core]\n\texcludesFile = ' >> .git/config
)

git clone --bare . non-bare-without-worktree
(cd non-bare-without-worktree
  git config core.bare false
)

git init unborn;
