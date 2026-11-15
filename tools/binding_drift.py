#!/usr/bin/env python3
"""Find references whose text survived a merge but whose binding did not.

A textually clean merge or rebase can still change what a name refers to:
one side adds a closer candidate (an inherent method, a shadowing item, a
glob import), the other adds or keeps a use of the old one.  Both sides
compile, the result compiles, and the call goes somewhere neither author saw.

    binding_drift.py compare --merged DIR --side DIR [--side DIR]
    binding_drift.py replay  --workspace SUBDIR [--merges] [--scan] [--limit N]

`compare` takes checkouts of one Cargo workspace.  `replay` walks the
rebases recorded in the jjosh operation log and compares, for every
rebased commit, the old commit and the new parent against the rebased
result; with `--merges` it walks the merge commits of the history instead
and compares both parents against the merge.

Bindings come from `rust-analyzer scip`.  Every reference in the merged
tree that sits on a line also present in a side is expected to bind as it
did in one of the sides that has the line.  Findings are classified:

    rebind    the old target still exists in the merged tree
    moved     the old target is gone (rename or move on the other side)
    lost      resolved in the sides, unresolved in the merged tree
    gained    unresolved in the sides, resolved in the merged tree
    local     a local variable now refers to a different definition
"""

import argparse
import difflib
import hashlib
import json
import os
import re
import subprocess
import sys
import tempfile
from collections import Counter
from pathlib import Path

CACHE = Path(os.environ.get("XDG_CACHE_HOME", Path.home() / ".cache")) / "binding-drift"

# Without build scripts and proc macros an index takes seconds instead of a
# build.  Names that need them stay unresolved on every side alike.
RA_CONFIG = {
    "cargo": {"buildScripts": {"enable": False}, "sysroot": "discover"},
    "procMacro": {"enable": False},
    "cachePriming": {"enable": False},
}


# --- SCIP wire format: just the fields we read ------------------------------

def varint(buf, i):
    shift = value = 0
    while True:
        b = buf[i]
        i += 1
        value |= (b & 0x7F) << shift
        if b < 0x80:
            return value, i
        shift += 7


def fields(buf):
    i = 0
    while i < len(buf):
        tag, i = varint(buf, i)
        number, wire = tag >> 3, tag & 7
        if wire == 0:
            value, i = varint(buf, i)
        elif wire == 2:
            size, i = varint(buf, i)
            value, i = buf[i:i + size], i + size
        elif wire == 1:
            value, i = buf[i:i + 8], i + 8
        elif wire == 5:
            value, i = buf[i:i + 4], i + 4
        else:
            raise ValueError(f"wire type {wire}")
        yield number, value


def packed_ints(buf):
    i = 0
    while i < len(buf):
        value, i = varint(buf, i)
        yield value


def load_index(path):
    """{relative_path: [(line, col, end_col, symbol, is_definition)]}"""
    documents = {}
    for number, document in fields(memoryview(Path(path).read_bytes())):
        if number != 2:
            continue
        name, occurrences = None, []
        for number, value in fields(document):
            if number == 1:
                name = bytes(value).decode()
            elif number == 2:
                span, symbol, roles = [], "", 0
                for number, item in fields(value):
                    if number == 1:
                        span = list(packed_ints(item))
                    elif number == 2:
                        symbol = bytes(item).decode()
                    elif number == 3:
                        roles = item
                if symbol and len(span) == 3:
                    occurrences.append((span[0], span[1], span[2], symbol, bool(roles & 1)))
        documents[name] = occurrences
    return documents


# --- indexing ---------------------------------------------------------------

def tree_digest(root):
    digest = hashlib.sha256()
    for path in sorted(root.rglob("*")):
        if "target" in path.parts or ".git" in path.parts:
            continue
        if path.is_file() and (path.suffix == ".rs" or path.name.startswith("Cargo.")):
            digest.update(str(path.relative_to(root)).encode() + b"\0")
            digest.update(path.read_bytes())
    return digest.hexdigest()[:24]


def toolchain():
    """The monorepo's pinned rust-analyzer, usable from checkouts outside it."""
    binary = subprocess.run(
        ["rustup", "which", "rust-analyzer"], cwd=Path(__file__).parent,
        check=True, capture_output=True, text=True).stdout.strip()
    return binary, Path(binary).parents[1].name


