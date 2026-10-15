//! Diagnostic Engine: normalize compiler, linter and test-runner output into structured diagnostics.

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Diagnostic {
    pub severity: String,
    pub file: Option<String>,
    pub line: Option<usize>,
    pub col: Option<usize>,
    pub code: Option<String>,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    /// also fails on a pristine checkout of HEAD (set by `verify --changed`)
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub preexisting: bool,
    /// introduced on the first run but passed when its test file was re-run alone (set by `verify --changed`)
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub flaky: bool,
}

macro_rules! re {
    ($s:expr) => {
        LazyLock::new(|| Regex::new($s).unwrap())
    };
}
static ANSI: LazyLock<Regex> = re!(r"\x1b\[[0-9;?]*[A-Za-z]");
static RUST_HEAD: LazyLock<Regex> = re!(r"^(error|warning)(?:\[(E\d{4})\])?: (.+)$");
static RUST_LOC: LazyLock<Regex> = re!(r"^\s*--> (.+?):(\d+):(\d+)$");
static RUST_PANIC: LazyLock<Regex> = re!(r"^thread '([^']+)'(?: \(\d+\))? panicked at (.+?):(\d+):(\d+):?\s*(.*)$");
// tsc and the C# compiler (Unity/.NET) share `file(line,col): error CODE: msg`
static TSC: LazyLock<Regex> = re!(r"^(.+?)(?:\((\d+),(\d+)\)|:(\d+):(\d+)) ?[:-] (error|warning) ((?:TS|CS)\d+): (.+?)(?: \[[^\]]*\])?$");
static GCC: LazyLock<Regex> = re!(r"^(.+?):(\d+):(\d+): (?:fatal )?(error|warning)(?:\[([^\]]+)\])?: (.+)$");
static PYTEST: LazyLock<Regex> = re!(r"^(FAILED|ERROR) (\S+?\.py)(?:::(\S+))?(?: - (.+))?$");
static PY_LOC: LazyLock<Regex> = re!(r"^(\S+?\.py):(\d+): (\w+(?:Error|Exception|Exit))$");
// mypy/pyright-style: `pkg/mod.py:12: error: msg  [code]` or `pkg/mod.py: error: msg` (no line)
static MYPY: LazyLock<Regex> = re!(r"^(\S+?\.pyi?):(?:(\d+):)?(?:(\d+):)? (error|warning|note): (.+?)(?:\s+\[([\w-]+)\])?$");
// ruff/flake8/pycodestyle concise: `src/a.py:86:89: E501 Line too long (120 > 88)`
static RUFF: LazyLock<Regex> = re!(r"^(\S+?\.pyi?):(\d+):(\d+): ([A-Z]{1,4}\d{2,4}) (.+)$");
// eslint "stylish": a file line, then indented `  12:5  error  msg  rule` lines
static ESLINT_FILE: LazyLock<Regex> = re!(r"^(/\S+|\S+\.(?:[cm]?[jt]sx?|vue|svelte|astro))$");
static ESLINT_ROW: LazyLock<Regex> = re!(r"^\s+(\d+):(\d+)\s+(error|warning)\s+(.+?)\s{2,}(\S+)$");
// Swift runtime traps and uncaught Objective-C exceptions (iOS/macOS app logs)
static SWIFT_FATAL: LazyLock<Regex> = re!(r"^(?:.*?: )?(\S+?\.swift):(\d+): Fatal error: (.+)$");
static OBJC_UNCAUGHT: LazyLock<Regex> = re!(r"\*\*\* Terminating app due to uncaught exception '(\w+)', reason: '(.+)'");
static GO: LazyLock<Regex> = re!(r"^\s*(?:\./)?(\S+?\.go):(\d+)(?::(\d+))?: (.+)$");
static NODE_FAIL: LazyLock<Regex> = re!(r"^\s*not ok \d+ - (.+)$");
static NODE_LOC: LazyLock<Regex> = re!(r"^\s*location: '(?:file://)?(.+?):(\d+):(\d+)'$");
// node >= 23 spec reporter summary: `test at src/a.test.ts:5:1` then `✖ name (2.0ms)`
static NODE_AT: LazyLock<Regex> = re!(r"^\s*test at (?:file://)?(.+?):(\d+):(\d+)$");
static NODE_X: LazyLock<Regex> = re!(r"^\s*✖ (.+?) \([\d.]+m?s\)$");
// vitest: `FAIL  file > suite > test`, or `FAIL  file [ file ]` when the whole file failed to load
static JEST_FAIL: LazyLock<Regex> = re!(r"^\s*FAIL\s+(\S+)(?:\s+\[[^\]]*\])?(\s+>\s+.+)?\s*$");
static VITEST_FILE: LazyLock<Regex> = re!(r"^\s*❯\s+(\S+\.(?:test|spec)\.[cm]?[jt]sx?)\s+\(");
static TEST_X: LazyLock<Regex> = re!(r"^\s*[×✕]\s+(.+?)(?:\s+\(?\d+(?:\.\d+)?\s?m?s\)?)?$");
static GODOT_HEAD: LazyLock<Regex> = re!(r"^(?:USER )?(SCRIPT ERROR|ERROR|WARNING|SCRIPT WARNING): (.+)$");
static GODOT_AT: LazyLock<Regex> = re!(r"^\s*at: .*?\(?res://([^:()]+):(\d+)\)?\s*$");
static NEXT_ERR: LazyLock<Regex> = re!(r"^\s*⨯\s+(.+)$");
static MISSING: LazyLock<Regex> =
    re!(r"(command not found: \S+|\S+: command not found|No module named '[^']+'|Cannot find module '[^']+')");

