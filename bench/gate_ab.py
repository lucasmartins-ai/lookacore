#!/usr/bin/env python3
"""Gate A/B: does `lkc gate` (Claude Code Stop hook) make a real headless agent leave fewer broken changes?

Each task (from agent_tasks.py select) runs twice in fresh clones with `claude -p`, only project and local
settings loaded (no user hooks, plugins or MCP servers), so the arms differ in one thing:

- plain: no extra hooks (the repo's own .claude/settings.json, if it versions one, loads in both arms)
- gate:  .claude/settings.local.json with the SessionStart/Stop hooks `lkc init` installs

The agent is never told about lkc; in the gate arm it only meets it when the Stop hook blocks.
Per run: hidden tests accepted, full-suite regression (independent of lkc), `lkc verify --changed` on the
final tree, gate blocks, cost, turns, wall time. Results are appended to --out (JSONL) and resumable.

usage: gate_ab.py --tasks tasks.json --out runs.jsonl [--model sonnet] [--jobs 4] [--only ID,ID] [--arms plain,gate]
       gate_ab.py --summary runs.jsonl
"""
import argparse, json, os, shutil, subprocess, sys, tempfile, threading, time
from concurrent.futures import ThreadPoolExecutor

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import agent_tasks  # noqa: E402

HOOKS = {"hooks": {
    "SessionStart": [{"hooks": [{"type": "command", "command": 'PATH="$HOME/.cargo/bin:$PATH" lkc gate --hook claude-start', "timeout": 30}]}],
    "Stop": [{"hooks": [{"type": "command", "command": 'PATH="$HOME/.cargo/bin:$PATH" lkc gate --hook claude', "timeout": 900}]}],
}}
PROMPT = "{task}\n\nImplement this change in the repository in the current directory. Leave your changes uncommitted."
# variables of the session running this script; a nested `claude` must not think it is inside it
DROP = ("CLAUDECODE", "CLAUDE_PROJECT_DIR", "CLAUDE_CODE_ENTRYPOINT", "CLAUDE_CODE_SSE_PORT", "CLAUDE_ENV_FILE")
lock = threading.Lock()
limited = threading.Event()  # the account hit its usage limit: stop starting runs, resume later


def run_one(t, arm, model, out, tasks_path, diffs=None):
    if limited.is_set():
        return
    wd = tempfile.mkdtemp(prefix=f"gateab-{arm}-")
    try:
        agent_tasks.checkout(t["repo"], f"{t['sha']}^", wd)
        head = agent_tasks.sh(["git", "-C", wd, "rev-parse", "HEAD"]).stdout.strip()
        with open(os.path.join(wd, ".git/info/exclude"), "a") as f:
            f.write("\n.looka/\n.claude/\n")
        if arm == "gate":
            # settings.local.json: untracked and absent from a fresh clone, so the repo's own
            # .claude/settings.json (some repos version one) loads unchanged in both arms
            os.makedirs(os.path.join(wd, ".claude"), exist_ok=True)
            json.dump(HOOKS, open(os.path.join(wd, ".claude/settings.local.json"), "w"))
        env = {k: v for k, v in os.environ.items() if k not in DROP}
        cmd = ["claude", "-p", PROMPT.format(task=t["task"]), "--model", model, "--setting-sources", "project,local",
               "--strict-mcp-config", "--dangerously-skip-permissions", "--output-format", "json", "--no-session-persistence"]
        t0 = time.time()
        try:
            p = subprocess.run(cmd, cwd=wd, env=env, capture_output=True, text=True, timeout=3600)
            raw, timed_out = p.stdout, False
        except subprocess.TimeoutExpired as e:
            raw, timed_out = (e.stdout or b"").decode() if isinstance(e.stdout, bytes) else (e.stdout or ""), True
        wall = time.time() - t0
        try:
            res = json.loads(raw[raw.index("{"):])
        except ValueError:
            res = {"raw_tail": raw[-500:]}
        if res.get("is_error") and "limit" in str(res.get("result", "")):
            limited.set()
            raise RuntimeError(f"usage limit: {res.get('result')}")
        # an agent that committed anyway: bring its work back to uncommitted so every check sees it
        committed = agent_tasks.sh(["git", "-C", wd, "rev-parse", "HEAD"]).stdout.strip() != head
        if committed:
            agent_tasks.sh(["git", "-C", wd, "reset", "-q", "--soft", head])
        gate_state = {}
        try:
            gate_state = json.load(open(os.path.join(wd, ".looka/gate.json")))
        except (OSError, ValueError):
            pass
        v = agent_tasks.sh(["lkc", "verify", "--changed", "--json", "--timeout", "900", "--root", wd], timeout=1800).stdout
        try:
            v = json.loads(v[v.index("{"):])
        except ValueError:
            v = {}
        intro = [f"{e['kind']}: {d.get('file')}:{d.get('line')} {d.get('message', '')[:160]}" for e in v.get("results", []) if not e.get("ok")
                 for d in e.get("diagnostics", []) if not d.get("preexisting")][:8]
        if diffs:  # raw diffs quote private repos: kept next to --out, never in the repo
            open(os.path.join(diffs, f"{t['id']}.{arm}.diff"), "w").write(agent_tasks.sh(["git", "-C", wd, "diff", "HEAD"]).stdout)
        s = agent_tasks.sh([sys.executable, os.path.join(os.path.dirname(__file__), "agent_tasks.py"), "score", tasks_path, t["id"], wd], timeout=3600).stdout
        s = json.loads(s.strip().splitlines()[-1]) if s.strip() else {}
        row = {"id": t["id"], "arm": arm, "model": model, "accepted": s.get("accepted"), "regression": s.get("regression"),
               "suite_ok": s.get("suite_ok"), "files_changed": s.get("files_changed"),
               "lkc_ok": v.get("ok"), "lkc_introduced": v.get("introduced"), "lkc_diags": intro, "gate_blocks": gate_state.get("blocks", 0), "gate_hooked": bool(gate_state.get("sessions")), "gate_checks": gate_state.get("checks", []),
               "committed": committed, "timed_out": timed_out, "wall_s": round(wall), "turns": res.get("num_turns"),
               "cost_usd": res.get("total_cost_usd"), "is_error": res.get("is_error"), "result_tail": str(res.get("result", res.get("raw_tail", "")))[-300:]}
    except Exception as e:  # one broken task must not stop the run
        row = {"id": t["id"], "arm": arm, "model": model, "error": repr(e)[:500]}
    finally:
        shutil.rmtree(wd, ignore_errors=True)  # a scratch clone this run created
    with lock:
        with open(out, "a") as f:
            f.write(json.dumps(row) + "\n")
        print(json.dumps({k: row.get(k) for k in ("id", "arm", "accepted", "regression", "lkc_ok", "gate_blocks", "wall_s", "cost_usd", "error")}), flush=True)


