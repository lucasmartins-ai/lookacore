# Benchmarks

Three questions, three benchmarks. All numbers below were measured on 2026-10-10 on the owner's own repositories
(TypeScript/Next.js, Python, Rust, Godot) with the scripts in `bench/`.

## 1. Context retrieval — does `looka context` find the files a task touches?

`bench/retrieval.py`. Each non-merge commit (1–8 modified source files) is replayed on its parent tree
(`git clone --shared` + checkout of the parent, so no method sees the future). The commit subject is the task;
the files the commit modified are the gold set. Methods:

- **looka** — `looka context` (BM25 over identifier tokens + path/symbol boosts + git recency prior + one hop over the import graph)
- **bm25** — classic BM25 over the same tokenization
- **grep** — files ranked by raw term counts
- **aider** — aider-chat `RepoMap.get_ranked_tags` (tree-sitter tags + PageRank), fed the same task text the way aider does

Weights were tuned only on three dev repos (two Next.js apps, one Python library). The held-out repos were never used for tuning.

**Held-out** (six other repos, TypeScript and Python — 120 commits):

| method | hit@1 | recall@5 | recall@10 | MRR | median latency |
|---|---|---|---|---|---|
| **looka** | **0.525** | **0.562** | **0.632** | **0.647** | 561 ms |
| bm25 | 0.375 | 0.453 | 0.581 | 0.517 | 93 ms |
| grep | 0.258 | 0.319 | 0.475 | 0.396 | 158 ms |
| aider repo-map | 0.100 | 0.130 | 0.177 | 0.196 | 17.9 s |

Dev (90 commits): looka hit@1 0.389 / MRR 0.529 vs bm25 0.311 / 0.457. The first version of looka (substring
matching, no length normalisation) scored *below* BM25 (MRR 0.419 vs 0.429); the table above is after the fixes.

Paired sign test, looka vs bm25, held-out: hit@1 25 wins / 7 losses (p = 0.002), MRR 45 / 21 (p = 0.004),
recall@10 22 / 7 (p = 0.008). All 210 commits: p < 0.001 on each.

Caveat on aider: its repo map is built to rank symbols for a chat with files already added, not to retrieve files
from a one-line task; this is aider used as a file retriever, out of the box.

## 2. Behaviour — does `looka probe` catch real breakage?

Run on clones of real projects, then with one bug injected, then with the fix:

| project | kind | clean | injected bug | caught as |
|---|---|---|---|---|
| a Godot 4.7 game | Godot 4.7 game | PASS (import, 2 QA runners, 600 frames) | parse error in `main.gd` | `main.gd:57 Parse Error: Identifier "undeclared_speed" not declared` |
| a Godot 4.7 game | Godot 4.7 game | — | call to a missing method in `_ready` | `main.gd:57 Invalid call. Nonexistent function …` |
| a Next.js 16 app | Next.js 16 | PASS | `onClick` in a component rendered by a Server Component (the class of a real production outage in repo A) | `500 /` + `Error: Event handlers cannot be passed to Client Component props.`; PASS again after `'use client'` |
| a static web game | static web game | — | none injected | **real bug found**: `dev/v4-room.html` loads `css/game.css` relative to `dev/` (404) |
| fixtures/web-game | static web game | PASS | JS error on load | `js/game.js:4 Uncaught ReferenceError` via headless Chromium |
| LookaProbe (fixture) | SwiftUI iOS app, iOS 27 simulator | PASS (build, launch, running, screenshot, app stdout) | out-of-range array read in `.onAppear` | `LookaProbeApp.swift:24 crashed: EXC_BREAKPOINT in ContentView.body.getter (Fatal error: Index out of range)` |
| LookaProbe (fixture) | SwiftUI iOS app | — | undefined identifier | `LookaProbe/LookaProbeApp.swift:19:27 cannot find 'tapCount' in scope` |
| repo A | Next.js 16 + Prisma | — | none | 60 pages crawled; 500s traced to `Missing configured driver adapter` (no DB in the sandboxed env) and 404 links behind feature flags |

## 3. Agents — do agents finish more tasks with looka?

`bench/agent_tasks.py`, SWE-bench style. A task is a real commit that changes source and tests, where the
commit's tests fail on the parent and pass on the commit. The agent gets only the commit subject on the parent
tree (future refs removed); acceptance = the commit's own test files, applied after the agent finishes.
Same model (Sonnet) in both arms; arm B is told to use `looka context`, `impact` and `verify --changed`.

Four rounds, 38 distinct tasks from five repos (A, B: Next.js apps; C: Python library; D, E: TypeScript);
19 tasks were run twice, so 57 paired runs per arm. Compared **per task** (success rate of A vs B on the same
task), so repeated runs are not counted as independent samples:

