#!/usr/bin/env python3
"""SWE-bench-style tasks from real commits, for the agent A/B benchmark.

A task = a commit that changes source AND tests. The agent gets the commit message on the parent tree;
acceptance = the commit's own test files, applied after the agent finishes, must pass.

  agent_tasks.py select --out tasks.json [--per-repo N] REPO...   # find + validate (fail at parent, pass at commit)
  agent_tasks.py setup tasks.json TASK_ID WORKDIR                 # fresh clone at parent, no future refs
  agent_tasks.py score tasks.json TASK_ID WORKDIR                 # apply hidden tests, run them + regressions
"""
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile

TEST = re.compile(r"(^|/)(__tests__|tests?)/|\.(test|spec)\.[jt]sx?$|(^|/)test_[^/]+\.py$|_test\.py$")
SRC = re.compile(r"\.(ts|tsx|js|jsx|mjs|py)$")


def sh(cmd, cwd=None, timeout=600, check=False):
    return subprocess.run(cmd, cwd=cwd, capture_output=True, text=True, timeout=timeout, check=check)


def git(repo, *a):
    return sh(["git", "-C", repo, *a], check=True).stdout


def checkout(repo, rev, dest):
    sh(["git", "clone", "-q", "--shared", "--no-checkout", repo, dest], check=True)
    sh(["git", "-C", dest, "checkout", "-q", "--detach", rev], check=True)
    # hide the future: no remotes, branches or tags pointing past the parent
    sh(["git", "-C", dest, "remote", "remove", "origin"])
    for ref in git(dest, "for-each-ref", "--format=%(refname)").split():
        sh(["git", "-C", dest, "update-ref", "-d", ref])
    # copy, never symlink: an agent's `pip install -e .` / `npm i` must not touch the real repo's environment
    # (a symlinked .venv let one agent re-point the owner's LCC install at a scratch copy).
    # `cp -c` is an APFS clone on macOS, so this costs no disk; falls back to a plain copy elsewhere.
    for dep in ("node_modules", ".venv"):
        src = os.path.join(repo, dep)
        if os.path.isdir(src) and not os.path.exists(os.path.join(dest, dep)):
            if sh(["cp", "-cR", src, os.path.join(dest, dep)]).returncode:
                sh(["cp", "-R", src, os.path.join(dest, dep)], check=True)


def vitest(cwd, files, timeout=600):
    """Runs the given test files with the repo's runner: pytest for .py, vitest otherwise ([] = whole suite)."""
    if any(f.endswith(".py") for f in files) or (not files and not os.path.exists(os.path.join(cwd, "node_modules/.bin/vitest"))):
        venv = os.path.join(cwd, ".venv/bin/python")
        py = venv if os.path.exists(venv) else "python3"
        r = sh([py, "-m", "pytest", "-q", "-x", *files], cwd=cwd, timeout=timeout)
    else:
        r = sh(["node_modules/.bin/vitest", "run", "--reporter=dot", *files], cwd=cwd, timeout=timeout)
    return r.returncode == 0, (r.stdout + r.stderr)[-3000:]


def candidates(repo, n):
    out = []
    for line in git(repo, "log", "--no-merges", "--format=%H", "-n", "2000").split():
        try:
            st = git(repo, "diff", "--numstat", "--no-renames", f"{line}^", line)
        except subprocess.CalledProcessError:
            continue
        rows = [l.split("\t") for l in st.splitlines()]
        files = [(int(a) if a.isdigit() else 999) + (int(d) if d.isdigit() else 999) for a, d, _ in rows]
        paths = [p for _, _, p in rows]
        tests = [p for p in paths if TEST.search(p) and SRC.search(p)]
        srcs = [p for p in paths if SRC.search(p) and not TEST.search(p)]
        if 1 <= len(tests) <= 3 and 1 <= len(srcs) <= 4 and len(paths) <= 8 and sum(files) <= 250:
            out.append({"sha": line, "tests": tests, "src": srcs})
    return out[: n * 15]


