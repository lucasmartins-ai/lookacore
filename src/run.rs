//! Execution sandbox + Universal Build & Test: run only discovered commands, collect evidence.

use crate::detect::{Cmd, Component, ProjectMap};
use crate::diag::{self, Diagnostic};
use serde::Serialize;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const OUT_CAP: usize = 256 * 1024;

pub const DEFAULT_KINDS: &[&str] = &["typecheck", "lint", "test"];

/// Env vars toolchains need; everything else (tokens, keys, cloud creds) is dropped.
/// Extra names can be passed with LOOKA_ENV_PASS=NAME1,NAME2.
pub const ENV_KEEP: &[&str] = &[
    "PATH", "HOME", "USER", "LOGNAME", "SHELL", "LANG", "LC_ALL", "LC_CTYPE", "TERM", "TMPDIR", "CARGO_HOME",
    "RUSTUP_HOME", "RUSTUP_TOOLCHAIN", "GOPATH", "GOROOT", "GOCACHE", "GOMODCACHE", "GOFLAGS", "NVM_DIR",
    "VIRTUAL_ENV", "PYTHONPATH", "JAVA_HOME", "ANDROID_HOME", "ANDROID_SDK_ROOT", "FLUTTER_ROOT", "CC", "CXX",
    "PKG_CONFIG_PATH", "SDKROOT", "DEVELOPER_DIR",
];

#[derive(Serialize)]
pub struct Evidence {
    pub component: String,
    pub kind: String,
    pub command: String,
    pub ok: bool,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub duration_ms: u128,
    pub diagnostics: Vec<Diagnostic>,
    pub output_tail: String,
}

#[derive(Serialize)]
pub struct Report {
    pub ok: bool,
    pub root: String,
    pub finished_at: u64,
    pub results: Vec<Evidence>,
    pub skipped: Vec<String>,
    /// with `--changed`: failures not reproduced on a pristine HEAD checkout. ok = (introduced == 0).
    pub introduced: Option<usize>,
    pub saved_to: Option<String>,
}

/// Variables replicated from the project's CI job (`--ci`): they reach every command, on top of the allowlist.
pub static CI_ENV: std::sync::Mutex<Vec<(String, String)>> = std::sync::Mutex::new(Vec::new());

pub fn safe_env() -> Vec<(String, String)> {
    let extra = std::env::var("LOOKA_ENV_PASS").unwrap_or_default();
    let ci: Vec<(String, String)> = CI_ENV.lock().map(|v| v.clone()).unwrap_or_default();
    std::env::vars()
        .filter(|(k, _)| ENV_KEEP.contains(&k.as_str()) || extra.split(',').any(|e| e.trim() == k))
        .filter(|(k, _)| !ci.iter().any(|(c, _)| c == k))
        .chain(ci.clone())
        // no FORCE_COLOR: several tools treat any value, even "0", as "force colour on"
        .chain([("CI", "1"), ("NO_COLOR", "1"), ("CLICOLOR", "0"), ("CARGO_TERM_COLOR", "never")].map(|(k, v)| (k.into(), v.into())))
        .collect()
}

pub fn tail(s: &str, n: usize) -> String {
    let mut i = s.len().saturating_sub(n);
    while !s.is_char_boundary(i) {
        i += 1;
    }
    s[i..].to_string()
}