| arm | accepted runs | tasks where this arm did better | median tokens | median wall time |
|---|---|---|---|---|
| A — no looka | 19/57 (33%) | 0 | 76.4k | 43 s |
| B — looka | 22/57 (39%) | 3 | 81.0k | 91 s |

35 tasks tied. Sign test over the 3 non-tied tasks: p = 0.25. **There is no statistically supported
difference in task completion.** The earlier 4–0 in favour of B (round 3) did not replicate when the same 19
tasks were re-run with the current binary (round 4 alone: A 8, B 7). B costs ~6% more tokens and ~2× wall time.

What the agent runs did produce is a list of real product bugs, all fixed with regression tests:
- `verify --changed` took 2 min 16 s on repo A (> 10 min under 12 concurrent agents) → lint narrowed to
  changed files, baselines cached per HEAD → 35 s cold, 9 s cached.
- vitest suite-load failures (`FAIL file [ file ]`), mypy `file.py: error:`, ruff full-format output and eslint
  stylish output were not parsed, so pre-existing failures looked introduced.
- a baseline cached by an older looka binary was reused by a newer parser → the binary is now part of the key.
- `FORCE_COLOR=0` turned colours *on* in some tools → removed.
- reports buried introduced failures under pre-existing ones and had no pass count.
- `context` could not point at `package.json` for a "lint script" task → build config files are now indexed.

**Smaller model (Haiku), round 5.** The hypothesis was that a weaker model gains more from better context. The 26
round-4 tasks were re-run with Haiku in both arms (same prompts, one run per arm per task): **A 14/26, B 14/26**, per task
1–1 with 24 ties (p = 1.0). Median tokens were equal (~92k); B took ~1.9× the wall time (median 346 s vs 186 s).
The hypothesis is not supported on this task set either. Two product bugs came out of it:
- an agent called the MCP `looka_verify` without `root`, got the server's own project verified and took the PASS as
  its own → every MCP text result now starts with `root: <path>`.
- the benchmark symlinked the source repo's `.venv`, so one agent's `pip install -e .` re-pointed the owner's real
  install at a scratch copy → `.venv`/`node_modules` are now APFS-cloned, never symlinked (the install was restored).

Interpretation: on these tasks the strong model already navigates small and medium repos well; the measurable
value of looka is in the capabilities benchmarked directly (retrieval, failure attribution, behaviour probes,
cross-stack contracts), not in a higher solve rate. Neither a strong (Sonnet) nor a small (Haiku) model
solved more tasks with looka here; a larger or harder task set (bigger repos, cross-module changes) is the open question.

## 4. Portuguese task text over English code

60 held-out tasks were rewritten in pt-BR the way a Brazilian developer types them (keeping the English terms
people keep: hero, tracking, CSP…). Before any pt-BR support, hit@1 fell from 0.533 (English) to 0.400.
After accent folding, Portuguese plurals, a dev-vocabulary dictionary and a vocabulary bridge for terms absent
from the code — all enabled only when the task is detected as Portuguese — measured on a **fresh** sample of
58 other tasks (never inspected while building the dictionary):

| | before | after |
|---|---|---|
| pt-BR hit@1 | 0.310 | **0.345** |
| pt-BR recall@10 | 0.447 | **0.488** |
| pt-BR MRR | 0.435 | **0.478** |
| English hit@1 (same tasks) | 0.397 | 0.397 |

The gap to English (0.397) is narrowed, not closed.

## 5. Mutation — does `verify --changed` tell a breaking change from a harmless one?

`bench/mutation.py`, 2026-10-11, on the five owner repos with test suites (A: Next.js 16 + Prisma + Postgres, B: Next.js + Supabase,
C: Python library, D: Next.js site, E: TypeScript service), cloned at HEAD. Per repo, 12 source files that `impact` links to tests got one
injected bug each (flipped comparison, `&&`↔`||`, `true`↔`false`, `+1`↔`-1`) and, separately, a harmless
comment. The oracle is the full test suite: a mutant counts only if a test file fails that does not fail on
HEAD (24 of 60 mutants; the rest survive every test). Signals compared: the full suite's exit code (what an
agent sees without attribution) and `lkc verify --changed`.

| | detectable bugs caught | false alarms on harmless edits | median time |
|---|---|---|---|
| **`lkc verify --changed`** | **24/24** | **0/60** | 25 s |
| full suite, exit code | 24/24 | 12/60 | 16 s |

- The 12 false alarms of the full suite are all of repo A's harmless edits: one test fails on HEAD (it needs a
  database), so the exit code is non-zero after any change. `lkc` attributes it to HEAD.
