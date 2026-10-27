//! Universal Tool API: the same operations as the CLI, over MCP (JSON-RPC 2.0, newline-delimited stdio).

use crate::{assets, blender, ci, context, contracts, detect, diag, fmt, impact, probe, run};
use serde_json::{json, Value};
use std::fs;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

const INSTRUCTIONS: &str = "looka gives you a map of any repository and evidence-based verification. \
Workflow: looka_map once per repo → looka_context with the task before opening files → looka_impact before \
changing a shared file or API → edit → looka_verify (changed=true) → looka_probe for web apps and games. A task is done only when looka_verify reports PASS; \
if it fails, fix the listed diagnostics first.";

fn tools() -> Value {
    let root = json!({"type": "string", "description": "project root (default: server cwd)"});
    let as_json = json!({"type": "boolean", "description": "return JSON instead of compact markdown"});
    let base = json!({"type": "string", "description": "git ref: also the commits since its merge-base with HEAD (implies changed=true)"});
    json!([
        {"name": "looka_map", "description": "Project map: languages, components (Cargo, npm/pnpm/bun, Python, Go, CMake, Flutter, Gradle, Make), discovered install/build/typecheck/lint/test commands with toolchain availability, entrypoints, test files.",
         "inputSchema": {"type": "object", "properties": {"root": root, "json": as_json}}},
        {"name": "looka_context", "description": "Compile a small ranked context pack for a task: relevant files, outlines and snippets with line numbers, plus which commands verify them. Use instead of reading the whole repo.",
         "inputSchema": {"type": "object", "required": ["task"], "properties": {"task": {"type": "string"}, "budget": {"type": "integer", "description": "max snippet chars (default 16000)"}, "root": root, "json": as_json}}},
        {"name": "looka_outline", "description": "Symbols (functions, types, classes) declared in a file, with line numbers.",
         "inputSchema": {"type": "object", "required": ["file"], "properties": {"file": {"type": "string"}, "root": root}}},
        {"name": "looka_impact", "description": "Impact of changing files or symbols: references in other files, tests likely affected (two hops), commands to verify. Use changed=true for uncommitted git changes, base=<ref> for a range of commits too.",
         "inputSchema": {"type": "object", "properties": {"targets": {"type": "array", "items": {"type": "string"}}, "changed": {"type": "boolean"}, "base": base, "root": root, "json": as_json}}},
        {"name": "looka_verify", "description": "Run the project's own typecheck/lint/test commands in a scrubbed environment with a timeout, and return PASS/FAIL with structured diagnostics. Evidence is saved under .looka/evidence.",
         "inputSchema": {"type": "object", "properties": {"ci": {"type": "boolean", "description": "first replicate the CI test job from .github/workflows: env, Postgres/Redis services on a free port, DB setup steps"}, "kinds": {"type": "array", "items": {"type": "string", "enum": ["build", "typecheck", "lint", "test"]}}, "component": {"type": "string", "description": "component dir from looka_map"}, "changed": {"type": "boolean", "description": "only tests affected by uncommitted changes (skips pre-existing unrelated failures)"}, "base": base, "timeout_secs": {"type": "integer"}, "root": root, "json": as_json}}},
        {"name": "looka_probe", "description": "Run the project and observe it. Web: starts the dev server (or serves index.html), crawls routes from the file system and links, reports HTTP status per page and server-log errors. Godot: runs the game headless ~600 frames and reports script errors with res:// locations.",
         "inputSchema": {"type": "object", "properties": {"ci": {"type": "boolean", "description": "first replicate the CI test job from .github/workflows: env, Postgres/Redis services on a free port, DB setup steps"}, "component": {"type": "string"}, "pages": {"type": "integer", "description": "max pages to crawl (default 30)"}, "timeout_secs": {"type": "integer"}, "root": root, "json": as_json}}},
        {"name": "looka_assets", "description": "Game/3D asset report for the target engine (web/three.js, godot, unity, unreal): textures (size, power-of-two), glTF models (triangles, materials, Draco/meshopt/WebP), WAV weight, who references each asset (res://, loader paths, Unity GUIDs), missing references. With blend=<file.blend>: inspect the Blender scene; add export_for=three|godot|unity|unreal to export it headless with that engine's settings.",
         "inputSchema": {"type": "object", "properties": {"blend": {"type": "string"}, "export_for": {"type": "string", "enum": ["three", "godot", "unity", "unreal"]}, "out": {"type": "string"}, "root": root, "json": as_json}}},
        {"name": "looka_contracts", "description": "Cross-stack API map: endpoints served (Next route handlers, Express/Fastify/Hono, FastAPI/Flask, axum, Go net/http) and client calls (fetch/axios, Godot HTTPRequest, UnityWebRequest, Python requests/httpx), with broken calls and endpoints nobody calls.",
         "inputSchema": {"type": "object", "properties": {"root": root, "json": as_json}}},
        {"name": "looka_diagnose", "description": "Parse raw compiler/test/log output into structured diagnostics (file, line, code, hint).",
         "inputSchema": {"type": "object", "required": ["text"], "properties": {"text": {"type": "string"}, "json": as_json}}}
    ])
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default()
}

