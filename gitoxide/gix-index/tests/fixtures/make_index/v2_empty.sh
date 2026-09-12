#!/usr/bin/env bash
set -eu -o pipefail

git init -q

# Create an empty index.
case ${GIX_TEST_FIXTURE_HASH:-sha1} in
  sha1)
    git read-tree 4b825dc642cb6eb9a060e54bf8d69288fbee4904 ;;
  sha256)
    git read-tree 6ef19b41225c5369f1c104d45d8d85efa9b057b53b14b4b9b939dd74decc5321 ;;
  *)
    exit 1 ;;
esac