- Narrowing to the affected tests missed none of the 24 bugs.
- The first run had 3 `lkc` false alarms, which were two bugs, both fixed and re-measured above: pytest was
  handed `.test.mjs` files in a mixed Python/Node root, and the HEAD clone lacked git-ignored generated code
  (`prisma/generated-client`), so HEAD failed differently.
- Time: `lkc` also runs typecheck and lint, so it is slower than the bare test suite on small repos (repo B
  37 s vs 9 s) and faster on the larger one (repo A 15 s vs 59 s). Runs were 5 repos in parallel, so absolute
  times are inflated.
- Caveat: 24 detectable mutants is a small sample, and only 1 of 5 repos had a failing HEAD.

With `--ci` the same repo A tree goes from 1 failing test to 2825/2825 (Postgres replicated from its CI job),
and `probe` pages that answered 500 for lack of `DATABASE_URL` render.

## 6. Gate — do real agents leave fewer broken changes with the Stop hook?

`bench/gate_ab.py`, 2026-10-14. The 39 tasks of section 3, each run twice as a real headless agent
(`claude -p`, only project and local settings loaded: no user hooks, plugins or MCP servers), once with the
`lkc gate` SessionStart/Stop hooks and once without. The agent is never told about lkc; it meets the gate only
when the Stop hook blocks. Three rounds: Sonnet, Haiku, Sonnet again (117 pairs, 234 runs, about US$ 25).

| | without gate | with gate |
|---|---|---|
| hidden tests accepted | 69/117 | 67/117 (p = 0.63) |
| full-suite regressions | 0 | 0 |
| runs ending with failures `lkc verify --changed` reports as introduced | 5 | 0 |
| runs the gate blocked (all fixed by the agent before stopping) | — | 4 |
| median wall time | 56 s | 125 s |

- **Not a supported effect yet**: 5 vs 0 is paired 5–0, sign test p = 0.062, and the judge is `lkc` itself. The
  failures seen were lint (`@typescript-eslint/no-explicit-any` twice on the same task, ruff `zip()` without
  `strict=`); 2 of the 5 were not recorded. No run in either arm broke the full test suite, so on these tasks the
  gate enforced lint/type cleanliness, not functional correctness.
- No change in solve rate, no extra cost; the price is time: the Stop hook re-runs typecheck, lint and the
  affected tests (repo A: about 40 s → 200 s per run).
- The run found 3 `lkc` false alarms, each reproduced and fixed (counts above exclude them): a UI test that failed
  once under 4 concurrent agents and passed 6/6 alone (now re-run alone before blocking, never for tests the change
  reaches); a Playwright spec handed to vitest, whose config excludes it (`--passWithNoTests` when narrowing);
  `mypy .` scanning `.venv` stubs the symlinked baseline skips. One gate block was such a false alarm, which the
  gate's loop guard let through after one retry.

### Gate speed (2026-10-15)

The gate's price in section 6 was time. Same 12 repo A tasks, real Sonnet agents, 4 concurrent:

| median wall per run | old lkc (3 rounds) | lkc 1d50d25 (2 rounds) |
|---|---|---|
| with gate | 198 s, 218 s, 236 s | **99 s, 103 s** |
| without gate | 72 s, 60 s, 57 s | 54 s, 60 s |

8/12 accepted in every arm of every round. What changed: prose (`.md`, `.txt`, `.rst`) no longer forces the full
lint and test suites (agents update a CHANGELOG on nearly every task); typecheck, lint, tests and baselines run in
parallel; SessionStart warms the HEAD typecheck baseline in the background. Of the 24 gate checks, 18 took 6–46 s;
the 6 slower ones (83–162 s) touched files that force the full suites (`.mdx` the app imports, `.env.example`,
`package.json`), which is deliberate. Each check's duration is kept in `.looka/gate.json`.

## Reproduce

```bash
python3 bench/retrieval.py --looka target/release/looka --aider-python <venv>/bin/python \
  --per-repo 30 --out results.json REPO...
python3 bench/agent_tasks.py select --per-repo 5 --out tasks.json REPO...
python3 bench/agent_tasks.py setup tasks.json TASK_ID WORKDIR
python3 bench/agent_tasks.py score tasks.json TASK_ID WORKDIR
python3 bench/mutation.py --per-repo 12 --seed 11 --out mutation.json REPO
python3 bench/gate_ab.py --tasks tasks.json --out runs.jsonl --model sonnet --jobs 4   # then --summary runs.jsonl
```

`LOOKA_RANK="b=0.4,graph=0.3,…"` overrides ranking weights for sweeps (see `rank` in `src/context.rs`).
Aggregates: `bench/results/summary.json`. Raw per-commit results stay local (git-ignored): they quote private repositories.