const RUST_SUMMARY: &[&str] = &["could not compile", "aborting due to", "test failed", "build failed", "warning emitted", "warnings emitted", "generated"];

fn d(sev: &str, file: Option<&str>, line: Option<&str>, col: Option<&str>, code: Option<&str>, msg: &str) -> Diagnostic {
    let n = |s: Option<&str>| s.and_then(|v| v.parse().ok());
    Diagnostic {
        severity: sev.to_string(),
        file: file.map(String::from),
        line: n(line),
        col: n(col),
        code: code.map(String::from),
        message: msg.trim().to_string(),
        hint: hint(code, msg),
        preexisting: false,
    flaky: false,
    }
}

fn hint(code: Option<&str>, msg: &str) -> Option<String> {
    let m = msg.to_lowercase();
    let h = if m.contains("no module named") || m.contains("cannot find module") || code == Some("TS2307") {
        "missing dependency or wrong import path: check the manifest, then run the component's install command"
    } else if m.contains("command not found") {
        "toolchain missing on PATH: install it, or check `looka map` for available commands"
    } else if matches!(code, Some("E0425" | "E0433" | "E0412" | "TS2304")) {
        "unresolved name: missing import, typo, or symbol was renamed"
    } else if matches!(code, Some("E0308" | "TS2322" | "TS2345")) {
        "type mismatch: compare expected vs found types at this location"
    } else if m.contains("assert") {
        "test expectation failed: compare expected vs actual values"
    } else {
        return None;
    };
    Some(h.to_string())
}

fn g<'a>(c: &regex::Captures<'a>, k: usize) -> Option<&'a str> {
    c.get(k).map(|m| m.as_str()).filter(|s| !s.is_empty())
}

pub fn strip_ansi(s: &str) -> String {
    ANSI.replace_all(s, "").into_owned()
}

