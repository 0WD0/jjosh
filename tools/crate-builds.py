#!/usr/bin/env python3
"""Write the BUILD.bazel of each local crate that jjosh links.

Run in jjosh/ after a local crate is added, moved, or gains a build script
or a procedural macro; see ../crate.bzl.  Whatever a BUILD file says after
the line "# Written by hand:" stays.
"""
import json, subprocess
from pathlib import Path

# What follows this line in a BUILD file is kept.
MARK = "\n# Written by hand:\n"

metadata = json.loads(subprocess.check_output(
    ["cargo", "metadata", "--locked", "--format-version", "1"]))
packages = {package["id"]: package for package in metadata["packages"]}
nodes = {node["id"]: node for node in metadata["resolve"]["nodes"]}
root = next(id for id, package in packages.items() if package["name"] == "jjosh-cli")
closure, pending = set(), [root]
while pending:
    id = pending.pop()
    if id in closure:
        continue
    closure.add(id)
    pending += [dep["pkg"] for dep in nodes[id]["deps"]
                if any(kind["kind"] != "dev" for kind in dep["dep_kinds"])]
for id in sorted(closure - {root}):
    package = packages[id]
    if package["source"]:
        continue
    directory = Path(package["manifest_path"]).parent
    arguments = [f'package = "{package["name"]}"']
    for target in package["targets"]:
        path = str(Path(target["src_path"]).relative_to(directory))
        if "proc-macro" in target["kind"] or "lib" in target["kind"]:
            if path != "src/lib.rs":
                arguments.append(f'crate_root = "{path}"')
            if "proc-macro" in target["kind"]:
                arguments.append("proc_macro = True")
        elif "custom-build" in target["kind"]:
            arguments.append(f'build_script = "{path}"')
    build = directory / "BUILD.bazel"
    kept = build.read_text().partition(MARK)[2] if build.exists() else ""
    build.write_text(
        'load("//jjosh:crate.bzl", "jjosh_crate")\n\n'
        'exports_files(["Cargo.toml"], visibility = ["//visibility:public"])\n\n'
        "jjosh_crate(" + ("\n    " + ",\n    ".join(sorted(arguments)) + ",\n" if len(arguments) > 1 else arguments[0]) + ")\n"
        + (MARK + kept if kept else ""))
    print(directory.relative_to(Path.cwd()))
