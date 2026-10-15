//! Behaviour probes: run the thing and observe it. Web apps are started with their own dev command (or served
//! by a built-in static server) and crawled over HTTP; Godot games are run headless for a few hundred frames.

use crate::detect::{Cmd, Component, ProjectMap};
use crate::diag::{self, Diagnostic};
use crate::run;
use regex::Regex;
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeSet, VecDeque};
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream, ToSocketAddrs};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const WEB: &[&str] = &["next", "vite", "nuxt", "svelte", "@sveltejs/kit", "express", "fastify", "hono", "astro", "react", "vue"];
const LOG_CAP: usize = 512 * 1024;

static URL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"https?://(?:localhost|127\.0\.0\.1|0\.0\.0\.0|\[::1?\]|\[::\]):(\d{2,5})").unwrap());
static LINK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)\b(href|src)\s*=\s*["']([^"'#?\s]+)"#).unwrap());
// server logs: crashes and request errors that test-runner patterns don't cover
static SERVER_ERR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*(?:FATAL\s*:?\s*(.+)|(?:[A-Z]\w*)?Error(?: \[\w+\])?: (.+)|Unhandled (?:Runtime )?(?:Error|Rejection):?\s*(.+))$").unwrap()
});
static CONSOLE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?:INFO|WARNING|ERROR):CONSOLE(?:\(\d+\)|:\d+)\] "(.+)", source: (\S*) \((\d+)\)"#).unwrap()
});
static ASSET: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\.(css|js|mjs|map|json|xml|txt|ico|png|jpe?g|gif|svg|webp|avif|woff2?|ttf|otf|mp3|mp4|webm|wav|ogg|pdf|zip)$").unwrap()
});

#[derive(Serialize)]
pub struct Page {
    pub path: String,
    pub status: Option<u16>,
    pub ms: u128,
    pub bytes: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Serialize)]
pub struct Probe {
    pub ok: bool,
    pub target: String,
    pub component: String,
    pub command: Option<String>,
    pub url: Option<String>,
    pub pages: Vec<Page>,
    /// dynamic route templates found in the file system, and those no crawled link reached
    pub templates: usize,
    pub unreached_templates: Vec<String>,
    pub diagnostics: Vec<Diagnostic>,
    /// pages were also loaded in headless Chromium (JS runtime errors checked)
    pub browser: bool,
    /// pages loaded in the browser, including client-side routes found in the rendered DOM
    pub browser_pages: Vec<String>,
    #[serde(skip)]
    js: Option<(Vec<Diagnostic>, Vec<String>, Option<String>)>,
    /// screenshot of the first canvas page (or the home page), for agents that can look at images
    pub screenshot: Option<String>,
    pub note: Option<String>,
    pub log_tail: String,
    pub saved_to: Option<String>,
}

impl Probe {
    fn new(target: &str, component: &str) -> Self {
        Probe {
            ok: false,
            target: target.into(),
            component: component.into(),
            command: None,
            url: None,
            pages: vec![],
            templates: 0,
            unreached_templates: vec![],
            diagnostics: vec![],
            browser: false,
            browser_pages: vec![],
            js: None,
            screenshot: None,
            note: None,
            log_tail: String::new(),
            saved_to: None,
        }
    }
}

fn is_web(c: &Component) -> bool {
    c.ecosystem == "node" && c.frameworks.iter().any(|f| WEB.contains(&f.as_str()))
}

fn static_root(dir: &Path) -> Option<PathBuf> {
    ["", "public", "dist", "www", "web"].iter().map(|d| dir.join(d)).find(|d| d.join("index.html").is_file())
}

pub fn probe(root: &Path, map: &ProjectMap, component: Option<&str>, max_pages: usize, timeout: Duration) -> Probe {
    let pick = |f: &dyn Fn(&Component) -> bool| {
        map.components.iter().filter(|c| component.is_none_or(|d| c.dir == d.trim_end_matches('/'))).find(|c| f(c))
    };
    let mut p = if let Some(c) = pick(&|c| c.ecosystem == "godot") {
        godot(root, c, timeout)
    } else if let Some(c) = pick(&|c| c.ecosystem == "xcode") {
        ios(root, c, timeout)
    } else if let Some(c) = pick(&|c| is_web(c) && c.cmd("run").is_some_and(|r| r.available)) {
        web_dev(root, c, max_pages, timeout)
    } else {
        let dir = component.map_or(root.to_path_buf(), |d| root.join(d));
        match static_root(&dir) {
            Some(s) => web_static(root, &s, max_pages),
            None => {
                let mut p = Probe::new("none", component.unwrap_or("."));
                p.note = Some(
                    "nothing to probe: no Godot project, no web component with a dev/start script, no index.html".into(),
                );
                p
            }
        }
    };
    p.saved_to = save(root, &p).ok();
    p
}

// ---------------------------------------------------------------- Godot

fn godot(root: &Path, c: &Component, timeout: Duration) -> Probe {
    let mut p = Probe::new("godot", &c.dir);
    // ~10 s of game time at 60 fps: main scene loads, _ready/_process run, script errors surface
    let cmd = Cmd { kind: "probe", run: "godot --headless --path . --quit-after 600".into(), available: true };
    let ev = run::exec(root, c, &cmd, timeout.min(Duration::from_secs(300)));
    p.command = Some(cmd.run);
    p.ok = !ev.timed_out && ev.exit_code.is_some() && !ev.diagnostics.iter().any(|d| d.severity == "error");
    if ev.timed_out {
        p.note = Some("game did not quit within the timeout".into());
    }
    p.diagnostics = ev.diagnostics;
    p.log_tail = ev.output_tail;
    p
}

// ---------------------------------------------------------------- iOS simulator

fn simctl(args: &[&str]) -> Option<String> {
    let o = Command::new("xcrun").arg("simctl").args(args).stdin(Stdio::null()).output().ok()?;
    o.status.success().then(|| String::from_utf8_lossy(&o.stdout).into_owned())
}

/// LOOKA_SIMULATOR (name or UDID), else a booted iPhone, else the first available iPhone (booted here).
fn simulator() -> Option<(String, String, bool)> {
    let list: Value = serde_json::from_str(&simctl(&["list", "devices", "available", "-j"])?).ok()?;
    let mut devices: Vec<(String, String, bool)> = list["devices"]
        .as_object()?
        .iter()
        .filter(|(runtime, _)| runtime.contains("iOS"))
        .flat_map(|(_, ds)| ds.as_array().into_iter().flatten())
        .filter_map(|d| Some((d["udid"].as_str()?.to_string(), d["name"].as_str()?.to_string(), d["state"] == "Booted")))
        .collect();
    if let Ok(want) = std::env::var("LOOKA_SIMULATOR") {
        devices.retain(|(u, n, _)| *u == want || *n == want);
    }
    devices.sort_by_key(|(_, n, booted)| (!booted, !n.starts_with("iPhone")));
    devices.into_iter().next()
}

