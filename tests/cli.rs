use serde_json::Value;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const FIXTURES: [(&str, &str, &str); 3] = [
    ("rs-app", "rust", "src/pricing.rs"),
    ("py-app", "python", "shop/pricing.py"),
    ("ts-app", "node", "src/pricing.ts"),
];

fn fixture(name: &str) -> String {
    format!("{}/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))
}

fn looka(args: &[&str]) -> (i32, Value) {
    let o = Command::new(env!("CARGO_BIN_EXE_lkc")).args(args).arg("--json").output().unwrap();
    let out = String::from_utf8_lossy(&o.stdout);
    (o.status.code().unwrap_or(-1), serde_json::from_str(&out).unwrap_or_else(|e| panic!("{e}: {out}")))
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap().flatten() {
        let name = e.file_name();
        if matches!(name.to_str(), Some("target" | ".looka" | "__pycache__" | ".pytest_cache")) {
            continue;
        }
        if e.path().is_dir() {
            copy_dir(&e.path(), &to.join(name));
        } else {
            std::fs::copy(e.path(), to.join(name)).unwrap();
        }
    }
}

fn scratch(name: &str) -> PathBuf {
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let d = std::env::temp_dir().join(format!("looka-test-{}-{n}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    copy_dir(Path::new(&fixture(name)), &d);
    d
}

#[test]
fn maps_context_and_impact_on_every_fixture() {
    for (name, eco, pricing) in FIXTURES {
        let root = fixture(name);
        let (_, map) = looka(&["map", "--root", &root]);
        assert_eq!(map["components"][0]["ecosystem"], eco, "{name}");
        assert!(map["components"][0]["commands"].as_array().unwrap().iter().any(|c| c["kind"] == "test"), "{name}");

        let (_, ctx) = looka(&["context", "change discount rules", "--root", &root]);
        assert_eq!(ctx["files"][0]["path"], pricing, "{name}: {ctx}");

        let (_, imp) = looka(&["impact", pricing, "--root", &root]);
        let tests = imp["tests"].as_array().unwrap();
        assert!(!tests.is_empty(), "{name}: pricing change must reach cart tests via the cart consumer: {imp}");
    }
}

#[test]
fn verify_passes_on_clean_fixtures_and_reports_broken_ones() {
    for (name, _, _) in FIXTURES {
        let (_, map) = looka(&["map", "--root", &fixture(name)]);
        let test_cmd = map["components"][0]["commands"].as_array().unwrap().iter().find(|c| c["kind"] == "test").cloned().unwrap();
        if test_cmd["available"] != true {
            eprintln!("skipping {name}: toolchain missing");
            continue;
        }
        let dir = scratch(name);
        let root = dir.to_str().unwrap();
        let (code, ok) = looka(&["verify", "--kinds", "test", "--root", root]);
        assert_eq!((code, &ok["ok"]), (0, &Value::Bool(true)), "{name}: {ok}");
        assert!(dir.join(".looka/evidence/latest.json").is_file());

        let test_file = ["tests/cart.rs", "tests/test_cart.py", "src/cart.test.ts"].iter().map(|f| dir.join(f)).find(|p| p.is_file()).unwrap();
        let src = std::fs::read_to_string(&test_file).unwrap().replace("1350", "13000"); // different length: pytest's pyc check is mtime(s)+size
        std::fs::write(&test_file, src).unwrap();
        let (code, bad) = looka(&["verify", "--kinds", "test", "--root", root]);
        assert_eq!((code, &bad["ok"]), (1, &Value::Bool(false)), "{name}");
        let diags = bad["results"][0]["diagnostics"].as_array().unwrap();
        assert!(diags.iter().any(|d| d["file"].as_str().is_some_and(|f| f.contains("cart"))), "{name}: {bad}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn mcp_server_lists_and_calls_tools() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_lkc"))
        .args(["mcp", "--root", &fixture("py-app")])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let mut rpc = |msg: &str| {
        writeln!(stdin, "{msg}").unwrap();
        serde_json::from_str::<Value>(&lines.next().unwrap().unwrap()).unwrap()
    };
    let init = rpc(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}"#);
    assert_eq!(init["result"]["serverInfo"]["name"], "looka");
    let list = rpc(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}
{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#);
    assert_eq!(list["id"], 2, "notification must not get a response");
    assert!(list["result"]["tools"].as_array().unwrap().len() >= 6);
    let call = rpc(r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"looka_context","arguments":{"task":"discount"}}}"#);
    let text = call["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("shop/pricing.py"));
    assert!(text.starts_with("root: "), "the root used must be stated: {text}");
    let bad = rpc(r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"nope","arguments":{}}}"#);
    assert_eq!(bad["result"]["isError"], true);
    drop(stdin);
    child.wait().unwrap();
}

#[test]
fn verify_does_not_hang_on_detached_descendants() {
    let dir = std::env::temp_dir().join(format!("looka-test-{}-detached", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // child escapes the process group with setsid and keeps stdout open for 30s; parent exits 0
    std::fs::write(dir.join("Makefile"), "test:\n\tpython3 -c 'import os,time; (os.fork()==0) and (os.setsid(), time.sleep(30))'\n").unwrap();
    let t = std::time::Instant::now();
    let (_, r) = looka(&["verify", "--kinds", "test", "--timeout", "3", "--root", dir.to_str().unwrap()]);
    assert!(t.elapsed().as_secs() < 15, "verify blocked on a detached process: {r}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn mcp_rejects_bad_root_and_survives_garbage() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_lkc"))
        .args(["mcp", "--root", &fixture("py-app")])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    stdin.write_all(b"\xff\n").unwrap();
    assert_eq!(serde_json::from_str::<Value>(&lines.next().unwrap().unwrap()).unwrap()["error"]["code"], -32700);
    for (args, why) in [(r#"{"root":"/nonexistent/typo"}"#, "missing root"), (r#"{"root":"/"}"#, "root outside project")] {
        writeln!(stdin, r#"{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"name":"looka_map","arguments":{args}}}}}"#).unwrap();
        let v: Value = serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap();
        assert_eq!(v["result"]["isError"], true, "{why}: {v}");
    }
    writeln!(stdin, r#"{{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{{"name":"looka_outline","arguments":{{"file":"../../../../../../etc/passwd"}}}}}}"#).unwrap();
    let v: Value = serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap();
    assert_eq!(v["result"]["isError"], true, "{v}");
    drop(stdin);
    child.wait().unwrap();
}

#[test]
fn verify_changed_separates_preexisting_from_introduced_failures() {
    let dir = scratch("py-app");
    let root = dir.to_str().unwrap();
    let git = |args: &[&str]| assert!(Command::new("git").arg("-C").arg(&dir).args(args).output().unwrap().status.success());
    // HEAD already has a failing expectation
    let t = dir.join("tests/test_cart.py");
    std::fs::write(&t, std::fs::read_to_string(&t).unwrap() + "\n\ndef test_broken_on_head():\n    assert 1 == 2\n").unwrap();
    // fails exactly once after `.noise` appears (removes it), like a timing-sensitive test under load
    let noise = dir.join(".noise").display().to_string();
    std::fs::write(
        dir.join("tests/test_unrelated.py"),
        format!("import os\n\ndef test_unrelated():\n    if os.path.exists({noise:?}):\n        os.remove({noise:?})\n        assert False, 'noise'\n"),
    )
    .unwrap();
    // tracked source in a dir merely *named* like build output
    std::fs::create_dir_all(dir.join("tests/build")).unwrap();
    std::fs::write(dir.join("tests/build/test_flag.py"), "def test_flag():\n    assert True\n").unwrap();
    git(&["init", "-q"]);
    git(&["add", "."]);
    git(&["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-qm", "base"]);

    let pricing = dir.join("shop/pricing.py");
    let src = std::fs::read_to_string(&pricing).unwrap();
    std::fs::write(&pricing, src.clone() + "\n# harmless\n").unwrap();
    let (code, ok) = looka(&["verify", "--changed", "--kinds", "test", "--root", root]);
    assert_eq!((code, &ok["introduced"]), (0, &Value::from(0)), "pre-existing failure must not block: {ok}");

    std::fs::write(&pricing, src.replace("cents - cents", "cents + cents")).unwrap();
    let (code, bad) = looka(&["verify", "--changed", "--kinds", "test", "--root", root]);
    assert_eq!(code, 1, "{bad}");
    assert!(bad["introduced"].as_u64().unwrap() >= 1, "{bad}");
    std::fs::write(&pricing, &src).unwrap();
    // same size, same second as the mutant: Python would reuse its stale .pyc
    let _ = std::fs::remove_dir_all(dir.join("shop/__pycache__"));

    // a test the change doesn't reach that fails once (noise under load) and passes re-run alone: flaky, not blocking
    let marker = dir.join(".noise");
    std::fs::write(&marker, "").unwrap();
    std::fs::write(dir.join("notes.txt"), "a non-source change: the whole suite runs\n").unwrap();
    let (code, noisy) = looka(&["verify", "--changed", "--kinds", "test", "--root", root]);
    assert_eq!((code, &noisy["introduced"]), (0, &Value::from(0)), "unrelated flaky failure must not block: {noisy}");
    assert!(noisy.to_string().contains("\"flaky\":true"), "{noisy}");
    std::fs::remove_file(dir.join("notes.txt")).unwrap();

    // the same once-only failure in a test the change touches may be a race it introduced: it keeps blocking
    std::fs::write(
        dir.join("tests/test_once.py"),
        "import os\n\ndef test_fails_first_time():\n    p = os.path.join(os.path.dirname(__file__), '.ran')\n    if not os.path.exists(p):\n        open(p, 'w').close()\n        assert False, 'first run'\n",
    )
    .unwrap();
    let (code, racy) = looka(&["verify", "--changed", "--kinds", "test", "--root", root]);
    assert_eq!(code, 1, "intermittent failure in a changed test must block: {racy}");
    std::fs::remove_file(dir.join("tests/test_once.py")).unwrap();
    std::fs::remove_file(dir.join("tests/.ran")).unwrap();

    // a failure there is introduced, not waved through as dependency noise
    std::fs::write(dir.join("tests/build/test_flag.py"), "def test_flag():\n    assert False\n").unwrap();
    let (code, bad) = looka(&["verify", "--changed", "--kinds", "test", "--root", root]);
    assert_eq!(code, 1, "{bad}");
    assert!(bad["introduced"].as_u64().unwrap_or(0) >= 1, "a tracked file under build/ is a change, and its failure is introduced: {bad}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn plugin_gate_runs_nothing_in_an_untrusted_repo() {
    let dir = scratch("py-app");
    let cfg = dir.join(".cfg");
    let git = |args: &[&str]| assert!(Command::new("git").arg("-C").arg(&dir).args(args).output().unwrap().status.success());
    git(&["init", "-q"]);
    git(&["add", "."]);
    git(&["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-qm", "base"]);
    let hook = |kind: &str| {
        use std::io::Write;
        let mut c = Command::new(env!("CARGO_BIN_EXE_lkc"))
            .args(["gate", "--hook", kind, "--require-trust"])
            .env("XDG_CONFIG_HOME", &cfg)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        c.stdin.take().unwrap().write_all(serde_json::json!({"session_id": "p", "cwd": dir}).to_string().as_bytes()).unwrap();
        String::from_utf8_lossy(&c.wait_with_output().unwrap().stdout).into_owned()
    };
    // a cloned repo's scripts are not run just because a session opened in it
    assert!(hook("claude-start").contains("lkc trust"));
    let pricing = dir.join("shop/pricing.py");
    std::fs::write(&pricing, std::fs::read_to_string(&pricing).unwrap().replace("cents - cents", "cents + cents")).unwrap();
    assert_eq!(hook("claude").trim(), "", "untrusted: nothing ran, nothing blocks");
    assert!(!dir.join(".looka/gate.json").exists(), "no state, no warm-up, no checks");
    // once trusted, the same hooks gate as usual
    let o = Command::new(env!("CARGO_BIN_EXE_lkc")).args(["trust", "--root", dir.to_str().unwrap()]).env("XDG_CONFIG_HOME", &cfg).output().unwrap();
    assert!(String::from_utf8_lossy(&o.stdout).contains("trusted"));
    git(&["stash", "-q"]);
    assert_eq!(hook("claude-start").trim(), "");
    git(&["stash", "pop", "-q"]);
    assert!(hook("claude").contains("\"block\""), "trusted: a broken change blocks");
    // a repo cloned inside the trusted one is not trusted with it
    let nested = dir.join("vendor/other");
    std::fs::create_dir_all(&nested).unwrap();
    assert!(Command::new("git").arg("-C").arg(&nested).args(["init", "-q"]).output().unwrap().status.success());
    let o = Command::new(env!("CARGO_BIN_EXE_lkc")).args(["gate", "--hook", "claude-start", "--require-trust", "--root", nested.to_str().unwrap()]).env("XDG_CONFIG_HOME", &cfg).output().unwrap();
    assert!(String::from_utf8_lossy(&o.stdout).contains("lkc trust"), "nested repo must ask for its own trust");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn verify_changed_ignores_prose_when_narrowing() {
    let dir = scratch("py-app");
    let root = dir.to_str().unwrap();
    let git = |args: &[&str]| assert!(Command::new("git").arg("-C").arg(&dir).args(args).output().unwrap().status.success());
    git(&["init", "-q"]);
    git(&["add", "."]);
    git(&["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-qm", "base"]);
    // agents update a changelog on nearly every task: that alone must not force the full suite
    std::fs::write(dir.join("CHANGELOG.md"), "- pricing tweak\n").unwrap();
    let pricing = dir.join("shop/pricing.py");
    std::fs::write(&pricing, std::fs::read_to_string(&pricing).unwrap() + "\n# harmless\n").unwrap();
    let (code, v) = looka(&["verify", "--changed", "--kinds", "test", "--root", root]);
    assert_eq!(code, 0, "{v}");
    let cmd = v["results"][0]["command"].as_str().unwrap_or("");
    assert!(cmd.contains("tests/test_cart.py"), "narrowed to the affected test, not the whole suite: {cmd}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn probe_static_site_catches_missing_assets() {
    let dir = scratch("web-game");
    let root = dir.to_str().unwrap();
    let (code, bad) = looka(&["probe", "--root", root]);
    assert_eq!(code, 1, "css/game.css is referenced but missing: {bad}");
    let pages = bad["pages"].as_array().unwrap();
    assert!(pages.iter().any(|p| p["path"] == "/css/game.css" && p["status"] == 404), "{bad}");
    assert!(pages.iter().any(|p| p["path"] == "/levels.html" && p["status"] == 200), "link crawl: {bad}");

    std::fs::create_dir_all(dir.join("css")).unwrap();
    std::fs::write(dir.join("css/game.css"), "canvas{border:1px solid}").unwrap();
    let (code, ok) = looka(&["probe", "--root", root]);
    assert_eq!(code, 0, "{ok}");

    // JS runtime errors, when a headless Chromium is installed
    if ok["browser"] == true {
        let js = dir.join("js/game.js");
        std::fs::write(&js, std::fs::read_to_string(&js).unwrap() + "\nwindow.addEventListener('load', () => missingLoop());\n").unwrap();
        let (code, bad) = looka(&["probe", "--root", root]);
        assert_eq!(code, 1, "{bad}");
        assert!(bad["diagnostics"].as_array().unwrap().iter().any(|d| d["message"].as_str().unwrap().contains("missingLoop")), "{bad}");
        std::fs::write(&js, "const c = document.getElementById('c');\n").unwrap();

        // a route that only exists in the rendered DOM (client-side link) must still be visited
        std::fs::write(dir.join("route.html"), "<!doctype html><html><body><script>spaRouteBoom()</script></body></html>").unwrap();
        std::fs::write(dir.join("index.html"), "<!doctype html><html><body><script>const a=document.createElement('a');a.href='route.html';document.body.appendChild(a);</script></body></html>").unwrap();
        let (_, spa) = looka(&["probe", "--pages", "1", "--root", root]);
        assert!(spa["browser_pages"].as_array().unwrap().iter().any(|p| p == "/route.html"), "{spa}");
        assert!(spa["diagnostics"].as_array().unwrap().iter().any(|d| d["message"].as_str().unwrap().contains("spaRouteBoom")), "{spa}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn game_engines_and_assets() {
    let (_, unity) = looka(&["map", "--root", &fixture("unity-game")]);
    assert_eq!(unity["components"][0]["ecosystem"], "unity");
    assert_eq!(unity["components"][0]["name"], "Unity 6000.0.23f1");
    let (_, unreal) = looka(&["map", "--root", &fixture("unreal-game")]);
    assert_eq!(unreal["components"][0]["ecosystem"], "unreal");
    assert!(unreal["components"][0]["commands"][0]["run"].as_str().unwrap().contains("ShooterEditor"));

    // Unity: the prefab references the texture by the GUID in its .meta file
    let (_, imp) = looka(&["impact", "Assets/Textures/hero.png", "--root", &fixture("unity-game")]);
    assert!(imp["references"].as_array().unwrap().iter().any(|r| r["file"] == "Assets/Prefabs/Player.prefab"), "{imp}");

    // a prefab pointing at a GUID no .meta declares is Unity's "Missing Prefab"
    let (code, ua) = looka(&["assets", "--root", &fixture("unity-game")]);
    assert_eq!(code, 1);
    assert_eq!(ua["missing"][0]["referenced"], "guid:deadbeefdeadbeefdeadbeefdeadbeef", "{ua}");
    assert_eq!(ua["missing"].as_array().unwrap().len(), 1, "texture and script GUIDs resolve: {ua}");

    // Godot can't import Draco-compressed glTF: an error, with the scene that loads it
    let (code, godot) = looka(&["assets", "--root", &fixture("godot-game")]);
    assert_eq!(code, 1, "{godot}");
    let rock = godot["assets"].as_array().unwrap().iter().find(|a| a["path"] == "assets/models/rock.gltf").unwrap().clone();
    assert_eq!(rock["facts"]["triangles"], 12);
    assert!(rock["issues"][0]["message"].as_str().unwrap().contains("Draco"));
    assert_eq!(rock["used_by"][0], "main.tscn");
}

#[test]
fn blender_exports_for_each_engine() {
    let has_blender = Command::new("sh").args(["-c", "command -v blender || test -x /Applications/Blender.app/Contents/MacOS/Blender"]).status().is_ok_and(|s| s.success());
    if !has_blender {
        eprintln!("skipping: Blender not installed");
        return;
    }
    let dir = scratch("game-art");
    let root = dir.to_str().unwrap();
    let (_, inspect) = looka(&["assets", "inspect", "art/hero.blend", "--root", root]);
    assert!(inspect[0]["stats"]["triangles"].as_u64().unwrap() > 15_000, "subdivision modifier counted: {inspect}");
    assert!(inspect[0]["issues"].as_array().unwrap().iter().any(|i| i.as_str().unwrap().contains("unapplied scale")));

    let (_, web) = looka(&["assets", "export", "art/hero.blend", "--for", "three", "--out", "web/hero.glb", "--root", root]);
    assert!(web[0]["exported_facts"]["compression"].as_array().unwrap().iter().any(|c| c == "KHR_draco_mesh_compression"), "{web}");
    let (_, godot) = looka(&["assets", "export", "art/hero.blend", "--for", "godot", "--out", "godot/hero.glb", "--root", root]);
    assert!(godot[0]["exported_facts"]["compression"].as_array().is_none_or(|c| c.is_empty()), "no Draco for Godot: {godot}");
    let (_, unity) = looka(&["assets", "export", "art/hero.blend", "--for", "unity", "--root", root]);
    assert!(unity[0]["exported"].as_str().unwrap().ends_with(".fbx"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn probe_detects_blank_webgl_render() {
    let dir = scratch("webgl-game");
    let root = dir.to_str().unwrap();
    let (code, ok) = looka(&["probe", "--root", root]);
    if ok["browser"] != true {
        eprintln!("skipping: no headless Chromium");
        return;
    }
    assert_eq!(code, 0, "triangle on a blue background is a real frame: {ok}");
    if ok["screenshot"].is_null() {
        // slow CI runners can time out the capture; the probe then says so in `note` instead of failing
        eprintln!("skipping: screenshot not captured: {}", ok["note"]);
        return;
    }
    assert!(dir.join(ok["screenshot"].as_str().unwrap()).is_file());
    let html = std::fs::read_to_string(dir.join("index.html")).unwrap().replace("\nframe();", "\n// never started");
    std::fs::write(dir.join("index.html"), html).unwrap();
    let (code, bad) = looka(&["probe", "--root", root]);
    if bad["screenshot"].is_null() {
        eprintln!("skipping: screenshot not captured: {}", bad["note"]);
        return;
    }
    assert_eq!(code, 1, "{bad}");
    assert!(bad["diagnostics"].as_array().unwrap().iter().any(|d| d["code"] == "blank-render"), "{bad}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn contracts_connect_web_api_godot_game_and_python_worker() {
    let (code, c) = looka(&["contracts", "--root", &fixture("cross-stack")]);
    assert_eq!(code, 1, "the worker calls a route nobody serves: {c}");
    let ep = |m: &str, p: &str| c["endpoints"].as_array().unwrap().iter().find(|e| e["method"] == m && e["path"] == p).cloned().unwrap();
    assert_eq!(ep("POST", "/api/scores")["callers"][0], "game/net.gd:7");
    assert_eq!(ep("GET", "/api/scores/*")["callers"][0], "worker/worker.py:7");
    assert_eq!(c["broken"][0]["path"], "/api/leaderboard");
    let (_, imp) = looka(&["impact", "web/app/api/scores/route.ts", "--root", &fixture("cross-stack")]);
    assert!(imp["references"].as_array().unwrap().iter().any(|r| r["file"] == "game/net.gd" && r["symbol"] == "http"), "{imp}");
}

#[test]
fn probe_ios_app_in_simulator_catches_crash_with_source_line() {
    let has_sim = Command::new("sh").args(["-c", "xcrun simctl list devices available | grep -q iPhone"]).status().is_ok_and(|s| s.success());
    if !has_sim {
        eprintln!("skipping: no Xcode/iOS simulator");
        return;
    }
    let dir = scratch("ios-app");
    let root = dir.to_str().unwrap();
    let (code, ok) = looka(&["probe", "--root", root]);
    assert_eq!((code, &ok["target"]), (0, &Value::from("ios")), "{ok}");
    assert!(ok["log_tail"].as_str().unwrap().contains("LOOKA_APP_READY"), "app stdout captured: {ok}");
    assert!(dir.join(ok["screenshot"].as_str().unwrap()).is_file());

    let src = dir.join("LookaProbe/LookaProbeApp.swift");
    let text = std::fs::read_to_string(&src).unwrap();
    std::fs::write(&src, text.replace(r#".onAppear { print("LOOKA_APP_READY") }"#, ".onAppear { let items: [Int] = []; print(items[3]) }")).unwrap();
    let (code, bad) = looka(&["probe", "--root", root]);
    assert_eq!(code, 1, "{bad}");
    let crash = bad["diagnostics"].as_array().unwrap().iter().find(|d| d["code"] == "crash").cloned().unwrap();
    assert!(crash["message"].as_str().unwrap().contains("Index out of range"), "{crash}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn gate_blocks_claude_and_git_only_on_failures_the_session_introduced() {
    let dir = scratch("py-app");
    let (_, map) = looka(&["map", "--root", dir.to_str().unwrap()]);
    let test_cmd = map["components"][0]["commands"].as_array().unwrap().iter().find(|c| c["kind"] == "test").cloned().unwrap();
    if test_cmd["available"] != true {
        eprintln!("skipping: pytest missing");
        return;
    }
    let git = |args: &[&str]| assert!(Command::new("git").arg("-C").arg(&dir).args(args).output().unwrap().status.success());
    git(&["init", "-q"]);
    git(&["add", "."]);
    git(&["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-qm", "base"]);
    let hook = |kind: &str, input: Value| {
        use std::io::Write;
        let mut c = Command::new(env!("CARGO_BIN_EXE_lkc"))
            .args(["gate", "--hook", kind])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        c.stdin.take().unwrap().write_all(input.to_string().as_bytes()).unwrap();
        let o = c.wait_with_output().unwrap();
        (o.status.code().unwrap_or(-1), String::from_utf8_lossy(&o.stdout).into_owned())
    };
    let ev = |session: &str, active: bool| serde_json::json!({"session_id": session, "cwd": dir, "stop_hook_active": active});
    let pricing = dir.join("shop/pricing.py");
    let good = std::fs::read_to_string(&pricing).unwrap();

    assert_eq!(hook("claude-start", ev("s1", false)).0, 0);
    std::fs::write(&pricing, good.replace("cents - cents", "cents + cents")).unwrap();
    // a warm-up that died mid-way leaves its marker: the check must not wait on a dead process
    std::fs::create_dir_all(dir.join(".looka")).unwrap();
    std::fs::write(dir.join(".looka/warming"), "999999").unwrap();
    let t0 = std::time::Instant::now();
    let (code, out) = hook("claude", ev("s1", false));
    assert!(t0.elapsed() < std::time::Duration::from_secs(120), "waited on a dead warm-up");
    assert_eq!(code, 0);
    let v: Value = serde_json::from_str(out.trim()).unwrap_or_else(|e| panic!("{e}: {out}"));
    assert_eq!(v["decision"], "block", "{out}");
    assert!(v["reason"].as_str().unwrap().contains("introduce"), "{out}");
    // blocked once, nothing changed since: let the agent stop instead of looping
    assert_eq!(hook("claude", ev("s1", true)).1.trim(), "");
    // git refuses the same tree
    // as git runs it: in the repo, no --root, and possibly inside a Claude Code session for another project
    let o = Command::new(env!("CARGO_BIN_EXE_lkc")).args(["gate", "--hook", "git"]).current_dir(&dir).env("CLAUDE_PROJECT_DIR", std::env::temp_dir()).output().unwrap();
    assert_eq!(o.status.code(), Some(1), "{}", String::from_utf8_lossy(&o.stderr));
    // a session that starts on top of someone else's broken work is not blamed for it
    assert_eq!(hook("claude-start", ev("s2", false)).0, 0);
    assert_eq!(hook("claude", ev("s2", false)).1.trim(), "");
    // fixed: no block
    std::fs::write(&pricing, good + "\n# reviewed\n").unwrap();
    assert_eq!(hook("claude", ev("s1", false)).1.trim(), "");
    // the same passing tree again (a second Stop, or the gate wired twice): no second check
    let checks = |d: &std::path::Path| serde_json::from_str::<Value>(&std::fs::read_to_string(d.join(".looka/gate.json")).unwrap()).unwrap()["checks"].as_array().map_or(0, |a| a.len());
    let before = checks(&dir);
    assert_eq!(hook("claude", ev("s1", false)).1.trim(), "");
    assert_eq!(checks(&dir), before, "an already-verified tree is not re-verified");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn init_merges_hooks_without_clobbering_settings() {
    let dir = scratch("py-app");
    assert!(Command::new("git").arg("-C").arg(&dir).args(["init", "-q"]).output().unwrap().status.success());
    std::fs::create_dir_all(dir.join(".claude")).unwrap();
    std::fs::write(dir.join(".claude/settings.json"), r#"{"permissions": {"allow": ["Bash(ls)"]}, "hooks": {"Stop": [{"hooks": [{"type": "command", "command": "echo mine"}]}]}}"#).unwrap();
    for _ in 0..2 {
        // a global core.hooksPath on the dev machine would make init (rightly) skip .git/hooks
        let o = Command::new(env!("CARGO_BIN_EXE_lkc")).args(["init", "--root", dir.to_str().unwrap()]).env("GIT_CONFIG_GLOBAL", "/dev/null").env("XDG_CONFIG_HOME", dir.join(".cfg")).output().unwrap();
        assert!(o.status.success());
    }
    let s: Value = serde_json::from_str(&std::fs::read_to_string(dir.join(".claude/settings.json")).unwrap()).unwrap();
    assert_eq!(s["permissions"]["allow"][0], "Bash(ls)");
    let stop = s["hooks"]["Stop"].to_string();
    assert!(stop.contains("echo mine") && stop.matches("lkc gate").count() == 1, "{stop}");
    assert!(s["hooks"]["SessionStart"].to_string().contains("claude-start"));
    assert!(dir.join(".git/hooks/pre-commit").is_file());
    assert_eq!(std::fs::read_to_string(dir.join(".gitignore")).unwrap().matches(".looka/").count(), 1);
    // a settings file that doesn't parse (trailing comma) is left exactly as it was, never replaced
    let broken = r#"{"permissions": {"allow": ["Bash(ls)",]}}"#;
    std::fs::write(dir.join(".claude/settings.json"), broken).unwrap();
    let o = Command::new(env!("CARGO_BIN_EXE_lkc")).args(["init", "--root", dir.to_str().unwrap()]).env("XDG_CONFIG_HOME", dir.join(".cfg")).output().unwrap();
    assert!(String::from_utf8_lossy(&o.stdout).contains("not valid JSON"));
    // init is an explicit opt-in: it trusts the repo for the plugin's hooks
    assert!(std::fs::read_to_string(dir.join(".cfg/lookacore/trusted")).unwrap().lines().count() == 1);
    assert_eq!(std::fs::read_to_string(dir.join(".claude/settings.json")).unwrap(), broken);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn verify_ci_replicates_the_workflow_env_and_db_setup() {
    let dir = scratch("py-app");
    let (_, map) = looka(&["map", "--root", dir.to_str().unwrap()]);
    let test_cmd = map["components"][0]["commands"].as_array().unwrap().iter().find(|c| c["kind"] == "test").cloned().unwrap();
    if test_cmd["available"] != true {
        eprintln!("skipping: pytest missing");
        return;
    }
    std::fs::create_dir_all(dir.join(".github/workflows")).unwrap();
    std::fs::write(
        dir.join(".github/workflows/ci.yml"),
        "name: ci\non: [push]\njobs:\n  test:\n    runs-on: ubuntu-latest\n    env:\n      LKC_CI_FLAG: \"on\"\n      TOKEN: ${{ secrets.TOKEN }}\n      NODE_OPTIONS: --require ./evil.js\n      PYTHONPATH: /tmp/evil\n    steps:\n      - uses: actions/checkout@v4\n      - run: pip install -r requirements.txt\n      - name: Seed database\n        run: echo pwned > pwned.txt\n      - name: Prepare database\n        run: python3 manage.py migrate\n      - run: python3 -m pytest -q\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("tests/test_ci_env.py"),
        "import os, pathlib\n\ndef test_ci_env():\n    assert os.environ.get('LKC_CI_FLAG') == 'on'\n    assert 'TOKEN' not in os.environ\n    assert 'NODE_OPTIONS' not in os.environ and os.environ.get('PYTHONPATH') != '/tmp/evil'\n    assert pathlib.Path('db_ready.txt').exists()\n",
    )
    .unwrap();
    std::fs::write(dir.join("manage.py"), "import pathlib, sys\nif sys.argv[1:] == ['migrate']:\n    pathlib.Path('db_ready.txt').write_text('ok')\n").unwrap();
    let root = dir.to_str().unwrap();
    let (code, plain) = looka(&["verify", "--kinds", "test", "--root", root]);
    assert_eq!(code, 1, "without --ci the CI-only env is missing: {plain}");
    let (code, ci) = looka(&["verify", "--ci", "--kinds", "test", "--root", root]);
    assert_eq!(code, 0, "{ci}");
    assert!(!dir.join("pwned.txt").exists(), "arbitrary shell from the workflow must not run");
    let _ = std::fs::remove_dir_all(&dir);
}
