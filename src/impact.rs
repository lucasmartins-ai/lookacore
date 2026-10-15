//! Change Intelligence: who consumes a file or symbol, which tests cover it, what to run after editing it.

use crate::context::{imports, load, outline, Doc};
use crate::detect::{is_test, rel, ProjectMap};
use regex::Regex;
use serde::Serialize;
use std::collections::{BTreeSet, HashSet};
use std::path::Path;
use std::process::Command;
use std::sync::LazyLock;

static IMPORT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*(?:import\b|from\s+\S+\s+import\b|use\s|mod\s|pub\s+(?:use|mod)\s|#include\b|export\s.*\bfrom\b)|\brequire\(|\bimport\(").unwrap()
});

const GENERIC: &[&str] = &[
    "new", "main", "init", "__init__", "default", "test", "setup", "run", "get", "set", "from", "into", "drop", "fmt",
    "clone", "render", "constructor", "index", "lib", "mod", "app", "self", "handler", "props",
];

#[derive(Serialize)]
pub struct Ref {
    pub file: String,
    pub line: usize,
    pub symbol: String,
    pub text: String,
}

#[derive(Serialize)]
pub struct Impact {
    pub targets: Vec<String>,
    pub symbols: Vec<String>,
    pub references: Vec<Ref>,
    pub truncated: usize,
    pub tests: Vec<String>,
    pub verify: Vec<String>,
}

fn stem(rel: &str) -> String {
    let mut parts = rel.rsplit('/');
    let file = parts.next().unwrap_or(rel);
    let s = file.split('.').next().unwrap_or(file);
    if matches!(s, "mod" | "index" | "lib" | "main" | "__init__") {
        parts.next().unwrap_or(s).to_string()
    } else {
        s.to_string()
    }
}

fn word_re(words: &[String]) -> Option<Regex> {
    (!words.is_empty()).then(|| {
        let alt: Vec<String> = words.iter().map(|w| regex::escape(w)).collect();
        Regex::new(&format!(r"\b(?:{})\b", alt.join("|"))).unwrap()
    })
}

/// Files (other than `exclude`) that mention `symbols` or import a module named in `stems`.
fn refs(docs: &[Doc], symbols: &[String], stems: &[String], exclude: &BTreeSet<String>) -> Vec<Ref> {
    let (sym_re, stem_re) = (word_re(symbols), word_re(stems));
    let mut out = vec![];
    for d in docs.iter().filter(|d| !d.is_doc && !exclude.contains(&d.rel)) {
        for (i, line) in d.text.lines().enumerate() {
            let hit = sym_re.as_ref().and_then(|r| r.find(line)).map(|m| m.as_str().to_string()).or_else(|| {
                stem_re.as_ref().filter(|r| IMPORT.is_match(line) && r.is_match(line)).map(|_| "import".to_string())
            });
            if let Some(symbol) = hit {
                out.push(Ref { file: d.rel.clone(), line: i + 1, symbol, text: line.trim().chars().take(160).collect() });
            }
        }
    }
    out
}

fn decls(d: &Doc) -> Vec<String> {
    let mut v: Vec<String> = vec![];
    for s in outline(&d.text) {
        if s.kind != "impl" && s.name.len() >= 3 && !GENERIC.contains(&s.name.as_str()) && !s.name.starts_with("test") && !v.contains(&s.name) {
            v.push(s.name);
        }
    }
    v.truncate(100);
    v
}

pub fn impact(root: &Path, map: &ProjectMap, targets: &[String]) -> Impact {
    let docs = load(root);
    let (mut symbols, mut stems, mut files) = (vec![], vec![], BTreeSet::new());
    for t in targets {
        let p = root.join(t);
        if p.is_file() {
            let r = rel(root, &p.canonicalize().unwrap_or(p));
            if let Some(d) = docs.iter().find(|d| d.rel == r) {
                symbols.extend(decls(d).into_iter().filter(|s| !symbols.contains(s)).collect::<Vec<_>>());
            }
            stems.push(stem(&r));
            files.insert(r);
        } else if !symbols.contains(t) {
            symbols.push(t.clone());
        }
    }

    let mut references = refs(&docs, &symbols, &stems, &files);
    // HTTP endpoints: callers in any component (web client, game, worker) are consumers of the route file
    let contracts = crate::contracts::contracts(root, map);
    for e in contracts.endpoints.iter().filter(|e| files.contains(&e.file)) {
        for c in &e.callers {
            let (file, line) = c.rsplit_once(':').map_or((c.clone(), 0), |(f, l)| (f.to_string(), l.parse().unwrap_or(0)));
            references.push(Ref { file, line, symbol: "http".into(), text: format!("calls {} {}", e.method, e.path) });
        }
    }
    // assets (models, textures, audio, scenes): consumers come from the asset reference index
    if targets.iter().any(|t| crate::assets::kind_of(Path::new(t)).is_some()) {
        let report = crate::assets::report(root, map);
        for a in report.assets.iter().filter(|a| targets.iter().any(|t| t.trim_start_matches("./") == a.path)) {
            for u in &a.used_by {
                references.push(Ref { file: u.clone(), line: 0, symbol: "asset".into(), text: format!("loads {}", a.path) });
            }
        }
    }
    let consumers: BTreeSet<String> = references.iter().map(|r| r.file.clone()).filter(|f| !is_test(f)).collect();
    let mut tests: BTreeSet<String> = files.iter().filter(|f| is_test(f)).cloned().collect();

    // tests that depend on the targets through the import graph (up to 3 importer hops)
    let imp = imports(&docs);
    let mut rev: Vec<Vec<usize>> = vec![vec![]; docs.len()];
    for (i, js) in imp.iter().enumerate() {
        for &j in js {
            rev[j].push(i);
        }
    }
    let mut frontier: Vec<usize> = docs.iter().enumerate().filter(|(_, d)| files.contains(&d.rel)).map(|(i, _)| i).collect();
    let in_graph = frontier.iter().any(|&i| !imp[i].is_empty() || !rev[i].is_empty());
    let mut seen: HashSet<usize> = frontier.iter().copied().collect();
    for _ in 0..3 {
        let mut next = vec![];
        for i in frontier {
            for &k in &rev[i] {
                if seen.insert(k) {
                    if is_test(&docs[k].rel) {
                        tests.insert(docs[k].rel.clone());
                    } else {
                        next.push(k);
                    }
                }
            }
        }
        frontier = next;
    }

    // fallback for code the graph can't resolve (symbol-only targets, go, C, unknown aliases): textual
    // references, then tests mentioning the direct consumers, then tests named after the target
    if !in_graph {
        tests.extend(references.iter().map(|r| r.file.clone()).filter(|f| is_test(f)));
        if !consumers.is_empty() {
            let (mut s2, mut st2) = (vec![], vec![]);
            for d in docs.iter().filter(|d| consumers.contains(&d.rel)) {
                s2.extend(decls(d));
                st2.push(stem(&d.rel));
            }
            let mut seen = files.clone();
            seen.extend(consumers.iter().cloned());
            tests.extend(refs(&docs, &s2, &st2, &seen).into_iter().map(|r| r.file).filter(|f| is_test(f)));
        }
        for t in &map.test_files {
            let name = t.rsplit('/').next().unwrap_or(t).to_lowercase();
            if stems.iter().any(|s| s.len() >= 3 && name.contains(&s.to_lowercase())) {
                tests.insert(t.clone());
            }
        }
    }

    let mut verify = vec![];
    for f in files.iter().chain(consumers.iter()) {
        if let Some(c) = map.owner(f) {
            if let Some(cmd) = c.cmd("test").or_else(|| c.cmd("typecheck")) {
                let line = format!("({}) {}", c.dir, cmd.run);
                if !verify.contains(&line) {
                    verify.push(line);
                }
            }
        }
    }
    let truncated = references.len().saturating_sub(300);
    references.truncate(300);
    Impact { targets: targets.to_vec(), symbols, references, truncated, tests: tests.into_iter().collect(), verify }
}

