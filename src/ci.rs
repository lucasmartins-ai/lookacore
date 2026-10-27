//! `--ci`: replicate the project's own CI test job locally before verify/probe — its env, its database
//! service and its DB setup steps — so pages and tests that need a database stop failing for lack of one.
//! Read from .github/workflows; services run from local binaries (Postgres via initdb/pg_ctl, Redis via
//! redis-server) on a free port, never touching a database the developer already runs.

use crate::{detect, run, yaml};
use regex::Regex;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

pub struct CiEnv {
    pub summary: Vec<String>,
    pg_dirs: Vec<(PathBuf, PathBuf)>, // (pg_ctl, data dir) to stop
    children: Vec<Child>,
}

impl Drop for CiEnv {
    fn drop(&mut self) {
        for (pg_ctl, dir) in &self.pg_dirs {
            let _ = Command::new(pg_ctl).arg("-D").arg(dir).args(["stop", "-m", "fast"]).stdout(Stdio::null()).stderr(Stdio::null()).status();
            let _ = std::fs::remove_dir_all(dir); // our own throwaway cluster
        }
        for c in &mut self.children {
            let _ = c.kill();
            let _ = c.wait();
        }
        if let Ok(mut v) = run::CI_ENV.lock() {
            v.clear();
        }
    }
}

/// App configuration only: variables that change how the toolchain itself runs never come from repository data.
fn replicable(k: &str) -> bool {
    let deny = ["PATH", "HOME", "SHELL", "ENV", "BASH_ENV", "NODE_OPTIONS", "NODE_PATH", "IFS", "TMPDIR", "USER", "LOGNAME"];
    let deny_prefix = ["LD_", "DYLD_", "PYTHON", "PERL5", "RUBY", "GIT_", "NPM_CONFIG", "CARGO", "RUSTC", "RUSTFLAGS", "LOOKA_"];
    Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*$").unwrap().is_match(k)
        && !deny.contains(&k)
        && !deny_prefix.iter().any(|p| k.to_ascii_uppercase().starts_with(p))
        && !k.ends_with("_PRELOAD")
        && !run::ENV_KEEP.contains(&k)
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").and_then(|l| l.local_addr()).map_or(55432, |a| a.port())
}

/// A test job: one whose steps run tests; jobs with services first (they are the ones that need replicating).
fn pick_job(root: &Path) -> Option<(String, String, Value, Value)> {
    let dir = root.join(".github/workflows");
    let mut best: Option<(i32, String, String, Value, Value)> = None;
    let runs_tests = Regex::new(r"\b(test|pytest|vitest|jest|cargo test|go test)\b").unwrap();
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let p = e.path();
        if !p.extension().is_some_and(|x| x == "yml" || x == "yaml") {
            continue;
        }
        let wf = yaml::parse(&std::fs::read_to_string(&p).unwrap_or_default());
        for (name, job) in wf["jobs"].as_object().into_iter().flatten() {
            let runs: String = job["steps"].as_array().into_iter().flatten().filter_map(|s| s["run"].as_str()).collect::<Vec<_>>().join("\n");
            if !runs_tests.is_match(&runs) {
                continue;
            }
            let score = if job["services"].is_object() { 2 } else { 1 };
            if best.as_ref().is_none_or(|b| score > b.0) {
                best = Some((score, p.file_name().unwrap().to_string_lossy().into_owned(), name.clone(), wf.clone(), job.clone()));
            }
        }
    }
    best.map(|(_, f, n, wf, job)| (f, n, wf, job))
}

fn pg_bin(major: Option<&str>, tool: &str) -> Option<PathBuf> {
    // PATH first: the developer's own choice wins over well-known install locations
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    for prefix in ["/opt/homebrew/opt", "/usr/local/opt"] {
        if let Some(m) = major {
            dirs.push(PathBuf::from(format!("{prefix}/postgresql@{m}/bin")));
        }
        if let Ok(rd) = std::fs::read_dir(prefix) {
            let mut v: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("postgresql"))).map(|p| p.join("bin")).collect();
            v.sort();
            dirs.extend(v.into_iter().rev());
        }
    }
    if let Some(m) = major {
        dirs.push(PathBuf::from(format!("/usr/lib/postgresql/{m}/bin")));
    }
    dirs.into_iter().map(|d| d.join(tool)).find(|p| p.is_file())
}

