"""jjosh's local crates; tools/crate-builds.py writes their BUILD files."""

load("@jjosh_crates//:data.bzl", "DEP_DATA")
load("@jjosh_crates//:defs.bzl", "aliases", "all_crate_deps", "all_crate_features")
load("//bazel:cargo_crate.bzl", "cargo_crate")

_HUB = struct(
    DEP_DATA = DEP_DATA,
    aliases = aliases,
    all_crate_deps = all_crate_deps,
    all_crate_features = all_crate_features,
)

def jjosh_crate(**arguments):
    cargo_crate(_HUB, visibility = ["//jjosh:__subpackages__"], **arguments)