def index(root, inputs=None):
    """Index the workspace at `root`; `inputs` is the directory holding everything it reads."""
    root = Path(root).resolve()
    CACHE.mkdir(parents=True, exist_ok=True)
    out = CACHE / f"{tree_digest(inputs or root)}.scip"
    if not out.exists():
        config = CACHE / "ra-config.json"
        config.write_text(json.dumps(RA_CONFIG))
        partial = out.with_suffix(".partial")
        analyzer, channel = toolchain()
        run = subprocess.run(
            [analyzer, "scip", ".", "--output", str(partial), "--config-path", str(config)],
            cwd=root, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True,
            env={**os.environ, "RUSTUP_TOOLCHAIN": channel},
        )
        if run.returncode:
            reason = [l.strip() for l in run.stderr.splitlines() if "rror" in l or "ailed" in l]
            raise RuntimeError("rust-analyzer scip: " + " / ".join(reason[:3]))
        partial.rename(out)
    return World(root, load_index(out))


def unversioned(symbol):
    """Drop the package version: a version bump is not a rebinding."""
    parts = symbol.split(" ", 4)
    if len(parts) == 5:
        parts[3] = "."
    return " ".join(parts)


class World:
    def __init__(self, root, documents):
        self.root = root
        self.documents = documents
        self.defined = {
            unversioned(symbol)
            for occurrences in documents.values()
            for (_, _, _, symbol, is_def) in occurrences if is_def
        }
        self._lines = {}

    def lines(self, name):
        if name not in self._lines:
            path = self.root / name
            self._lines[name] = path.read_text(errors="replace").splitlines() if path.is_file() else None
        return self._lines[name]

    def bindings(self, name):
        """{(line, col, end_col): symbol} for references, plus local definition lines."""
        references, local_defs = {}, {}
        for line, col, end, symbol, is_def in self.documents.get(name, ()):
            if is_def:
                if symbol.startswith("local "):
                    local_defs[symbol] = (line, col)
            else:
                references[(line, col, end)] = symbol
        return references, local_defs


# --- comparison -------------------------------------------------------------

def line_map(merged, side):
    """{merged line: side line} for lines the two texts share."""
    if merged == side:
        return {i: i for i in range(len(merged))}
    mapping = {}
    matcher = difflib.SequenceMatcher(None, merged, side, autojunk=False)
    for a, b, size in matcher.get_matching_blocks():
        for k in range(size):
            mapping[a + k] = b + k
    return mapping


UNKNOWN = object()


def normalise(symbol, local_defs, to_merged):
    """A binding comparable across worlds; UNKNOWN when it cannot be compared."""
    if symbol is None:
        return None
    if not symbol.startswith("local "):
        return unversioned(symbol)
    if symbol not in local_defs:
        return UNKNOWN
    line, col = local_defs[symbol]
    if line not in to_merged:
        return UNKNOWN
    return ("local", to_merged[line], col)


def compare(merged, sides):
    findings = []
    for name in merged.documents:
        text = merged.lines(name)
        if text is None:
            continue
        m_refs, m_locals = merged.bindings(name)
        identity = {i: i for i in range(len(text))}
        expected = {}  # position in merged -> [binding per side that has the line]
        for side in sides:
            side_text = side.lines(name)
            if side_text is None:
                continue
            forward = line_map(text, side_text)
            backward = {s: m for m, s in forward.items()}
            s_refs, s_locals = side.bindings(name)
            positions = {key for key in m_refs if key[0] in forward}
            positions |= {(backward[l], c, e) for (l, c, e) in s_refs if l in backward}
            for line, col, end in positions:
                raw = s_refs.get((forward[line], col, end))
                expected.setdefault((line, col, end), []).append(normalise(raw, s_locals, backward))
        for position, wanted in sorted(expected.items()):
            got = normalise(m_refs.get(position), m_locals, identity)
            if got is UNKNOWN or UNKNOWN in wanted or got in wanted:
                continue
            line, col, end = position
            findings.append({
                "kind": classify(got, wanted, merged),
                "file": name,
                "line": line + 1,
                "text": text[line].strip(),
                "name": text[line][col:end],
                "was": sorted({str(w) for w in wanted}),
                "now": str(got),
            })
    return findings


def classify(got, wanted, merged):
    if got is None:
        return "lost"
    if all(w is None for w in wanted):
        return "gained"
    if isinstance(got, tuple) or any(isinstance(w, tuple) for w in wanted):
        return "local"
    olds = [w for w in wanted if w is not None]
    workspace_prefixes = {d.rsplit(" ", 1)[0] for d in merged.defined}
    for old in olds:
        external = old.rsplit(" ", 1)[0] not in workspace_prefixes
        if old in merged.defined or external:
            return "rebind"
    return "moved"


