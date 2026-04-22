#!/bin/bash
set -eu

root=$1
output_corpus=$2
corpus_dir=$(readlink -f -- "$root/gix-index/fuzz/corpus/index_file")

cd "$corpus_dir"
find . -type f | sort | zip -q -@ "$output_corpus"
