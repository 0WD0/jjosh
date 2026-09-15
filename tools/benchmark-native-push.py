#!/usr/bin/env python3
"""Compare whole-project push preparation using temporary, local Git repositories.

Example (run from jjosh/):
    python3 tools/benchmark-native-push.py /tmp/jjosh-before target/release/jjosh

No remote is contacted and no push is performed. All fixture data is disposable.
The binaries must support whole-project remotes and --ignore-working-copy.
"""

import argparse
import json
import os
from pathlib import Path
import re
import shutil
import statistics
import subprocess
import tempfile
import time


def positive(value):
    number = int(value)
    if number < 1:
        raise argparse.ArgumentTypeError("must be positive")
    return number


def executable(value):
    path = Path(shutil.which(value) or value).resolve()
    if not path.is_file() or not os.access(path, os.X_OK):
        raise argparse.ArgumentTypeError(f"not executable: {value}")
    return path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("baseline", type=executable)
    parser.add_argument("candidate", type=executable)
    parser.add_argument("--commits", type=positive, default=1000)
    parser.add_argument("--refs", type=positive, default=10)
    parser.add_argument("--runs", type=positive, default=5)
    args = parser.parse_args()
    git = shutil.which("git")
    if git is None:
        parser.error("git must be on PATH")

    with tempfile.TemporaryDirectory(prefix="jjosh-native-push-bench-") as directory:
        root = Path(directory)
        repo = root / "repo"
        remote = root / "remote.git"
        home = root / "home"
        repo.mkdir()
        home.mkdir()
        config = root / "config.toml"
        config.write_text(
            "[user]\nname = 'Benchmark'\nemail = 'benchmark@example.invalid'\n"
            "[revset-aliases]\n'immutable_heads()' = 'root()'\n",
            encoding="utf-8",
        )
        env = {
            "PATH": os.environ["PATH"],
            "HOME": str(home),
            "XDG_CONFIG_HOME": str(home),
            "JJ_CONFIG": str(config),
            "GIT_CONFIG_NOSYSTEM": "1",
            "GIT_CONFIG_GLOBAL": os.devnull,
            "LANG": "C.UTF-8",
        }

        def run(program, *command, data=None):
            return subprocess.run(
                [str(program), *map(str, command)],
                cwd=repo,
                env=env,
                input=data,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                check=True,
            )

        def jj(binary, *command):
            return run(binary, "--no-pager", "--color=never", *command)

        run(git, "init", "-b", "main")
        run(git, "init", "--bare", remote)
        # Deterministic history: half the commits change the project, half are
        # pruned. Fast-import avoids measuring thousands of fixture CLI calls.
        records = []
        for i in range(args.commits):
            message = f"benchmark change {i}\n"
            contents = f"value {i}\n"
            path = "pkg/value.txt" if i % 2 == 0 else "outside.txt"
            records.append(
                "commit refs/heads/main\n"
                f"committer Benchmark <benchmark@example.invalid> {1700000000 + i} +0000\n"
                f"data {len(message)}\n{message}"
                f"M 100644 inline {path}\ndata {len(contents)}\n{contents}\n"
            )
        run(git, "fast-import", "--quiet", data="".join(records).encode())
        jj(args.baseline, "git", "init", "--colocate")
        jj(args.baseline, "project", "add", "p", "--path", "pkg")
        jj(args.baseline, "git", "remote", "add", "origin", remote,
           "--project", "p", "--whole")
        tips = run(git, "rev-list", "main").stdout.decode().splitlines()
        for i in range(args.refs):
            # Related, but not identical, heads exercise shared ancestor work.
            tip = tips[min(i * max(1, args.commits // (2 * args.refs)), len(tips) - 1)]
            jj(args.baseline, "--ignore-working-copy", "bookmark", "set",
               f"bench-{i:03}#p", "-r", tip)

        def operation(binary):
            return jj(binary, "--ignore-working-copy", "op", "log",
                      "--no-graph", "-n", "1", "-T", "id").stdout

        initial_op = operation(args.baseline)
        results = []
        for count in sorted({1, args.refs}):
            command = ["--ignore-working-copy", "git", "push", "--remote", "origin#p"]
            for i in range(count):
                command += ["--bookmark", f"bench-{i:03}#p"]
            command.append("--dry-run")
            samples = {"baseline": [], "candidate": []}
            expected_targets = None

            def measure(label, binary, record):
                nonlocal expected_targets
                start = time.perf_counter()
                output = jj(binary, *command)
                elapsed = time.perf_counter() - start
                targets = re.findall(rb"New raw target: ([0-9a-f]+)", output.stderr)
                if len(targets) != count:
                    raise RuntimeError("dry-run did not report every requested ref")
                if expected_targets is None:
                    expected_targets = targets
                elif targets != expected_targets:
                    raise RuntimeError("exported object IDs changed between runs or binaries")
                if record:
                    samples[label].append(elapsed * 1000)

            binaries = [("baseline", args.baseline), ("candidate", args.candidate)]
            for label, binary in binaries:
                measure(label, binary, False)
            for i in range(args.runs):
                # Alternate order to reduce systematic warm-cache bias.
                for label, binary in binaries[::1 if i % 2 == 0 else -1]:
                    measure(label, binary, True)
            results.append({
                "commits": args.commits,
                "refs": count,
                "runs": args.runs,
                "median_ms": {key: round(statistics.median(value), 2)
                              for key, value in samples.items()},
                "samples_ms": {key: [round(item, 2) for item in value]
                               for key, value in samples.items()},
                "identical_exported_ids": True,
            })
        if operation(args.candidate) != initial_op:
            raise RuntimeError("dry-run changed the operation")
        if run(git, "--git-dir", remote, "for-each-ref").stdout:
            raise RuntimeError("dry-run published a remote ref")
        print(json.dumps(results, indent=2))


if __name__ == "__main__":
    try:
        main()
    except subprocess.CalledProcessError as error:
        raise SystemExit(error.stderr.decode(errors="replace")) from error
