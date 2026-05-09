#!/usr/bin/env bash
set -eu -o pipefail

export XDG_CONFIG_HOME=$(pwd)/config

git init -q

mkdir done dtwo dthree
touch one two three done/one dtwo/two dthree/three
git add one two done/one
: >.git/info/exclude
git update-index --untracked-cache
