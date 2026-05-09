#!/usr/bin/env bash
set -eu -o pipefail

. "$(dirname -- "${BASH_SOURCE[0]}")/untracked_cache_empty.sh"
git status > /dev/null