pub fn parse(raw: &str) -> Vec<Diagnostic> {
    let text = ANSI.replace_all(raw, "");
    let lines: Vec<&str> = text.lines().collect();
    let mut out: Vec<Diagnostic> = vec![];
    let mut test_file: Option<String> = None; // vitest/jest: failing test lines follow their file's line
    let mut lint_file: Option<String> = None; // eslint stylish: rows follow their file's line
    for (i, l) in lines.iter().enumerate() {
        let next = || lines.get(i + 1).map(|s| s.trim()).unwrap_or("");
        let found = if let Some(c) = GODOT_HEAD.captures(l) {
            let loc = lines.iter().skip(i + 1).take(3).find_map(|n| GODOT_AT.captures(n));
            let sev = if c[1].contains("WARNING") { "warning" } else { "error" };
            Some(d(sev, loc.as_ref().map(|m| &m[1]), loc.as_ref().map(|m| &m[2]), None, None, &c[2]))
        } else if let Some(c) = NEXT_ERR.captures(l) {
            Some(d("error", None, None, None, None, &c[1]))
        } else if let Some(c) = RUST_HEAD.captures(l) {
            let loc = lines.iter().skip(i + 1).take(4).find_map(|n| RUST_LOC.captures(n));
            let msg = &c[3];
            if loc.is_none() && RUST_SUMMARY.iter().any(|s| msg.contains(s)) {
                None
            } else {
                Some(d(&c[1], loc.as_ref().map(|m| &m[1]), loc.as_ref().map(|m| &m[2]), loc.as_ref().map(|m| &m[3]), g(&c, 2), msg))
            }
        } else if let Some(c) = RUST_PANIC.captures(l) {
            let msg = g(&c, 5).unwrap_or_else(next);
            Some(d("error", g(&c, 2), g(&c, 3), g(&c, 4), g(&c, 1), msg))
        } else if let Some(c) = TSC.captures(l) {
            Some(d(&c[6], g(&c, 1), g(&c, 2).or(g(&c, 4)), g(&c, 3).or(g(&c, 5)), g(&c, 7), &c[8]))
        } else if let Some(c) = GCC.captures(l) {
            Some(d(&c[4], g(&c, 1), g(&c, 2), g(&c, 3), g(&c, 5), &c[6]))
        } else if let Some(c) = SWIFT_FATAL.captures(l) {
            Some(d("error", g(&c, 1), g(&c, 2), None, Some("swift-fatal"), &format!("Fatal error: {}", &c[3])))
        } else if let Some(c) = OBJC_UNCAUGHT.captures(l) {
            Some(d("error", None, None, None, g(&c, 1), &c[2]))
        } else if let Some(c) = RUFF.captures(l) {
            Some(d("error", g(&c, 1), g(&c, 2), g(&c, 3), g(&c, 4), &c[5]))
        } else if let Some(c) = ESLINT_FILE.captures(l) {
            lint_file = Some(c[1].to_string());
            None
        } else if let Some(c) = ESLINT_ROW.captures(l).filter(|_| lint_file.is_some()) {
            Some(d(&c[3], lint_file.as_deref(), g(&c, 1), g(&c, 2), g(&c, 5), &c[4]))
        } else if let Some(c) = MYPY.captures(l) {
            (&c[4] != "note").then(|| d(&c[4], g(&c, 1), g(&c, 2), g(&c, 3), g(&c, 6), &c[5]))
        } else if let Some(c) = PYTEST.captures(l) {
            Some(d("error", g(&c, 2), None, None, g(&c, 3), g(&c, 4).unwrap_or(&c[1])))
        } else if let Some(c) = PY_LOC.captures(l) {
            Some(d("error", g(&c, 1), g(&c, 2), None, None, &c[3]))
        } else if let Some(c) = GO.captures(l) {
            Some(d("error", g(&c, 1), g(&c, 2), g(&c, 3), None, &c[4]))
        } else if let Some(c) = NODE_FAIL.captures(l) {
            Some(d("error", None, None, None, None, &format!("test failed: {}", &c[1])))
        } else if let Some(c) = NODE_LOC.captures(l) {
            // node --test prints the failing test's location in the TAP yaml block after `not ok`
            out.pop_if(|p| p.file.is_none() && p.message.starts_with("test failed: "))
                .map(|p| Diagnostic { file: Some(c[1].to_string()), line: c[2].parse().ok(), col: c[3].parse().ok(), ..p })
        } else if let Some(c) = NODE_AT.captures(l) {
            Some(d("error", g(&c, 1), g(&c, 2), g(&c, 3), None, "test failed"))
        } else if let Some(c) = NODE_X.captures(l) {
            // names the test located by the `test at` line just before; a bare ✖ (progress output) adds nothing
            out.pop_if(|p| p.message == "test failed").map(|p| Diagnostic { message: format!("test failed: {}", &c[1]), ..p })
        } else if let Some(c) = VITEST_FILE.captures(l) {
            test_file = Some(c[1].to_string());
            None
        } else if let Some(c) = TEST_X.captures(l) {
            Some(d("error", test_file.as_deref(), None, None, None, &format!("test failed: {}", &c[1])))
        } else if let Some(c) = JEST_FAIL.captures(l) {
            // `FAIL file > suite > test` repeats a × line already reported; bare `FAIL file` is a suite failure
            test_file = Some(c[1].to_string());
            c.get(2).is_none().then(|| d("error", g(&c, 1), None, None, None, "test suite failed"))
        } else {
            MISSING.captures(l).map(|c| d("error", None, None, None, None, &c[1]))
        };
        if let Some(x) = found {
            if !out.contains(&x) {
                out.push(x);
            }
        }
    }
    out.sort_by_key(|x| x.severity != "error");
    out
}

