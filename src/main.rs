mod assets;
mod blender;
mod ci;
mod context;
mod contracts;
mod detect;
mod diag;
mod gate;
mod impact;
mod mcp;
mod probe;
mod ptbr;
mod run;
mod yaml;

use serde::Serialize;
use std::collections::HashMap;
use std::io::Read;
use std::path::PathBuf;
use std::process::exit;
use std::time::Duration;

const USAGE: &str = "lkc (LookaCore) — development layer for AI coding agents · alias: looka

usage: looka <command> [args] [--root DIR] [--json]

  map                         components, toolchains, build/typecheck/lint/test commands
  context <task…> [--budget N] ranked files, outlines and snippets for a task
  outline <file…>             symbols declared in files
  impact <file|symbol…>       consumers, affected tests, what to verify
  impact --changed            same, for uncommitted git changes
  impact --base REF           same, for the commits since the merge-base with REF plus uncommitted changes
  verify [--kinds a,b] [--component DIR] [--timeout SECS] [--changed] [--base REF] [--ci]
                              run discovered checks (default typecheck,lint,test); exit 1 on FAIL
                              --changed: only the tests affected by uncommitted changes
                              --base REF: --changed over the commits since the merge-base with REF too,
                              failures attributed against that merge-base (pre-push, CI)
                              --ci: first replicate the CI test job (env, Postgres/Redis services on a
                              free port, DB setup steps such as migrations) from .github/workflows
  probe [--component DIR] [--pages N] [--ci]
                              run it and observe: start the web app (or serve index.html) and crawl it,
                              or run a Godot game headless; exit 1 on FAIL
  assets                      models/textures/audio: size, triangles, compression, budget for the target
                              engine (web, godot, unity, unreal), who uses each asset, missing refs
  assets inspect <file.blend> Blender scene: triangles after modifiers, materials, missing textures, scale
  assets export [<file.blend…>|--changed] [--for three|godot|unity|unreal] [--out PATH]
                              export through headless Blender with the target's settings
  contracts                   HTTP endpoints served by each component and the calls other components make
                              (web, Godot, Unity, Python clients): broken calls, endpoints without callers
  init                        wire the gate into this project: Claude Code SessionStart/Stop hooks
                              (.claude/settings.json, merged) and a git pre-commit hook
  trust                       let the gate run this repo's commands when the plugin's hooks fire (lkc init
                              trusts too); the list is ~/.config/lookacore/trusted
  gate [--hook claude|claude-start|git] [--require-trust]
                              verify --changed for the agent: blocks Claude Code from finishing, or git
                              from committing, while the changes introduce failures
  diagnose                    parse compiler/test output from stdin
  mcp                         serve all of the above as MCP tools over stdio
";

const VALUED: &[&str] = &["root", "budget", "kinds", "component", "timeout", "pages", "for", "out", "hook", "base"];

/// Uncommitted files under root (including untracked), any type.
fn git_changed(root: &std::path::Path) -> Vec<String> {
    let out = std::process::Command::new("git").arg("-C").arg(root).args(["status", "--porcelain", "-z", "--untracked-files=all"]).output();
    let Ok(o) = out else { return vec![] };
    let raw = String::from_utf8_lossy(&o.stdout).into_owned();
    let mut it = raw.split('\0');
    let mut v = vec![];
    while let Some(e) = it.next() {
        if e.len() > 3 {
            if e.starts_with('R') {
                it.next();
            }
            v.push(e[3..].to_string());
        }
    }
    v
}

/// `--ci`: replicate the CI test job (env, database services, setup steps); torn down when dropped.
fn ci_env(root: &std::path::Path, map: &detect::ProjectMap) -> Option<ci::CiEnv> {
    let c = ci::prepare(root, map);
    match &c {
        Some(c) => c.summary.iter().for_each(|l| eprintln!("lkc: {l}")),
        None => eprintln!("lkc: --ci: no .github/workflows job that runs tests; running without it"),
    }
    c
}

pub fn fmt<T: Serialize>(json: bool, v: &T, md: impl FnOnce() -> String) -> String {
    if json { serde_json::to_string_pretty(v).unwrap_or_default() + "\n" } else { md() }
}

