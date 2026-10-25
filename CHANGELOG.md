# Changelog

## Unreleased

- **Gate: nothing ran is not a pass.** Changed source files with zero commands run (no component, tool
  missing, no affected test) now block as `NOT VERIFIED: no check ran for changed source files …` (git:
  exit 1; Claude Code: `{"decision":"block"}`, once). Docs, config or asset changes alone still pass.
- **`--base <ref>`** on `verify` and `impact` (CLI and MCP `looka_verify`/`looka_impact`): the change set is
  `git diff <ref>...HEAD` plus the working tree, so committed work can be verified (pre-push, CI), and failures
  are attributed against a pristine clone at the merge-base (baseline cache keyed by that commit). A bad ref
  exits 2. The JSON report carries `base`.

## 0.2.0 (2026-10-15)

- **Claude Code plugin** (`plugin/`, marketplace in `.claude-plugin/`): the gate's SessionStart/Stop hooks and the
  MCP server, no `lkc init` needed. Without the `lkc` binary it warns once and blocks nothing. It runs nothing
  in a repo until you `lkc trust` it (`lkc init` trusts too): verifying executes the repo's own scripts.
- **Gate speed**: prose edits (`.md`, `.txt`, `.rst`) no longer force full suites; typecheck, lint, tests and
  HEAD baselines run in parallel; SessionStart warms the HEAD typecheck baseline in the background; a tree that
  already passed is not re-verified. Real agents on repo A: gated median 198–236 s → 99–103 s.
- **Fewer false alarms**: a test failing once under load is re-run alone (never for tests the change reaches);
  a narrowed selection the runner's config excludes is not a failure; diagnostics in git-ignored dependency
  dirs are not attributed to the diff.
- **Fixes**: tracked files under `build/` or `dist/` are changes (the gate passed them unverified);
  `lkc init` refuses to rewrite a `.claude/settings.json` it can't parse; `.looka/` ignores itself.
- `install.sh`: prebuilt binaries from GitHub releases, SHA-256 verified.
- Benchmarks: real-agent gate A/B (`bench/gate_ab.py`, 117 pairs), mutation benchmark (`bench/mutation.py`).

## 0.1.0

- `map`, `context`, `impact`, `verify` (`--changed`, `--ci`), `probe`, `assets`, `contracts`, `diagnose`, `mcp`,
  `gate`, `init`.
