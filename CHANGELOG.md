# Changelog

## Unreleased

- **Attribution counts.** A failure is pre-existing only when the baseline has one with the same file, id (test
  name, TS/rustc code), severity and message, and each baseline failure excuses one. Before, any look-alike
  excused all: one old `AssertionError` in a test file, or one old `Object is possibly 'undefined'.`, let every
  new one in that file through as PASS.
- **`--ci` stops when a service can't start.** A service with no local binary, or an image other than
  Postgres/Redis, now exits 2 (MCP: an error) before any setup step or test runs. Before, the job ran on with its
  URLs still naming the CI port on this machine, so a migration or seed could reach the developer's own database.
  Redis must also answer on its port.
- **Not verified, per file.** The gate blocks a changed source file that no check which ran could read, even when
  other checks ran: cargo in the root no longer vouches for a Python script, nor `web/`'s tsc for an `api/`
  package whose pytest is missing. A Rust component's tests also no longer take non-Rust test files.
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
