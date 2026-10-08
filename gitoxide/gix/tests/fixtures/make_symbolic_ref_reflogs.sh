#!/usr/bin/env bash
set -eu -o pipefail

# Three distinct main reflog entries exercise indexed lookups and dates before the log begins.
git init -q
export GIT_COMMITTER_DATE="2000-01-01 00:00:00 +0000"
git commit -q --allow-empty -m initial
export GIT_COMMITTER_DATE="2000-01-02 00:00:00 +0000"
git commit -q --allow-empty -m second
export GIT_COMMITTER_DATE="2000-01-03 00:00:00 +0000"
git commit -q --allow-empty -m third

# References outside the branch namespace have no own log, even through a symbolic chain.
# A symbolic branch gets one entry, which must take precedence over the longer target log.
git symbolic-ref refs/symref refs/heads/main
git symbolic-ref refs/symref-chain refs/symref
git symbolic-ref refs/heads/symref refs/heads/main

# Bake Git's object IDs or failure exit codes into the shared revspec baseline format.
function baseline() {
  printf '%s\n' "$1" >>baseline.git
  git rev-parse -q --verify "$1" >>baseline.git 2>/dev/null || echo $? >>baseline.git
}

for name in main refs/symref refs/symref-chain; do
  for query in 0 1 2 '1979-02-26 00:00:00 +0000'; do
    baseline "$name@{$query}"
  done
done
baseline 'refs/heads/symref@{1}'