fn ci_lines(ci: &Option<ci::CiEnv>) -> String {
    ci.as_ref().map(|c| c.summary.iter().map(|l| format!("{l}\n")).collect()).unwrap_or_default()
}

fn call(default_root: &Path, p: &Value) -> Value {
    let a = &p["arguments"];
    // tools stay inside the server's root: a typo'd root must not silently map/verify another project
    let inside = |p: &Path| fs::canonicalize(p).ok().filter(|c| c.starts_with(default_root));
    let root: PathBuf = match a["root"].as_str() {
        None => default_root.to_path_buf(),
        Some(r) => match inside(&default_root.join(r)) {
            Some(c) => c,
            None => return error(format!("root `{r}` does not exist or is outside {}", default_root.display())),
        },
    };
    let json = a["json"].as_bool().unwrap_or(false);
    let out: Result<String, String> = match p["name"].as_str().unwrap_or("") {
        "looka_map" => {
            let m = detect::detect(&root);
            Ok(fmt(json, &m, || detect::render(&m)))
        }
        "looka_context" => match a["task"].as_str() {
            Some(task) => {
                let m = detect::detect(&root);
                let pack = context::compile(&root, &m, task, a["budget"].as_u64().unwrap_or(16_000) as usize);
                Ok(fmt(json, &pack, || context::render(&pack)))
            }
            None => Err("missing required argument: task".into()),
        },
        "looka_outline" => match a["file"].as_str() {
            Some(f) => inside(&root.join(f))
                .ok_or_else(|| format!("{f}: not found inside the project"))
                .and_then(|p| fs::read_to_string(p).map_err(|e| e.to_string()))
                .map(|t| context::outline(&t).iter().map(|s| format!("L{} {} {}\n", s.line, s.kind, s.name)).collect()),
                
            None => Err("missing required argument: file".into()),
        },
        "looka_impact" => {
            let base = a["base"].as_str();
            let targets = if a["changed"].as_bool().unwrap_or(false) || base.is_some() { impact::changed_files(&root, base) } else { Ok(strings(&a["targets"])) };
            targets.and_then(|targets| {
                if targets.is_empty() {
                    return Err("no targets (pass targets, or changed=true inside a git repo with changes)".into());
                }
                let i = impact::impact(&root, &detect::detect(&root), &targets);
                Ok(fmt(json, &i, || impact::render(&i)))
            })
        }
        "looka_verify" => {
            let mut kinds = strings(&a["kinds"]);
            if kinds.is_empty() {
                kinds = run::DEFAULT_KINDS.iter().map(|s| s.to_string()).collect();
            }
            let timeout = Duration::from_secs(a["timeout_secs"].as_u64().unwrap_or(600).min(1800));
            let map = detect::detect(&root);
            let base = a["base"].as_str();
            let changed = match (a["changed"].as_bool().unwrap_or(false) || base.is_some()).then(|| impact::changes(&root, base)).transpose() {
                Ok(c) => c.map(|(files, complete)| run::Changed { tests: impact::impact(&root, &map, &files).tests, files, complete, base: base.map(String::from) }),
                Err(e) => return error(e),
            };
            let ci = match a["ci"].as_bool().unwrap_or(false).then(|| ci::prepare(&root, &map)).transpose() {
                Ok(c) => c.flatten(),
                Err(e) => return error(e),
            };
            let r = run::verify(&root, &map, &kinds, a["component"].as_str(), changed.as_ref(), timeout);
            Ok(ci_lines(&ci) + &fmt(json, &r, || run::render(&r)))
        }
        "looka_probe" => {
            let timeout = Duration::from_secs(a["timeout_secs"].as_u64().unwrap_or(600).min(1800));
            let pages = a["pages"].as_u64().unwrap_or(30).min(500) as usize;
            let map = detect::detect(&root);
            let ci = match a["ci"].as_bool().unwrap_or(false).then(|| ci::prepare(&root, &map)).transpose() {
                Ok(c) => c.flatten(),
                Err(e) => return error(e),
            };
            let p = probe::probe(&root, &map, a["component"].as_str(), pages, timeout);
            Ok(ci_lines(&ci) + &fmt(json, &p, || probe::render(&p)))
        }
        "looka_assets" => {
            let map = detect::detect(&root);
            match a["blend"].as_str() {
                Some(b) => match inside(&root.join(b)) {
                    Some(src) => {
                        let export = a["export_for"].as_str().map(|t| {
                            let out = a["out"].as_str().map(|o| root.join(o)).unwrap_or_else(|| blender::default_out(&root, &src, t));
                            (t, out)
                        });
                        let r = blender::run(&root, &src, export);
                        Ok(fmt(json, &r, || blender::render(&r)))
                    }
                    None => Err(format!("{b}: not found inside the project")),
                },
                None => {
                    let r = assets::report(&root, &map);
                    Ok(fmt(json, &r, || assets::render(&r)))
                }
            }
        }
        "looka_contracts" => {
            let c = contracts::contracts(&root, &detect::detect(&root));
            Ok(fmt(json, &c, || contracts::render(&c)))
        }
        "looka_diagnose" => {
            let d = diag::parse(a["text"].as_str().unwrap_or(""));
            Ok(fmt(json, &d, || diag::render(&d)))
        }
        n => Err(format!("unknown tool: {n}")),
    };
    match out {
        // say which project was used: an agent working elsewhere that omits `root` must see it got this one
        Ok(t) if json => json!({"content": [{"type": "text", "text": t}]}),
        Ok(t) => json!({"content": [{"type": "text", "text": format!("root: {}\n{t}", root.display())}]}),
        Err(e) => error(e),
    }
}

