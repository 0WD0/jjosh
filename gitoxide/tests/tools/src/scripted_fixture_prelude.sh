# Sourced into the Bash instance executing a fixture, not installed in its environment.
__gix_testtools_fixture_root=$PWD

gix_testtools_require_symlinks() {
  local probe status
  probe=$(mktemp -d "$__gix_testtools_fixture_root/.gix-symlink-preflight.XXXXXX") || return
  if ! printf "probe\n" >"$probe/target" || ! command -v ln >/dev/null; then
    rm -rf "$probe"
    return 1
  fi
  if ln -s target "$probe/link" 2>"$probe/error"; then
    if ! test -L "$probe/link"; then
      echo "symlink preflight: ln succeeded without creating a symbolic link" >&2
      rm -rf "$probe"
      return 1
    fi
    printf "supported\n" >"$__gix_testtools_fixture_root/__gix_testtools_symlinks__" || {
      rm -rf "$probe"
      return 1
    }
    rm -rf "$probe" || return
  else
    status=$?
    case "$status" in
      1) ;;
      *)
        cat "$probe/error" >&2
        rm -rf "$probe"
        return 1
        ;;
    esac
    { printf "unsupported\n"; cat "$probe/error"; } >"$__gix_testtools_fixture_root/__gix_testtools_symlinks__" || {
      rm -rf "$probe"
      return 1
    }
    rm -rf "$probe" || return
    exit 0
  fi
}
