#!/usr/bin/env python3
"""Mutation benchmark: does `lkc verify --changed` tell a breaking change from a harmless one?

For each repo (cloned at HEAD, dependencies APFS-cloned), pick source files that have tests, inject one
real bug per file (flip a comparison, && -> ||, true -> false, +1 -> -1) and add a harmless comment to
another copy. Signals compared:

- naive: run the full test command, FAIL if it exits non-zero (what an agent sees without attribution)
- lkc:   `lkc verify --changed` (narrowed tests + typecheck + lint, failures attributed against HEAD)

The oracle for "the bug is detectable" is the full suite: a mutant counts only if the full suite has a
failing test file that does not fail on HEAD. The full suite is run on every mutant, so misses are measured.

usage: mutation.py --out results.json [--per-repo 12] [--lkc PATH] REPO...
"""
import argparse, json, os, random, re, shutil, subprocess, sys, time

OPS = [
    (r"===", "!=="), (r"!==", "==="), (r"(?<![=!<>])==(?!=)", "!="), (r" < ", " >= "), (r" > ", " <= "),
    (r"&&", "||"), (r"\|\|", "&&"), (r"\btrue\b", "false"), (r"\bfalse\b", "true"),
    (r"\bTrue\b", "False"), (r"\bFalse\b", "True"), (r"\band\b", "or"), (r"\+ 1\b", "- 1"), (r"- 1\b", "+ 1"),
]
COMMENT = {".py": "#", ".ts": "//", ".tsx": "//", ".js": "//", ".jsx": "//", ".rs": "//", ".go": "//"}


def sh(cmd, cwd, timeout=1800, env=None):
    t = time.time()
    try:
        r = subprocess.run(cmd, cwd=cwd, shell=isinstance(cmd, str), capture_output=True, text=True, timeout=timeout, env=env)
        return r.returncode, r.stdout + r.stderr, time.time() - t
    except subprocess.TimeoutExpired as e:
        return 124, (e.stdout or "") if isinstance(e.stdout, str) else "", time.time() - t


def lkc_json(lkc, args, cwd, timeout=1800):
    code, out, secs = sh([lkc, *args, "--json", "--root", cwd], cwd, timeout)
    try:
        start = out.index("{")
        return code, json.loads(out[start:out.rindex("}") + 1]), secs
    except ValueError:
        return code, {}, secs


def clone(repo, dest):
    subprocess.run(["git", "clone", "-q", "--shared", repo, dest], check=True)
    for dep in ("node_modules", ".venv"):
        src = os.path.join(repo, dep)
        if os.path.isdir(src):
            if subprocess.run(["cp", "-cR", src, os.path.join(dest, dep)]).returncode:
                subprocess.run(["cp", "-R", src, os.path.join(dest, dep)], check=True)
    if os.path.exists(os.path.join(dest, "node_modules/.bin/prisma")):
        sh("node_modules/.bin/prisma generate", dest, 300)


def failing_files(lkc, output):
    p = subprocess.run([lkc, "diagnose", "--json"], input=output, capture_output=True, text=True)
    try:
        return {d["file"] for d in json.loads(p.stdout) if d.get("file") and d.get("severity") == "error"}
    except json.JSONDecodeError:
        return set()


def mutate(text, rng):
    lines = text.split("\n")
    spots = []
    for i, l in enumerate(lines):
        s = l.strip()
        if not s or s.startswith(("//", "#", "*", "/*", "import ", "from ", "export {", "console.")) or "'" in s and '"' in s:
            continue
        for pat, rep in OPS:
            for m in re.finditer(pat, l):
                # skip matches inside string literals (odd count of quotes before the match)
                before = l[: m.start()]
                if before.count('"') % 2 or before.count("'") % 2 or before.count("`") % 2:
                    continue
                spots.append((i, m.start(), m.end(), rep))
    if not spots:
        return None
    i, a, b, rep = rng.choice(spots)
    lines[i] = lines[i][:a] + rep + lines[i][b:]
    return "\n".join(lines), i + 1