def report(findings, out=sys.stdout):
    for f in findings:
        print(f"{f['kind']:7} {f['file']}:{f['line']}  `{f['name']}`  in  {f['text']}", file=out)
        print(f"          was {' | '.join(f['was'])}", file=out)
        print(f"          now {f['now']}", file=out)
    counts = Counter(f["kind"] for f in findings)
    print("total: " + (", ".join(f"{k} {v}" for k, v in sorted(counts.items())) or "no drift"), file=out)


# --- replaying a jjosh operation log ----------------------------------------

def jj(repo, *args):
    return subprocess.run(
        ["jjosh", "--ignore-working-copy", "-R", str(repo), *args],
        check=True, capture_output=True, text=True,
    ).stdout


def rebase_operations(repo):
    log = jj(repo, "op", "log", "--no-graph", "-T", 'id.short(16) ++ "\\t" ++ description.first_line() ++ "\\n"')
    for row in log.splitlines():
        op, _, description = row.partition("\t")
        if description.startswith("rebase "):
            yield op


ROW = 'change_id ++ " " ++ commit_id ++ " " ++ parents.map(|p| p.commit_id()).join(",") ++ " " ++ conflict ++ "\\n"'


def rebased_commits(repo, op):
    """(old commit, old parents, new commit, new parents) per rewritten, unconflicted change."""
    try:
        after = jj(repo, "log", "--no-graph", "--at-op", op, "-T", ROW,
                   "-r", f"mutable() & ~at_operation({op}-, all())")
        before = jj(repo, "log", "--no-graph", "--at-op", f"{op}-", "-T", ROW, "-r", "mutable()")
    except subprocess.CalledProcessError:
        return
    old = {}
    for row in before.splitlines():
        change, commit, parents, _ = row.split(" ")
        old[change] = (commit, parents.split(","))
    for row in after.splitlines():
        change, commit, parents, conflict = row.split(" ")
        if change in old and conflict == "false":
            yield (*old[change], commit, parents.split(","))


def is_rust(path):
    return path.endswith(".rs") or path.endswith("Cargo.toml")


def own_changes(repo, commits, workspace):
    """{commit: rust files it changed in the workspace}, one query per batch."""
    template = f'commit_id ++ " " ++ self.diff("{workspace}").files().map(|f| f.path()).join(",") ++ "\\n"'
    changed = {}
    for start in range(0, len(commits), 200):
        rows = jj(repo, "log", "--no-graph", "-T", template, "-r", "|".join(commits[start:start + 200]))
        for row in rows.splitlines():
            commit, _, files = row.partition(" ")
            changed[commit] = {f for f in files.split(",") if is_rust(f)}
    return changed


def changed_between(repo, a, b, workspace):
    out = jj(repo, "diff", "--from", a, "--to", b, "--name-only", workspace)
    return {p for p in out.splitlines() if is_rust(p)}


PATH_DEPENDENCY = re.compile(r'\bpath\s*=\s*"([^"]+)"')


