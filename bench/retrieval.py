#!/usr/bin/env python3
"""Retrieval benchmark: given a commit message, rank the files that commit modified.

Each commit is replayed on its parent tree (git archive), so no method sees the future.
Methods: looka (context compiler), bm25 (classic IR baseline), grep (term-count baseline),
aider (aider-chat RepoMap: tree-sitter tags + PageRank, if --aider-python is given).

usage: retrieval.py --looka PATH [--aider-python PY] [--per-repo N] --out results.json REPO...
"""
import argparse
import json
import math
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
from collections import Counter

SRC_EXT = {
    "rs", "ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs", "py", "go", "c", "h", "cc", "cpp", "cxx", "hpp",
    "java", "kt", "swift", "dart", "rb", "php", "cs", "gd", "lua", "vue", "svelte", "sql", "sh",
}
SKIP = {"node_modules", "target", ".venv", "venv", "__pycache__", ".next", "dist", "build", "vendor", ".git"}
NOISE = re.compile(r"^(merge|wip|bump|release|chore\(release\)|v?\d+\.\d+|initial commit|update readme)", re.I)


def is_src(p):
    return p.rsplit(".", 1)[-1] in SRC_EXT and not any(part in SKIP for part in p.split("/"))


def git(repo, *args):
    return subprocess.run(["git", "-C", repo, *args], capture_output=True, text=True, check=True).stdout


def commits(repo, n):
    out = []
    log = git(repo, "log", "--no-merges", "--format=%H%x00%s%x00%b%x1e", "-n", "2000")
    for rec in log.split("\x1e"):
        parts = rec.strip().split("\x00")
        if len(parts) < 2:
            continue
        sha, subject = parts[0], parts[1].strip()
        if len(subject) < 15 or NOISE.match(subject):
            continue
        try:
            st = git(repo, "diff", "--name-status", "--no-renames", f"{sha}^", sha)
        except subprocess.CalledProcessError:
            continue  # root commit
        changed = [l.split("\t", 1) for l in st.splitlines() if "\t" in l]
        modified = sorted({p for s, p in changed if s == "M" and is_src(p)})
        if 1 <= len(modified) <= 8 and len(changed) <= 20:
            out.append({"sha": sha, "task": subject, "gold": modified})
        if len(out) >= n:
            break
    return out


def tokens(s):
    words = re.findall(r"[A-Za-z][A-Za-z0-9]*|\d+", re.sub(r"([a-z0-9])([A-Z])", r"\1 \2", s))
    return [w.lower() for w in words if len(w) >= 3]


def src_files(root):
    for dp, dns, fns in os.walk(root):
        dns[:] = [d for d in dns if d not in SKIP and not d.startswith(".")]
        for f in fns:
            rel = os.path.relpath(os.path.join(dp, f), root)
            if is_src(rel) and os.path.getsize(os.path.join(dp, f)) <= 256_000:
                yield rel


def read(root, rel):
    try:
        with open(os.path.join(root, rel), errors="ignore") as fh:
            return fh.read()
    except OSError:
        return ""


def rank_bm25(root, files, task, k1=1.2, b=0.75):
    q = set(tokens(task))
    docs = {f: Counter(tokens(f) + tokens(read(root, f))) for f in files}
    n, avg = len(docs), sum(sum(c.values()) for c in docs.values()) / max(len(docs), 1)
    df = Counter(t for c in docs.values() for t in q if t in c)
    def score(c):
        dl = sum(c.values())
        return sum(
            math.log(1 + (n - df[t] + 0.5) / (df[t] + 0.5)) * c[t] * (k1 + 1) / (c[t] + k1 * (1 - b + b * dl / avg))
            for t in q if t in c
        )
    return [f for f, s in sorted(((f, score(c)) for f, c in docs.items()), key=lambda x: -x[1]) if s > 0]


def rank_grep(root, files, task):
    q = set(tokens(task))
    scores = {f: sum(read(root, f).lower().count(t) for t in q) for f in files}
    return [f for f, s in sorted(scores.items(), key=lambda x: -x[1]) if s > 0]


def rank_looka(looka, root, task):
    out = subprocess.run([looka, "context", task, "--json", "--root", root, "--budget", "40000"],
                         capture_output=True, text=True, timeout=120).stdout
    p = json.loads(out)
    return [f["path"] for f in p["files"]] + p["also"]


