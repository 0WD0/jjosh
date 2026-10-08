#!/usr/bin/env bash
set -eu -o pipefail

gix_testtools_require_symlinks

# Both variants keep the link target outside the repo, within the fixture.
echo external >external-ignore
mkdir repo
(
  cd repo
  git init -q
  case "$1" in
    indexed)
      # Preserve a regular index blob with skip-worktree while replacing its worktree file.
      echo indexed >.gitignore
      git add .gitignore
      git update-index --skip-worktree .gitignore
      rm .gitignore
      ln -s ../external-ignore .gitignore
      ;;
    symlink)
      # An indexed symlink must not provide ignore patterns through index fallback.
      ln -s ../external-ignore .gitignore
      git add .gitignore
      ;;
    *)
      echo "expected indexed or symlink" >&2
      exit 1
      ;;
  esac

  # Record each path's exit status: 0 is ignored, 1 is not ignored, and other exits are errors.
  for path in external indexed; do
    if git check-ignore "$path" >"../$path.git-check-ignore.out"; then
      status=0
    else
      status=$?
      test "$status" -eq 1 || exit "$status"
    fi
    printf '%s\n' "$status" >"../$path.git-check-ignore.status"
  done
)
