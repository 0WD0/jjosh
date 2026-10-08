#!/usr/bin/env bash
set -eu -o pipefail

git init module1
(cd module1
  touch tracked
  git add tracked
  git commit -m "init"
)

git init with-submodule
(cd with-submodule
  git submodule add ../module1 m1
  git commit -m "add submodule"
)

# Cloning without submodule recursion leaves m1 as an ordinary directory with a
# gitlink in the index, but no repository. Its contents must not make status dirty.
git clone --no-local with-submodule uninitialized
(cd uninitialized
  printf '%s' content >m1/untracked
)