pub fn render(ds: &[Diagnostic]) -> String {
    ds.iter()
        .map(|x| {
            let loc = match (&x.file, x.line, x.col) {
                (Some(f), Some(l), Some(c)) => format!("{f}:{l}:{c} "),
                (Some(f), Some(l), None) => format!("{f}:{l} "),
                (Some(f), None, _) => format!("{f} "),
                _ => String::new(),
            };
            let code = x.code.as_deref().map(|c| format!("[{c}] ")).unwrap_or_default();
            let hint = x.hint.as_deref().map(|h| format!("\n    hint: {h}")).unwrap_or_default();
            let pre = if x.preexisting { " (pre-existing on HEAD)" } else { "" };
            format!("  {}{pre} {loc}{code}{}{hint}\n", x.severity, x.message)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(s: &str) -> Diagnostic {
        let v = parse(s);
        assert!(!v.is_empty(), "nothing parsed from {s:?}");
        v[0].clone()
    }

    #[test]
    fn rustc_long_format() {
        let x = one("error[E0308]: mismatched types\n  --> src/cart.rs:12:9\n   |\nerror: could not compile `shop`");
        assert_eq!((x.file.as_deref(), x.line, x.col, x.code.as_deref()), (Some("src/cart.rs"), Some(12), Some(9), Some("E0308")));
        assert_eq!(parse("error: could not compile `shop` (lib) due to 1 previous error").len(), 0);
    }

    #[test]
    fn rust_panic_message_on_next_line() {
        let x = one("thread 'total_applies_discount' panicked at tests/cart.rs:7:5:\nassertion `left == right` failed");
        assert_eq!((x.file.as_deref(), x.line), (Some("tests/cart.rs"), Some(7)));
        assert!(x.message.contains("assertion"));
        assert!(x.hint.is_some());
        // rust >= 1.9x prints the thread id
        assert_eq!(one("thread 'main' (4778) panicked at src/main.rs:2:5:\nboom").line, Some(2));
    }

    #[test]
    fn short_rustc_and_gcc() {
        let x = one("src/lib.rs:3:5: error[E0425]: cannot find value `y` in this scope");
        assert_eq!((x.line, x.code.as_deref()), (Some(3), Some("E0425")));
        assert_eq!(one("main.c:10:2: fatal error: foo.h: No such file").file.as_deref(), Some("main.c"));
    }

    #[test]
    fn tsc_both_formats() {
        let a = one("src/cart.ts(4,7): error TS2322: Type 'string' is not assignable to type 'number'.");
        let b = one("\x1b[96msrc/cart.ts\x1b[0m:4:7 - error TS2322: Type 'string' is not assignable to type 'number'.");
        assert_eq!(a.line, Some(4));
        assert_eq!((b.file.as_deref(), b.col, b.code.as_deref()), (Some("src/cart.ts"), Some(7), Some("TS2322")));
    }

    #[test]
    fn pytest_and_go_and_node() {
        let x = one("FAILED tests/test_cart.py::test_total - assert 1300 == 1350");
        assert_eq!((x.file.as_deref(), x.code.as_deref()), (Some("tests/test_cart.py"), Some("test_total")));
        assert_eq!(one("    cart_test.go:14: got 3 want 4").line, Some(14));
        let n = one("not ok 1 - total applies discount\n  ---\n  location: '/repo/src/cart.test.ts:5:1'");
        assert_eq!((n.file.as_deref(), n.line), (Some("/repo/src/cart.test.ts"), Some(5)));
        let spec = parse("✖ total applies discount (2.0ms)\n✖ failing tests:\n\ntest at src/cart.test.ts:5:1\n✖ total applies discount (2.010584ms)\n  AssertionError");
        assert_eq!(spec.len(), 1, "{spec:?}");
        assert_eq!((spec[0].file.as_deref(), spec[0].line, spec[0].message.as_str()), (Some("src/cart.test.ts"), Some(5), "test failed: total applies discount"));
        let v = parse(" ❯ tests/a.test.ts (3 tests | 1 failed) 20ms\n     ✓ ok 1ms\n     × adds tax 3ms\n FAIL  tests/a.test.ts > cart > adds tax");
        assert_eq!(v.len(), 1, "{v:?}");
        assert_eq!((v[0].file.as_deref(), v[0].message.as_str()), (Some("tests/a.test.ts"), "test failed: adds tax"));
        let gd = one("SCRIPT ERROR: Invalid call. Nonexistent function 'jump' in base 'Node2D'.\n   at: _ready (res://player.gd:42)");
        assert_eq!((gd.file.as_deref(), gd.line), (Some("player.gd"), Some(42)));
        let pe = one("SCRIPT ERROR: Parse Error: Identifier \"spd\" not declared in the current scope.\n          at: GDScript::reload (res://main.gd:7)");
        assert_eq!(pe.line, Some(7));
        assert_eq!(one(" ⨯ Error: Event handlers cannot be passed to Client Component props.").severity, "error");
        let m = one("benchmarks/usability/run.py: error: Duplicate module named \"run\" (also at \"benchmarks/msi-bench/run.py\")");
        assert_eq!((m.file.as_deref(), m.line), (Some("benchmarks/usability/run.py"), None));
        assert_eq!(one("src/a.py:12: error: Incompatible return value type  [return-value]").code.as_deref(), Some("return-value"));
        let cs = one("Assets/Scripts/Player.cs(12,9): error CS0103: The name 'speed' does not exist in the current context");
        assert_eq!((cs.file.as_deref(), cs.line, cs.code.as_deref()), (Some("Assets/Scripts/Player.cs"), Some(12), Some("CS0103")));
        let r = one("src/lcc/cleaning/speech.py:86:89: E501 Line too long (120 > 88)");
        assert_eq!((r.file.as_deref(), r.line, r.code.as_deref()), (Some("src/lcc/cleaning/speech.py"), Some(86), Some("E501")));
        let e = parse("/repo/app/page.tsx\n  12:5  error  'x' is defined but never used  @typescript-eslint/no-unused-vars\n");
        assert_eq!((e[0].file.as_deref(), e[0].line, e[0].code.as_deref()), (Some("/repo/app/page.tsx"), Some(12), Some("@typescript-eslint/no-unused-vars")));
        let f = one("Swift/ContiguousArrayBuffer.swift:695: Fatal error: Index out of range");
        assert_eq!((f.code.as_deref(), f.message.as_str()), (Some("swift-fatal"), "Fatal error: Index out of range"));
        assert_eq!(one("*** Terminating app due to uncaught exception 'NSInvalidArgumentException', reason: '-[X y]: unrecognized selector'").code.as_deref(), Some("NSInvalidArgumentException"));
        let suite = one(" FAIL  tests/email/email-log.test.ts [ tests/email/email-log.test.ts ]");
        assert_eq!((suite.file.as_deref(), suite.message.as_str()), (Some("tests/email/email-log.test.ts"), "test suite failed"));
        assert!(one("ModuleNotFoundError: No module named 'shopx'").hint.is_some());
    }
}
