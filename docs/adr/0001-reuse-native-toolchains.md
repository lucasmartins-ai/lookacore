# 0001 — Reuse native toolchains; ship lexical v0 before semantic v1

Status: accepted (2026-10-10)

## Decision

- **Core in Rust**, one binary, CLI + MCP stdio over the same functions. No daemon, no index, no runtime deps.
- **Detection from manifests**, not from conventions we impose. The build system's own commands are what we run.
- **Symbols by regex, ranking by BM25 over identifier tokens + path/symbol boosts + git recency + one import-graph hop.** On 120 held-out commits: hit@1 0.525 vs BM25 0.375 vs aider repo-map 0.100 (`docs/benchmark.md`).
- **Sandbox = command allowlist + scrubbed env + process group + timeout.** No OS jail.

## Why

The first product has to prove agents do better on repos that already exist (see `docs/benchmark.md`).
Tree-sitter grammars, LSP clients, a persistent index and OS sandboxing each add weight before that is known.

## Upgrade paths (do when the benchmark shows the gap)

| Ceiling | Upgrade |
|---|---|
| Regex misses C/C++/Java decls, nested scopes | tree-sitter tags queries per language |
| Lexical ranking misses synonyms / cross-language tasks (pt-BR task, English code) | embeddings or a small translation table — measure first |
| Rescans repo every call (fine to ~10k files) | incremental index keyed by mtime/hash in `.looka/` |
| References are textual (false positives on common names) | LSP `textDocument/references` where a server exists |
| No FS/network isolation | `sandbox-exec` profile (macOS), `bwrap` (Linux), container |
| Whole-suite tests for cargo/make/gradle | per-runner filters exist for vitest, jest, pytest, go; add `cargo test -p`/name filters |
| Typecheck can't be narrowed (whole `tsc`) | incremental `tsc --build` / `tsserver`, or LSP diagnostics for changed files |
| Probe: no Android/Expo, no auth'd pages, no DB | Android emulator via adb, Expo dev client; seed/test env profiles |

Prior art reviewed for shape: aider repo-map, Serena (LSP over MCP), Nx/Turborepo project graphs, Sourcegraph SCIP.

## Later decisions (2026-10-10)

- `verify --changed` attributes failures by re-running the same command on a `git clone --shared` of HEAD;
  results are cached per (HEAD, component, command) in `.looka/baseline/`.
- `probe` is native: the project's own dev server or a built-in static server, an HTTP/1.1 client and crawler in
  std Rust, Godot run headless, and JS errors through any installed Chromium (`--headless=new --dump-dom`,
  console read from `--enable-logging=stderr`). No Playwright/Puppeteer dependency.
