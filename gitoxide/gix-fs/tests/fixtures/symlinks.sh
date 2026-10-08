#!/usr/bin/env bash
set -eu -o pipefail

# Prepare live and dangling relative file links without requiring test-time mutation.
# Create the dangling link while its target exists for native-strict Git Bash on Windows.
gix_testtools_require_symlinks
printf "contents" >file
ln -s file link
printf "contents" >removed-file
ln -s removed-file dangling-link
rm removed-file
