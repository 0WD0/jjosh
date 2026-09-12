#!/usr/bin/env bash
set -eu -o pipefail

git init -q

touch empty
echo -n "content" > executable
chmod +x executable
echo -n "filtered content" > filtered-executable
chmod +x filtered-executable

mkdir dir
echo "other content" > dir/content
echo "* filter=arrow" > .gitattributes
echo "executable -filter" >> .gitattributes
echo ".gitattributes -filter" >> .gitattributes

mkdir dir/sub-dir
echo "even other content" > dir/sub-dir/file

git add -A
git update-index --chmod=+x executable filtered-executable  # For Windows.
git commit -m "Commit"
