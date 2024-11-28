#!/usr/bin/env bash

set -eu -o pipefail

# Go to the worktree's root. (Even if the dir name ends in a newline.)
root_padded="$(git rev-parse --show-toplevel && echo -n .)"
root="${root_padded%$'\n.'}"
cd -- "$root"

symbolic_shebang="$(printf '#!' | od -An -ta)"
status=0

function check_item () {
  local mode="$1" oid="$2" path="$3" symbolic_magic

  # Extract the first two bytes (or less if shorter) and put in symbolic form.
  symbolic_magic="$(git cat-file blob "$oid" | od -N2 -An -ta)"

  # Check for inconsistency between the mode and whether `#!` is present.
  if [ "$mode" = 100644 ] && [ "$symbolic_magic" = "$symbolic_shebang" ]; then
    printf 'mode -x but has shebang: %q\n' "$path"
  elif [ "$mode" = 100755 ] && [ "$symbolic_magic" != "$symbolic_shebang" ]; then
    printf 'mode +x but no shebang: %q\n' "$path"
  else
    return 0
  fi

  status=1
}

readonly record_pattern='^([0-7]+) ([[:xdigit:]]+) [[:digit:]]+'$'\t''(.+)$'

# Check regular files named with a `.sh` suffix.
while IFS= read -rd '' record; do
  [[ $record =~ $record_pattern ]]
  mode="${BASH_REMATCH[1]}"
  oid="${BASH_REMATCH[2]}"
  path="${BASH_REMATCH[3]}"

  case "$mode" in
  100644 | 100755)
    check_item "${BASH_REMATCH[@]:1:3}"
    ;;
  esac
done < <(git ls-files -sz -- '*.sh')

exit "$status"