/// Build for the simulator, install, launch, watch for a crash, screenshot.
fn ios(root: &Path, c: &Component, timeout: Duration) -> Probe {
    let mut p = Probe::new("ios", &c.dir);
    let proj = c.manifest.trim_end_matches("/project.pbxproj").rsplit('/').next().unwrap_or("App.xcodeproj").to_string();
    let scheme = c.name.clone().unwrap_or_else(|| proj.trim_end_matches(".xcodeproj").to_string());
    let dd = ".looka/ios-dd";
    let build = Cmd {
        kind: "probe",
        run: format!(
            "xcodebuild -project {} -scheme {} -configuration Debug -destination 'generic/platform=iOS Simulator' -derivedDataPath {dd} build CODE_SIGNING_ALLOWED=NO",
            sq(&proj),
            sq(&scheme)
        ),
        available: true,
    };
    p.command = Some(build.run.clone());
    let ev = run::exec(root, c, &build, timeout.min(Duration::from_secs(900)));
    p.diagnostics = ev.diagnostics.into_iter().filter(|d| d.severity == "error").collect();
    if !ev.ok {
        p.note = Some(format!("build failed (exit {:?})", ev.exit_code));
        p.log_tail = ev.output_tail;
        return p;
    }
    let products = root.join(&c.dir).join(dd).join("Build/Products/Debug-iphonesimulator");
    let Some(app) = fs::read_dir(&products).ok().into_iter().flatten().flatten().map(|e| e.path()).find(|p| p.extension().is_some_and(|e| e == "app")) else {
        p.note = Some("build succeeded but no .app was produced".into());
        return p;
    };
    let plist = app.join("Info.plist");
    let bundle = Command::new("plutil").args(["-extract", "CFBundleIdentifier", "raw", "-o", "-"]).arg(&plist).output().ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default();
    let exe = Command::new("plutil").args(["-extract", "CFBundleExecutable", "raw", "-o", "-"]).arg(&plist).output().ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default();
    let Some((udid, name, booted)) = simulator() else {
        p.note = Some("no iOS simulator available (install a runtime: xcodebuild -downloadPlatform iOS)".into());
        return p;
    };
    if !booted {
        simctl(&["boot", &udid]);
    }
    simctl(&["bootstatus", &udid, "-b"]);
    p.url = Some(format!("simulator {name} ({udid})"));
    if simctl(&["install", &udid, &app.display().to_string()]).is_none() {
        p.note = Some("simctl install failed".into());
        return p;
    }
    let evidence = root.join(".looka/evidence");
    let _ = fs::create_dir_all(&evidence);
    let ts = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let launched_at = SystemTime::now();
    // --console-pty: the app gets a terminal, so Swift `print` is line-buffered and reaches us while it runs
    // (with --stdout=<file> it is block-buffered and lost when the app is terminated)
    let console = Command::new("xcrun")
        .args(["simctl", "launch", "--console-pty", "--terminate-running-process", &udid, &bundle])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn();
    let Ok(mut console) = console else {
        p.note = Some(format!("could not launch {bundle}"));
        return p;
    };
    let log_buf = Arc::new(Mutex::new(String::new()));
    for r in [Box::new(console.stdout.take().unwrap()) as Box<dyn Read + Send>, Box::new(console.stderr.take().unwrap())] {
        let buf = log_buf.clone();
        thread::spawn(move || {
            let (mut r, mut b) = (r, [0u8; 8192]);
            while let Ok(n @ 1..) = r.read(&mut b) {
                let mut g = buf.lock().unwrap();
                if g.len() < LOG_CAP {
                    g.push_str(&String::from_utf8_lossy(&b[..n]));
                }
            }
        });
    }
    // let it run long enough to finish launching and render the first screens
    thread::sleep(Duration::from_secs(6));
    let running = simctl(&["spawn", &udid, "launchctl", "list"]).is_some_and(|l| l.contains(&format!("UIKitApplication:{bundle}")));
    let shot = evidence.join(format!("shot-ios-{ts}.png"));
    if simctl(&["io", &udid, "screenshot", &shot.display().to_string()]).is_some() {
        p.screenshot = Some(crate::detect::rel(root, &shot));
        if running {
            if let Some(u) = fs::read(&shot).ok().and_then(|b| png_uniformity(&b)).filter(|u| *u >= 0.999) {
                p.diagnostics.push(diag_err("blank-render", None, format!("app is running but the screen is one flat colour ({:.1}%)", u * 100.0)));
            }
        }
    }
    simctl(&["terminate", &udid, &bundle]);
    thread::sleep(Duration::from_millis(300));
    unsafe { libc::killpg(console.id() as i32, libc::SIGKILL) };
    let _ = console.wait();
    let log = diag::strip_ansi(&log_buf.lock().unwrap());
    let _ = fs::write(evidence.join(format!("ios-{ts}.log")), &log);
    p.log_tail = run::tail(&log, 3000);
    let from_log: Vec<Diagnostic> = diag::parse(&log).into_iter().filter(|d| d.severity == "error").collect();
    if !running {
        // the reason: a crash report if macOS wrote one, else the runtime's own last words in the app log
        let crash = crash_report(&exe, launched_at);
        let reason = from_log.iter().find(|d| d.code.as_deref() == Some("swift-fatal") || d.code.as_deref().is_some_and(|c| c.ends_with("Exception")));
        let mut msg = match (&crash, reason) {
            (Some((_, m, _)), _) => m.clone(),
            (None, Some(r)) => format!("crashed: {}", r.message),
            (None, None) => "app is not running 6 s after launch (exited or crashed)".to_string(),
        };
        if let (Some(_), Some(r)) = (&crash, reason) {
            msg += &format!(" ({})", r.message);
        }
        // point at the project's own source file for the crashing frame, when the report has it
        let (file, line) = match crash.as_ref().and_then(|(_, _, loc)| loc.clone()) {
            Some((src, line)) => {
                let found = crate::detect::walk(&root.join(&c.dir)).into_iter().find(|f| f.file_name().and_then(|n| n.to_str()) == Some(src.rsplit('/').next().unwrap_or(&src)));
                (Some(found.map_or(src, |f| crate::detect::rel(root, &f))), Some(line))
            }
            None => (crash.map(|(f, _, _)| f), None),
        };
        let mut d = diag_err("crash", file, msg);
        d.line = line;
        p.diagnostics.push(d);
    }
    p.diagnostics.extend(from_log);
    p.ok = running && !p.diagnostics.iter().any(|d| d.severity == "error");
    p
}

