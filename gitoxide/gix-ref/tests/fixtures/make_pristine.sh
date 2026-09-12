#!/usr/bin/env bash
set -eu -o pipefail

git init untouched

git init changed-headref
(cd changed-headref
  echo "ref: refs/heads/other" >.git/HEAD
)

git init detached
(cd detached
  if test "$(git rev-parse --show-object-format)" = "sha256"; then
    printf "%0.sa" $(seq 1 64) >.git/HEAD
  else
    echo "abcdefabcdefabcdefabcdefabcdefabcdefabcd" >.git/HEAD
  fi
)

git init invalid-loose-ref
(cd invalid-loose-ref
  touch .git/refs/heads/empty
)
