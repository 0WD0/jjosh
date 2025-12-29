#!/usr/bin/env bash
set -eu -o pipefail

# Initialize main git repo
git init -q
git config --local commit.gpgsign false
git config user.email "test@example.com"
git config user.name "Test User"

# Create a nested directory structure
mkdir -p some/path
TF=$(pwd)/some/path

# Create files in the nested path
touch "$TF/foo" "$TF/bar" "$TF/blah"
echo "merry" > "$TF/foo"
echo "christmas" > "$TF/bar"
git add some/path/foo some/path/bar
git commit -q -m "Initial commit"

# Modify a file to create a change
echo "new year" > "$TF/bar"

# Create a symlink pointing to the nested path
mkdir -p config
ln -s "$TF" config/repro_link
