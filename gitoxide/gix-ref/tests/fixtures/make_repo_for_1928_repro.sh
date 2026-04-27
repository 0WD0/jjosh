#!/usr/bin/env bash
set -eu -o pipefail

git init -q
hex_len=40
if test "$(git rev-parse --show-object-format)" = "sha256"; then
  hex_len=64
fi
oid () {
  printf "%0.s$1" $(seq 1 "$hex_len")
}

mkdir -p .git/refs/heads/a
cat <<EOF >.git/packed-refs
# pack-refs with: peeled fully-peeled sorted
$(oid 1) refs/heads/a-
$(oid 2) refs/heads/a/b
$(oid 3) refs/heads/a0
EOF

mkdir -p .git/refs/heads/a
oid a >.git/refs/heads/a-
oid b >.git/refs/heads/a/b
oid c >.git/refs/heads/a0