fn error(e: String) -> Value {
    json!({"content": [{"type": "text", "text": e}], "isError": true})
}

pub fn serve(root: PathBuf) {
    let mut stdout = io::stdout();
    let (mut stdin, mut raw) = (io::stdin().lock(), Vec::new());
    // ponytail: calls run inline, so a long verify delays pings; move to a worker thread if clients time out.
    loop {
        raw.clear();
        if stdin.read_until(b'\n', &mut raw).unwrap_or(0) == 0 {
            break;
        }
        let line = String::from_utf8_lossy(&raw);
        if line.trim().is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                let r = json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": e.to_string()}});
                let _ = writeln!(stdout, "{r}");
                continue;
            }
        };
        if msg.is_array() {
            let r = json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32600, "message": "batch requests are not supported"}});
            let _ = writeln!(stdout, "{r}");
            continue;
        }
        let method = msg["method"].as_str().unwrap_or("");
        let result = match method {
            "initialize" => Ok(json!({
                "protocolVersion": msg["params"]["protocolVersion"].as_str().unwrap_or("2025-06-18"),
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "looka", "version": env!("CARGO_PKG_VERSION")},
                "instructions": INSTRUCTIONS
            })),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools": tools()})),
            "tools/call" => Ok(call(&root, &msg["params"])),
            _ => Err(json!({"code": -32601, "message": format!("method not found: {method}")})),
        };
        let Some(id) = msg.get("id").filter(|v| !v.is_null()) else { continue }; // notification
        let resp = match result {
            Ok(r) => json!({"jsonrpc": "2.0", "id": id, "result": r}),
            Err(e) => json!({"jsonrpc": "2.0", "id": id, "error": e}),
        };
        let _ = writeln!(stdout, "{resp}");
        let _ = stdout.flush();
    }
}
