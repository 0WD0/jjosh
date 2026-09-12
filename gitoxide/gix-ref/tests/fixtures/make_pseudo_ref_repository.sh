#!/usr/bin/env bash
set -eu -o pipefail

git init -q
git commit -m "init" --allow-empty

git rev-parse HEAD > .git/JIRI_HEAD
touch .git/SOME_ALL_CAPS_FILE
touch .git/refs/SHOULD_BE_EXCLUDED_HEAD

head_id=$(git rev-parse HEAD)
cat <<EOF >> .git/FETCH_HEAD
$head_id		branch 'main' of https://github.com/Byron/gitoxide
$head_id	not-for-merge	branch 'faster-discovery' of https://github.com/Byron/gitoxide
EOF
