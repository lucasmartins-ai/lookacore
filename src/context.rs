//! Context Compiler: turn a task description into a small, ranked pack of files, symbols and snippets.

use crate::detect::{self, ProjectMap};
use regex::Regex;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::sync::LazyLock;

// ponytail: regex symbol extraction covers decls in Rust/TS/JS/Python/Go/Kotlin-ish syntax.
// Upgrade: tree-sitter tags + reference graph ranking (aider repo-map style) when precision matters.
static DECL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?m)^[ \t]*(?:export\s+(?:default\s+)?)?(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?(?:unsafe\s+)?(?:abstract\s+)?(fn|struct|enum|trait|impl(?:<[^>]*>)?|mod|class|def|function\*?|interface|type|func(?:\s*\([^)]*\))?)\s+([A-Za-z_]\w*)",
    )
    .unwrap()
});
static TOPVAR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^(?:export\s+)?(?:const|let|var)\s+([A-Za-z_]\w*)\s*[=:]").unwrap());

const STOP: &[&str] = &[
    "the", "and", "for", "with", "that", "this", "from", "into", "when", "then", "should", "must", "use", "using",
    "add", "fix", "make", "new", "all", "any", "its", "are", "was", "not", "but", "can", "will", "each", "also",
    "only", "update", "change", "implement", "create", "remove", "support", "code", "file", "files", "function",
    "bug", "feature", "test", "tests", "que", "para", "com", "uma", "dos", "das", "nos", "nas", "por", "não", "nao",
    "quando", "adicionar", "corrigir", "criar", "fazer", "deve", "precisa", "todo", "toda", "isso", "esse", "essa",
    "como", "mais", "mudar", "alterar", "faz", "deixa", "ajusta", "ajustar", "corrige", "resolve", "resolver",
    "adiciona", "atualiza", "atualizar", "integra", "integrar", "pra", "pro", "sem", "menos", "onde", "novo", "nova",
    "cada", "esta", "este", "aqui", "agora", "ainda", "tambem", "depois", "antes", "entre", "sobre", "pelo", "pela",
    "sim", "ser", "ter", "tem", "vai", "vou", "fica", "ficar", "quero", "queria", "favor", "porque", "entao", "melhora",
    "melhorar", "implementa", "implementar", "garante", "garantir", "usar", "usa", "nao",
];

#[derive(Serialize, Clone, Debug)]
pub struct Symbol {
    pub name: String,
    pub kind: String,
    pub line: usize,
}

#[derive(Serialize)]
pub struct Snip {
    pub start: usize,
    pub end: usize,
    pub code: String,
}

#[derive(Serialize)]
pub struct FileCtx {
    pub path: String,
    pub score: f64,
    pub why: Vec<String>,
    pub outline: Vec<Symbol>,
    pub snippets: Vec<Snip>,
}

#[derive(Serialize)]
pub struct Pack {
    pub task: String,
    pub terms: Vec<String>,
    pub files: Vec<FileCtx>,
    pub also: Vec<String>,
    pub verify: Vec<String>,
    pub chars: usize,
}

pub fn outline(text: &str) -> Vec<Symbol> {
    let nl: Vec<usize> = text.match_indices('\n').map(|(i, _)| i).collect();
    let line = |off: usize| nl.partition_point(|&i| i < off) + 1;
    let mut v: Vec<Symbol> = DECL
        .captures_iter(text)
        .map(|c| Symbol {
            name: c[2].to_string(),
            kind: c[1].split(|ch: char| ch == '(' || ch == '<' || ch.is_whitespace()).next().unwrap_or("").trim_end_matches('*').to_string(),
            line: line(c.get(2).unwrap().start()),
        })
        .chain(TOPVAR.captures_iter(text).map(|c| Symbol {
            name: c[1].to_string(),
            kind: "const".into(),
            line: line(c.get(1).unwrap().start()),
        }))
        .collect();
    v.sort_by_key(|s| s.line);
    v
}