fn start_postgres(svc: &Value, ci: &mut CiEnv) -> Result<(u16, u16), String> {
    let image = svc["image"].as_str().unwrap_or("");
    let major = image.split(':').nth(1).and_then(|t| t.split(['.', '-']).next()).filter(|m| m.chars().all(|c| c.is_ascii_digit()));
    let (Some(initdb), Some(pg_ctl), Some(createdb)) = (pg_bin(major, "initdb"), pg_bin(major, "pg_ctl"), pg_bin(major, "createdb")) else {
        return Err(format!("{image}: no local Postgres binaries (install postgresql{}; Homebrew or apt)", major.map(|m| format!("@{m}")).unwrap_or_default()));
    };
    let ident = Regex::new(r"^[A-Za-z_][A-Za-z0-9_]{0,62}$").unwrap();
    let user = svc["env"]["POSTGRES_USER"].as_str().filter(|u| ident.is_match(u)).unwrap_or("postgres").to_string();
    let db = svc["env"]["POSTGRES_DB"].as_str().filter(|d| ident.is_match(d)).unwrap_or(&user).to_string();
    // the service's own password, which the job's DATABASE_URL already carries; TCP requires it
    let password = svc["env"]["POSTGRES_PASSWORD"].as_str().filter(|p| !p.contains("${{")).unwrap_or("postgres").to_string();
    let host_port = svc["ports"][0].as_str().and_then(|p| p.split(':').next()).and_then(|p| p.parse().ok()).unwrap_or(5432);
    let port = free_port();
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let dir = std::env::temp_dir().join(format!("lkc-ci-pg-{}-{nanos}", std::process::id()));
    let locale = [("LANG", "en_US.UTF-8"), ("LC_ALL", "en_US.UTF-8")];
    let ok = |c: &mut Command| c.envs(locale).stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success());
    let pwfile = std::env::temp_dir().join(format!("lkc-ci-pw-{}-{nanos}", std::process::id()));
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&pwfile);
        if f.and_then(|mut f| f.write_all(password.as_bytes())).is_err() {
            return Err("could not write the password file".into());
        }
    }
    let made = ok(Command::new(&initdb).arg("-D").arg(&dir).args(["-U", &user, "--auth-local=trust", "--auth-host=scram-sha-256"]).arg(format!("--pwfile={}", pwfile.display())));
    let _ = std::fs::remove_file(&pwfile);
    if !made {
        return Err("initdb failed".into());
    }
    ci.pg_dirs.push((pg_ctl.clone(), dir.clone()));
    // TCP only (-k ''): a socket path under a long temp dir exceeds the 103-byte limit on macOS
    let opts = format!("-p {port} -k '' -c listen_addresses=127.0.0.1 -c fsync=off");
    if !ok(Command::new(&pg_ctl).arg("-D").arg(&dir).args(["-o", &opts, "-l"]).arg(dir.join("log")).args(["-w", "start"])) {
        return Err(format!("pg_ctl start failed; see {}", dir.join("log").display()));
    }
    if db != user && !ok(Command::new(&createdb).env("PGPASSWORD", &password).args(["-h", "127.0.0.1", "-p", &port.to_string(), "-U", &user, "--", &db])) {
        return Err(format!("createdb {db} failed"));
    }
    ci.summary.push(format!("postgres {} on 127.0.0.1:{port} (CI's :{host_port}), db `{db}`", major.unwrap_or("?")));
    Ok((host_port, port))
}

fn start_redis(svc: &Value, ci: &mut CiEnv) -> Result<(u16, u16), String> {
    let bin = std::env::var_os("PATH").and_then(|p| std::env::split_paths(&p).map(|d| d.join("redis-server")).find(|f| f.is_file()));
    let Some(bin) = bin.or_else(|| Some(PathBuf::from("/opt/homebrew/bin/redis-server")).filter(|p| p.is_file())) else {
        return Err("redis: no local redis-server".into());
    };
    let host_port = svc["ports"][0].as_str().and_then(|p| p.split(':').next()).and_then(|p| p.parse().ok()).unwrap_or(6379);
    let port = free_port();
    let child = Command::new(bin).args(["--port", &port.to_string(), "--save", "", "--appendonly", "no"]).stdout(Stdio::null()).stderr(Stdio::null()).spawn().map_err(|e| e.to_string())?;
    ci.children.push(child); // before the check: torn down with the rest if it never listens
    let up = (0..30).any(|_| {
        std::thread::sleep(Duration::from_millis(100));
        std::net::TcpStream::connect(("127.0.0.1", port)).is_ok()
    });
    if !up {
        return Err(format!("redis-server did not listen on 127.0.0.1:{port}"));
    }
    ci.summary.push(format!("redis on 127.0.0.1:{port} (CI's :{host_port})"));
    Ok((host_port, port))
}

/// Workflow `run:` text is repository data: run only a single plain command that is either a script the
/// package manifest declares or a known migration tool, never arbitrary shell.
fn allowed_setup(root: &Path, wd: &str, cmd: &str) -> bool {
    let cmd = cmd.trim();
    if cmd.contains('\n') || cmd.chars().any(|c| ";|&`$<>(){}\\'\"*?!".contains(c)) {
        return false;
    }
    let w: Vec<&str> = cmd.split_whitespace().collect();
    let script = match w.as_slice() {
        ["npm" | "pnpm" | "bun" | "yarn", "run", s, ..] => Some(*s),
        ["yarn" | "pnpm", s, ..] if !matches!(*s, "add" | "dlx" | "exec") => Some(*s),
        _ => None,
    };
    if let Some(s) = script {
        let pkg = std::fs::read_to_string(root.join(wd).join("package.json")).unwrap_or_default();
        return serde_json::from_str::<Value>(&pkg).is_ok_and(|p| p["scripts"][s].is_string());
    }
    matches!(w.as_slice(), ["npx", "prisma", ..] | ["prisma", ..] | ["alembic", ..] | [_, "manage.py", "migrate", ..] | ["python" | "python3", "-m", "alembic", ..])
}

