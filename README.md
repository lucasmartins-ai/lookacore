# LookaCore (LKC)

The command is `lkc` (`looka` is kept as an alias).

A development layer for AI coding agents. It does not generate apps and does not replace Claude Code, Codex,
Cursor or local models. Its core job is to stop an agent from calling a change done while it breaks something:

```bash
lkc init     # Claude Code Stop hook + git pre-commit: the agent can't finish, nor commit, while its changes
             # introduce failures (failures that already exist on HEAD don't count), or change source
             # code that no check could run against (NOT VERIFIED; docs, config and assets alone pass)
```

Around that gate, it gives any agent the same seven things in any repository:

1. **a map** of what the project is and how it builds and tests (`map`)
2. **a small context pack** for the task at hand instead of the whole repo (`context`, `outline`)
3. **change impact**: consumers and the tests that depend on a file through the import graph (`impact`)
4. **evidence**: the project's own checks run in a controlled environment, with normalized diagnostics, and
   failures attributed to the change or to HEAD (`verify`, `verify --changed`, `diagnose`)
5. **behaviour**: run the thing and observe it — crawl a web app, run a game headless, screenshot it (`probe`)
6. **assets**: one view of models, textures and audio across Blender, three.js, Godot, Unity and Unreal —
   budgets per target engine, who loads each asset, and headless Blender exports per engine (`assets`)
7. **contracts**: the HTTP API each component serves and who calls it from which stack — web client, Godot or
   Unity game, Python worker — with broken calls and endpoints nobody calls (`contracts`)

The existing architecture is input to understand, not a rule to impose. Compilers, test runners and build
systems stay the authority; looka discovers them and normalizes what they say.

## Measured (owner's repos — details in [docs/benchmark.md](docs/benchmark.md))

- **Breaking vs harmless changes** (mutation benchmark, 5 repos, 2026-10-11): `verify --changed` caught all
  24 injected bugs the test suites can detect and raised 0 false alarms on 60 harmless edits. Running the full
  suite also caught 24/24 but alarmed on 12/60: every harmless edit in the repo whose HEAD already fails a test.
- **Gate with real agents** (117 paired headless `claude -p` runs, 2026-10-14): same solve rate (69 vs 67) and
  0 full-suite regressions in both arms; 5 runs without the gate ended with failures lkc reports as introduced
  (lint) vs 0 with it — indicative only (p = 0.06). Cost: median run 56 s → 125 s at the time; after the gate
  speed work, on repo A's 12 tasks the gated agent's median went from 198–236 s to 99–103 s (54–60 s ungated).
- **CI replication**: `--ci` turned repo A's 1 database-dependent test failure into 2825/2825 and its
  pages that answered 500 (no `DATABASE_URL`) into rendered pages.

- **Context retrieval**, 120 held-out commits: the right file first in 52.5% of tasks vs 37.5% for BM25 and
  10% for aider's repo-map (p = 0.002 vs BM25, paired sign test); median 0.56 s vs aider's 17.9 s.
- **Behaviour**: `probe` caught an injected Godot parse error and runtime error with `file.gd:line`, a Next.js
  Server/Client Component 500 with the exact server error, a JS `ReferenceError` in a web game, and one real
  broken stylesheet path in an existing project.
- **Agents**, 38 real commits replayed SWE-bench style (57 paired runs), same model: 22/57 accepted with looka
  vs 19/57 without; per task 3 better, 0 worse, 35 tied (p = 0.25). With a smaller model (Haiku, 26 tasks): 14 vs 14.
  **No statistically supported difference in solve rate**; looka's measured value is in retrieval, failure attribution, probes and contracts.
- **Portuguese tasks** over English code: hit@1 0.310 → 0.345 on a fresh sample; English unchanged.

## Install