pub fn terms(task: &str) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    let mut add = |t: String| {
        if t.len() >= 3 && !STOP.contains(&t.as_str()) && !out.contains(&t) {
            out.push(t);
        }
    };
    let pt = crate::ptbr::is_portuguese(task);
    for word in task.split(|c: char| !c.is_alphanumeric()).filter(|w| w.len() >= 3) {
        toks(word).into_iter().for_each(&mut add);
        if !pt {
            continue;
        }
        // Portuguese task, English code: add the English words too (the Portuguese one stays, routes use it)
        let w: String = word.to_lowercase().chars().map(crate::ptbr::fold).collect();
        if let Some(sg) = crate::ptbr::singular(&w) {
            add(sg);
        }
        let keys = [Some(w.clone()), crate::ptbr::singular(&w), Some(stem(w.clone()))];
        if let Some(en) = keys.iter().flatten().find_map(|k| crate::ptbr::english(k)) {
            en.split_whitespace().map(|e| stem(e.to_string())).for_each(&mut add);
        }
    }
    out
}

fn split_ident(w: &str) -> Vec<String> {
    let (mut parts, mut cur, mut prev_lower) = (vec![], String::new(), false);
    for ch in w.chars() {
        if (ch == '_' || (ch.is_uppercase() && prev_lower)) && !cur.is_empty() {
            parts.push(cur.to_lowercase());
            cur.clear();
        }
        if ch != '_' {
            cur.push(ch);
        }
        prev_lower = ch.is_lowercase() || ch.is_ascii_digit();
    }
    if !cur.is_empty() {
        parts.push(cur.to_lowercase());
    }
    parts
}

pub struct Doc {
    pub rel: String,
    pub text: String,
    pub symbols: Vec<Symbol>,
    pub is_doc: bool,
    tf: HashMap<String, u32>,
    len: usize,
    path_toks: HashSet<String>,
    sym_toks: HashSet<String>,
}

fn stem(w: String) -> String {
    // crude plural stem, applied identically to queries and documents
    if w.len() > 4 && w.ends_with('s') && !w.ends_with("ss") { w[..w.len() - 1].to_string() } else { w }
}

/// Identifier-aware tokens: `applyDiscount`, `apply_discount` and `apply-discount` all yield apply, discount
/// (plus the whole identifier when it has parts).
pub fn toks(s: &str) -> Vec<String> {
    let mut out = vec![];
    let folded: String = s.chars().map(crate::ptbr::fold).collect();
    for word in folded.split(|c: char| !c.is_alphanumeric()).filter(|w| w.len() >= 3) {
        let parts = split_ident(word);
        if parts.len() > 1 {
            out.push(stem(word.to_lowercase()));
        }
        out.extend(parts.into_iter().filter(|p| p.len() >= 3).map(stem));
    }
    out
}

/// Build/runtime configuration a task may need to touch (scripts, bundler, CI, deploy), not lockfiles.
fn is_config(p: &Path) -> bool {
    let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let stem = name.split('.').next().unwrap_or("");
    matches!(name, "package.json" | "tsconfig.json" | "Cargo.toml" | "pyproject.toml" | "go.mod" | "Dockerfile" | "Makefile"
        | "docker-compose.yml" | "docker-compose.yaml" | "wrangler.toml" | "vercel.json" | "netlify.toml" | "railway.json"
        | "project.godot" | "pubspec.yaml" | "app.json" | "eas.json" | ".env.example" | "middleware.ts")
        || matches!(stem, "next" | "vite" | "vitest" | "jest" | "playwright" | "tailwind" | "postcss" | "eslint" | "astro" | "nuxt" | "svelte")
            && name.contains(".config.")
        || p.components().any(|c| c.as_os_str() == "workflows") && name.ends_with(".yml")
}

/// Source files, markdown docs and build configuration under 256 KB.
pub fn load(root: &Path) -> Vec<Doc> {
    detect::walk(root)
        .into_iter()
        .filter_map(|p| {
            let is_doc = p.extension().is_some_and(|e| e == "md") || is_config(&p);
            if detect::language(&p).is_none() && !is_doc {
                return None;
            }
            if fs::metadata(&p).ok()?.len() > 256_000 {
                return None;
            }
            let text = fs::read_to_string(&p).ok()?;
            let rel = detect::rel(root, &p);
            let symbols = outline(&text);
            let body = toks(&text);
            let mut tf = HashMap::new();
            for t in &body {
                *tf.entry(t.clone()).or_insert(0) += 1;
            }
            Some(Doc {
                path_toks: toks(&rel).into_iter().collect(),
                sym_toks: symbols.iter().flat_map(|s| toks(&s.name)).collect(),
                len: body.len(),
                tf,
                rel,
                symbols,
                text,
                is_doc,
            })
        })
        .collect()
}

