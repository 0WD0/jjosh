#!/usr/bin/env bash
set -eu -o pipefail

# Simulate unavailable links and broken fixture setup without changing process-global state.
case "${1:?preflight scenario}" in
  supported) ;;
  unsupported)
    ln() { echo "simulated symlink permission denied" >&2; return 1; }
    ;;
  copy)
    ln() { printf "copied target contents" >"$3"; }
    ;;
  failure) ;;
  crash)
    ln() { echo "simulated crashing ln" >&2; return 139; }
    ;;
  unexpected)
    ln() { echo "simulated unexpected ln error" >&2; return 2; }
    ;;
  *) exit 1 ;;
esac

# The result belongs to the fixture root, not the directory the script has entered.
mkdir nested
cd nested
gix_testtools_require_symlinks
test "$0" = "${BASH_SOURCE[0]}"
printf "%s" "$1" >scenario

# The injected helper must not leak into child Bash instances.
bash -c "! declare -F gix_testtools_require_symlinks"
if test "$1" = failure; then
  echo "intentional failure after preflight" >&2
  exit 1
fi
printf "contents" >target
ln -s target link
