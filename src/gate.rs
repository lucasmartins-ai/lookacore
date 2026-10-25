//! The gate: `verify --changed` run for the agent, not by it — from Claude Code's Stop hook when the agent
//! says it is done, or from git's pre-commit hook. Verification no longer depends on the agent choosing to
//! call a tool (in the A/B runs several agents skipped it, one verified the wrong repository).

use crate::{detect, impact, run};
use serde_json::{json, Value};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Hash of everything uncommitted (diff against HEAD, plus untracked files): equal before and after means
/// nobody touched the tree. Covers deletions and config edits, which `impact::changes` does not list.
fn fingerprint(root: &Path, files: &[String]) -> u64 {
    let git = |args: &[&str]| std::process::Command::new("git").arg("-C").arg(root).args(args).output().map(|o| o.stdout).unwrap_or_default();
    let mut h = DefaultHasher::new();
    // .looka/ holds this gate's own state and evidence: writing it must not count as a change
    git(&["diff", "HEAD", "--binary", "--", ".", ":(exclude).looka"]).hash(&mut h);
    // untracked build/test output (__pycache__, .pytest_cache, node_modules…) is not a change either
    let status = String::from_utf8_lossy(&git(&["status", "--porcelain", "--", ".", ":(exclude).looka"])).into_owned();
    for l in status.lines().filter(|l| !(l.starts_with("??") && impact::generated(Path::new(l.get(3..).unwrap_or(""))))) {
        l.hash(&mut h);
    }
    for f in files {
        std::fs::read(root.join(f)).unwrap_or_default().hash(&mut h); // untracked contents are not in the diff
    }
    h.finish()
}

fn state_path(root: &Path) -> PathBuf {
    root.join(".looka/gate.json")
}

fn load(root: &Path) -> Value {
    std::fs::read_to_string(state_path(root)).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(json!({}))
}

fn save(root: &Path, v: &Value) {
    let _ = std::fs::create_dir_all(root.join(".looka"));
    // self-ignoring, as verify's evidence dir is: the plugin runs the gate in repos `lkc init` never touched
    let gi = root.join(".looka/.gitignore");
    if !gi.exists() {
        let _ = std::fs::write(gi, "*\n");
    }
    let _ = std::fs::write(state_path(root), v.to_string());
}

/// Repos the user chose to let the gate run in (`lkc trust`, `lkc init`): one git toplevel per line.
fn trust_file() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config"))).map(|d| d.join("lookacore/trusted"))
}

/// The repository a path belongs to (its git toplevel), or the path itself outside git. Trust is per repo,
/// never by prefix: a repo cloned inside a trusted one (a submodule, something the agent fetched) is not
/// trusted with it, and no entry can cover a whole tree such as $HOME.
fn repo_of(root: &Path) -> PathBuf {
    let top = std::process::Command::new("git").arg("-C").arg(root).args(["rev-parse", "--show-toplevel"]).output().ok().filter(|o| o.status.success());
    // raw bytes, minus exactly the one newline git appends: lossy UTF-8 or trimming every trailing newline would
    // map a dir named "repo\n" or one with invalid bytes onto a different, trusted repo
    let top = top.map(|o| {
        use std::os::unix::ffi::OsStrExt;
        let b = o.stdout.strip_suffix(b"\n").unwrap_or(&o.stdout);
        PathBuf::from(std::ffi::OsStr::from_bytes(b))
    });
    let p = top.unwrap_or_else(|| root.to_path_buf());
    std::fs::canonicalize(&p).unwrap_or(p)
}

pub fn trusted(root: &Path) -> bool {
    let list = trust_file().and_then(|f| std::fs::read_to_string(f).ok()).unwrap_or_default();
    let repo = repo_of(root);
    // read exactly as `trust` writes: no trimming (a dir named "a " must not trust "a"), exact match only
    list.split('\n').any(|l| Path::new(l).is_absolute() && Path::new(l) == repo)
}