fn diag_err(code: &str, file: Option<String>, message: String) -> Diagnostic {
    Diagnostic { severity: "error".into(), file, line: None, col: None, code: Some(code.into()), message, hint: None, preexisting: false, flaky: false }
}

/// The newest macOS crash report for `exe` written after `since`: (report file, exception summary, and the
/// first frame in the app's own binary with its source location when symbolicated).
type CrashReport = (String, String, Option<(String, usize)>);

fn crash_report(exe: &str, since: SystemTime) -> Option<CrashReport> {
    let dir = PathBuf::from(std::env::var("HOME").ok()?).join("Library/Logs/DiagnosticReports");
    thread::sleep(Duration::from_secs(2)); // ReportCrash writes asynchronously
    let report = fs::read_dir(&dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with(&format!("{exe}-")) && n.ends_with(".ips")))
        .filter(|p| fs::metadata(p).and_then(|m| m.modified()).is_ok_and(|m| m >= since))
        .max_by_key(|p| fs::metadata(p).and_then(|m| m.modified()).ok())?;
    let text = fs::read_to_string(&report).ok()?;
    // .ips = one JSON header line, then the JSON body
    let body: Value = text.split_once('\n').and_then(|(_, b)| serde_json::from_str(b).ok())?;
    let ex = &body["exception"];
    let mut msg = format!("crashed: {} {}", ex["type"].as_str().unwrap_or("?"), ex["signal"].as_str().unwrap_or(""));
    if let Some(reason) = body["asi"].as_object().and_then(|a| a.values().next()).and_then(|v| v.get(0)).and_then(Value::as_str) {
        msg += &format!(" — {}", reason.trim());
    }
    let frames = body["threads"].as_array().and_then(|ts| ts.iter().find(|t| t["triggered"] == true)).and_then(|t| t["frames"].as_array());
    // debug builds put app code in `<App>.debug.dylib`: the first frame from the app's own image is the useful one
    let image = |f: &Value| f["imageIndex"].as_u64().and_then(|i| body["usedImages"].get(i as usize)).and_then(|im| im["name"].as_str()).unwrap_or("");
    let own = frames.and_then(|fs| fs.iter().find(|f| image(f) == exe || image(f).starts_with(&format!("{exe}."))));
    let top = frames.and_then(|fs| fs.iter().find(|f| f["symbol"].is_string()));
    if let Some(f) = own.or(top) {
        msg += &format!(" in {}", f["symbol"].as_str().unwrap_or("?"));
    }
    let loc = own.and_then(|f| Some((f["sourceFile"].as_str()?.to_string(), f["sourceLine"].as_u64()? as usize)));
    Some((report.display().to_string(), msg, loc))
}

fn sq(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

// ---------------------------------------------------------------- web

struct Server {
    pgid: Option<i32>,
    stop: Arc<AtomicBool>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(g) = self.pgid {
            unsafe { libc::killpg(g, libc::SIGKILL) };
        }
    }
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").and_then(|l| l.local_addr()).map_or(0, |a| a.port())
}

fn web_dev(root: &Path, c: &Component, max_pages: usize, timeout: Duration) -> Probe {
    let mut p = Probe::new("web", &c.dir);
    let run_cmd = c.cmd("run").map(|r| r.run.clone()).unwrap_or_default();
    p.command = Some(run_cmd.clone());
    let port = free_port();
    let log = Arc::new(Mutex::new(String::new()));
    let spawned = Command::new("sh")
        .arg("-c")
        .arg(&run_cmd)
        .current_dir(root.join(&c.dir))
        .env_clear()
        .envs(run::safe_env())
        .env("PORT", port.to_string())
        .env("BROWSER", "none")
        .env("NODE_ENV", "development")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn();
    let mut child = match spawned {
        Ok(ch) => ch,
        Err(e) => {
            p.note = Some(format!("could not start `{run_cmd}`: {e}"));
            return p;
        }
    };
    let server = Server { pgid: Some(child.id() as i32), stop: Arc::new(AtomicBool::new(false)) };
    for r in [Box::new(child.stdout.take().unwrap()) as Box<dyn Read + Send>, Box::new(child.stderr.take().unwrap())] {
        let log = log.clone();
        thread::spawn(move || {
            let (mut r, mut buf) = (r, [0u8; 8192]);
            while let Ok(n @ 1..) = r.read(&mut buf) {
                let mut l = log.lock().unwrap();
                l.push_str(&String::from_utf8_lossy(&buf[..n]));
                if l.len() > 2 * LOG_CAP {
                    let cut = l.len() - LOG_CAP;
                    let cut = (cut..l.len()).find(|&i| l.is_char_boundary(i)).unwrap_or(cut);
                    l.drain(..cut);
                }
            }
        });
    }

    // the server tells us where it listens; fall back to the PORT we asked for
    let start = Instant::now();
    let boot = timeout.min(Duration::from_secs(180));
    let host = loop {
        if let Some(m) = URL.captures(&log.lock().unwrap()) {
            break Some(format!("localhost:{}", &m[1]));
        }
        if TcpStream::connect_timeout(&([127, 0, 0, 1], port).into(), Duration::from_millis(200)).is_ok() {
            break Some(format!("localhost:{port}"));
        }
        if let Ok(Some(st)) = child.try_wait() {
            p.note = Some(format!("dev server exited before listening ({st})"));
            break None;
        }
        if start.elapsed() > boot {
            p.note = Some(format!("dev server did not listen within {}s", boot.as_secs()));
            break None;
        }
        thread::sleep(Duration::from_millis(250));
    };
    if let Some(h) = &host {
        p.url = Some(format!("http://{h}"));
        let (seeds, templates) = routes(&root.join(&c.dir));
        p.templates = templates.len();
        (p.pages, p.unreached_templates) = crawl(h, seeds, &templates, max_pages, Duration::from_secs(90));
        p.js = browser_pass(root, &format!("http://{h}"), &p.pages);
        thread::sleep(Duration::from_millis(300)); // let the server flush errors for the last request
    }
    drop(server);
    let _ = child.wait();
    let text = log.lock().unwrap().clone();
    finish(&mut p, &text, host.is_some());
    p
}