def summary(path):
    rows = [json.loads(l) for l in open(path) if l.strip()]
    by = {}
    for r in rows:  # the last good row per task and arm (runs are resumable)
        if "error" not in r and not r.get("is_error"):
            by.setdefault(r["id"], {})[r["arm"]] = r
    pairs = [(d["plain"], d["gate"]) for d in by.values() if "plain" in d and "gate" in d]

    def arm(i, key, want=True):
        return sum(1 for p in pairs if p[i].get(key) is want)

    med = lambda xs: sorted(xs)[len(xs) // 2] if xs else None  # noqa: E731
    out = {"pairs": len(pairs)}
    for i, name in ((0, "plain"), (1, "gate")):
        rs = [p[i] for p in pairs]
        out[name] = {
            "accepted": arm(i, "accepted"), "suite_regression": arm(i, "regression"),
            "lkc_introduced_failures": arm(i, "lkc_ok", False), "gate_blocked_runs": sum(1 for r in rs if r.get("gate_blocks")),
            "median_wall_s": med([r["wall_s"] for r in rs]), "median_cost_usd": med([r["cost_usd"] for r in rs if r.get("cost_usd") is not None]),
            "total_cost_usd": round(sum(r.get("cost_usd") or 0 for r in rs), 2),
        }
    # paired: runs that ended broken (suite regression or lkc-introduced failures) in one arm and not the other
    broken = lambda r: bool(r.get("regression")) or r.get("lkc_ok") is False  # noqa: E731
    out["broken_only_plain"] = sum(1 for a, b in pairs if broken(a) and not broken(b))
    out["broken_only_gate"] = sum(1 for a, b in pairs if broken(b) and not broken(a))
    out["accepted_only_plain"] = sum(1 for a, b in pairs if a.get("accepted") and not b.get("accepted"))
    out["accepted_only_gate"] = sum(1 for a, b in pairs if b.get("accepted") and not a.get("accepted"))
    print(json.dumps(out, indent=2))
    return out


if __name__ == "__main__":
    ap = argparse.ArgumentParser()
    ap.add_argument("--tasks")
    ap.add_argument("--out")
    ap.add_argument("--model", default="sonnet")
    ap.add_argument("--jobs", type=int, default=4)
    ap.add_argument("--only", default="")
    ap.add_argument("--arms", default="plain,gate")
    ap.add_argument("--summary")
    a = ap.parse_args()
    if a.summary:
        summary(a.summary)
        sys.exit()
    tasks = json.load(open(a.tasks))
    if a.only:
        tasks = [t for t in tasks if t["id"] in a.only.split(",")]
    done = set()
    if os.path.exists(a.out):
        done = {(r["id"], r["arm"]) for r in map(json.loads, filter(str.strip, open(a.out))) if "error" not in r and not r.get("is_error")}
    diffs = a.out + ".diffs"
    os.makedirs(diffs, exist_ok=True)
    jobs = [(t, arm) for t in tasks for arm in a.arms.split(",") if (t["id"], arm) not in done]
    with ThreadPoolExecutor(a.jobs) as ex:
        for t, arm in jobs:
            ex.submit(run_one, t, arm, a.model, a.out, os.path.abspath(a.tasks), diffs)
