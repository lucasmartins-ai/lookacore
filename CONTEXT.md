# Domain language

- **Project map** — what `looka map` returns: languages, components, entrypoints, test files. Rebuilt on every call (no index yet).
- **Component** — one buildable unit, identified by `(dir, ecosystem)`, found from a manifest. A Cargo/CMake/Gradle root swallows nested manifests (workspaces); npm and Python components can nest (monorepos).
- **Ecosystem adapter** — a function in `src/detect.rs` that reads one manifest kind and emits commands. Adding a stack = adding one adapter.
- **Command kind** — `install | build | typecheck | lint | test | run`. `verify` runs `typecheck, lint, test` by default; never `install` or `run`.
- **Available** — the command's binary exists on PATH (or in the component, e.g. `.venv/bin/python`, `node_modules/.bin/tsc`). Unavailable commands are reported as skipped, never as passing.
- **Context pack** — ranked files with outline and snippets for a task, capped by a char budget, plus the commands that verify those files.
- **Impact** — textual references to a target's symbols, plus the tests that depend on it through the import graph (up to 3 importer hops; textual 2-hop fallback when the graph cannot resolve the target).
- **Evidence** — the saved result of a verify run: command, exit code, duration, diagnostics, output tail.
- **Introduced vs pre-existing failure** — with `verify --changed`, a failure that also occurs on a pristine clone of HEAD (with `--base`, of the merge-base) is pre-existing; PASS = zero introduced.
- **Base ref** — `--base <ref>` on `verify`/`impact`: the change set becomes the commits since the merge-base of `<ref>` and HEAD (`git diff <ref>...HEAD`) plus the working tree, and the attribution baseline is that merge-base commit (cache keyed by it). For pre-push and CI, where the work is already committed.
- **Probe** — behaviour evidence: the app/game is run and observed (HTTP crawl of a web app, headless Godot run). Saved as `.looka/evidence/probe-<ts>.json`.
- **Dynamic template** — a file-system route with parameters (`/foods/[slug]`); the probe tries to reach each one through a crawled link.
- **Asset** — a model, texture, audio, scene or font file. Its *users* are the runtime files that reference it (quoted paths, `res://`, loader paths, `${}` patterns, Unity `.meta` GUIDs); docs, tests and tools never count as users.
- **Target profile** — `web | godot | unity | unreal`, from the detected engine; sets asset budgets and export settings.
- **Shipped asset** — loaded by runtime code, under a static web root, or inside an engine project; only shipped assets get budget warnings.
- **Blank render** — a canvas page whose whole screenshot is ≥ 99.9% one colour.
- **Contract** — an HTTP endpoint (method + normalized path, params as `*`, catch-alls as `**`) and the calls that reach it from any component. A *broken* call targets a path prefix this repo serves but no endpoint matches.
- **Gate** — `verify --changed` run *for* the agent: Claude Code's Stop hook blocks finishing, git's pre-commit blocks committing, while the changes introduce failures. A session is judged only on what changed after it started (SessionStart fingerprint).
- **Not verified** — a changed source file that no command which ran could read: zero commands ran (no component, tool missing, no affected test), or the ones that ran belong to other components or languages (cargo beside a Python script). Never a pass: the gate blocks it once, like a failure. Changes to docs, config or assets alone are not source and still pass; source no adapter reads (shell, SQL) counts only when nothing ran.
- **Pre-existing match** — a failure is pre-existing when the baseline has the same file, id (`code`: test name, compiler code), severity and message, counted: each baseline diagnostic excuses one.
- **CI replication** — `--ci`: the CI test job's env, database services and setup steps reproduced locally before verify/probe; only declared package scripts and migration tools run.
- **False alarm** — a check that fails on a harmless change; running the full suite raises one on every change when HEAD already fails.
- **Diagnostic** — normalized `{severity, file, line, col, code, message, hint}` from any tool's output.

## Constraints

- Language-, architecture- and agent-agnostic: never require a project to change shape to be understood.
- Compilers and test runners are the authority; looka never reimplements them.
- A task is verified only by evidence from the project's own commands.
