#!/usr/bin/env bash

# Remap local Rust source paths to stable virtual roots for release builds.
# Run from the workspace root; requires Bash, cargo, rustc, and jq:
#   bash etc/scripts/remap-release-paths.sh build cargo build --release --locked --bins
#   bash etc/scripts/remap-release-paths.sh check target/release/gix target/release/ein
# `build` also accepts cross; `check` returns nonzero for known local path leaks
# or scan errors (warning-only in release CI). This is best effort; see DEVELOPMENT.md.

set -euo pipefail

release_path_aliases() {
    local path="$1" destination="$2" canonical
    if command -v cygpath >/dev/null 2>&1; then
        path="$(cygpath -am -- "$path")" || return
    fi
    printf '%s\t%s\n' "$path" "$destination"
    printf '%s\t%s\n' "${path//\//\\}" "$destination"
    if [[ -d "$path" ]]; then
        canonical="$(cd -- "$path" && pwd -P)" || return
        if command -v cygpath >/dev/null 2>&1; then
            canonical="$(cygpath -am -- "$canonical")" || return
        fi
        printf '%s\t%s\n' "$canonical" "$destination"
        printf '%s\t%s\n' "${canonical//\//\\}" "$destination"
    fi
}

release_local_roots() {
    local home="${USERPROFILE:-$HOME}" target_dir sysroot
    target_dir="$(cargo metadata --locked --no-deps --format-version=1 | jq -br .target_directory)" || return
    sysroot="$(rustc --print sysroot)" || return
    sysroot="${sysroot//$'\r'/}"
    release_path_aliases "$home" /build-home || return
    release_path_aliases "${RUSTUP_HOME:-$home/.rustup}" /rustup || return
    release_path_aliases "${CARGO_HOME:-$home/.cargo}" /cargo || return
    release_path_aliases "$PWD" /gitoxide || return
    if [[ -n "${GITHUB_WORKSPACE:-}" ]]; then
        release_path_aliases "$GITHUB_WORKSPACE" /gitoxide || return
    fi
    release_path_aliases "$sysroot" /rust || return
    release_path_aliases "$target_dir" /generated
}

release_remap_flags() {
    local roots="$1" source destination
    {
        if [[ -n "$roots" ]]; then
            printf '%s\n' "$roots"
        fi
        release_path_aliases "${TMPDIR:-${TEMP:-/tmp}}" /build-tmp || return
        # cross 0.2.5 container mounts; newer cross versions may retain host paths.
        printf '%s\n' $'/project\t/gitoxide' $'/cargo\t/cargo' $'/rust\t/rust' $'/target\t/generated' $'target\t/generated'
    } | awk -F '\t' '{ print length($1) "\t" $0 }' | sort -n | cut -f2- |
        while IFS=$'\t' read -r source destination; do
            # rustc uses the last matching rule, so nested roots must follow parents.
            printf '%s\n' "--remap-path-prefix=$source=$destination"
        done
}

release_build() {
    local roots="$1" flags flag remaps
    shift
    if [[ "${CARGO_ENCODED_RUSTFLAGS+x}" ]]; then
        flags="$CARGO_ENCODED_RUSTFLAGS"
    else
        # Cargo splits RUSTFLAGS on whitespace; shell quotes are not interpreted.
        flags="$(printf '%s' "${RUSTFLAGS:-}" | awk '{ for (i = 1; i <= NF; i++) { printf "%s%s", sep, $i; sep = "\037" } }')" || return
    fi
    remaps="$(release_remap_flags "$roots")" || return
    while IFS= read -r flag; do
        if [[ -n "$flags" ]]; then
            flags+=$'\x1f'
        fi
        flags+="$flag"
    done <<< "$remaps"
    # Keep the parent environment unchanged, including flags used to install cross.
    # Git Bash must not rewrite virtual roots when launching native Cargo on Windows.
    MSYS2_ENV_CONV_EXCL="${MSYS2_ENV_CONV_EXCL:+$MSYS2_ENV_CONV_EXCL;}CARGO_ENCODED_RUSTFLAGS" \
        CARGO_ENCODED_RUSTFLAGS="$flags" "$@"
}

release_check() {
    local roots="$1" binary source destination prefix grep_status status=0
    shift
    for binary in "$@"; do
        [[ -f "$binary" ]] || { printf 'missing release binary: %s\n' "$binary" >&2; return 1; }
        while IFS=$'\t' read -r source destination; do
            prefix="${source%/}/"
            if [[ "$source" == *\\* ]]; then
                prefix="${source%\\}\\"
            fi
            if LC_ALL=C grep -aFq -- "$prefix" "$binary"; then
                printf '%s: embedded local build path prefix: %s\n' "$binary" "$prefix" >&2
                status=1
            else
                grep_status=$?
                if [[ "$grep_status" -ne 1 ]]; then
                    printf '%s: could not scan release binary\n' "$binary" >&2
                    return "$grep_status"
                fi
            fi
        done < <(
            printf '%s\n' "$roots"
            printf '%s\n' $'/project\t/gitoxide'
            # `/target/` alone is also a legitimate Git ignore-pattern literal in ein.
            printf '%s\t/generated\n' "/target/${TARGET:-release-github}" /target/debug /target/release /target/release-github /target/build
        )
    done
    return "$status"
}

release_paths_main() {
    local operation="${1:-}" roots
    if [[ "$#" -lt 2 ]] || [[ "$operation" != build && "$operation" != check ]]; then
        printf 'usage: %s build COMMAND [ARGS...] | check BINARY...\n' "$0" >&2
        return 2
    fi
    shift
    roots="$(release_local_roots)" || return
    if [[ "$operation" == build ]]; then
        release_build "$roots" "$@"
    else
        release_check "$roots" "$@"
    fi
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
    release_paths_main "$@"
fi
