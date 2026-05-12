#!/usr/bin/env bash
set -eu -o pipefail

git init -q

mkdir tracked-dir untracked-dir-2 untracked-dir-3
touch tracked-root-one tracked-root-two untracked-root-file \
    tracked-dir/tracked-file \
    untracked-dir-2/untracked-file-two \
    untracked-dir-3/untracked-file-three
git add tracked-root-one tracked-root-two tracked-dir/tracked-file
: >.git/info/exclude
git update-index --untracked-cache