The binary, then the Claude Code plugin (the gate's hooks and the MCP server, no `lkc init` needed):

```bash
curl -fsSL https://raw.githubusercontent.com/lucasmartins-ai/lookacore/main/install.sh | sh
```

```
/plugin install lookacore --marketplace lucasmartins-ai/lookacore
```

From source: `cargo install --path .`. Single static binary, no runtime; prebuilt for macOS (arm64, x86_64) and
Linux x86_64. Without the binary the plugin says so at session start and blocks nothing.

The plugin's hooks fire in every repo, and verifying means running the repo's own scripts, so the gate stays
off until you trust a repo: `lkc trust` in it (`lkc init` trusts too). Until then it only says how, once per
session. The list lives in `~/.config/lookacore/trusted`.

## CLI

```bash
lkc map                                   # components, toolchains, commands (⚠ marks missing tools)
lkc context "cap discount coupons at 50%" # ranked files + outlines + snippets, ~4k tokens by default
lkc outline src/cart.rs
lkc impact src/pricing.rs                 # who uses it; tests that import it (import graph, 3 hops)
lkc impact --changed                      # same, for uncommitted git changes
lkc impact --base origin/main             # same, for the commits since the merge-base with that ref too
lkc verify                                # typecheck + lint + test; exit 1 on FAIL
lkc verify --kinds test --component dashboard
lkc verify --changed                      # only tests affected by uncommitted changes; failures that also
                                            # happen on a pristine HEAD clone are marked pre-existing
lkc verify --changed --ci                 # same, with the CI job's env + database replicated locally
lkc verify --base origin/main             # --changed over a range of commits (pre-push, CI): the commits
                                            # since the merge-base plus uncommitted work, attributed against
                                            # a pristine checkout of that merge-base
lkc init                                  # wire the gate: Claude Code SessionStart/Stop hooks + git pre-commit
lkc gate --hook git                       # what the hooks run
lkc probe                                 # web: start the dev server (or serve index.html), crawl routes,
                                            # check status, assets and server-log errors; Godot: run headless
cargo build 2>&1 | lkc diagnose
```

Every command takes `--root DIR` and `--json`.

## MCP (Claude Code, Codex, Cursor, any MCP client)

`lkc mcp` serves the same operations as tools: `looka_map`, `looka_context`, `looka_outline`,
`looka_impact`, `looka_verify`, `looka_probe`, `looka_diagnose`.

```bash
claude mcp add looka -- lkc mcp
```

Codex (`~/.codex/config.toml`):

```toml
[mcp_servers.looka]
command = "lkc"
args = ["mcp"]
```

Cursor (`.cursor/mcp.json`): `{"mcpServers": {"looka": {"command": "lkc", "args": ["mcp"]}}}`

### Agent contract (paste into AGENTS.md / CLAUDE.md)

With `lkc init` the last rule is enforced by the hooks; the contract still helps agents without hooks.

```md
- Unfamiliar repo: run `looka map` once.
- Before opening files for a task: `looka context "<task>"`.
- Before changing a shared file or public API: `looka impact <file>`.
- A task is done only when `looka verify --changed` reports PASS (no failures introduced). On FAIL, fix the
  diagnostics it marks as introduced.
- Web apps and games: also run `looka probe` and fix any failing page, missing asset or script error.
```

## Supported ecosystems

| Manifest | Commands discovered |
|---|---|
| `Cargo.toml` (workspaces) | build, check, clippy, test, run |
| `package.json` (npm/pnpm/yarn/bun, monorepo folders) | install + scripts: build, typecheck, lint, test, dev; `tsc --noEmit` fallback |
| `pyproject.toml` / `setup.py` / `requirements.txt` | install, pytest, ruff, mypy/pyright; prefers `.venv` |
| `go.mod` | build, vet, test |
| `CMakeLists.txt` | configure+build, ctest |
| `pubspec.yaml` | pub get, analyze, test |
| `build.gradle(.kts)` | build, test (wrapper if present) |
| `Makefile` | build, check, lint, test targets |
| `project.godot` | `--import` (typecheck), `extends SceneTree` runners in `qa/` `test/` `tests/` |
| `*.xcodeproj` | xcodebuild build (simulator, no signing), test |

## Contracts: where stacks meet

```bash
lkc contracts                       # endpoints ← callers, broken calls, endpoints without callers
lkc impact app/api/scores/route.ts  # the Godot script, the React page and the Python job that call it
```

Servers: Next.js route handlers (`app/**/route.ts`, `pages/api`), Express/Fastify/Hono, FastAPI/Flask, axum,
Go `net/http`. Clients: `fetch`/axios/ky (including `${base}/api/x` and path literals passed through helpers),
Godot `HTTPRequest`, Unity `UnityWebRequest`, Python `requests`/`httpx` (f-string bases too). Third-party URLs
are ignored. On the owner's repos it found two real 404s in one Next.js app — calls left behind when a payment-provider route and
an account route were removed — and no false positives on four other repos.

## Games: one layer across Blender, three.js, Godot, Unity, Unreal

```bash
lkc map                                          # engines detected: godot, unity (exact editor version),
                                                   # unreal (.uproject, plugins), three.js/R3F/Babylon/Phaser/Pixi
lkc assets                                       # budget for the detected target, who loads what, missing refs
lkc impact assets/models/rock.gltf               # which scenes/scripts/prefabs load this asset
lkc assets inspect art/hero.blend                # tris after modifiers, materials, missing textures, unapplied scale
lkc assets export art/hero.blend --for godot     # → assets/models/hero.glb  (no Draco: Godot 4 can't import it)
lkc assets export art/hero.blend --for three     # → public/models/hero.glb  (Draco + WebP)
lkc assets export art/hero.blend --for unity     # → Assets/Models/hero.fbx  (Y up, unit scale applied)
lkc assets export art/hero.blend --for unreal    # → SourceArt/hero.fbx      (Z up, centimetres)
lkc assets export --changed --for godot          # only the .blend files modified since the last commit
lkc probe                                        # three.js/WebGL: JS errors (source-mapped), screenshot,
                                                   # blank-frame detection; Godot: headless run
```

| | detect | build / typecheck | test | assets |
|---|---|---|---|---|
| Godot 4 | `project.godot` | `--import` | `extends SceneTree` runners | `res://` refs, Draco = error |
| Unity | `ProjectVersion.txt` → editor from Unity Hub (or `UNITY_PATH`) | batch-mode compile (C# `CS0103` diagnostics) | `-runTests` EditMode | `.meta` GUIDs → prefabs/scenes/materials |
| Unreal | `*.uproject` → `UE_ROOT` / Epic layout | `Build.sh <Name>Editor` | `Automation RunTests` | FBX export conventions |
| three.js & web 3D | `three`, `@react-three/fiber`, Babylon, Phaser, Pixi, PlayCanvas | project scripts | project scripts | loader paths, `${}`-built paths as globs, 2048px/100k-tri web budget |
| Blender | `.blend` files, `blender` on PATH (or `LOOKA_BLENDER`) | — | — | headless inspect/export via an embedded script |

**Experimental: Unity and Unreal.** Their commands are generated from the project and checked on fixtures only;
no editor was available to run them. They run only where the editor is installed (otherwise reported as
skipped, never as passing). Godot, Blender and Chromium paths are verified on real projects.

## Probe: sites, web games, Godot games

| Target | How it runs | What it checks |
|---|---|---|
| Web app (Next, Vite, Nuxt, SvelteKit, Express, Fastify, Hono, Astro…) | the component's own `dev`/`start` script, scrubbed env, own process group; URL read from the server output | `/`, file-system routes (Next `app/` + `pages/`), one link per dynamic template (`/foods/[slug]`), then crawled links; 5xx, 404 links, missing scripts/styles/images/audio, server-log errors (`⨯`, `FATAL`, `…Error:`) |
| Static site / web game (`index.html`) | built-in static file server (no dependency, path-traversal safe) | every `.html`, links, referenced assets |
| Godot 4 game | `godot --headless --path . --quit-after 600` | `SCRIPT ERROR` / parse errors with `res://file.gd:line` |
| iOS app (`*.xcodeproj`) | `xcodebuild` for the simulator, boot a simulator (`LOOKA_SIMULATOR` or the first iPhone), install, launch with the app's console attached | Swift/Clang build errors; still running after 6 s; crash report → exception + the app's own frame with `File.swift:line`; Swift `Fatal error:` / uncaught ObjC exceptions from the log; screenshot + blank-screen check |

JS runtime errors and screenshots need any installed Chromium-family browser (Chrome, Chromium, Brave, Edge,
or `LOOKA_BROWSER`); WebGL runs on SwiftShader, so 3D games render headless. Error lines are mapped back to the
original source through the served source maps (Vite inline maps, `.map` files).
Not yet: Android emulators, Expo/React Native; blank-frame detection looks at the whole frame, so a black canvas
under an HTML HUD is not flagged.

Diagnostics are parsed from rustc/cargo (long + short), panics, tsc, gcc/clang, go, pytest, node `--test`,
jest/vitest, and missing-module / missing-tool errors.

## Verification and the sandbox

`verify` runs only commands that `map` discovered (an agent cannot pass arbitrary shell), in the component
directory, with an allowlisted environment (tokens, keys and cloud credentials are dropped; pass extras with
`LOOKA_ENV_PASS=NAME1,NAME2`), in its own process group, killed on timeout (default 600s). Each run is saved
to `.looka/evidence/<ts>.json` and `latest.json` (git-ignored automatically). No commands ran = not verified;
for the gate, so is a changed source file that no command which ran could read (a Python script beside a Rust
crate, an `api/` package whose pytest is missing while `web/` type-checks).

Not yet: filesystem/network jails. See [ADR 0001](docs/adr/0001-reuse-native-toolchains.md).

`verify --changed` narrows tests through the import graph (vitest, jest, pytest file filters; go packages;
other runners run the full suite), giving each runner only tests in its own language. When something fails,
it clones HEAD (`git clone --shared`, with everything git ignores — installs, generated clients, build
output — linked in), re-runs the same command, and marks matching failures `pre-existing`. PASS then means:
no failure introduced by the working-tree changes.

`verify --base <ref>` (MCP: `base`) widens the change set to `git diff <ref>...HEAD` plus the working tree, so
committed work is verified too (a clean tree after `git commit` is otherwise "nothing changed"). The baseline
clone is checked out at the merge-base of `<ref>` and HEAD, not at the ref's tip: a base branch that moved on
must not explain away a failure the range added. When `<ref>` is an ancestor of HEAD the two are the same. A
ref that doesn't resolve exits 2 instead of reporting no changes. `impact --base <ref>` takes the same set.

`--ci` (on `verify` and `probe`) first replicates the project's CI test job from `.github/workflows`: the
job's env (secrets and `${{ }}` expressions skipped; variables that change how programs run, such as `PATH`,
`LD_*`, `NODE_OPTIONS` and `PYTHON*`, are never replicated), its Postgres/Redis services from local binaries
on a free port with URLs rewritten, and its database setup steps. Setup steps are repository data, so only
package scripts the manifest declares and migration tools (`prisma`, `alembic`, `manage.py migrate`) run,
never arbitrary shell. The services are torn down afterwards. A service that can't be started here (no local
binary, an image other than Postgres/Redis) stops `--ci` with exit 2 before anything runs: the job's URLs would
still name the CI port on this machine, which may be your own database.

## Development

```bash
cargo test        # unit tests + end-to-end on fixtures/{rs,py,ts}-app (also the benchmark repos)
cargo clippy --all-targets -- -D warnings
```

Domain terms: [CONTEXT.md](CONTEXT.md). Benchmark protocol: [docs/benchmark.md](docs/benchmark.md).

## License

MIT, see [LICENSE](LICENSE).