fn web_static(root: &Path, dir: &Path, max_pages: usize) -> Probe {
    let mut p = Probe::new("web-static", &crate::detect::rel(root, dir));
    let listener = match TcpListener::bind("127.0.0.1:0") {
        Ok(l) => l,
        Err(e) => {
            p.note = Some(format!("static server: {e}"));
            return p;
        }
    };
    let port = listener.local_addr().map_or(0, |a| a.port());
    let server = Server { pgid: None, stop: Arc::new(AtomicBool::new(false)) };
    let (stop, base) = (server.stop.clone(), fs::canonicalize(dir).unwrap_or(dir.to_path_buf()));
    let _ = listener.set_nonblocking(true);
    thread::spawn(move || {
        while !stop.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((s, _)) => {
                    let base = base.clone();
                    thread::spawn(move || serve_file(s, &base));
                }
                Err(_) => thread::sleep(Duration::from_millis(10)),
            }
        }
    });
    let host = format!("127.0.0.1:{port}");
    p.command = Some(format!("built-in static server on {}", crate::detect::rel(root, dir)));
    p.url = Some(format!("http://{host}"));
    let mut seeds = vec!["/".to_string()];
    for f in crate::detect::walk(dir) {
        if f.extension().is_some_and(|e| e == "html") {
            seeds.push(format!("/{}", crate::detect::rel(dir, &f)));
        }
    }
    (p.pages, _) = crawl(&host, seeds, &[], max_pages, Duration::from_secs(20));
    p.js = browser_pass(root, &format!("http://{host}"), &p.pages);
    drop(server);
    finish(&mut p, "", true);
    p
}

/// A Chromium-family browser for JS runtime errors: LOOKA_BROWSER, then the usual install locations.
fn browser() -> Option<PathBuf> {
    if let Ok(b) = std::env::var("LOOKA_BROWSER") {
        return Some(PathBuf::from(b)).filter(|p| p.is_file());
    }
    let mac = [
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
        "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser",
        "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
    ];
    mac.iter().map(PathBuf::from).find(|p| p.is_file()).or_else(|| {
        ["google-chrome", "chromium", "chromium-browser", "brave-browser"].iter().find_map(|b| {
            std::env::var_os("PATH").and_then(|p| std::env::split_paths(&p).map(|d| d.join(b)).find(|f| f.is_file()))
        })
    })
}

