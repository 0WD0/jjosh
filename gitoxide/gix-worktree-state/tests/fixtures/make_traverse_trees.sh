#!/usr/bin/env bash
set -eu -o pipefail

# Makes a repo carrying a tree structure representing the given path to a blob.
# File content is from stdin. Args are repo name, path, -x or +x, and tr sets.
function make_repo() (
  local repo="$1" path="$2" xbit="$3" set1="$4" set2="$5"
  local dir dir_standin path_standin

  git init -- "$repo"
  cd -- "$repo" # Temporary, as the function body is a ( ) subshell.

  dir="${path%/*}"
  dir_standin="$(tr "$set1" "$set2" <<<"$dir")"
  path_standin="$(tr "$set1" "$set2" <<<"$path")"
  mkdir -p -- "$dir_standin"
  cat >"$path_standin"
  # The index stores this mtime as 0x3a000d0a, including CRLF bytes. Text-mode sed
  # on Windows would drop the CR and corrupt the index when rewriting it below.
  TZ=UTC touch -t 200011011231.06 -- "$path_standin"
  git add --chmod="$xbit" -- "$path_standin"
  cp .git/index old_index
  # Perl supports binary I/O on all platforms, including those without sed -b.
  perl -0777 -pe '
    BEGIN { binmode STDIN; binmode STDOUT; ($from, $to) = splice @ARGV, 0, 2 }
    s/\Q$from\E/$to/g
  ' "$path_standin" "$path" <old_index >.git/index
  git commit -m 'Initial commit'
)

make_repo traverse_dotdot_trees '../outside' -x '.' '@' \
  <<<'A file outside the working tree, somehow.'

make_repo traverse_dotgit_trees '.git/hooks/pre-commit' +x '.' '@' <<'EOF'
#!/bin/sh
printf 'Vulnerable!\n'
date >vulnerable
EOF

make_repo traverse_dotgit_stream '.git::$INDEX_ALLOCATION/hooks/pre-commit' +x ':' ',' <<'EOF'
#!/bin/sh
printf 'Vulnerable!\n'
date >vulnerable
EOF