pub fn trust(root: &Path) -> String {
    let root = repo_of(root);
    // one path per line: a dir name holding a newline would smuggle in a second entry (such as "/")
    let Some(path) = root.to_str().filter(|p| !p.contains(['\n', '\r'])) else {
        return format!("refusing to trust {:?}: the path is not plain UTF-8 or holds a line break\n", root);
    };
    if trusted(&root) {
        return format!("already trusted: {path}\n");
    }
    let Some(f) = trust_file() else { return "no HOME or XDG_CONFIG_HOME: nowhere to record trust\n".into() };
    let _ = std::fs::create_dir_all(f.parent().unwrap_or(Path::new(".")));
    let mut list = std::fs::read_to_string(&f).unwrap_or_default();
    if !list.is_empty() && !list.ends_with('\n') {
        list.push('\n');
    }
    list += &format!("{path}\n");
    match std::fs::write(&f, list) {
        Ok(()) => format!("trusted: {path} (the gate may now run its typecheck, lint and test commands)\n"),
        Err(e) => format!("could not write {}: {e}\n", f.display()),
    }
}

/// Returns the process exit code. `hook`: "claude-start" (SessionStart), "claude" (Stop) or "git" (pre-commit).
/// `require_trust` (the plugin's hooks, which fire in every repo): run nothing in a repo the user hasn't trusted,
/// since verifying means executing the repo's own scripts (`npm run test`…) and a cloned repo's may be hostile.
pub fn gate(root: Option<PathBuf>, hook: &str, timeout: Duration, require_trust: bool) -> i32 {
    let mut input = Value::Null;
    if hook.starts_with("claude") {
        let mut s = String::new();
        let _ = std::io::stdin().read_to_string(&mut s);
        input = serde_json::from_str(&s).unwrap_or(Value::Null);
    }
    // git runs its hooks in the repository being committed; CLAUDE_PROJECT_DIR (set inside a Claude Code
    // session) names the session's project, which may be another repo, so only Claude hooks may use it
    let claude = hook.starts_with("claude");
    let root = root
        .or_else(|| input["cwd"].as_str().map(PathBuf::from))
        .or_else(|| std::env::var_os("CLAUDE_PROJECT_DIR").filter(|_| claude).map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("."));
    // absolute: attribution strips the repo root from paths, which a relative "." never matches
    let root = std::fs::canonicalize(&root).unwrap_or(root);
    if require_trust && !trusted(&root) {
        let repo = std::process::Command::new("git").arg("-C").arg(&root).args(["rev-parse", "--git-dir"]).output().is_ok_and(|o| o.status.success());
        if hook == "claude-start" && repo {
            let msg = "LookaCore: the verification gate is off in this repo until you trust it. Run `lkc trust` here to let it run this repo's typecheck, lint and test commands when the agent finishes.";
            println!("{}", json!({ "systemMessage": msg }));
        }
        return 0;
    }
    let session = input["session_id"].as_str().unwrap_or("").to_string();
    let (files, complete) = impact::changes(&root, None).unwrap_or((vec![], true));
    let fp = fingerprint(&root, &files).to_string();
    let mut state = load(&root);

    if hook == "warm" {
        // spawned by claude-start: compute the HEAD baselines now, while the agent works
        if let Err(e) = run::warm(&root, &detect::detect(&root), timeout) {
            eprintln!("lkc gate: baseline warm-up skipped: {e}");
        }
        return 0;
    }
    if hook == "claude-start" {
        // what was already uncommitted before the agent started is the owner's work, not the agent's
        state["sessions"][&session] = json!(fp);
        save(&root, &state);
        // the Stop-hook check otherwise clones HEAD and re-runs the typecheck there (about a minute on a Next.js
        // app whose HEAD has type errors); its own process group, so it outlives this hook without holding it open
        if let Ok(exe) = std::env::current_exe() {
            use std::os::unix::process::CommandExt;
            let _ = std::process::Command::new(exe)
                .args(["gate", "--hook", "warm", "--root"])
                .arg(&root)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .process_group(0)
                .spawn();
        }
        return 0;
    }
    if files.is_empty() && complete {
        return 0;
    }
    // this exact tree already passed (the agent stopped again without edits, or the gate is wired twice: project
    // hooks from `lkc init` plus the plugin's): nothing new to verify
    if state["passed"].as_str() == Some(fp.as_str()) {
        return 0;
    }
    if hook == "claude" {
        if state["sessions"][&session].as_str() == Some(fp.as_str()) {
            return 0; // nothing changed during this session
        }
        if input["stop_hook_active"] == true && state["blocked"].as_str() == Some(fp.as_str()) {
            // blocked once already and the agent changed nothing since: let it stop rather than loop
            eprintln!("lkc gate: the previous block still applies (see its report); stopping without further changes");
            return 0;
        }
    }

    let t0 = std::time::Instant::now();
    let map = detect::detect(&root);
    let changed = run::Changed { tests: impact::impact(&root, &map, &files).tests, files, complete, base: None };
    let kinds: Vec<String> = run::DEFAULT_KINDS.iter().map(|s| s.to_string()).collect();
    let r = run::verify(&root, &map, &kinds, None, Some(&changed), timeout);
    // nothing ran for changed source: unverified, not passed. Docs, config or assets alone still pass.
    let unverified = r.results.is_empty() && !changed.files.is_empty();
    let pass = r.ok || (r.results.is_empty() && !unverified);
    // what each check cost and found: the gate's price has to stay visible (last 50 kept)
    let check = json!({"hook": hook, "ms": t0.elapsed().as_millis() as u64, "ok": pass, "complete": changed.complete,
        "steps": r.results.iter().map(|e| json!([e.kind, e.duration_ms])).collect::<Vec<_>>()});
    let mut checks = state["checks"].as_array().cloned().unwrap_or_default();
    checks.push(check);
    let skip = checks.len().saturating_sub(50);
    state["checks"] = json!(checks[skip..]);
    if pass {
        if let Some(o) = state.as_object_mut() {
            o.remove("blocked");
        }
        state["passed"] = json!(fp);
        save(&root, &state);
        return 0;
    }
    state["blocked"] = json!(fp);
    state["blocks"] = json!(state["blocks"].as_u64().unwrap_or(0) + 1); // how often the gate intervened
    save(&root, &state);
    let mut report = run::render(&r);
    if report.len() > 6000 {
        let cut = (0..=6000).rev().find(|i| report.is_char_boundary(*i)).unwrap_or(0);
        report.truncate(cut);
        report += "\n… (truncated; full report in .looka/evidence/latest.json)\n";
    }
    let reason = if unverified {
        let mut list = changed.files[..changed.files.len().min(20)].join(", ");
        if changed.files.len() > 20 {
            list += &format!(" … ({} more)", changed.files.len() - 20);
        }
        format!("LookaCore gate: NOT VERIFIED: no check ran for changed source files {list}\n\n{report}")
    } else {
        format!("LookaCore gate: your changes introduce failures. Fix them before finishing.\n\n{report}")
    };
    if hook == "claude" {
        println!("{}", json!({"decision": "block", "reason": reason}));
        0
    } else {
        eprint!("{reason}");
        1
    }
}