fn main() {
    let (mut cmd, mut pos, mut flags) = (None, vec![], HashMap::new());
    let mut it = std::env::args().skip(1);
    while let Some(s) = it.next() {
        if let Some(f) = s.strip_prefix("--") {
            let (k, v) = match f.split_once('=') {
                Some((k, v)) => (k.to_string(), Some(v.to_string())),
                None if VALUED.contains(&f) => (f.to_string(), it.next()),
                None => (f.to_string(), None),
            };
            flags.insert(k, v);
        } else if cmd.is_none() {
            cmd = Some(s);
        } else {
            pos.push(s);
        }
    }
    if flags.contains_key("version") || cmd.as_deref() == Some("version") {
        println!("lkc {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    let get = |k: &str| flags.get(k).cloned().flatten();
    let json = flags.contains_key("json");
    let root: PathBuf = match std::fs::canonicalize(get("root").unwrap_or_else(|| ".".into())) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("looka: bad --root: {e}");
            exit(2)
        }
    };

    let mut code = 0;
    let out = match cmd.as_deref() {
        Some("map" | "detect") => {
            let m = detect::detect(&root);
            fmt(json, &m, || detect::render(&m))
        }
        Some("context") if !pos.is_empty() => {
            let budget = get("budget").and_then(|b| b.parse().ok()).unwrap_or(16_000);
            let pack = context::compile(&root, &detect::detect(&root), &pos.join(" "), budget);
            fmt(json, &pack, || context::render(&pack))
        }
        Some("outline") if !pos.is_empty() => {
            let all: Vec<(String, Vec<context::Symbol>)> = pos
                .iter()
                .map(|f| (f.clone(), std::fs::read_to_string(root.join(f)).map(|t| context::outline(&t)).unwrap_or_default()))
                .collect();
            fmt(json, &all, || {
                all.iter()
                    .map(|(f, syms)| format!("{f}\n{}", syms.iter().map(|s| format!("  L{} {} {}\n", s.line, s.kind, s.name)).collect::<String>()))
                    .collect()
            })
        }
        Some("impact") => {
            let base = get("base");
            let targets = if flags.contains_key("changed") || base.is_some() {
                impact::changed_files(&root, base.as_deref()).unwrap_or_else(|e| {
                    eprintln!("looka impact: {e}");
                    exit(2)
                })
            } else {
                pos.clone()
            };
            if targets.is_empty() {
                eprintln!("looka impact: no targets (pass files/symbols, or --changed inside a git repo with changes)");
                exit(2)
            }
            let i = impact::impact(&root, &detect::detect(&root), &targets);
            fmt(json, &i, || impact::render(&i))
        }
        Some("verify") => {
            let kinds: Vec<String> = get("kinds").map_or_else(
                || run::DEFAULT_KINDS.iter().map(|s| s.to_string()).collect(),
                |k| k.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
            );
            let timeout = Duration::from_secs(get("timeout").and_then(|t| t.parse().ok()).unwrap_or(600));
            let map = detect::detect(&root);
            let _ci = flags.contains_key("ci").then(|| ci_env(&root, &map)).flatten();
            let base = get("base");
            let changed = (flags.contains_key("changed") || base.is_some()).then(|| {
                let (files, complete) = impact::changes(&root, base.as_deref()).unwrap_or_else(|e| {
                    eprintln!("looka verify: {e}");
                    exit(2)
                });
                run::Changed { tests: impact::impact(&root, &map, &files).tests, files, complete, base }
            });
            let r = run::verify(&root, &map, &kinds, get("component").as_deref(), changed.as_ref(), timeout);
            code = if r.ok { 0 } else { 1 };
            fmt(json, &r, || run::render(&r))
        }
        Some("gate") => {
            let timeout = Duration::from_secs(get("timeout").and_then(|t| t.parse().ok()).unwrap_or(840));
            let hook = get("hook").unwrap_or_else(|| "git".into());
            exit(gate::gate(get("root").map(PathBuf::from), &hook, timeout, flags.contains_key("require-trust")))
        }
        Some("init") => gate::init(&root),
        Some("trust") => gate::trust(&root),
        Some("probe") => {
            let pages = get("pages").and_then(|t| t.parse().ok()).unwrap_or(30);
            let timeout = Duration::from_secs(get("timeout").and_then(|t| t.parse().ok()).unwrap_or(600));
            let map = detect::detect(&root);
            let _ci = flags.contains_key("ci").then(|| ci_env(&root, &map)).flatten();
            let p = probe::probe(&root, &map, get("component").as_deref(), pages, timeout);
            code = if p.ok { 0 } else { 1 };
            fmt(json, &p, || probe::render(&p))
        }
        Some("assets") => {
            let map = detect::detect(&root);
            match pos.first().map(String::as_str) {
                Some(sub @ ("inspect" | "export")) => {
                    let target = get("for").unwrap_or_else(|| match assets::profile(&map).as_str() {
                        "web" => "three".into(),
                        p => p.to_string(),
                    });
                    // no files: every .blend in the project, or with --changed only the ones modified in git
                    let mut srcs: Vec<PathBuf> = pos[1..].iter().map(|f| root.join(f)).collect();
                    if srcs.is_empty() {
                        let blend = |p: &PathBuf| p.extension().is_some_and(|e| e == "blend");
                        srcs = if flags.contains_key("changed") {
                            git_changed(&root).into_iter().map(|f| root.join(f)).filter(blend).collect()
                        } else {
                            detect::walk(&root).into_iter().filter(blend).collect()
                        };
                    }
                    if srcs.is_empty() {
                        eprintln!("looka assets {sub}: no .blend files to process");
                        exit(2)
                    }
                    let results: Vec<blender::BlendResult> = srcs
                        .iter()
                        .map(|src| {
                            let export = (sub == "export").then(|| {
                                let out = get("out").filter(|_| srcs.len() == 1).map(|o| root.join(o)).unwrap_or_else(|| blender::default_out(&root, src, &target));
                                (target.as_str(), out)
                            });
                            blender::run(&root, src, export)
                        })
                        .collect();
                    code = if results.iter().all(|r| r.ok) { 0 } else { 1 };
                    fmt(json, &results, || results.iter().map(blender::render).collect())
                }
                _ => {
                    let r = assets::report(&root, &map);
                    code = if r.ok { 0 } else { 1 };
                    fmt(json, &r, || assets::render(&r))
                }
            }
        }
        Some("contracts") => {
            let c = contracts::contracts(&root, &detect::detect(&root));
            code = if c.ok { 0 } else { 1 };
            fmt(json, &c, || contracts::render(&c))
        }
        Some("diagnose") => {
            let mut s = String::new();
            let _ = std::io::stdin().read_to_string(&mut s);
            let d = diag::parse(&s);
            fmt(json, &d, || diag::render(&d))
        }
        Some("mcp") => {
            mcp::serve(root);
            return;
        }
        Some("help") | None => USAGE.to_string(),
        Some(_) => {
            eprint!("{USAGE}");
            exit(2)
        }
    };
    print!("{out}");
    exit(code)
}
