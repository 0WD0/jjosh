"""One rule set per local crate of jjosh's Cargo workspace.

Cargo.toml and Cargo.lock stay the declaration of dependencies and
features; a crate's BUILD file only says what Cargo.toml does not put in
@jjosh_crates: where the library starts, and whether it is a procedural
macro or has a build script.  tools/crate-builds.py writes those files.
"""

load("@jjosh_crates//:data.bzl", "DEP_DATA")
load("@jjosh_crates//:defs.bzl", "aliases", "all_crate_deps", "all_crate_features")
load("@rules_rs//rs:cargo_build_script.bzl", "cargo_build_script")
load("@rules_rust//rust:defs.bzl", "rust_library", "rust_proc_macro")

_LINUX = [
    "@platforms//os:linux",
    "@platforms//cpu:x86_64",
]

def jjosh_crate(package, crate_root = "src/lib.rs", proc_macro = False, build_script = None, rustc_env = {}):
    """Defines the library of the Cargo package PACKAGE in this directory.

    The target is named after the directory, as @jjosh_crates refers to it.
    """
    data = DEP_DATA[native.package_name()]
    name = native.package_name().rsplit("/", 1)[-1]
    srcs = native.glob(["**/*.rs"], allow_empty = True)
    compile_data = native.glob(
        ["**"],
        exclude = ["**/*.rs", "**/* *", "BUILD.bazel", "tests/**", "benches/**", "examples/**"],
        allow_empty = True,
    )
    common = dict(
        aliases = aliases(),
        compile_data = compile_data,
        crate_features = all_crate_features(),
        edition = data["edition"],
        rustc_env = {"CARGO_PKG_NAME": package} | rustc_env,
        srcs = srcs,
        target_compatible_with = _LINUX,
        version = data["version"],
    )
    deps = all_crate_deps(normal = True)
    if build_script:
        cargo_build_script(
            name = "_bs",
            crate_name = "build_script_build",
            crate_root = build_script,
            data = compile_data,
            pkg_name = package,
            deps = all_crate_deps(build = True),
            **common
        )
        deps = deps + [":_bs"]
    (rust_proc_macro if proc_macro else rust_library)(
        name = name,
        crate_name = data["crate_name"],
        crate_root = crate_root,
        visibility = ["//jjosh:__subpackages__"],
        deps = deps,
        **common
    )