// `cargo install` puts lkc in ~/.cargo/bin, which many shells (and so hook processes) don't have on PATH
const CLAUDE_HOOKS: &str = r#"{"SessionStart": [{"hooks": [{"type": "command", "command": "PATH=\"$HOME/.cargo/bin:$PATH\" lkc gate --hook claude-start", "timeout": 30}]}],
 "Stop": [{"hooks": [{"type": "command", "command": "PATH=\"$HOME/.cargo/bin:$PATH\" lkc gate --hook claude", "timeout": 900}]}]}"#;

/// Wire the gate into a project: Claude Code hooks in .claude/settings.json (merged, never replaced) and a
/// git pre-commit hook when the repository has none of its own.
pub fn init(root: &Path) -> String {
    // wiring the gate in is the explicit opt-in the plugin's hooks wait for
    let mut out = trust(root);
    let path = root.join(".claude/settings.json");
    let mut settings: Value = match std::fs::read_to_string(&path) {
        Err(_) => json!({}),
        Ok(s) => match serde_json::from_str(&s) {
            Ok(v) => v,
            // never replace a settings file we can't read back: it holds the owner's permissions and hooks
            Err(e) => return format!("Claude Code: .claude/settings.json is not valid JSON ({e}); fix it, then run `lkc init` again\n"),
        },
    };
    let wanted: Value = serde_json::from_str(CLAUDE_HOOKS).unwrap();
    let mut added = vec![];
    for (event, entries) in wanted.as_object().unwrap() {
        let list = &mut settings["hooks"][event];
        if !list.is_array() {
            *list = json!([]);
        }
        let present = list.to_string().contains("lkc gate");
        if !present {
            list.as_array_mut().unwrap().extend(entries.as_array().unwrap().iter().cloned());
            added.push(event.as_str());
        }
    }
    if added.is_empty() {
        out += "Claude Code: gate hooks already in .claude/settings.json\n";
    } else {
        let _ = std::fs::create_dir_all(root.join(".claude"));
        match std::fs::write(&path, serde_json::to_string_pretty(&settings).unwrap() + "\n") {
            Ok(()) => out += &format!("Claude Code: added {} hook(s) to .claude/settings.json\n", added.join(" + ")),
            Err(e) => out += &format!("Claude Code: could not write .claude/settings.json: {e}\n"),
        }
    }
    let ignore = root.join(".gitignore");
    let current = std::fs::read_to_string(&ignore).unwrap_or_default();
    if !current.lines().any(|l| matches!(l.trim(), ".looka" | ".looka/" | "/.looka" | "/.looka/")) {
        let sep = if current.is_empty() || current.ends_with('\n') { "" } else { "\n" };
        if std::fs::write(&ignore, format!("{current}{sep}.looka/\n")).is_ok() {
            out += ".gitignore: added .looka/ (gate state and evidence)\n";
        }
    }
    let hooks_path = std::process::Command::new("git").arg("-C").arg(root).args(["config", "core.hooksPath"]).output();
    let custom = hooks_path.ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).filter(|p| !p.is_empty());
    let git_dir = std::process::Command::new("git").arg("-C").arg(root).args(["rev-parse", "--git-dir"]).output();
    match git_dir.ok().filter(|o| o.status.success()).map(|o| root.join(String::from_utf8_lossy(&o.stdout).trim())) {
        None => out += "git: not a repository, pre-commit hook skipped\n",
        Some(_) if custom.is_some() => {
            // git ignores .git/hooks when core.hooksPath is set (husky, or a global hooks dir)
            out += &format!("git: core.hooksPath is {}; add `lkc gate --hook git` to the pre-commit there\n", custom.unwrap_or_default())
        }
        Some(dir) => {
            let hook = dir.join("hooks/pre-commit");
            if hook.exists() {
                out += "git: a pre-commit hook already exists; add `lkc gate --hook git` to it\n";
            } else {
                let _ = std::fs::create_dir_all(dir.join("hooks"));
                let ok = std::fs::write(&hook, "#!/bin/sh\n# LookaCore gate: block commits that introduce failures\nPATH=\"$HOME/.cargo/bin:$PATH\" exec lkc gate --hook git\n").is_ok();
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755));
                }
                out += if ok { "git: installed .git/hooks/pre-commit\n" } else { "git: could not write the pre-commit hook\n" };
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trust_list_cannot_be_widened_by_odd_dir_names() {
        let base = std::env::temp_dir().join(format!("lkc-trust-{}", std::process::id()));
        let cfg = base.join("cfg");
        std::env::set_var("XDG_CONFIG_HOME", &cfg);
        // a repo whose name smuggles a second line ("/") must be refused, not trust everything
        let evil = base.join("x\n");
        std::fs::create_dir_all(evil.join("y")).unwrap();
        assert!(trust(&evil.join("y")).starts_with("refusing"));
        assert!(!trusted(Path::new("/usr")));
        // "a " is not "a"
        let spaced = base.join("a ");
        std::fs::create_dir_all(&spaced).unwrap();
        std::fs::create_dir_all(base.join("a")).unwrap();
        assert!(trust(&spaced).starts_with("trusted"));
        let canon = std::fs::canonicalize(&base).unwrap();
        assert!(trusted(&canon.join("a ")) && !trusted(&canon.join("a")));
        // a git repo named "a\n" is not the trusted "a" once git's own trailing newline is stripped
        let nl = base.join("a\n");
        std::fs::create_dir_all(&nl).unwrap();
        assert!(std::process::Command::new("git").arg("-C").arg(&nl).args(["init", "-q"]).status().unwrap().success());
        assert!(trust(&base.join("a")).starts_with("trusted"));
        assert!(!trusted(&nl), "a repo whose name ends in a newline must not inherit a's trust");
        let _ = std::fs::remove_dir_all(&base);
    }
}
