#!/usr/bin/env bash
set -eu -o pipefail

# Initialize main git repo
git init -q
git config --local commit.gpgsign false
git config user.email "test@example.com"
git config user.name "Test User"

# Create a symlink and add it to git
ln -s target broken_link
git add broken_link
git commit -q -m "Add symlink"

# Now break the symlink by removing the target
# The symlink exists but points to nothing
# Note: the symlink was stored in the index, so git knows about it
