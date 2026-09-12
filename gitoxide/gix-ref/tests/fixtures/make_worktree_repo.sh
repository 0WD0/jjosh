#!/usr/bin/env bash
set -eu -o pipefail

mkdir repo && cd repo
git init -q

git checkout -q -b main
git commit -q --allow-empty -m c1
git commit -q --allow-empty -m c2
git commit -q --allow-empty -m c3
git commit -q --allow-empty -m c4

git tag t1
git tag -m "tag object" dt1

git bisect start && git bisect bad HEAD

git update-ref refs/stacks/common :/c1


git worktree add ../w-detached HEAD~1
printf "gitdir: ../repo/.git/worktrees/w-detached\n" >../w-detached/.git
printf "../../../../w-detached/.git\n" >.git/worktrees/w-detached/gitdir
(
  cd ../w-detached
  git bisect start
  git bisect bad HEAD

  git update-ref refs/stacks/wtdetached :/c2
)

git worktree add ../w1
printf "gitdir: ../repo/.git/worktrees/w1\n" >../w1/.git
printf "../../../../w1/.git\n" >.git/worktrees/w1/gitdir
(
  cd ../w1
  git reset --hard HEAD~2

  git bisect start
  git bisect bad HEAD

  git update-ref refs/stacks/w1 :/c3
)

if [ "${1:-}"  = "packed" ]; then
  git pack-refs --all --prune
fi