/// Load each page in headless Chromium and collect uncaught exceptions and failed resource loads.
/// Also follows links found in the *rendered* DOM, so client-side routes of SPAs get visited.
fn js_errors(bin: &Path, base: &str, paths: &[String], max: usize) -> (Vec<Diagnostic>, Vec<String>, Vec<String>) {
    let mut canvas = vec![];
    let mut out: Vec<Diagnostic> = vec![];
    let mut queue: VecDeque<String> = paths.iter().cloned().collect();
    let mut seen: BTreeSet<String> = queue.iter().cloned().collect();
    let mut visited = vec![];
    while let Some(path) = queue.pop_front() {
        if visited.len() >= max {
            break;
        }
        visited.push(path.clone());
        let path = &path;
        let profile = std::env::temp_dir().join(format!("looka-chrome-{}-{}", std::process::id(), out.len() + path.len()));
        let child = Command::new(bin)
            // software WebGL (SwiftShader): headless has no GPU, and 3D games must still get a context
            .args(["--headless=new", "--use-angle=swiftshader", "--enable-unsafe-swiftshader", "--no-first-run", "--no-default-browser-check", "--use-mock-keychain"])
            .args(["--password-store=basic", "--disable-extensions", "--enable-logging=stderr", "--v=0", "--virtual-time-budget=4000", "--dump-dom"])
            .arg(format!("--user-data-dir={}", profile.display()))
            .arg(format!("{base}{path}"))
            .env_clear()
            .envs(run::safe_env())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn();
        let Ok(mut child) = child else { break };
        let pgid = child.id() as i32;
        let (dom, log) = (Arc::new(Mutex::new(String::new())), Arc::new(Mutex::new(String::new())));
        for (r, buf) in [(Box::new(child.stdout.take().unwrap()) as Box<dyn Read + Send>, dom.clone()), (Box::new(child.stderr.take().unwrap()), log.clone())] {
            thread::spawn(move || {
                let (mut r, mut b) = (r, [0u8; 8192]);
                while let Ok(n @ 1..) = r.read(&mut b) {
                    let mut g = buf.lock().unwrap();
                    if g.len() < LOG_CAP {
                        g.push_str(&String::from_utf8_lossy(&b[..n]));
                    }
                }
            });
        }
        // chrome prints the DOM after the virtual time budget but may not exit: stop it once the DOM is out
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(25) && !dom.lock().unwrap().contains("</html>") {
            if matches!(child.try_wait(), Ok(Some(_))) {
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }
        thread::sleep(Duration::from_millis(300));
        unsafe { libc::killpg(pgid, libc::SIGKILL) };
        let _ = child.wait();
        let _ = fs::remove_dir_all(&profile);
        if dom.lock().unwrap().contains("<canvas") {
            canvas.push(path.clone());
        }
        let dom = dom.lock().unwrap();
        let page_base = base_of(path, &dom);
        for m in LINK.captures_iter(&dom) {
            if &m[1] == "href" {
                if let Some(link) = resolve(&page_base, &m[2]).filter(|l| !ASSET.is_match(l) && !l.starts_with("/_next")) {
                    if seen.insert(link.clone()) {
                        queue.push_back(link);
                    }
                }
            }
        }
        for c in CONSOLE.captures_iter(&log.lock().unwrap()) {
            let msg = &c[1];
            let failed_load = msg.starts_with("Failed to load resource");
            if !(msg.starts_with("Uncaught") || failed_load || msg.contains("Error:")) {
                continue;
            }
            let src = c[2].strip_prefix(base).unwrap_or(&c[2]).to_string();
            let d = Diagnostic {
                severity: "error".into(),
                file: Some(if src.is_empty() { path.clone() } else { src }),
                line: c[3].parse().ok().filter(|l| *l > 0),
                col: None,
                code: Some(format!("js@{path}")),
                message: msg.to_string(),
                hint: None,
                preexisting: false,
            flaky: false,
            };
            if !out.iter().any(|o| o.file == d.file && o.message == d.message) {
                out.push(d);
            }
        }
    }
    (out, visited, canvas)
}

/// Pages worth loading in a browser: HTML answers (not assets, not failures), at most `n`.
fn html_pages(pages: &[Page], n: usize) -> Vec<String> {
    pages.iter().filter(|p| p.status == Some(200) && !ASSET.is_match(&p.path) && !p.path.ends_with(".webmanifest")).take(n).map(|p| p.path.clone()).collect()
}

/// JS errors on up to 12 pages, then a screenshot of the first page with a <canvas> (else the first page):
/// a canvas page whose whole frame is one flat colour is a blank render (lost WebGL context, empty scene, bad camera).
fn browser_pass(root: &Path, base: &str, pages: &[Page]) -> Option<(Vec<Diagnostic>, Vec<String>, Option<String>)> {
    if std::env::var("LOOKA_NO_BROWSER").is_ok() {
        return None;
    }
    let bin = browser()?;
    let (mut diags, visited, canvas) = js_errors(&bin, base, &html_pages(pages, 8), 12);
    source_map(base, &mut diags);
    let Some(target) = canvas.first().or(visited.first()) else { return Some((diags, visited, None)) };
    let dir = root.join(".looka/evidence");
    let _ = fs::create_dir_all(&dir);
    let ts = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let shot = dir.join(format!("shot-{ts}.png"));
    // a failed screenshot must not throw away the JS errors already collected
    let Some(png) = screenshot(&bin, &format!("{base}{target}"), &shot) else { return Some((diags, visited, None)) };
    if !canvas.is_empty() {
        // ponytail: whole-frame uniformity, since the canvas rect isn't known; a black canvas under an HTML HUD
        // is missed. Upgrade: read the canvas bounding box over the DevTools protocol.
        if let Some(u) = png_uniformity(&png).filter(|u| *u >= 0.999) {
            diags.push(Diagnostic {
                severity: "error".into(),
                file: Some(target.clone()),
                line: None,
                col: None,
                code: Some("blank-render".into()),
                message: format!("canvas rendered a blank frame ({:.1}% of pixels are one colour)", u * 100.0),
                hint: Some("WebGL context failed, the scene or camera is empty, or the render loop never ran; check JS errors and asset loads".into()),
                preexisting: false,
            flaky: false,
            });
        }
    }
    Some((diags, visited, Some(crate::detect::rel(root, &shot))))
}

fn finish(p: &mut Probe, log: &str, listened: bool) {
    p.diagnostics = diag::parse(log);
    match p.js.take() {
        Some((js, visited, shot)) => {
            p.browser = true;
            p.diagnostics.extend(js);
            p.browser_pages = visited;
            if shot.is_none() && p.note.is_none() {
                p.note = Some("screenshot not captured (Chromium timed out); blank-render check skipped".into());
            }
            p.screenshot = shot;
        }
        None if listened && p.note.is_none() => {
            p.note = Some("JS runtime errors not checked: no headless Chromium found (set LOOKA_BROWSER)".into())
        }
        None => {}
    }
    let clean = diag::strip_ansi(log);
    for l in clean.lines() {
        if let Some(c) = SERVER_ERR.captures(l) {
            let msg = (1..=3).find_map(|i| c.get(i)).map_or("", |m| m.as_str()).trim().to_string();
            if !msg.is_empty() && !p.diagnostics.iter().any(|d| d.message.contains(&msg) || msg.contains(&d.message)) {
                p.diagnostics.push(Diagnostic {
                    severity: "error".into(), file: None, line: None, col: None, code: None,
                    message: msg, hint: None, preexisting: false,
                flaky: false,
                });
            }
        }
    }
    p.log_tail = run::tail(log, 3000);
    let bad_pages = p.pages.iter().filter(|g| g.error.is_some()).count();
    p.ok = listened && !p.pages.is_empty() && bad_pages == 0 && !p.diagnostics.iter().any(|d| d.severity == "error");
}

fn serve_file(mut s: TcpStream, base: &Path) {
    // accepted sockets inherit O_NONBLOCK from the listener on BSD/macOS: a read could return before the request
    let _ = s.set_nonblocking(false);
    let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
    let mut buf = [0u8; 8192];
    let n = s.read(&mut buf).unwrap_or(0);
    let req = String::from_utf8_lossy(&buf[..n]);
    let raw = req.split_whitespace().nth(1).unwrap_or("/").split(['?', '#']).next().unwrap_or("/");
    let path = percent_decode(raw);
    let mut f = base.join(path.trim_start_matches('/'));
    if f.is_dir() {
        f = f.join("index.html");
    }
    // never serve outside the served directory
    let ok = fs::canonicalize(&f).ok().filter(|c| c.starts_with(base) && c.is_file());
    let (status, body, ctype) = match ok.and_then(|c| fs::read(&c).ok().map(|b| (b, c))) {
        Some((b, c)) => ("200 OK", b, mime(&c)),
        None => ("404 Not Found", b"not found".to_vec(), "text/plain"),
    };
    let head = format!("HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
    let _ = s.write_all(head.as_bytes()).and_then(|_| s.write_all(&body));
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let (mut out, mut i) = (Vec::with_capacity(b.len()), 0);
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Some(v) = std::str::from_utf8(&b[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn mime(p: &Path) -> &'static str {
    match p.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript",
        "css" => "text/css",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "wasm" => "application/wasm",
        "mp3" => "audio/mpeg",
        "ogg" => "audio/ogg",
        "wav" => "audio/wav",
        _ => "application/octet-stream",
    }
}

/// Routes from file-system routing (Next app/ and pages/ routers): static paths to seed the crawl, and
/// dynamic templates (`/foods/[slug]`) the crawler should hit at least once through a discovered link.
fn routes(dir: &Path) -> (Vec<String>, Vec<(String, Regex)>) {
    let (mut out, mut dynamic) = (vec!["/".to_string()], vec![]);
    for base in ["app", "src/app", "pages", "src/pages"] {
        let d = dir.join(base);
        if !d.is_dir() {
            continue;
        }
        let app = base.ends_with("app");
        for f in crate::detect::walk(&d) {
            let rel = crate::detect::rel(&d, &f);
            let (stem, ext) = rel.rsplit_once('.').unwrap_or((&rel, ""));
            if !matches!(ext, "tsx" | "ts" | "jsx" | "js" | "mdx") {
                continue;
            }
            if app && !(stem == "page" || stem.ends_with("/page")) {
                continue;
            }
            let segs: Vec<&str> = stem
                .split('/')
                .filter(|s| !(app && (s.starts_with('(') || *s == "page")))
                .filter(|s| app || *s != "index")
                .collect();
            let special = |s: &&str| matches!(*s, "404" | "500" | "_error") || s.contains(".test") || s.contains(".spec");
            if segs.iter().any(|s| s.starts_with('_') || s.starts_with('@') || *s == "api" || special(s) || *s == "__tests__") {
                continue;
            }
            let route = format!("/{}", segs.join("/"));
            if segs.iter().any(|s| s.starts_with('[')) {
                let pat: Vec<String> = segs
                    .iter()
                    .map(|s| match *s {
                        s if s.starts_with("[[...") => ".*".to_string(),
                        s if s.starts_with("[...") => ".+".to_string(),
                        s if s.starts_with('[') => "[^/]+".to_string(),
                        s => regex::escape(s),
                    })
                    .collect();
                if let Ok(re) = Regex::new(&format!("^/{}/?$", pat.join("/"))) {
                    dynamic.push((route, re));
                }
            } else {
                out.push(route);
            }
        }
    }
    out.sort();
    out.dedup();
    dynamic.sort_by(|a, b| a.0.cmp(&b.0));
    dynamic.dedup_by(|a, b| a.0 == b.0);
    (out, dynamic)
}

fn get(host: &str, path: &str, timeout: Duration) -> Result<(u16, String, Option<String>), String> {
    let addrs: Vec<_> = host.to_socket_addrs().map_err(|e| e.to_string())?.collect();
    let mut s = addrs
        .iter()
        .find_map(|a| TcpStream::connect_timeout(a, Duration::from_secs(5)).ok())
        .ok_or_else(|| "connection refused".to_string())?;
    s.set_read_timeout(Some(timeout)).map_err(|e| e.to_string())?;
    let req = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: looka-probe\r\nAccept: text/html,*/*\r\nConnection: close\r\n\r\n"
    );
    s.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
    // bounded: 8 MB and one overall deadline, so a large asset or a never-ending stream can't stall the crawl
    let (mut raw, mut buf, start) = (Vec::new(), [0u8; 16384], Instant::now());
    loop {
        match s.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => raw.extend_from_slice(&buf[..n]),
            Err(e) if raw.is_empty() => return Err(format!("read: {e}")),
            Err(_) => break,
        }
        if raw.len() >= 8 << 20 || start.elapsed() > timeout {
            break;
        }
    }
    let text = String::from_utf8_lossy(&raw).into_owned();
    let status = text.split_whitespace().nth(1).and_then(|c| c.parse().ok()).ok_or("not an HTTP response")?;
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    let location = head
        .lines()
        .find_map(|l| l.split_once(':').filter(|(k, _)| k.eq_ignore_ascii_case("location")).map(|(_, v)| v.trim().to_string()));
    Ok((status, body.to_string(), location))
}

fn crawl(host: &str, seeds: Vec<String>, templates: &[(String, Regex)], max_pages: usize, per_page: Duration) -> (Vec<Page>, Vec<String>) {
    // "/" first; its links (dynamic templates first) are crawled before the remaining file-system routes
    let (home, rest): (Vec<String>, Vec<String>) = seeds.into_iter().partition(|s| s == "/");
    let mut queue: VecDeque<(String, bool)> = home.into_iter().map(|s| (s, true)).collect();
    let mut later: VecDeque<String> = rest.into();
    let mut sampled: Vec<bool> = vec![false; templates.len()];
    let mut seen = BTreeSet::new();
    let mut pages = vec![];
    let mut assets: BTreeSet<String> = BTreeSet::new();
    let mut refused = 0;
    while let Some((path, seeded)) = queue.pop_front().or_else(|| later.pop_front().map(|s| (s, true))) {
        if pages.len() >= max_pages || !seen.insert(path.clone()) {
            continue;
        }
        if refused >= 3 {
            pages.push(Page { path: "(crawl stopped)".into(), status: None, ms: 0, bytes: 0, error: Some("server stopped answering; see diagnostics".into()) });
            break;
        }
        let t = Instant::now();
        let mut page = Page { path: path.clone(), status: None, ms: 0, bytes: 0, error: None };
        match get(host, &path, per_page) {
            Ok((status, body, location)) => {
                page.status = Some(status);
                page.bytes = body.len();
                page.error = match status {
                    500.. => Some(format!("server error {status}")),
                    404 if !seeded => Some("broken link (404)".into()),
                    404 => Some("route from the file system answered 404".into()),
                    _ if body.contains("Application error: a client-side exception") => Some("client-side exception page".into()),
                    _ => None,
                };
                if let Some(loc) = location.filter(|l| l.starts_with('/')) {
                    queue.push_back((loc, false));
                }
                let base = base_of(&path, &body);
                for m in LINK.captures_iter(&body) {
                    let Some(link) = resolve(&base, &m[2]) else { continue };
                    if ASSET.is_match(&link) || &m[1] == "src" {
                        assets.insert(link);
                    } else if !link.starts_with("/_next") {
                        // first link into a not-yet-seen dynamic template jumps the queue
                        match templates.iter().position(|(_, re)| re.is_match(&link)) {
                            Some(i) if !sampled[i] => {
                                sampled[i] = true;
                                queue.push_front((link, false));
                            }
                            _ => queue.push_back((link, false)),
                        }
                    }
                }
            }
            Err(e) => {
                refused += 1;
                page.error = Some(e);
            }
        }
        if page.status.is_some() {
            refused = 0;
        }
        page.ms = t.elapsed().as_millis();
        pages.push(page);
    }
    // scripts, styles, audio, images the pages reference: a missing one breaks a game or a layout
    for a in assets.into_iter().filter(|a| !a.starts_with("/_next")).take(300) {
        let t = Instant::now();
        let (status, error) = match get(host, &a, Duration::from_secs(20)) {
            Ok((st, _, _)) if st >= 400 => (Some(st), Some(format!("missing asset ({st})"))),
            Ok((st, _, _)) => (Some(st), None),
            Err(e) => (None, Some(e)),
        };
        // only failures are listed for assets, to keep the report small
        if error.is_some() {
            pages.push(Page { path: a, status, ms: t.elapsed().as_millis(), bytes: 0, error });
        }
    }
    let unreached = templates.iter().zip(&sampled).filter(|(_, s)| !**s).map(|(t, _)| t.0.clone()).collect();
    (pages, unreached)
}

/// The path relative links on `page` resolve against: its `<base href>` when it has one, else the page itself.
fn base_of(page: &str, html: &str) -> String {
    static BASE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"(?i)<base\s[^>]*href\s*=\s*["']([^"']*)["']"#).unwrap());
    let dir = |b: String| if b.ends_with('/') { b } else { b.rsplit_once('/').map_or("/".into(), |(d, _)| format!("{d}/")) };
    BASE.captures(html).and_then(|m| resolve(page, &m[1])).map(dir).unwrap_or_else(|| page.to_string())
}

/// Same-origin path for a link found on `page`, or None for external/data/js links.
fn resolve(page: &str, link: &str) -> Option<String> {
    let l = link.trim();
    if l.is_empty() || l.starts_with("//") || l.contains(':') {
        return None; // http:, https:, data:, mailto:, javascript:
    }
    let joined = if l.starts_with('/') {
        l.to_string()
    } else {
        let dir = if page.ends_with('/') { page.to_string() } else { page.rsplit_once('/').map_or("/".into(), |(d, _)| format!("{d}/")) };
        format!("{dir}{l}")
    };
    let mut segs: Vec<&str> = vec![];
    for s in joined.split('/') {
        match s {
            "" | "." => {}
            ".." => {
                segs.pop();
            }
            s => segs.push(s),
        }
    }
    let trailing = joined.ends_with('/') && !segs.is_empty();
    Some(format!("/{}{}", segs.join("/"), if trailing { "/" } else { "" }))
}

fn save(root: &Path, p: &Probe) -> std::io::Result<String> {
    let dir = root.join(".looka/evidence");
    fs::create_dir_all(&dir)?;
    let gi = root.join(".looka/.gitignore");
    if !gi.exists() {
        fs::write(gi, "*\n")?;
    }
    let ts = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let name = format!(".looka/evidence/probe-{ts}.json");
    fs::write(root.join(&name), serde_json::to_string_pretty(p)?)?;
    Ok(name)
}

pub fn render(p: &Probe) -> String {
    let mut s = format!("# probe ({}): {}\n", p.target, if p.ok { "PASS" } else { "FAIL" });
    if let Some(c) = &p.command {
        s += &format!("command: `{c}` in ({})\n", p.component);
    }
    if let Some(u) = &p.url {
        s += &format!("url: {u}\n");
    }
    if let Some(n) = &p.note {
        s += &format!("note: {n}\n");
    }
    let bad: Vec<&Page> = p.pages.iter().filter(|g| g.error.is_some()).collect();
    if !p.pages.is_empty() {
        let js = if p.browser { format!(" · {} loaded in headless Chromium for JS errors", p.browser_pages.len()) } else { String::new() };
        s += &format!("pages: {} checked, {} failing{js}\n", p.pages.len(), bad.len());
    }
    if p.templates > 0 {
        s += &format!("dynamic templates: {} reached of {}\n", p.templates - p.unreached_templates.len(), p.templates);
        if !p.unreached_templates.is_empty() {
            s += &format!("  not reached (no link found): {}\n", p.unreached_templates.join(", "));
        }
    }
    for g in &bad {
        s += &format!("  ✗ {} {} — {} ({} ms)\n", g.status.map_or("---".into(), |c| c.to_string()), g.path, g.error.as_deref().unwrap_or(""), g.ms);
    }
    for g in p.pages.iter().filter(|g| g.error.is_none()).take(15) {
        s += &format!("  ✓ {} {} ({} ms)\n", g.status.unwrap_or(0), g.path, g.ms);
    }
    if !p.diagnostics.is_empty() {
        s += &diag::render(&p.diagnostics.iter().take(20).cloned().collect::<Vec<_>>());
    }
    if !p.ok && p.diagnostics.is_empty() && bad.is_empty() && !p.log_tail.is_empty() {
        let lines: Vec<&str> = p.log_tail.lines().collect();
        s += &lines[lines.len().saturating_sub(15)..].iter().map(|l| format!("    {l}\n")).collect::<String>();
    }
    if let Some(shot) = &p.screenshot {
        s += &format!("screenshot: {shot}\n");
    }
    if let Some(e) = &p.saved_to {
        s += &format!("evidence: {e}\n");
    }
    s
}


/// Fraction of pixels sharing the most common colour in an 8-bit RGB/RGBA PNG (1.0 = completely uniform).
fn png_uniformity(png: &[u8]) -> Option<f64> {
    let (w, h) = crate::assets::image_size(png)?;
    let (depth, ctype) = (*png.get(24)?, *png.get(25)?);
    let bpp = match (depth, ctype) {
        (8, 2) => 3,
        (8, 6) => 4,
        _ => return None,
    };
    let (mut i, mut idat) = (8usize, Vec::new());
    while i + 8 <= png.len() {
        let len = u32::from_be_bytes(png[i..i + 4].try_into().ok()?) as usize;
        if &png[i + 4..i + 8] == b"IDAT" {
            idat.extend_from_slice(png.get(i + 8..i + 8 + len)?);
        }
        i += 12 + len;
    }
    let raw = miniz_oxide::inflate::decompress_to_vec_zlib(&idat).ok()?;
    let stride = w as usize * bpp;
    let (mut prev, mut cur) = (vec![0u8; stride], vec![0u8; stride]);
    let mut counts: std::collections::HashMap<[u8; 3], u32> = std::collections::HashMap::new();
    for row in 0..h as usize {
        let line = raw.get(row * (stride + 1)..(row + 1) * (stride + 1))?;
        let (f, data) = (line[0], &line[1..]);
        for x in 0..stride {
            let a = if x >= bpp { cur[x - bpp] } else { 0 };
            let (b, c) = (prev[x], if x >= bpp { prev[x - bpp] } else { 0 });
            cur[x] = data[x].wrapping_add(match f {
                1 => a,
                2 => b,
                3 => ((a as u16 + b as u16) / 2) as u8,
                4 => {
                    let p = a as i16 + b as i16 - c as i16;
                    let (pa, pb, pc) = ((p - a as i16).abs(), (p - b as i16).abs(), (p - c as i16).abs());
                    if pa <= pb && pa <= pc { a } else if pb <= pc { b } else { c }
                }
                _ => 0,
            });
        }
        // sample every 4th pixel: plenty to tell a rendered scene from a flat frame
        for px in (0..w as usize).step_by(4) {
            let o = px * bpp;
            *counts.entry([cur[o], cur[o + 1], cur[o + 2]]).or_insert(0) += 1;
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    let total: u32 = counts.values().sum();
    Some(*counts.values().max()? as f64 / total.max(1) as f64)
}

/// Screenshot one page in headless Chromium; returns the PNG bytes.
fn screenshot(bin: &Path, url: &str, out: &Path) -> Option<Vec<u8>> {
    let profile = std::env::temp_dir().join(format!("looka-shot-{}", std::process::id()));
    let mut child = Command::new(bin)
        .args(["--headless=new", "--use-angle=swiftshader", "--enable-unsafe-swiftshader", "--no-first-run", "--no-default-browser-check"])
        .args(["--use-mock-keychain", "--password-store=basic", "--hide-scrollbars", "--window-size=1280,720", "--virtual-time-budget=5000"])
        .arg(format!("--user-data-dir={}", profile.display()))
        .arg(format!("--screenshot={}", out.display()))
        .arg(url)
        .env_clear()
        .envs(run::safe_env())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .ok()?;
    let pgid = child.id() as i32;
    let t = Instant::now();
    // the file appears when the capture is done; chrome may linger afterwards
    while t.elapsed() < Duration::from_secs(30) && fs::metadata(out).map_or(true, |m| m.len() == 0) {
        if matches!(child.try_wait(), Ok(Some(_))) {
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    thread::sleep(Duration::from_millis(300));
    unsafe { libc::killpg(pgid, libc::SIGKILL) };
    let _ = child.wait();
    let _ = fs::remove_dir_all(&profile);
    fs::read(out).ok()
}

fn b64(s: &str) -> Option<Vec<u8>> {
    let val = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return None,
        } as u32)
    };
    let bytes: Vec<u8> = s.bytes().filter(|c| !c.is_ascii_whitespace() && *c != b'=').collect();
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    for chunk in bytes.chunks(4) {
        let mut n = 0u32;
        for (i, c) in chunk.iter().enumerate() {
            n |= val(*c)? << (18 - 6 * i);
        }
        out.extend_from_slice(&n.to_be_bytes()[1..chunk.len()]);
    }
    Some(out)
}

/// Original (source index, 0-based line) for a 1-based generated line, from a source map's `mappings`.
fn map_line(mappings: &str, gen_line: usize) -> Option<(usize, usize)> {
    let (mut src, mut line) = (0i64, 0i64);
    for (i, group) in mappings.split(';').enumerate() {
        for seg in group.split(',').filter(|s| !s.is_empty()) {
            // base64 VLQ: fields are generated col, source, original line, original col[, name]
            let (mut fields, mut v, mut shift) = (Vec::with_capacity(5), 0i64, 0);
            for c in seg.bytes() {
                let d = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/".iter().position(|x| *x == c)? as i64;
                v |= (d & 31) << shift;
                if d & 32 != 0 {
                    shift += 5;
                    if shift > 60 {
                        return None;
                    }
                } else {
                    fields.push(if v & 1 == 1 { -(v >> 1) } else { v >> 1 });
                    v = 0;
                    shift = 0;
                }
            }
            if fields.len() >= 4 {
                src += fields[1];
                line += fields[2];
                if i + 1 == gen_line {
                    return Some((usize::try_from(src).ok()?, usize::try_from(line).ok()?));
                }
            }
        }
    }
    None
}

/// Rewrite JS diagnostics from served/transformed lines to original source lines using the served source map.
fn source_map(base: &str, diags: &mut [Diagnostic]) {
    let host = base.trim_start_matches("http://").to_string();
    let mut cache: std::collections::HashMap<String, Option<Value>> = std::collections::HashMap::new();
    for d in diags.iter_mut().filter(|d| d.code.as_deref().is_some_and(|c| c.starts_with("js@"))) {
        let (Some(file), Some(line)) = (d.file.clone(), d.line) else { continue };
        let map = cache.entry(file.clone()).or_insert_with(|| {
            let (_, body, _) = get(&host, &file, Duration::from_secs(10)).ok()?;
            let url = body.rsplit("sourceMappingURL=").next().filter(|_| body.contains("sourceMappingURL="))?.split_whitespace().next()?.to_string();
            let json = if let Some(data) = url.split_once("base64,").map(|(_, d)| d.to_string()) {
                b64(&data)?
            } else {
                let dir = file.rsplit_once('/').map_or("", |(d, _)| d);
                get(&host, &resolve(&format!("{dir}/"), &url)?, Duration::from_secs(10)).ok()?.1.into_bytes()
            };
            serde_json::from_slice(&json).ok()
        });
        let Some(m) = map.as_ref() else { continue };
        if let Some((src, orig)) = map_line(m["mappings"].as_str().unwrap_or(""), line) {
            if let Some(name) = m["sources"].get(src).and_then(Value::as_str) {
                // Vite and webpack give paths relative to the map or absolute; keep the served path if it names the same file
                let same = name.rsplit('/').next() == file.rsplit('/').next();
                d.file = Some(if same { file.clone() } else { name.trim_start_matches("webpack://").to_string() });
                d.line = Some(orig + 1);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_routes_skip_groups_dynamic_and_api() {
        let d = std::env::temp_dir().join(format!("looka-routes-{}", std::process::id()));
        for f in ["app/page.tsx", "app/(shop)/cart/page.tsx", "app/foods/[slug]/page.tsx", "app/api/x/route.ts", "app/about/page.mdx", "pages/legacy.tsx", "pages/_app.tsx", "pages/404.tsx", "pages/500.tsx", "pages/legacy.test.tsx"] {
            let p = d.join(f);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, "").unwrap();
        }
        let (st, dy) = routes(&d);
        assert_eq!(st, vec!["/", "/about", "/cart", "/legacy"]);
        assert_eq!(dy.len(), 1);
        assert!(dy[0].1.is_match("/foods/acana-puppy") && !dy[0].1.is_match("/foods/a/b"));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn honours_base_href() {
        let html = r#"<base href="../"><link rel="stylesheet" href="css/game.css">"#;
        let base = base_of("/dev/v4-room.html", html);
        assert_eq!(resolve(&base, "css/game.css").as_deref(), Some("/css/game.css"));
        assert_eq!(base_of("/dev/a.html", "<p>no base</p>"), "/dev/a.html");
    }

    #[test]
    fn resolves_relative_links() {
        assert_eq!(resolve("/dev/v4-farm.html", "../js/game.js").as_deref(), Some("/js/game.js"));
        assert_eq!(resolve("/", "css/a.css").as_deref(), Some("/css/a.css"));
        assert_eq!(resolve("/a/b", "./c").as_deref(), Some("/a/c"));
        assert_eq!(resolve("/", "https://x.com/a.js"), None);
        assert_eq!(resolve("/", "data:image/png;base64,xx"), None);
    }

    #[test]
    fn source_map_lines() {
        // generated line 3 maps to source 0, original line 1 (0-based): "AAAA;;AACA" style mappings
        assert_eq!(map_line("AAAA;;AACA", 3), Some((0, 1)));
        assert_eq!(map_line("AAAA;AAEA", 2), Some((0, 2)));
        assert_eq!(b64("eyJhIjoxfQ==").as_deref(), Some(&br#"{"a":1}"#[..]));
    }

    #[test]
    fn percent_decoding() {
        assert_eq!(percent_decode("/a%20b/%2e%2e"), "/a b/..");
        assert_eq!(percent_decode("/bad%zz%"), "/bad%zz%");
    }
}
