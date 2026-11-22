# The Python preset's mutants command: mutmut 3.8.1, driven as slimfit
# drives cargo-mutants (ADR 0006-0008). mutmut takes no diff: it mutates
# every function of its source paths, as copies beside the original, and
# runs mutants by name. So this lists the mutants of the files a diff
# touches (mutmut's own generator, nothing run), keeps those whose lines
# the diff adds, and names each as cargo-mutants does,
# `<file>:<line>:<column>: replace <old> with <new> in <function>`, from
# where its copy differs from the original's. `--list --json` prints
# them in cargo-mutants' list shape. Otherwise the kept mutants that a
# `-F` (an anchored regular expression) names run, in a copy of the
# tree's files (mutmut writes its `mutants/` where it runs) beside
# `--output`, and their outcomes go to `--output` in cargo-mutants'
# outcomes.json shape: slimfit reads both as it reads cargo-mutants'.
# Usage: python-mutants.py --output <file> --in-diff <diff> [--list --json] [-F <re>]...
import ast
import difflib
import io
import json
import os
import re
import shutil
import subprocess
import sys
from contextlib import redirect_stdout
from pathlib import Path

from mutmut.__main__ import write_all_mutants_to_file
from mutmut.configuration import config
from mutmut.utils.format_utils import get_mutant_name

CAUGHT = {1, 3, 24, -24, 36, 152, 255}
MISSED = {0, 5, 33}


def added(diff):
    """The lines a unified diff adds, per file."""
    out, file, at = {}, None, 0
    for line in diff.splitlines():
        if line.startswith("+++ "):
            file = line[4:].removeprefix("b/")
        elif line.startswith("@@ "):
            at = int(line.split(" +")[1].split(" ")[0].split(",")[0])
        elif line.startswith("+"):
            out.setdefault(file, set()).add(at)
            at += 1
        elif line.startswith(" "):
            at += 1
    return out


TOKEN = re.compile(r"\w+|\*\*=?|//=?|<<=?|>>=?|->|:=|[=!<>]=|[-+*/%@&|^~<>=]=?|\S")


def changed(old, new):
    """Where `old` and `new` first differ, by Python tokens, and what
    differs: its offset in `old`, and the text of each in between."""
    a, b = list(TOKEN.finditer(old)), list(TOKEN.finditer(new))
    p = next((i for i, (x, y) in enumerate(zip(a, b)) if x.group() != y.group()), min(len(a), len(b)))
    rest = min(len(a), len(b)) - p
    s = next((i for i in range(rest) if a[-1 - i].group() != b[-1 - i].group()), rest)
    span = lambda t, text: (t[p].start(), t[len(t) - s - 1].end()) if p < len(t) - s else None
    at = lambda t, text: span(t, text) or ((t[p - 1].end(),) * 2 if p else (0, 0))
    (i, j), (k, l) = at(a, old), at(b, new)
    return i, old[i:j], new[k:l]


def defs(tree):
    """Each function of a module, by mutmut's mangled name of it."""
    out = {}
    for node in tree.body:
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
            out[node.name] = node
        elif isinstance(node, ast.ClassDef):
            for f in node.body:
                if isinstance(f, (ast.FunctionDef, ast.AsyncFunctionDef)):
                    out[f"xǁ{node.name}ǁ{f.name}"] = f
    return out