// ponytail: isolation = command allowlist (only discovered commands) + scrubbed env + own process group
// + timeout. No filesystem/network jail yet; upgrade path: sandbox-exec (macOS) / bwrap (Linux) / container.
pub fn exec(root: &Path, comp: &Component, cmd: &Cmd, timeout: Duration) -> Evidence {
    let dir = root.join(&comp.dir);
    let start = Instant::now();
    let mut ev = Evidence {
        component: comp.dir.clone(),
        kind: cmd.kind.to_string(),
        command: cmd.run.clone(),
        ok: false,
        exit_code: None,
        timed_out: false,
        duration_ms: 0,
        diagnostics: vec![],
        output_tail: String::new(),
    };
    let spawned = Command::new("sh")
        .arg("-c")
        .arg(&cmd.run)
        .current_dir(&dir)
        .env_clear()
        .envs(safe_env())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn();
    let mut child = match spawned {
        Ok(c) => c,
        Err(e) => {
            ev.output_tail = format!("spawn failed: {e}");
            return ev;
        }
    };
    let pgid = child.id() as i32;
    // Readers keep only the last OUT_CAP bytes and report through a channel, so a descendant that
    // escaped the process group (setsid daemons) holding our pipes can't block us past the deadline.
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    let reader = |mut r: Box<dyn Read + Send>, tx: mpsc::Sender<Vec<u8>>| {
        thread::spawn(move || {
            let (mut keep, mut buf) = (Vec::new(), [0u8; 8192]);
            while let Ok(n @ 1..) = r.read(&mut buf) {
                keep.extend_from_slice(&buf[..n]);
                if keep.len() > 2 * OUT_CAP {
                    keep.drain(..keep.len() - OUT_CAP);
                }
            }
            let _ = tx.send(keep);
        });
    };
    reader(Box::new(child.stdout.take().unwrap()), tx.clone());
    reader(Box::new(child.stderr.take().unwrap()), tx);
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) if start.elapsed() > timeout => {
                ev.timed_out = true;
                unsafe { libc::killpg(pgid, libc::SIGKILL) };
                break child.wait().ok();
            }
            Ok(None) => thread::sleep(Duration::from_millis(50)),
            Err(_) => break None,
        }
    };
    // reap leftovers (watchers, servers) still holding our pipes.
    // ponytail: pgid could in theory be reused between reap and kill; waitid(WNOWAIT) if that ever bites.
    unsafe { libc::killpg(pgid, libc::SIGKILL) };
    let mut text = String::new();
    for _ in 0..2 {
        let left = timeout.saturating_sub(start.elapsed()).max(Duration::from_secs(2));
        match rx.recv_timeout(left) {
            Ok(b) => text += &(String::from_utf8_lossy(&b).into_owned() + "\n"),
            Err(_) => {
                text += "[looka: output still held open by a detached process; stopped reading]\n";
                break;
            }
        }
    }
    ev.duration_ms = start.elapsed().as_millis();
    ev.exit_code = status.and_then(|s| s.code());
    ev.ok = !ev.timed_out && ev.exit_code == Some(0);
    ev.diagnostics = diag::parse(&text)
        .into_iter()
        .map(|mut x| {
            if let Some(f) = &x.file {
                let p = Path::new(f);
                if p.is_absolute() {
                    // /tmp vs /private/tmp (and other symlinks): compare canonical paths
                    let canon = fs::canonicalize(p).unwrap_or(p.to_path_buf());
                    x.file = canon.strip_prefix(root).ok().map(|r| r.display().to_string()).or(x.file.take());
                } else if comp.dir != "." && dir.join(p).exists() {
                    x.file = Some(format!("{}/{}", comp.dir, f));
                }
            }
            x
        })
        .collect();
    ev.output_tail = tail(text.trim(), if ev.ok { 400 } else { 4000 });
    ev
}