AIDER_SNIPPET = r"""
import json, os, re, sys
os.environ.setdefault("LITELLM_LOCAL_MODEL_COST_MAP", "True")
from aider.repomap import RepoMap
from aider.io import InputOutput
from aider.models import Model
root, task, files = sys.argv[1], sys.argv[2], json.load(sys.stdin)
io = InputOutput(yes=True, pretty=False, fancy_input=False)
rm = RepoMap(map_tokens=1024, root=root, main_model=Model("gpt-4o-mini"), io=io)
abs_files = [os.path.join(root, f) for f in files]
idents = set(re.split(r"\W+", task))  # what aider's get_ident_mentions does
mentioned = {f for f in abs_files if os.path.basename(f) in task}
ranked = rm.get_ranked_tags([], abs_files, mentioned, idents)
seen = []
for t in ranked:
    if t[0] not in seen:
        seen.append(t[0])
print(json.dumps(seen))
"""


def rank_aider(py, root, files, task):
    r = subprocess.run([py, "-c", AIDER_SNIPPET, root, task], input=json.dumps(files),
                       capture_output=True, text=True, timeout=900)
    if r.returncode != 0:
        raise RuntimeError(r.stderr[-800:])
    return json.loads(r.stdout.strip().splitlines()[-1])


def metrics(ranked, gold):
    ranked = [f for f in ranked if is_src(f)]
    g = set(gold)
    first = next((i for i, f in enumerate(ranked) if f in g), None)
    return {
        "hit1": float(bool(ranked) and ranked[0] in g),
        "r5": len(g & set(ranked[:5])) / len(g),
        "r10": len(g & set(ranked[:10])) / len(g),
        "mrr": 0.0 if first is None else 1.0 / (first + 1),
    }


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("repos", nargs="+")
    ap.add_argument("--looka", required=True)
    ap.add_argument("--aider-python")
    ap.add_argument("--per-repo", type=int, default=30)
    ap.add_argument("--out", required=True)
    ap.add_argument("--methods", default="looka,bm25,grep,aider")
    ap.add_argument("--tasks", help="JSON list of {sha, task}: only these commits, with these task texts (e.g. translations)")
    a = ap.parse_args()
    methods = [m for m in a.methods.split(",") if m != "aider" or a.aider_python]
    rows = []
    for repo in a.repos:
        name = os.path.basename(os.path.abspath(repo))
        cs = commits(repo, a.per_repo)
        if a.tasks:
            override = {t["sha"]: t["task"] for t in json.load(open(a.tasks))}
            cs = [{**c, "task": override[c["sha"][:10]]} for c in cs if c["sha"][:10] in override]
        print(f"{name}: {len(cs)} commits", file=sys.stderr)
        for c in cs:
            tmp = tempfile.mkdtemp(prefix="lookabench-")
            try:
                # shared clone at the parent: history up to (not including) the commit, source repo untouched
                subprocess.run(["git", "clone", "-q", "--shared", "--no-checkout", repo, tmp], check=True)
                subprocess.run(["git", "-C", tmp, "checkout", "-q", f"{c['sha']}^"], check=True)
                files = sorted(src_files(tmp))
                gold = [g for g in c["gold"] if g in set(files)]
                if not gold:
                    continue
                row = {"repo": name, "sha": c["sha"][:10], "task": c["task"], "gold": gold, "n_files": len(files)}
                for m in methods:
                    t0 = time.time()
                    try:
                        ranked = {
                            "looka": lambda: rank_looka(a.looka, tmp, c["task"]),
                            "bm25": lambda: rank_bm25(tmp, files, c["task"]),
                            "grep": lambda: rank_grep(tmp, files, c["task"]),
                            "aider": lambda: rank_aider(a.aider_python, tmp, files, c["task"]),
                        }[m]()
                        row[m] = {**metrics(ranked, gold), "ms": int((time.time() - t0) * 1000), "top": ranked[:5]}
                    except Exception as e:  # keep the run going; failures are reported per row
                        row[m] = {"error": str(e)[:300]}
                rows.append(row)
                print(f"  {c['sha'][:8]} " + " ".join(f"{m}={row[m].get('r10', 'ERR')!s:.4}" for m in methods), file=sys.stderr)
            finally:
                shutil.rmtree(tmp, ignore_errors=True)
    summary = {}
    for m in methods:
        ok = [r[m] for r in rows if "error" not in r[m]]
        if ok:
            summary[m] = {k: round(sum(x[k] for x in ok) / len(ok), 3) for k in ("hit1", "r5", "r10", "mrr")}
            summary[m]["median_ms"] = sorted(x["ms"] for x in ok)[len(ok) // 2]
            summary[m]["n"] = len(ok)
            summary[m]["errors"] = len(rows) - len(ok)
    with open(a.out, "w") as fh:
        json.dump({"summary": summary, "rows": rows}, fh, indent=1)
    print(json.dumps(summary, indent=1))


if __name__ == "__main__":
    main()