def mutants(file):
    """mutmut's mutants of `file`: each one's key, where its copy first
    differs from the original, and what it puts there."""
    source = Path(file).read_text()
    with redirect_stdout(sys.stderr):
        mutated = write_all_mutants_to_file(out=io.StringIO(), source=source, filename=Path(file))
    lines = mutated.code.splitlines()
    copies = {n.name: n for n in ast.walk(ast.parse(mutated.code)) if isinstance(n, ast.FunctionDef)}
    originals = defs(ast.parse(source))
    text = lambda n: lines[n.lineno - 1 : n.end_lineno]
    for name in mutated.mutant_names:
        mangled = name.partition("__mutmut_")[0]
        orig, copy = copies.get(mangled + "__mutmut_orig"), copies.get(name)
        unmangled = mangled.removeprefix("x_")
        at = originals.get(unmangled)
        if not (orig and copy and at):
            continue
        a, b = text(orig), text(copy)
        b[0] = b[0].replace(name, mangled + "__mutmut_orig", 1)
        op = next((o for o in difflib.SequenceMatcher(None, a, b).get_opcodes() if o[0] != "equal"), None)
        if not op:
            continue
        _, i1, i2, j1, j2 = op
        whole = "\n".join(a[i1:i2])
        start, old, new = changed(whole, "\n".join(b[j1:j2]))
        line = at.lineno + i1 + whole[:start].count("\n")
        column = start - whole[:start].rfind("\n")
        function = unmangled.replace("xǁ", "").replace("ǁ", ".")
        flat = lambda t: " ".join(t.split()) or "nothing"
        yield {
            "key": get_mutant_name(Path(file), name),
            "name": f"{file}:{line}:{column}: replace {flat(old)} with {flat(new)} in {function}",
            "file": file,
            "span": {"start": {"line": line, "column": column}, "end": {"line": at.lineno + max(i2 - 1, i1)}},
            "function": {"function_name": function},
            "replacement": flat(new),
            "genre": "Mutmut",
        }


def mutated(file):
    """Whether mutmut mutates `file`: in its source paths and not excluded."""
    c = config()
    inside = any(Path(file).resolve().is_relative_to(Path(p).resolve()) for p in c.source_paths)
    return file.endswith(".py") and inside and c.should_mutate(file)


def main(args):
    diff, output, listing, picks = None, None, False, []
    while args:
        a = args.pop(0)
        if a == "--in-diff":
            diff = args.pop(0)
        elif a == "--output":
            output = Path(args.pop(0))
        elif a == "-F":
            picks.append(re.compile(args.pop(0)))
        elif a == "--list":
            listing = True
        elif a != "--json":
            sys.exit(f"python-mutants: no such argument {a!r}")
    if diff is None or output is None:
        sys.exit("python-mutants: --output <file> and --in-diff <diff> are needed: nothing mutates the whole codebase")
    lines = added(Path(diff).read_text())
    kept = [
        m
        for f in sorted(lines)
        if mutated(f)
        for m in mutants(f)
        if lines[f] & set(range(m["span"]["start"]["line"], m["span"]["end"]["line"] + 1))
        if listing or any(p.fullmatch(m["name"]) for p in picks)
    ]
    if listing:
        print(json.dumps([{k: v for k, v in m.items() if k != "key"} for m in kept], indent=1))
        return 0
    output.unlink(missing_ok=True)
    if not kept:
        return 0
    # mutmut writes `mutants/` where it runs: a copy of the files git
    # does not ignore, so the tree is left as it was.
    work = output.parent / "mutmut-tree"
    shutil.rmtree(work, ignore_errors=True)
    files = subprocess.run(["git", "ls-files", "-co", "--exclude-standard", "-z"], capture_output=True, check=True)
    for f in filter(None, files.stdout.decode().split("\0")):
        if os.path.isfile(f):
            (work / f).parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(f, work / f)
    keys = [m["key"] for m in kept]
    rc = subprocess.run([sys.executable, "-m", "mutmut", "run", *keys], cwd=work, stdout=sys.stderr).returncode
    exits = {}
    for meta in (work / "mutants").rglob("*.meta"):
        exits.update(json.loads(meta.read_text()).get("exit_code_by_key", {}))
    outcomes = []
    for m in kept:
        code = exits.get(m["key"])
        summary = "CaughtMutant" if code in CAUGHT else "MissedMutant" if code in MISSED else "Unviable"
        mutant = {k: v for k, v in m.items() if k != "key"}
        outcomes.append({"scenario": {"Mutant": mutant}, "summary": summary, "exit_code": code})
    output.write_text(json.dumps({"outcomes": outcomes}, indent=1))
    return rc


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