/// Tuning knob for benchmark sweeps: LOOKA_RANK="path=2,sym=1".
fn w(name: &str, default: f64) -> f64 {
    std::env::var("LOOKA_RANK")
        .ok()
        .and_then(|v| v.split(',').find_map(|kv| kv.split_once('=').filter(|(k, _)| *k == name).and_then(|(_, x)| x.parse().ok())))
        .unwrap_or(default)
}

/// Commits since each file was last touched (0 = HEAD), from the last 300 commits.
fn recency(root: &Path) -> HashMap<String, usize> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["log", "-n", "300", "--name-only", "--format=%x00", "--relative"])
        .output();
    let mut m = HashMap::new();
    if let Some(o) = out.ok().filter(|o| o.status.success()) {
        for (i, commit) in String::from_utf8_lossy(&o.stdout).split('\0').skip(1).enumerate() {
            for f in commit.lines().filter(|l| !l.is_empty()) {
                m.entry(f.to_string()).or_insert(i);
            }
        }
    }
    m
}

fn common_prefix(a: &str, b: &str) -> usize {
    a.chars().zip(b.chars()).take_while(|(x, y)| x == y).count()
}

/// Vocabulary bridge: a task term that occurs in no file (typically a Portuguese word for English code:
/// visualizacao, componente, catalogo) is joined by the code tokens sharing a long prefix with it
/// (visualization, component, catalog). Terms the code already contains are never expanded.
fn bridge(docs: &[Doc], terms: &[String], on: bool) -> Vec<String> {
    let mut out: Vec<String> = terms.to_vec();
    if !on {
        return out;
    }
    let present = |t: &str| docs.iter().any(|d| d.tf.contains_key(t) || d.path_toks.contains(t));
    let mut vocab: Option<HashSet<&str>> = None;
    for t in terms.iter().filter(|t| t.len() >= 5 && !present(t)) {
        let vocab = vocab.get_or_insert_with(|| docs.iter().flat_map(|d| d.tf.keys().map(String::as_str).chain(d.path_toks.iter().map(String::as_str))).collect());
        let need = 5.max((t.len() * 6).div_ceil(10));
        let mut cands: Vec<(usize, &str)> = vocab.iter().map(|v| (common_prefix(t, v), *v)).filter(|(p, v)| *p >= need && v.len() >= 5).collect();
        cands.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.len().cmp(&b.1.len())));
        for (_, v) in cands.into_iter().take(2) {
            if !out.iter().any(|o| o == v) {
                out.push(v.to_string());
            }
        }
    }
    out
}

