#!/usr/bin/env bash
set -eu -o pipefail

# Dot-dot normalization must skip the inner repository and discover the outer one.
# Both repositories are fixtures, not the potentially foreign-owned source checkout.
git init -q .
git init -q inner
mkdir -p inner/some/very/deeply/nested/subdir