def bench_repo(repo, lkc, per_repo, work, rng):
    name = os.path.basename(repo.rstrip("/")).replace(" ", "_")
    base = os.path.join(work, name)
    if not os.path.exists(base):
        clone(repo, base)
    m = lkc_json(lkc, ["map"], base)[1]
    eco = {".py": "python", ".rs": "cargo", ".go": "go"}
    comps = [(c, next(x["run"] for x in c.get("commands", []) if x["kind"] == "test" and x.get("available")))
             for c in m.get("components", []) if any(x["kind"] == "test" and x.get("available") for x in c.get("commands", []))]
    if not comps:
        return {"repo": name, "skipped": "no runnable test command"}
    baselines = {}

    def owner(f):
        """component whose dir contains f and whose ecosystem matches the file's language"""
        want = eco.get(os.path.splitext(f)[1], "node")
        fits = [(c, t) for c, t in comps if (c["dir"] == "." or f.startswith(c["dir"].rstrip("/") + "/")) and c["ecosystem"] == want]
        return max(fits, key=lambda ct: len(ct[0]["dir"]), default=None)

    def baseline(c, t):
        if c["dir"] not in baselines:
            code, out, secs = sh(t, os.path.join(base, c["dir"]))
            baselines[c["dir"]] = (code, failing_files(lkc, out), secs)
        return baselines[c["dir"]]

    src = [f for f in subprocess.run(["git", "ls-files"], cwd=base, capture_output=True, text=True).stdout.split()
           if os.path.splitext(f)[1] in COMMENT and not re.search(r"(test|spec|__tests__|fixtures?|mock)", f, re.I)]
    rng.shuffle(src)
    rows = []
    for f in src:
        if len([r for r in rows if r["kind"] == "mutant"]) >= per_repo:
            break
        ct = owner(f)
        if not ct:
            continue
        comp, test_cmd = ct
        cdir = os.path.join(base, comp["dir"])
        code0, base_fail, _ = baseline(comp, test_cmd)
        imp = lkc_json(lkc, ["impact", f], base, 120)[1]
        if not imp.get("tests"):
            continue  # no test reaches this file: neither signal can see a bug here
        path = os.path.join(base, f)
        orig = open(path, encoding="utf-8", errors="surrogateescape").read()
        mut = mutate(orig, rng)
        if not mut:
            continue
        for kind, text in (("mutant", mut[0]), ("noop", orig.rstrip("\n") + f"\n{COMMENT[os.path.splitext(f)[1]]} reviewed\n")):
            open(path, "w", encoding="utf-8", errors="surrogateescape").write(text)
            vcode, v, vsecs = lkc_json(lkc, ["verify", "--changed", "--timeout", "900"], base)
            fcode, fout, fsecs = sh(test_cmd, cdir)
            new_fail = sorted(failing_files(lkc, fout) - base_fail)
            rows.append({
                "component": comp["dir"], "file": f, "line": mut[1], "kind": kind,
                "oracle_breaks": bool(new_fail) or (code0 == 0 and fcode != 0),
                "naive_fail": fcode != 0, "lkc_fail": v.get("ok") is False,
                "lkc_introduced": v.get("introduced"), "lkc_secs": round(vsecs, 1), "full_secs": round(fsecs, 1),
                "new_failing": new_fail[:5],
            })
            print(json.dumps({"repo": name, **rows[-1]}), flush=True)
            subprocess.run(["git", "checkout", "-q", "--", f], cwd=base)
    heads = {d: {"exit": b[0], "failing_files": len(b[1]), "secs": round(b[2], 1)} for d, b in baselines.items()}
    return {"repo": name, "head": heads, "rows": rows}


def summarize(results):
    out = {}
    for r in results:
        rows = r.get("rows", [])
        muts = [x for x in rows if x["kind"] == "mutant" and x["oracle_breaks"]]
        noops = [x for x in rows if x["kind"] == "noop"]
        out[r["repo"]] = {
            "detectable_mutants": len(muts),
            "lkc_caught": sum(x["lkc_fail"] for x in muts),
            "naive_caught": sum(x["naive_fail"] for x in muts),
            "noop_changes": len(noops),
            "lkc_false_alarms": sum(x["lkc_fail"] for x in noops),
            "naive_false_alarms": sum(x["naive_fail"] for x in noops),
            "median_lkc_secs": sorted(x["lkc_secs"] for x in rows)[len(rows) // 2] if rows else None,
            "median_full_secs": sorted(x["full_secs"] for x in rows)[len(rows) // 2] if rows else None,
            "head": r.get("head"),
        }
    return out


if __name__ == "__main__":
    ap = argparse.ArgumentParser()
    ap.add_argument("repos", nargs="+")
    ap.add_argument("--out", required=True)
    ap.add_argument("--per-repo", type=int, default=12)
    ap.add_argument("--lkc", default=shutil.which("lkc") or "lkc")
    ap.add_argument("--work", default=os.path.join(os.environ.get("TMPDIR", "/tmp"), "lkc-mutation"))
    ap.add_argument("--seed", type=int, default=7)
    a = ap.parse_args()
    os.makedirs(a.work, exist_ok=True)
    rng = random.Random(a.seed)
    results = [bench_repo(r, a.lkc, a.per_repo, a.work, rng) for r in a.repos]
    json.dump({"results": results, "summary": summarize(results)}, open(a.out, "w"), indent=2)
    print(json.dumps(summarize(results), indent=2))