/// BM25 over identifier tokens, plus path/symbol field boosts and a git recency prior.
pub fn rank<'a>(root: &Path, docs: &'a [Doc], task: &str, terms: &[String]) -> Vec<(f64, Vec<String>, &'a Doc)> {
    let task_l = task.to_lowercase();
    let n = docs.len() as f64;
    let avg = docs.iter().map(|d| d.len).sum::<usize>() as f64 / n.max(1.0);
    let (k1, b) = (w("k1", 1.2), w("b", 0.4));
    let (wp, ws, wr, wd) = (w("path", 1.5), w("sym", 0.7), w("rec", 0.5), w("decay", 30.0));
    let terms = &bridge(docs, terms, w("bridge", 1.0) > 0.0 && crate::ptbr::is_portuguese(task));
    let idf: Vec<f64> = terms
        .iter()
        .map(|t| {
            let df = docs.iter().filter(|d| d.tf.contains_key(t) || d.path_toks.contains(t)).count() as f64;
            (1.0 + (n - df + 0.5) / (df + 0.5)).ln()
        })
        .collect();
    let rec = recency(root);
    let mut hits: Vec<(f64, Vec<String>, &Doc)> = docs
        .iter()
        .map(|d| {
            let (mut score, mut why) = (0.0, vec![]);
            let path = d.rel.to_lowercase();
            let fname = path.rsplit('/').next().unwrap_or(&path);
            if task_l.contains(&path) || (fname.contains('.') && task_l.contains(fname)) {
                score += 50.0;
                why.push("named in task".to_string());
            }
            for (t, idf) in terms.iter().zip(&idf) {
                if let Some(&f) = d.tf.get(t) {
                    let f = f as f64;
                    score += idf * f * (k1 + 1.0) / (f + k1 * (1.0 - b + b * d.len as f64 / avg));
                }
                if d.path_toks.contains(t) {
                    score += wp * idf;
                    why.push(format!("path:{t}"));
                }
                if d.sym_toks.contains(t) {
                    score += ws * idf;
                    why.push(format!("symbol:{t}"));
                }
            }
            if score <= 0.0 {
                return (0.0, why, d);
            }
            if let Some(&i) = rec.get(&d.rel) {
                score *= 1.0 + wr * (-(i as f64) / wd).exp();
                if i < 10 {
                    why.push("recently changed".to_string());
                }
            }
            if d.is_doc {
                score *= 0.5;
            }
            (score, why, d)
        })
        .collect();
    // one hop over the import graph: importers/importees of strong hits (their tests, the page using a
    // component) inherit a share of the best neighbour's score
    let wg = w("graph", 0.3);
    if wg > 0.0 {
        let base: Vec<f64> = hits.iter().map(|h| h.0).collect();
        for (i, nbrs) in graph(docs).iter().enumerate() {
            if let Some((j, best)) = nbrs.iter().map(|&j| (j, base[j])).max_by(|a, b| a.1.total_cmp(&b.1)) {
                if best > 0.0 {
                    // gmax=1: neighbours become candidates but never outrank a stronger lexical hit
                    hits[i].0 = if w("gmax", 1.0) > 0.0 { hits[i].0.max(wg * best) } else { hits[i].0 + wg * best };
                    if base[i] < wg * best {
                        hits[i].1.push(format!("imports/imported by {}", docs[j].rel));
                    }
                }
            }
        }
    }
    hits.retain(|h| h.0 > 0.0);
    hits
}

static IMPORT_SPEC: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?m)(?:\bfrom\s+['"]([^'"]+)['"]|\brequire\(\s*['"]([^'"]+)['"]|\bimport\s*\(?\s*['"]([^'"]+)['"]|^\s*from\s+([\w.]+)\s+import\b|^\s*import\s+([\w.]+)|^\s*(?:pub\s+)?use\s+(\w+(?:::\w+)+)|^\s*(?:pub\s+)?mod\s+(\w+)\s*;)"#,
    )
    .unwrap()
});

fn module_key(rel: &str) -> String {
    let no_ext = rel.rsplit_once('.').map_or(rel, |(a, _)| a);
    ["/index", "/__init__", "/mod"].iter().find_map(|s| no_ext.strip_suffix(s)).unwrap_or(no_ext).to_string()
}

fn normalize(path: &str) -> String {
    let mut out: Vec<&str> = vec![];
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            s => out.push(s),
        }
    }
    out.join("/")
}

/// Undirected import graph between loaded docs.
pub fn graph(docs: &[Doc]) -> Vec<Vec<usize>> {
    let mut g: Vec<HashSet<usize>> = vec![HashSet::new(); docs.len()];
    for (i, js) in imports(docs).into_iter().enumerate() {
        for j in js {
            g[i].insert(j);
            g[j].insert(i);
        }
    }
    g.into_iter().map(|s| s.into_iter().collect()).collect()
}

