#!/usr/bin/env python3
"""Find references whose text survived a merge but whose binding did not.

A textually clean merge or rebase can still change what a name refers to:
one side adds a closer candidate (an inherent method, a shadowing item, a
glob import), the other adds or keeps a use of the old one.  Both sides
compile, the result compiles, and the call goes somewhere neither author saw.

    binding_drift.py compare --merged DIR --side DIR [--side DIR]
    binding_drift.py replay  --repo DIR --workspace SUBDIR [--limit N]

`compare` takes checkouts of one Cargo workspace.  `replay` walks the
rebases recorded in a jjosh operation log and compares, for every rebased
commit, the old commit and the new parent against the rebased result.

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


def index(root):
    root = Path(root).resolve()
    CACHE.mkdir(parents=True, exist_ok=True)
    out = CACHE / f"{tree_digest(root)}.scip"
    if not out.exists():
        config = CACHE / "ra-config.json"
        config.write_text(json.dumps(RA_CONFIG))
        partial = out.with_suffix(".partial")
        analyzer, channel = toolchain()
        subprocess.run(
            [analyzer, "scip", ".", "--output", str(partial), "--config-path", str(config)],
            cwd=root, check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
            env={**os.environ, "CARGO_NET_OFFLINE": "true", "RUSTUP_TOOLCHAIN": channel},
        )
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


def rebased_commits(repo, op):
    """(old commit, new commit) for every commit the operation rewrote."""
    template = 'commit_id ++ " " ++ predecessors.map(|p| p.commit_id()).join(",") ++ "\\n"'
    try:
        before = set(jj(repo, "log", "--no-graph", "--at-op", f"{op}-", "-r", "all()", "-T", 'commit_id ++ "\\n"').split())
        rows = jj(repo, "log", "--no-graph", "--at-op", op, "-r", "mutable()", "-T", template).splitlines()
    except subprocess.CalledProcessError:
        return
    for row in rows:
        new, _, olds = row.partition(" ")
        if new in before:
            continue
        for old in filter(None, olds.split(",")):
            if old in before:
                yield old, new


def changed_rust(repo, a, b, workspace):
    out = jj(repo, "diff", "--from", a, "--to", b, "--name-only", workspace)
    return {p for p in out.splitlines() if p.endswith(".rs") or p.endswith("Cargo.toml")}


def checkout(repo, commit, workspace, into):
    """Materialise one workspace of a commit through the git backend."""
    git_dir = jj(repo, "git", "root").strip()
    into.mkdir(parents=True)
    archive = subprocess.Popen(
        ["git", "--git-dir", git_dir, "archive", commit, workspace], stdout=subprocess.PIPE)
    subprocess.run(["tar", "-x", "-C", str(into)], stdin=archive.stdout, check=True)
    if archive.wait():
        raise RuntimeError(f"git archive {commit} {workspace}")
    return into / workspace


def replay(repo, workspace, limit, log):
    seen, done = set(), 0
    for op in rebase_operations(repo):
        for old, new in rebased_commits(repo, op):
            if (old, new) in seen:
                continue
            seen.add((old, new))
            try:
                old_parent = jj(repo, "log", "--no-graph", "--at-op", f"{op}-", "-r", f"{old}-", "-T", 'commit_id ++ "\\n"').split()
                new_parent = jj(repo, "log", "--no-graph", "--at-op", op, "-r", f"{new}-", "-T", 'commit_id ++ "\\n"').split()
                conflicted = jj(repo, "log", "--no-graph", "--at-op", op, "-r", new, "-T", "conflict").strip() == "true"
                if len(old_parent) != 1 or len(new_parent) != 1 or old_parent == new_parent or conflicted:
                    continue
                mine = changed_rust(repo, old_parent[0], old, workspace)
                theirs = changed_rust(repo, old_parent[0], new_parent[0], workspace)
            except subprocess.CalledProcessError:
                continue
            if not mine or not theirs:
                continue
            record = {"op": op, "old": old, "new": new, "parent": new_parent[0],
                      "mine": len(mine), "theirs": len(theirs), "overlap": len(mine & theirs)}
            with tempfile.TemporaryDirectory(dir=CACHE) as tmp:
                try:
                    worlds = [index(checkout(repo, c, workspace, Path(tmp) / tag))
                              for tag, c in (("merged", new), ("mine", old), ("theirs", new_parent[0]))]
                    record["findings"] = compare(worlds[0], worlds[1:])
                except (subprocess.CalledProcessError, RuntimeError) as error:
                    record["error"] = str(error)[:200]
            log.write(json.dumps(record, ensure_ascii=False) + "\n")
            log.flush()
            kinds = Counter(f["kind"] for f in record.get("findings", ()))
            print(f"{op} {old[:12]} -> {new[:12]}  mine {len(mine)} theirs {len(theirs)}  "
                  + (record.get("error") or ", ".join(f"{k} {v}" for k, v in sorted(kinds.items())) or "clean"),
                  flush=True)
            done += 1
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
    args = parser.parse_args()
    if args.command == "compare":
        findings = compare(index(args.merged), [index(side) for side in args.side])
        report(findings)
        sys.exit(1 if findings else 0)
    with open(args.log, "a") as log:
        replay(Path(args.repo).resolve(), args.workspace, args.limit, log)


if __name__ == "__main__":
    main()