/// Shell-quote a path; a leading `-` gets `./` so a file name can't become a tool option.
fn quote(s: &str) -> String {
    let s = if s.starts_with('-') { format!("./{s}") } else { s.to_string() };
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// The component's test command narrowed to `tests` (root-relative), for runners that take file filters.
/// None = runner can't filter (cargo, make, …): run the full suite.
/// Whether this component's test runner can take `test`: a repo with both pytest and vitest at the root
/// must not hand `.test.mjs` files to pytest.
fn runs_test(c: &Component, test: &str) -> bool {
    let inside = c.dir == "." || test.starts_with(&format!("{}/", c.dir));
    let ext = test.rsplit_once('.').map_or("", |(_, e)| e);
    inside
        && match c.ecosystem {
            "python" => ext == "py",
            "node" => matches!(ext, "js" | "jsx" | "ts" | "tsx" | "mjs" | "cjs" | "mts" | "cts"),
            "go" => test.ends_with("_test.go"),
            "cargo" => ext == "rs",
            _ => true,
        }
}

fn narrowed(c: &Component, cmd: &Cmd, tests: &[String]) -> Option<Cmd> {
    let raw: Vec<&str> = tests
        .iter()
        .filter(|t| runs_test(c, t))
        .filter_map(|t| if c.dir == "." { Some(t.as_str()) } else { t.strip_prefix(&format!("{}/", c.dir)) })
        .collect();
    let mine: Vec<String> = raw.iter().map(|t| quote(t)).collect();
    let base = match c.ecosystem {
        // the selection may hold files the runner's config excludes (a Playwright spec next to vitest units):
        // matching none of them is not a failure
        "node" if c.frameworks.iter().any(|f| f == "vitest") => "node_modules/.bin/vitest run --passWithNoTests".to_string(),
        "node" if c.frameworks.iter().any(|f| f == "jest") => "node_modules/.bin/jest --passWithNoTests".to_string(),
        "python" if cmd.run.contains("pytest") => cmd.run.clone(),
        "go" => {
            let mut pkgs: Vec<String> = raw.iter().map(|t| quote(&format!("./{}", t.rsplit_once('/').map_or(".", |(d, _)| d)))).collect();
            pkgs.dedup();
            return Some(Cmd { kind: "test", run: format!("go test {}", pkgs.join(" ")), available: cmd.available });
        }
        _ => return None,
    };
    Some(Cmd { kind: "test", run: format!("{base} {}", mine.join(" ")), available: cmd.available })
}

/// Uncommitted source changes and the tests that depend on them (from `impact`).
pub struct Changed {
    pub files: Vec<String>,
    pub tests: Vec<String>,
    /// false: a deletion or non-source change was seen, so lint and tests run in full
    pub complete: bool,
}

/// The component's lint command narrowed to the changed files, for eslint- and ruff-based linters.
/// Some(None) = nothing of this component's changed; None = linter can't be narrowed, run it whole.
fn narrowed_lint(c: &Component, cmd: &Cmd, files: &[String]) -> Option<Option<Cmd>> {
    let mine: Vec<&str> = files
        .iter()
        .filter_map(|f| if c.dir == "." { Some(f.as_str()) } else { f.strip_prefix(&format!("{}/", c.dir)) })
        .collect();
    let pick = |exts: &[&str]| -> Vec<String> {
        mine.iter().filter(|f| f.rsplit_once('.').is_some_and(|(_, e)| exts.contains(&e))).map(|f| quote(f)).collect()
    };
    let (bin, args) = match c.ecosystem {
        "node" => {
            let script = fs::read_to_string(Path::new(&c.manifest)).unwrap_or_default();
            let uses_eslint = script.contains("eslint") || script.contains("next lint");
            if !uses_eslint || !cmd.run.contains("lint") {
                return None;
            }
            ("node_modules/.bin/eslint".to_string(), pick(&["js", "jsx", "ts", "tsx", "mjs", "cjs", "mts", "cts"]))
        }
        "python" if cmd.run.contains("ruff") => (cmd.run.trim_end_matches(" .").to_string(), pick(&["py"])),
        _ => return None,
    };
    Some((!args.is_empty()).then(|| Cmd { kind: "lint", run: format!("{bin} {}", args.join(" ")), available: cmd.available }))
}

/// `changed`: lint only the changed files and run only the tests that depend on them, where the tools allow it,
/// then attribute any failure to the changes or to HEAD.
pub fn verify(root: &Path, map: &ProjectMap, kinds: &[String], component: Option<&str>, changed: Option<&Changed>, timeout: Duration) -> Report {
    let (mut jobs, mut skipped) = (vec![], vec![]);
    for c in map.components.iter().filter(|c| component.is_none_or(|f| c.dir == f.trim_end_matches('/'))) {
        for k in kinds {
            let Some(full) = c.cmd(k) else { continue };
            let narrow;
            let narrow_lint;
            let cmd = match changed {
                Some(ch) if *k == "lint" && ch.complete => {
                    // manifest path is root-relative; resolve it for the script check
                    let abs = Component { manifest: root.join(&c.manifest).display().to_string(), ..c.clone() };
                    match narrowed_lint(&abs, full, &ch.files) {
                        Some(None) => {
                            skipped.push(format!("({}) lint: no changed files to lint", c.dir));
                            continue;
                        }
                        Some(Some(n)) => {
                            narrow_lint = n;
                            &narrow_lint
                        }
                        None => full,
                    }
                }
                Some(Changed { tests, complete: true, .. }) if *k == "test" => {
                    let inside = tests.iter().any(|t| runs_test(c, t));
                    if !inside {
                        skipped.push(format!("({}) test: no tests affected by the changes", c.dir));
                        continue;
                    }
                    narrow = narrowed(c, full, tests);
                    narrow.as_ref().unwrap_or(full)
                }
                _ => full,
            };
            if cmd.available {
                eprintln!("looka: {k} ({}) `{}`", c.dir, cmd.run);
                jobs.push((c, cmd.clone()));
            } else {
                skipped.push(format!("({}) {k}: `{}` — tool not on PATH", c.dir, cmd.run));
            }
        }
    }
    // typecheck, lint and tests are independent: run them at once (the gate waits on the slowest, not the sum)
    // ponytail: one thread per command, no pool; a repo with dozens of components would oversubscribe the CPU
    let mut results = parallel(jobs.iter().map(|(c, cmd)| move || exec(root, c, cmd, timeout)).collect());
    let introduced = changed.and_then(|ch| match attribute(root, map, &mut results, ch, timeout) {
        Ok(n) => Some(n),
        Err(e) => {
            skipped.push(format!("failure attribution: no pristine HEAD baseline ({e})"));
            None
        }
    });
    let finished_at = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let mut r = Report {
        ok: !results.is_empty() && match introduced {
            Some(n) => n == 0,
            None => results.iter().all(|e| e.ok),
        },
        introduced,
        root: root.display().to_string(),
        finished_at,
        results,
        skipped,
        saved_to: None,
    };
    r.saved_to = save(root, &r).ok();
    r
}

/// Re-run failed commands (typecheck, lint, tests) on a pristine clone of HEAD and mark failures that already happen there.
/// Returns the number of failures introduced by the working-tree changes, or why no baseline could be made.
fn attribute(root: &Path, map: &ProjectMap, results: &mut [Evidence], changed: &Changed, timeout: Duration) -> Result<usize, String> {
    if results.iter().all(|e| e.ok) {
        return Ok(0);
    }
    wait_for_warmup(root, timeout);
    let bk = BaseKey::new(root, map)?;
    let misses: Vec<usize> = (0..results.len()).filter(|&i| !results[i].ok && !bk.path(&results[i].component, &results[i].command).is_file()).collect();
    let jobs: Vec<(String, String)> = misses.iter().map(|&i| (results[i].component.clone(), results[i].command.clone())).collect();
    run_baselines(root, map, &bk, &jobs, timeout)?;
    let key = |e: &Evidence| bk.path(&e.component, &e.command);
    let mut introduced = 0;
    for e in results.iter_mut().filter(|e| !e.ok) {
        let Some(before) = fs::read_to_string(key(e)).ok().and_then(|t| serde_json::from_str::<Baseline>(&t).ok()) else {
            introduced += 1;
            continue;
        };
        for x in &mut e.diagnostics {
            // a diagnostic in a git-ignored dependency dir (.venv, node_modules…) is not the diff's doing: the
            // baseline symlinks those dirs, which tools scanning `.` skip. Ignored, not just named like one:
            // tracked source under a `build/` or `dist/` dir must still count, or the gate fails open there.
            let ignored = |f: &str| {
                crate::impact::generated(Path::new(f))
                    && Command::new("git").arg("-C").arg(root.join(&e.component)).args(["check-ignore", "-q", "--", f]).status().is_ok_and(|s| s.success())
            };
            x.preexisting = x.file.as_deref().is_some_and(ignored) || before.diagnostics.iter().any(|b| b.file == x.file && b.message == x.message);
        }
        if e.kind == "test" && !e.timed_out {
            rerun_alone(root, map, e, changed, timeout);
        }
        let new = e.diagnostics.iter().filter(|x| !x.preexisting && !x.flaky).count();
        // a failure we can't parse, or a run cut short, can't be attributed: count it as introduced
        introduced += if e.diagnostics.is_empty() || e.timed_out { new.max(1) } else { new };
    }
    Ok(introduced)
}

/// Runs the closures on their own threads and returns their results in order.
fn parallel<T: Send, F: FnOnce() -> T + Send>(fs: Vec<F>) -> Vec<T> {
    std::thread::scope(|s| fs.into_iter().map(|f| s.spawn(f)).collect::<Vec<_>>().into_iter().map(|h| h.join().expect("verify worker panicked")).collect())
}

/// A test failure the baseline doesn't explain may still be flaky (timing under load, test order): re-run just the
/// failing test files once, and mark them flaky if they all pass alone, so the gate doesn't block on noise.
/// Never for a test the change touches or reaches: an intermittent failure there may be a race the change introduced
/// (or a test written to fail once), so it keeps blocking.
fn rerun_alone(root: &Path, map: &ProjectMap, e: &mut Evidence, changed: &Changed, timeout: Duration) {
    let new: Vec<usize> = (0..e.diagnostics.len()).filter(|&i| !e.diagnostics[i].preexisting).collect();
    let Some(c) = map.components.iter().find(|c| c.dir == e.component) else { return };
    if new.is_empty() || new.iter().any(|&i| e.diagnostics[i].file.is_none()) {
        return;
    }
    // diagnostics are relative to the component; narrowed() takes root-relative test paths
    let mut files: Vec<String> = new.iter().filter_map(|&i| e.diagnostics[i].file.clone()).map(|f| if c.dir == "." { f } else { format!("{}/{f}", c.dir) }).collect();
    files.sort();
    files.dedup();
    let affected = |f: &String| changed.files.contains(f) || changed.tests.contains(f);
    if files.len() > 20 || files.iter().any(affected) {
        return;
    }
    let Some(cmd) = c.cmd("test").and_then(|full| narrowed(c, full, &files)) else { return };
    eprintln!("looka: re-running {} failing test file(s) alone to rule out flakiness", files.len());
    let again = exec(root, c, &cmd, timeout);
    // only a clean pass clears them: messages can vary between runs (values, timings), so no per-message matching
    if again.ok && !again.timed_out {
        for i in new {
            e.diagnostics[i].flaky = true;
        }
    }
}

fn git_out(args: &[&str]) -> Result<String, String> {
    let o = Command::new("git").args(args).output().map_err(|e| e.to_string())?;
    if o.status.success() { Ok(String::from_utf8_lossy(&o.stdout).trim().to_string()) } else { Err(String::from_utf8_lossy(&o.stderr).trim().to_string()) }
}

/// Where a baseline result is cached: a function of (HEAD, component, command, installed deps, lkc binary).
struct BaseKey {
    cache: PathBuf,
    head: String,
    locks: Vec<u8>,
    exe: Option<SystemTime>,
}

impl BaseKey {
    fn new(root: &Path, map: &ProjectMap) -> Result<Self, String> {
        let head = git_out(&["-C", root.to_str().ok_or("non-utf8 path")?, "rev-parse", "HEAD"])?;
        // installed deps are shared with the baseline (symlinked), so they are part of the key
        let locks: Vec<u8> = ["package-lock.json", "pnpm-lock.yaml", "yarn.lock", "bun.lock", "Cargo.lock", "poetry.lock", "uv.lock", "requirements.txt", "go.sum"]
            .iter()
            .flat_map(|f| map.components.iter().map(move |c| root.join(&c.dir).join(f)))
            .filter_map(|p| fs::read(p).ok())
            .flatten()
            .collect();
        // the looka binary is part of the key: a newer parser must not reuse diagnostics an older one produced
        let exe = std::env::current_exe().and_then(fs::metadata).and_then(|m| m.modified()).ok();
        Ok(BaseKey { cache: root.join(".looka/baseline"), head, locks, exe })
    }
    fn path(&self, component: &str, command: &str) -> PathBuf {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (component, command, &self.locks, self.exe).hash(&mut h);
        self.cache.join(format!("{}-{:016x}.json", self.head, h.finish()))
    }
}

/// Runs (component, command) pairs on a pristine clone of HEAD, in parallel, and caches each result.
fn run_baselines(root: &Path, map: &ProjectMap, bk: &BaseKey, jobs: &[(String, String)], timeout: Duration) -> Result<(), String> {
    if jobs.is_empty() {
        return Ok(());
    }
    let root_s = root.to_str().ok_or("non-utf8 path")?;
    let top = fs::canonicalize(git_out(&["-C", root_s, "rev-parse", "--show-toplevel"])?).map_err(|e| e.to_string())?;
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
    let base = std::env::temp_dir().join(format!("looka-base-{}-{nanos}", std::process::id()));
    git_out(&["clone", "-q", "--shared", top.to_str().ok_or("non-utf8 path")?, base.to_str().ok_or("non-utf8 path")?])?;
    // canonical, so absolute paths printed by tools strip the same way as in the working tree
    let base = fs::canonicalize(&base).map_err(|e| e.to_string())?;
    let base_root = base.join(root.strip_prefix(&top).map_err(|e| e.to_string())?);
    // what git ignores is not in the clone: installs, generated clients (prisma/generated-client), build
    // output. Without them HEAD fails differently ("Cannot find module …") and nothing matches as pre-existing.
    if let Ok(ignored) = git_out(&["-C", top.to_str().unwrap_or("."), "ls-files", "--others", "--ignored", "--exclude-standard", "--directory"]) {
        for rel in ignored.lines().map(|l| l.trim_end_matches('/')).filter(|l| !l.is_empty() && !l.split('/').any(|c| c == ".looka")) {
            let (src, dst) = (top.join(rel), base.join(rel));
            if !dst.exists() {
                if let Some(parent) = dst.parent() {
                    let _ = fs::create_dir_all(parent);
                }
                let _ = std::os::unix::fs::symlink(&src, &dst);
            }
        }
    }
    let _ = fs::create_dir_all(&bk.cache);
    let base_jobs: Vec<_> = jobs
        .iter()
        .filter_map(|(dir, command)| {
            let comp = map.components.iter().find(|c| &c.dir == dir)?;
            eprintln!("looka: baseline on HEAD for `{command}`");
            Some((comp, Cmd { kind: "baseline", run: command.clone(), available: true }))
        })
        .collect();
    let befores = parallel(base_jobs.iter().map(|(comp, cmd)| { let br = &base_root; move || exec(br, comp, cmd, timeout) }).collect());
    for ((comp, cmd), before) in base_jobs.iter().zip(befores) {
        if before.timed_out || before.exit_code.is_none() {
            continue; // not a baseline worth remembering; the failure stays unattributed
        }
        let b = Baseline { ok: before.ok, exit_code: before.exit_code, diagnostics: before.diagnostics };
        let _ = fs::write(bk.path(&comp.dir, &cmd.run), serde_json::to_string(&b).unwrap_or_default());
    }
    let _ = fs::remove_dir_all(&base);
    Ok(())
}

/// Pre-computes the HEAD baseline of each component's typecheck (always run in full, so its command doesn't
/// depend on the diff) while the agent works: the Stop-hook check then finds it cached instead of cloning HEAD.
pub fn warm(root: &Path, map: &ProjectMap, timeout: Duration) -> Result<(), String> {
    let marker = root.join(".looka/warming");
    let _ = fs::create_dir_all(root.join(".looka"));
    let _ = fs::write(&marker, std::process::id().to_string());
    let r = warm_inner(root, map, timeout);
    let _ = fs::remove_file(&marker);
    r
}

/// A warm-up still in flight will cache the very baselines a check needs: wait for it rather than run them twice.
/// Called only once a check has failures to attribute, so the check's own commands overlap the warm-up.
fn wait_for_warmup(root: &Path, timeout: Duration) {
    let marker = root.join(".looka/warming");
    let start = std::time::Instant::now();
    while let Ok(pid) = fs::read_to_string(&marker) {
        // a warm-up killed mid-way leaves its marker behind: only a live process counts
        let alive = Command::new("kill").args(["-0", pid.trim()]).stderr(Stdio::null()).status().is_ok_and(|s| s.success());
        if !alive || start.elapsed() > timeout {
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

fn warm_inner(root: &Path, map: &ProjectMap, timeout: Duration) -> Result<(), String> {
    let bk = BaseKey::new(root, map)?;
    let jobs: Vec<(String, String)> = map
        .components
        .iter()
        .filter_map(|c| c.cmd("typecheck").filter(|t| t.available).map(|t| (c.dir.clone(), t.run.clone())))
        .filter(|(d, r)| !bk.path(d, r).is_file())
        .collect();
    run_baselines(root, map, &bk, &jobs, timeout)
}

#[derive(Serialize, serde::Deserialize)]
struct Baseline {
    ok: bool,
    exit_code: Option<i32>,
    diagnostics: Vec<Diagnostic>,
}

fn save(root: &Path, r: &Report) -> std::io::Result<String> {
    let dir = root.join(".looka/evidence");
    fs::create_dir_all(&dir)?;
    let gi = root.join(".looka/.gitignore");
    if !gi.exists() {
        fs::write(gi, "*\n")?;
    }
    let body = serde_json::to_string_pretty(r)?;
    let name = format!(".looka/evidence/{}.json", r.finished_at);
    fs::write(root.join(&name), &body)?;
    fs::write(dir.join("latest.json"), &body)?;
    Ok(name)
}

pub fn render(r: &Report) -> String {
    let partial = match r.introduced {
        Some(0) if r.results.iter().any(|e| !e.ok) => " — no failures introduced (all failures pre-exist on HEAD)".to_string(),
        Some(n) if n > 0 => format!(" — {n} failure(s) introduced by the changes"),
        _ if r.ok && !r.skipped.is_empty() => " (partial: some checks skipped)".to_string(),
        _ => String::new(),
    };
    let mut s = format!("# verify: {}{partial}\n", if r.ok { "PASS" } else { "FAIL" });
    if r.results.is_empty() {
        s += "nothing ran — no verifiable command was found or available, so the change is NOT verified\n";
    }
    for e in &r.results {
        let status = if e.ok {
            "✓".to_string()
        } else if e.timed_out {
            "✗ timeout".to_string()
        } else {
            format!("✗ exit {}", e.exit_code.map_or("?".into(), |c| c.to_string()))
        };
        s += &format!("{status} ({}) {} `{}` {:.1}s\n", e.component, e.kind, e.command, e.duration_ms as f64 / 1000.0);
        // the runner's own count ("Tests 27 passed", "test result: ok. 3 passed", "5 passed in 0.3s")
        if let Some(sum) = e.output_tail.lines().rev().find(|l| l.contains(" passed") && l.chars().any(|c| c.is_ascii_digit())) {
            s += &format!("    runner: {}\n", sum.split_whitespace().collect::<Vec<_>>().join(" "));
        }
        if !e.ok {
            if e.diagnostics.is_empty() {
                let lines: Vec<&str> = e.output_tail.lines().collect();
                s += &lines[lines.len().saturating_sub(20)..].iter().map(|l| format!("    {l}\n")).collect::<String>();
            } else {
                // introduced failures first and in full; pre-existing ones only counted
                let new: Vec<_> = e.diagnostics.iter().filter(|d| !d.preexisting && !d.flaky).take(20).cloned().collect();
                let pre = e.diagnostics.iter().filter(|d| d.preexisting).count();
                let flaky: Vec<&str> = e.diagnostics.iter().filter(|d| d.flaky).filter_map(|d| d.file.as_deref()).collect();
                s += &diag::render(&new);
                if !flaky.is_empty() {
                    s += &format!("  … {} flaky: failed in the full run, passed when re-run alone ({})\n", flaky.len(), flaky.join(", "));
                }
                if pre > 0 {
                    s += &format!("  … {pre} pre-existing on HEAD (not caused by these changes; listed in the evidence file)\n");
                }
            }
        }
    }
    for k in &r.skipped {
        s += &format!("- skipped {k}\n");
    }
    if let Some(p) = &r.saved_to {
        s += &format!("evidence: {p}\n");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_selection_respects_the_runner_language() {
        let py = Component { dir: ".".into(), ecosystem: "python", manifest: String::new(), name: None, frameworks: vec![], commands: vec![] };
        let cmd = Cmd { kind: "test", run: "python -m pytest -q".into(), available: true };
        let n = narrowed(&py, &cmd, &["tests/test_a.py".into(), "test/index.test.js".into()]).unwrap();
        assert!(n.run.contains("tests/test_a.py") && !n.run.contains("index.test.js"), "{}", n.run);
    }

    #[test]
    fn tail_never_splits_a_character() {
        let log = "✓ Ready ➜ ".repeat(500);
        for n in 0..40 {
            let t = tail(&log, n);
            assert!(t.len() <= n + 3 && log.ends_with(&t));
        }
    }

    #[test]
    fn quote_blocks_shell_and_option_injection() {
        assert_eq!(quote("a b.ts"), "'a b.ts'");
        assert_eq!(quote("it's.ts"), "'it'\\''s.ts'");
        assert_eq!(quote("--config=x.js"), "'./--config=x.js'");
    }
}
