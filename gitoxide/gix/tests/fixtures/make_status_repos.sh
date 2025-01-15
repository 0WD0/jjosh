#!/usr/bin/env bash
set -eu -o pipefail

git init -q untracked-only
(cd untracked-only
  touch this
  mkdir subdir
  >subdir/that

  git add .
  git commit -q -m init

  mkdir new
  touch new/untracked subdir/untracked
)

git init git-mv
(cd git-mv
  echo hi > file
  git add file && git commit -m "init"

  git mv file renamed
)

git init racy-git
(cd racy-git
  echo hi >file
  git add file && git commit -m "init"

  echo ho >file && git add file
  echo ha >file
)

git init untracked-unborn
(cd untracked-unborn
  touch untracked
)