/// Directed: `imports(docs)[i]` = docs that doc i imports.
// ponytail: resolves relative JS/TS paths, `@/` `~/` aliases, python dotted modules, rust `crate::`/own-crate/mod
// paths (first segment dropped, so external crates simply don't resolve);
// tsconfig `paths`, package exports and go imports are not resolved yet.
pub fn imports(docs: &[Doc]) -> Vec<Vec<usize>> {
    let mut by_key: HashMap<String, usize> = HashMap::new();
    for (i, d) in docs.iter().enumerate().filter(|(_, d)| !d.is_doc) {
        let k = module_key(&d.rel);
        if let Some(src) = k.strip_prefix("src/") {
            by_key.entry(src.to_string()).or_insert(i);
        }
        by_key.entry(k).or_insert(i);
    }
    let mut g: Vec<HashSet<usize>> = vec![HashSet::new(); docs.len()];
    for (i, d) in docs.iter().enumerate().filter(|(_, d)| !d.is_doc) {
        let dir = d.rel.rsplit_once('/').map_or("", |(a, _)| a);
        for c in IMPORT_SPEC.captures_iter(&d.text) {
            let Some((k, spec)) = (1..=7).find_map(|k| c.get(k).map(|m| (k, m.as_str()))) else { continue };
            let mut keys = vec![];
            match k {
                1..=3 if spec.starts_with('.') => keys.push(normalize(&format!("{dir}/{spec}"))),
                1..=3 => keys.extend(["@/", "~/", "#/"].iter().filter_map(|p| spec.strip_prefix(p)).map(String::from)),
                4 | 5 => {
                    let rel = spec.trim_start_matches('.').replace('.', "/");
                    let up = spec.len() - spec.trim_start_matches('.').len();
                    if up > 0 {
                        let base: Vec<&str> = dir.split('/').collect();
                        let keep = base.len().saturating_sub(up - 1);
                        keys.push(normalize(&format!("{}/{rel}", base[..keep].join("/"))));
                    } else {
                        keys.push(rel.clone());
                        // `from a.b import c`: c may be a module or a symbol
                        if let Some((parent, _)) = rel.rsplit_once('/') {
                            keys.push(parent.to_string());
                        }
                    }
                }
                6 => {
                    let segs: Vec<&str> = spec.split("::").skip(1).collect();
                    for n in (1..=segs.len()).rev() {
                        keys.push(segs[..n].join("/"));
                    }
                }
                _ => keys.push(normalize(&format!("{dir}/{spec}"))),
            }
            // `./pricing.ts` and `./pricing` both name the module `pricing`
            let with_ext: Vec<String> = keys.iter().map(|k| module_key(k)).collect();
            if let Some(&j) = keys.iter().chain(&with_ext).find_map(|key| by_key.get(key.as_str())) {
                if j != i {
                    g[i].insert(j);
                }
            }
        }
    }
    g.into_iter().map(|s| s.into_iter().collect()).collect()
}

pub fn compile(root: &Path, map: &ProjectMap, task: &str, budget: usize) -> Pack {
    let terms = terms(task);
    let docs = load(root);
    let mut hits = rank(root, &docs, task, &terms);
    hits.sort_by(|a, b| b.0.total_cmp(&a.0));

    let top = hits.first().map_or(0.0, |h| h.0);
    let (mut files, mut also, mut used) = (vec![], vec![], 0usize);
    for (score, why, d) in hits.into_iter().filter(|h| h.0 >= top * 0.15) {
        if files.len() >= 12 || used >= budget {
            if also.len() < 20 {
                also.push(d.rel.clone());
            }
            continue;
        }
        let snippets = snippets(d, &terms, budget - used);
        let outline: Vec<Symbol> = d.symbols.iter().take(25).cloned().collect();
        used += snippets.iter().map(|s| s.code.len()).sum::<usize>() + outline.len() * 20 + d.rel.len() + 40;
        files.push(FileCtx { path: d.rel.clone(), score: (score * 10.0).round() / 10.0, why, outline, snippets });
    }

    let mut verify: Vec<String> = vec![];
    for f in &files {
        if let Some(c) = map.owner(&f.path) {
            for k in ["typecheck", "test"] {
                if let Some(cmd) = c.cmd(k) {
                    let line = format!("({}) {}", c.dir, cmd.run);
                    if !verify.contains(&line) {
                        verify.push(line);
                    }
                }
            }
        }
    }
    Pack { task: task.to_string(), terms, files, also, verify, chars: used }
}

