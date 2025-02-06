#!/usr/bin/env bash
set -eu -o pipefail

git init -q
echo content >file
ln -s file link

echo binary >exe && chmod +x exe
mkfifo fifo