def select(repos, per_repo, out_path):
    tasks = []
    for repo in repos:
        name = os.path.basename(os.path.abspath(repo)).replace(" ", "_")
        got = 0
        for c in candidates(repo, per_repo):
            if got >= per_repo:
                break
            tmp = tempfile.mkdtemp(prefix="lookatask-")
            try:
                checkout(repo, c["sha"], tmp)
                ok_at_commit, _ = vitest(tmp, c["tests"])
                if not ok_at_commit:
                    continue
                hidden = {t: open(os.path.join(tmp, t)).read() for t in c["tests"]}
                sh(["git", "-C", tmp, "checkout", "-q", "--detach", f"{c['sha']}^"], check=True)
                for t, body in hidden.items():
                    os.makedirs(os.path.dirname(os.path.join(tmp, t)) or tmp, exist_ok=True)
                    open(os.path.join(tmp, t), "w").write(body)
                ok_at_parent, _ = vitest(tmp, c["tests"])
                if ok_at_parent:
                    continue  # tests don't discriminate
                sh(["git", "-C", tmp, "checkout", "-q", "--", *c["tests"]])  # back to pristine parent
                for t in c["tests"]:
                    if not sh(["git", "-C", tmp, "cat-file", "-e", f"HEAD:{t}"]).returncode == 0:
                        os.remove(os.path.join(tmp, t))
                suite_ok, _ = vitest(tmp, [], timeout=900)
                msg = git(repo, "log", "-1", "--format=%B", c["sha"]).strip()
                msg = re.sub(r"\n*(Co-Authored-By|Signed-off-by|🤖).*", "", msg, flags=re.S | re.I).strip()
                tasks.append({"id": f"{name}-{c['sha'][:8]}", "repo": os.path.abspath(repo), "sha": c["sha"],
                              "task": msg, "hidden_tests": hidden, "src": c["src"], "suite_ok_at_parent": suite_ok})
                got += 1
                print(f"ok {name} {c['sha'][:8]} {msg.splitlines()[0][:80]}", file=sys.stderr)
            except (subprocess.SubprocessError, OSError, UnicodeDecodeError) as e:
                print(f"skip {c['sha'][:8]}: {e}", file=sys.stderr)
            finally:
                shutil.rmtree(tmp, ignore_errors=True)
    json.dump(tasks, open(out_path, "w"), indent=1)


def find(tasks_path, tid):
    return next(t for t in json.load(open(tasks_path)) if t["id"] == tid)


def setup(tasks_path, tid, workdir):
    t = find(tasks_path, tid)
    checkout(t["repo"], f"{t['sha']}^", workdir)
    print(workdir)


def score(tasks_path, tid, workdir):
    t = find(tasks_path, tid)
    diff = sh(["git", "-C", workdir, "diff", "--stat", "HEAD"]).stdout.strip().splitlines()
    untracked = sh(["git", "-C", workdir, "ls-files", "--others", "--exclude-standard"]).stdout.split()
    # regressions first, on the agent's tree as left (its own new tests included, hidden tests not yet applied)
    full_ok, _ = vitest(workdir, [], timeout=900)
    for path, body in t["hidden_tests"].items():
        p = os.path.join(workdir, path)
        os.makedirs(os.path.dirname(p), exist_ok=True)
        open(p, "w").write(body)
    passed, log = vitest(workdir, list(t["hidden_tests"]))
    base_ok = t["suite_ok_at_parent"]
    regress = base_ok and not full_ok
    print(json.dumps({"id": tid, "accepted": passed, "regression": regress, "suite_ok": full_ok,
                      "files_changed": max(len(diff) - 1, 0) + len(untracked), "log_tail": log[-800:]}))


if __name__ == "__main__":
    cmd, *rest = sys.argv[1:]
    if cmd == "select":
        out = rest[rest.index("--out") + 1]
        per = int(rest[rest.index("--per-repo") + 1]) if "--per-repo" in rest else 2
        repos = [r for i, r in enumerate(rest) if r not in ("--out", "--per-repo") and rest[i - 1] not in ("--out", "--per-repo")]
        select(repos, per, out)
    elif cmd == "setup":
        setup(*rest)
    elif cmd == "score":
        score(*rest)