fn snippets(d: &Doc, terms: &[String], max_chars: usize) -> Vec<Snip> {
    let lines: Vec<&str> = d.text.lines().collect();
    let mut ranges: Vec<(usize, usize)> = vec![];
    for (i, s) in d.symbols.iter().enumerate() {
        if toks(&s.name).iter().any(|t| terms.contains(t)) {
            let start = s.line - 1;
            let next = d.symbols.get(i + 1).map_or(lines.len(), |n| n.line - 1);
            ranges.push((start, next.max(start + 1).min(start + 40).min(lines.len())));
        }
    }
    for (i, l) in lines.iter().enumerate() {
        let lt = toks(l);
        if terms.iter().any(|t| lt.contains(t)) {
            ranges.push((i.saturating_sub(2), (i + 3).min(lines.len())));
        }
    }
    ranges.sort();
    let mut merged: Vec<(usize, usize)> = vec![];
    for (s, e) in ranges {
        match merged.last_mut() {
            Some(r) if s <= r.1 => r.1 = r.1.max(e),
            _ => merged.push((s, e)),
        }
    }
    let (mut out, mut used) = (vec![], 0);
    'outer: for (s, e) in merged {
        let mut code = String::new();
        let mut end = s;
        for (i, line) in lines.iter().enumerate().take(e).skip(s) {
            let l = format!("{:>4}| {}\n", i + 1, line);
            if used + l.len() > max_chars {
                if !code.is_empty() {
                    out.push(Snip { start: s + 1, end, code });
                }
                break 'outer;
            }
            used += l.len();
            code += &l;
            end = i + 1;
        }
        out.push(Snip { start: s + 1, end, code });
    }
    out
}

pub fn render(p: &Pack) -> String {
    let mut s = format!("# Context pack\ntask: {}\nterms: {}\n", p.task, p.terms.join(", "));
    if p.files.is_empty() {
        s += "\nNo matching files. Name a symbol, file path or domain word that appears in the code.\n";
    }
    for f in &p.files {
        s += &format!("\n## {} (score {}; {})\n", f.path, f.score, f.why.join(", "));
        if !f.outline.is_empty() {
            let o: Vec<String> = f.outline.iter().map(|x| format!("{} {} L{}", x.kind, x.name, x.line)).collect();
            s += &format!("outline: {}\n", o.join(" · "));
        }
        for sn in &f.snippets {
            s += &format!("```\n{}```\n", sn.code);
        }
    }
    if !p.also.is_empty() {
        s += &format!("\nalso relevant (not expanded): {}\n", p.also.join(", "));
    }
    if !p.verify.is_empty() {
        s += &format!("\nverify with: {}\n", p.verify.iter().map(|v| format!("`{v}`")).collect::<Vec<_>>().join(", "));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terms_split_identifiers_and_drop_stopwords() {
        assert_eq!(terms("Fix applyDiscount for coupons"), vec!["applydiscount", "apply", "discount", "coupon"]);
    }

    #[test]
    fn graph_resolves_relative_alias_and_python_imports() {
        let doc = |rel: &str, text: &str| Doc {
            rel: rel.into(), text: text.into(), symbols: vec![], is_doc: false, tf: HashMap::new(), len: 0,
            path_toks: HashSet::new(), sym_toks: HashSet::new(),
        };
        let docs = vec![
            doc("src/cart.ts", "import { applyDiscount } from \"./pricing.ts\";"),
            doc("src/pricing.ts", ""),
            doc("app/page.tsx", "import Cart from '@/components/cart'"),
            doc("components/cart/index.tsx", ""),
            doc("shop/cart.py", "from shop.pricing import apply_discount"),
            doc("shop/pricing.py", ""),
        ];
        let g = graph(&docs);
        assert_eq!((g[0].clone(), g[2].clone(), g[4].clone()), (vec![1], vec![3], vec![5]));
    }

    #[test]
    fn outline_finds_multi_language_decls() {
        let src = "pub fn a() {}\nclass B:\n    def c(self): pass\nfunc (r *T) D() {}\nexport const E = 1\nimpl<T> F for G {}\n";
        let names: Vec<_> = outline(src).into_iter().map(|s| (s.kind, s.name)).collect();
        let want = [("fn", "a"), ("class", "B"), ("def", "c"), ("func", "D"), ("const", "E"), ("impl", "F")];
        assert_eq!(names, want.map(|(k, n)| (k.to_string(), n.to_string())));
    }
}
