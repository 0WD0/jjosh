#!/usr/bin/env bash
set -eu -o pipefail

git init -q
git checkout -b main

echo "Proper Name <proper@example.com>" >.mailmap

git add .mailmap
git commit -q -m "initial mailmap"