/// Uncommitted files (tracked and untracked) under `root`, relative to it.
pub fn changed_files(root: &Path) -> Vec<String> {
    changes(root).0
}

/// Build output, caches and dependencies: never a change anyone made by hand.
pub fn generated(p: &Path) -> bool {
    p.components().any(|c| matches!(c.as_os_str().to_str(), Some("node_modules" | "target" | "__pycache__" | ".venv" | "dist" | "build" | ".next" | ".looka" | ".pytest_cache" | ".mypy_cache" | ".ruff_cache")))
}

/// Uncommitted source files under `root`, and whether that list covers every relevant change.
/// `false` when something that can't be narrowed changed (a deletion, config, fixture, manifest): callers
/// should then fall back to the full test/lint commands.
pub fn changes(root: &Path) -> (Vec<String>, bool) {
    let git = |args: &[&str]| Command::new("git").arg("-C").arg(root).args(args).output().ok().filter(|o| o.status.success());
    let (Some(top), Some(st)) = (git(&["rev-parse", "--show-toplevel"]), git(&["status", "--porcelain", "-z", "--untracked-files=all"])) else {
        return (vec![], true);
    };
    let top = Path::new(String::from_utf8_lossy(&top.stdout).trim()).to_path_buf();
    let raw = String::from_utf8_lossy(&st.stdout).into_owned();
    let mut entries = raw.split('\0');
    let (mut files, mut complete) = (vec![], true);
    while let Some(e) = entries.next() {
        if e.len() < 4 {
            continue;
        }
        let (status, path) = e.split_at(3);
        if status.starts_with('R') || status.starts_with('C') {
            entries.next(); // -z puts the rename source in its own field
        }
        let p = top.join(path);
        // build output only when untracked: tracked source under a dir named `build/` or `dist/` is a real change
        if !p.starts_with(root) || (status.starts_with("??") && generated(&p)) {
            continue;
        }
        // prose (CHANGELOG.md, docs/*.md) can't break a build or test, and agents update it on nearly every
        // task: it must not force the full lint and test suites. `.mdx` is not prose here (apps import it).
        // ponytail: a test that reads README.md would be missed; add a manifest list if that ever shows up
        if p.extension().and_then(|e| e.to_str()).is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "md" | "markdown" | "txt" | "rst")) {
            continue;
        }
        if status.contains('D') || !p.is_file() || crate::detect::language(&p).is_none() {
            complete = false;
            continue;
        }
        files.push(rel(root, &p));
    }
    (files, complete)
}

pub fn render(i: &Impact) -> String {
    let mut s = format!("# Impact: {}\nsymbols: {}\n", i.targets.join(", "), i.symbols.join(", "));
    let mut last = "";
    for r in &i.references {
        if r.file != last {
            s += &format!("\n{}\n", r.file);
            last = &r.file;
        }
        s += &format!("  L{} [{}] {}\n", r.line, r.symbol, r.text);
    }
    if i.references.is_empty() {
        s += "\nno references outside the targets\n";
    }
    if i.truncated > 0 {
        s += &format!("… {} more references\n", i.truncated);
    }
    s += &format!("\ntests likely affected: {}\n", if i.tests.is_empty() { "none found".into() } else { i.tests.join(", ") });
    if !i.verify.is_empty() {
        s += &format!("verify with: {}\n", i.verify.iter().map(|v| format!("`{v}`")).collect::<Vec<_>>().join(", "));
    }
    s
}