def checkout(repo, commit, workspace, into):
    """Materialise a workspace of a commit, with the directories it depends on by path."""
    git_dir = jj(repo, "git", "root").strip()
    into.mkdir(parents=True)
    wanted, present = [workspace], set()
    while wanted:
        directory = wanted.pop()
        if any(directory == p or directory.startswith(p + "/") for p in present):
            continue
        archive = subprocess.Popen(
            ["git", "--git-dir", git_dir, "archive", commit, directory],
            stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
        subprocess.run(["tar", "-x", "-C", str(into)], stdin=archive.stdout, check=True)
        if archive.wait():
            if directory == workspace:
                raise RuntimeError(f"{workspace} does not exist in {commit[:12]}")
            continue
        present.add(directory)
        for manifest in (into / directory).rglob("Cargo.toml"):
            for target in PATH_DEPENDENCY.findall(manifest.read_text(errors="replace")):
                relative = os.path.relpath(os.path.normpath(manifest.parent / target), into)
                if not relative.startswith("..") and not (into / relative).exists():
                    wanted.append(workspace_root(git_dir, commit, relative))
    return into


def workspace_root(git_dir, commit, package):
    """The enclosing Cargo workspace of a package directory, or the package itself."""
    parts = package.split("/")
    for depth in range(len(parts) - 1, 0, -1):
        manifest = subprocess.run(
            ["git", "--git-dir", git_dir, "show", f"{commit}:{'/'.join(parts[:depth])}/Cargo.toml"],
            capture_output=True, text=True)
        if manifest.returncode == 0 and "[workspace]" in manifest.stdout:
            return "/".join(parts[:depth])
    return package


def candidates(repo, workspace):
    """Rebased commits where both the commit and its new base changed the workspace."""
    seen = set()
    for op in rebase_operations(repo):
        rows = [row for row in rebased_commits(repo, op)
                if len(row[1]) == 1 and len(row[3]) == 1 and row[1] != row[3]]
        try:
            mine = own_changes(repo, [row[0] for row in rows], workspace)
        except subprocess.CalledProcessError:
            continue
        for old, (old_parent,), new, (new_parent,) in rows:
            if not mine.get(old) or (old, new_parent) in seen:
                continue
            seen.add((old, new_parent))
            try:
                theirs = changed_between(repo, old_parent, new_parent, workspace)
            except subprocess.CalledProcessError:
                continue
            if theirs:
                yield {"op": op, "old": old, "new": new, "parent": new_parent, "mine": len(mine[old]),
                       "theirs": len(theirs), "overlap": len(mine[old] & theirs)}


def merge_candidates(repo, workspace):
    """Two-parent merge commits where both parents contributed Rust changes."""
    git_dir = jj(repo, "git", "root").strip()

    def git(*args):
        return subprocess.run(["git", "--git-dir", git_dir, *args],
                              check=True, capture_output=True, text=True).stdout

    for row in git("log", "--merges", "--all", "--format=%H %P", "--", workspace).splitlines():
        merge, *parents = row.split()
        if len(parents) != 2:
            continue
        sides = [{p for p in git("diff", "--name-only", parent, merge, "--", workspace).splitlines() if is_rust(p)}
                 for parent in parents]
        # What differs from the second parent is what the first side brought, and vice versa.
        theirs, mine = sides
        if mine and theirs:
            yield {"op": "merge", "old": parents[0], "new": merge, "parent": parents[1],
                   "mine": len(mine), "theirs": len(theirs), "overlap": len(mine & theirs)}


def replay(repo, workspace, limit, log, scan, merges):
    source = merge_candidates if merges else candidates
    for done, record in enumerate(source(repo, workspace), 1):
        if not scan:
            with tempfile.TemporaryDirectory(dir=CACHE) as tmp:
                try:
                    trees = [checkout(repo, record[tag], workspace, Path(tmp) / tag)
                             for tag in ("new", "old", "parent")]
                    worlds = [index(tree / workspace, tree) for tree in trees]
                    record["findings"] = compare(worlds[0], worlds[1:])
                except (subprocess.CalledProcessError, RuntimeError) as error:
                    record["error"] = str(error)[:300]
        log.write(json.dumps(record, ensure_ascii=False) + "\n")
        log.flush()
        kinds = Counter(f["kind"] for f in record.get("findings", ()))
        print(f"{record['op']} {record['old'][:12]} -> {record['new'][:12]}  "
              f"mine {record['mine']} theirs {record['theirs']} overlap {record['overlap']}  "
              + (record.get("error") or ", ".join(f"{k} {v}" for k, v in sorted(kinds.items()))
                 or ("" if scan else "clean")), flush=True)
        if limit and done >= limit:
            return


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest="command", required=True)
    c = commands.add_parser("compare")
    c.add_argument("--merged", required=True)
    c.add_argument("--side", action="append", required=True)
    r = commands.add_parser("replay")
    r.add_argument("--repo", default=".")
    r.add_argument("--workspace", required=True)
    r.add_argument("--limit", type=int, default=0)
    r.add_argument("--log", default="binding-drift.jsonl")
    r.add_argument("--merges", action="store_true",
                   help="replay merge commits in the history instead of rebases in the operation log")
    r.add_argument("--scan", action="store_true", help="list candidates without indexing")
    args = parser.parse_args()
    if args.command == "compare":
        findings = compare(index(args.merged), [index(side) for side in args.side])
        report(findings)
        sys.exit(1 if findings else 0)
    with open(args.log, "a") as log:
        replay(Path(args.repo).resolve(), args.workspace, args.limit, log, args.scan, args.merges)


if __name__ == "__main__":
    main()
