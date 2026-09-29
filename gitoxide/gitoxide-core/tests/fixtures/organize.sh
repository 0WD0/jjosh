#!/usr/bin/env bash
set -eu -o pipefail

# A standalone worktree keeps organization tests independent of the source checkout.
# HEAD and config suffice for discovery; the payload verifies that the worktree moves intact.
# Tests override origin only in their disposable copies to exercise path confinement.
git init -q source
git -C source config remote.origin.url https://example.com/owner/repository.git
printf '%s' 'repository contents' > source/payload
mkdir destination
