#!/bin/sh
set -eu

# A merge plus an unmerged topic makes multi-tip reachability and de-duplication observable.
git init -q -b main .
git config user.name author
git config user.email author@example.com

commit () {
  echo "$1" >"$1"
  git add "$1"
  GIT_AUTHOR_DATE="$2 +0000" GIT_COMMITTER_DATE="$2 +0000" git commit -q -m "$1"
}

commit root "2000-01-01T00:00:00"
git tag -a v1 -m v1
git switch -q -c merged
commit merged "2000-01-02T00:00:00"
git switch -q main
commit main "2000-01-03T00:00:00"
git merge -q --no-edit merged
git switch -q -c topic HEAD~1
commit topic "2000-01-04T00:00:00"
git switch -q main