/// Replicate the CI test job. Ok(None) when the project has no workflow with a test job; Err when a service of the
/// job can't be started here, and then nothing runs.
pub fn prepare(root: &Path, map: &detect::ProjectMap) -> Result<Option<CiEnv>, String> {
    let Some((file, name, wf, job)) = pick_job(root) else { return Ok(None) };
    let mut ci = CiEnv { summary: vec![format!("ci: replicating `{name}` from .github/workflows/{file}")], pg_dirs: vec![], children: vec![] };
    let mut remap: Vec<(u16, u16)> = vec![];
    for (sname, svc) in job["services"].as_object().into_iter().flatten() {
        let image = svc["image"].as_str().unwrap_or(sname);
        let started = if image.starts_with("postgres") || image.contains("/postgres") {
            start_postgres(svc, &mut ci)
        } else if image.starts_with("redis") {
            start_redis(svc, &mut ci)
        } else {
            Err(format!("service `{sname}` ({image}) is not replicated"))
        };
        // without its own instance, the job's URLs still name the CI port on this machine: the developer's own
        // database, which setup steps (migrations, seeds) and tests would then write to. Services that did start
        // are torn down when `ci` drops.
        match started {
            Ok(m) => remap.push(m),
            Err(e) => return Err(format!("{e}: stopped before running anything, since the job's setup steps and tests would reach whatever already listens on its port here")),
        }
    }
    // workflow-level then job-level env; expressions (${{ secrets.X }}) can't be evaluated locally
    let mut env: Vec<(String, String)> = vec![];
    let (mut skipped, mut blocked) = (vec![], vec![]);
    for scope in [&wf["env"], &job["env"]] {
        for (k, v) in scope.as_object().into_iter().flatten() {
            let v = v.as_str().unwrap_or("").to_string();
            if v.contains("${{") {
                skipped.push(k.clone());
                continue;
            }
            if !replicable(k) {
                blocked.push(k.clone());
                continue;
            }
            // services moved to free ports: point URLs at them
            let v = remap.iter().fold(v, |v, (from, to)| v.replace(&format!("localhost:{from}"), &format!("127.0.0.1:{to}")).replace(&format!("127.0.0.1:{from}"), &format!("127.0.0.1:{to}")));
            env.retain(|(e, _)| e != k);
            env.push((k.clone(), v));
        }
    }
    ci.summary.push(format!("env: {} variable(s) from the job{}", env.len(), if skipped.is_empty() { String::new() } else { format!("; not set (CI expressions/secrets): {}", skipped.join(", ")) }));
    if !blocked.is_empty() {
        ci.summary.push(format!("⚠ not replicated (they change how programs run): {}", blocked.join(", ")));
    }
    *run::CI_ENV.lock().unwrap() = env;

    // database setup steps that the job runs before its tests (migrations, seeds, generators)
    let setup = Regex::new(r"(?i)\b(db|database|migrat\w*|prisma|seed|schema|alembic|manage\.py migrate)\b").unwrap();
    let install = Regex::new(r"^\s*(npm (ci|install)|pnpm i|yarn( install)?|pip install|poetry install|bun install)\b").unwrap();
    let test = Regex::new(r"\b(npm (run )?test|pytest|vitest|jest|cargo test|go test)\b").unwrap();
    let comp = map.components.first().cloned().unwrap_or_else(|| detect::Component { dir: ".".into(), ecosystem: "ci", manifest: String::new(), name: None, frameworks: vec![], commands: vec![] });
    for step in job["steps"].as_array().into_iter().flatten() {
        let Some(cmd) = step["run"].as_str() else { continue };
        if test.is_match(cmd) {
            break;
        }
        let what = format!("{} {cmd}", step["name"].as_str().unwrap_or(""));
        if !setup.is_match(&what) || install.is_match(cmd) {
            continue;
        }
        let wd = step["working-directory"].as_str().unwrap_or(".");
        let inside = !Path::new(wd).is_absolute() && std::fs::canonicalize(root.join(wd)).is_ok_and(|p| p.starts_with(root));
        if !inside || !allowed_setup(root, wd, cmd) {
            ci.summary.push(format!("⚠ setup step not run (only package scripts and migration tools run): `{}`", cmd.lines().next().unwrap_or("")));
            continue;
        }
        let ev = run::exec(root, &detect::Component { dir: wd.into(), ..comp.clone() }, &detect::Cmd { kind: "ci-setup", run: cmd.trim().to_string(), available: true }, Duration::from_secs(600));
        ci.summary.push(format!("{} setup `{}`{}", if ev.ok { "✓" } else { "✗" }, cmd.lines().next().unwrap_or(""), if ev.ok { String::new() } else { format!(": {}", ev.output_tail.lines().last().unwrap_or("")) }));
    }
    Ok(Some(ci))
}
