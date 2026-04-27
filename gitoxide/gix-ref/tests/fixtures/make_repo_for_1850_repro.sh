#!/usr/bin/env bash
set -eu -o pipefail

git init -q
hex_len=40
hash_kind=$(git rev-parse --show-object-format)
if test "$hash_kind" = "sha256"; then
  hex_len=64
fi
oid () {
  if test "$hash_kind" = "sha1"; then
    printf "%s" "$2"
  else
    printf "%0.s$1" $(seq 1 "$hex_len")
  fi
}

cat <<EOF >.git/packed-refs
# pack-refs with: peeled fully-peeled sorted
$(oid 1 17dad46c0ce3be4d4b6d45def031437ab2e40666) refs/heads/ig-branch-remote
$(oid 2 83a70366fcc1255d35a00102138293bac673b331) refs/heads/ig-inttest
$(oid 3 3333333333333333333333333333333333333333) refs/heads/ig-pr4021
$(oid 4 d773228d0ee0012fcca53fffe581b0fce0b1dc56) refs/heads/ig/aliases
$(oid 5 ba37abe04f91fec76a6b9a817d40ee2daec47207) refs/heads/ig/cifail
EOF

mkdir -p .git/refs/heads/ig/pr
oid 6 d22f46f3d7d2504d56c573b5fe54919bd16be48a >.git/refs/heads/ig/push-name
oid 7 4dec145966c546402c5a9e28b932e7c8c939e01e >.git/refs/heads/ig-pr4021
