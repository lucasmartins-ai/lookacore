//! Project Intelligence: discover components, toolchains and commands from the files that already exist.

use regex::Regex;
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

const SKIP_DIRS: &[&str] = &[
    "node_modules", "target", ".venv", "venv", "__pycache__", ".next", "dist", "build", "vendor", ".looka", ".git",
];

const ENTRYPOINTS: &[&str] = &[
    "main.rs", "lib.rs", "main.go", "main.py", "__main__.py", "app.py", "manage.py", "index.ts", "index.tsx",
    "index.js", "main.ts", "main.tsx", "App.tsx", "server.ts", "server.js", "main.c", "main.cpp", "main.dart",
];

#[derive(Serialize, Clone, Debug)]
pub struct Cmd {
    pub kind: &'static str,
    pub run: String,
    pub available: bool,
}

#[derive(Serialize, Clone, Debug)]
pub struct Component {
    pub dir: String,
    pub ecosystem: &'static str,
    pub manifest: String,
    pub name: Option<String>,
    pub frameworks: Vec<String>,
    pub commands: Vec<Cmd>,
}

impl Component {
    pub fn cmd(&self, kind: &str) -> Option<&Cmd> {
        self.commands.iter().find(|c| c.kind == kind)
    }
}

#[derive(Serialize, Debug)]
pub struct ProjectMap {
    pub root: String,
    pub files: usize,
    pub languages: BTreeMap<&'static str, usize>,
    pub components: Vec<Component>,
    pub entrypoints: Vec<String>,
    pub test_files: Vec<String>,
}

impl ProjectMap {
    /// Deepest component whose dir contains `rel`.
    pub fn owner(&self, rel: &str) -> Option<&Component> {
        self.components.iter().filter(|c| is_ancestor(&c.dir, rel)).max_by_key(|c| c.dir.len())
    }
}

pub fn walk(root: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = ignore::WalkBuilder::new(root)
        .require_git(false)
        .filter_entry(|e| {
            !(e.depth() > 0
                && e.file_type().is_some_and(|t| t.is_dir())
                && e.file_name().to_str().is_some_and(|n| SKIP_DIRS.contains(&n)))
        })
        .build()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
        .map(|e| e.into_path())
        .collect();
    files.sort();
    files
}

/// Every file under `dir`, ignoring .gitignore (for caches like Unity's Library/PackageCache).
pub fn walk_all(dir: &Path) -> Vec<PathBuf> {
    if !dir.is_dir() {
        return vec![];
    }
    ignore::WalkBuilder::new(dir)
        .standard_filters(false)
        .build()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
        .map(|e| e.into_path())
        .collect()
}

pub fn rel(root: &Path, p: &Path) -> String {
    let r = p.strip_prefix(root).unwrap_or(p).to_string_lossy().replace('\\', "/");
    if r.is_empty() { ".".into() } else { r }
}

fn is_ancestor(dir: &str, path: &str) -> bool {
    dir == "." || path == dir || path.starts_with(&format!("{dir}/"))
}

pub fn language(path: &Path) -> Option<&'static str> {
    Some(match path.extension()?.to_str()? {
        "rs" => "Rust",
        "ts" | "tsx" | "mts" | "cts" => "TypeScript",
        "js" | "jsx" | "mjs" | "cjs" => "JavaScript",
        "py" => "Python",
        "go" => "Go",
        "c" | "h" => "C",
        "cc" | "cpp" | "cxx" | "hpp" | "hh" => "C++",
        "java" => "Java",
        "kt" | "kts" => "Kotlin",
        "swift" => "Swift",
        "dart" => "Dart",
        "rb" => "Ruby",
        "php" => "PHP",
        "cs" => "C#",
        "gd" => "GDScript",
        "lua" => "Lua",
        "zig" => "Zig",
        "vue" => "Vue",
        "svelte" => "Svelte",
        "scala" => "Scala",
        "ex" | "exs" => "Elixir",
        "sql" => "SQL",
        "sh" | "bash" | "zsh" => "Shell",
        _ => return None,
    })
}

pub fn is_test(rel: &str) -> bool {
    let l = rel.to_lowercase();
    let name = l.rsplit('/').next().unwrap_or(&l);
    ["tests/", "test/", "__tests__/", "spec/"].iter().any(|d| l.starts_with(d) || l.contains(&format!("/{d}")))
        || name.starts_with("test_")
        || name.contains(".test.")
        || name.contains(".spec.")
        || name.ends_with("_test.go")
        || name.ends_with("_test.py")
}

pub fn has_bin(dir: &Path, bin: &str) -> bool {
    if bin.contains('/') {
        return dir.join(bin).is_file();
    }
    std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(bin).is_file()))
}

fn cmd(dir: &Path, kind: &'static str, bin: &str, run: impl Into<String>) -> Cmd {
    Cmd { kind, run: run.into(), available: has_bin(dir, bin) }
}

fn cap(re: &str, text: &str) -> Option<String> {
    Regex::new(re).ok()?.captures(text)?.get(1).map(|m| m.as_str().to_string())
}

fn found(names: &[&str], hit: impl Fn(&str) -> bool) -> Vec<String> {
    names.iter().filter(|n| hit(n)).map(|n| n.to_string()).collect()
}

fn comp(ecosystem: &'static str, name: Option<String>, frameworks: Vec<String>, commands: Vec<Cmd>) -> Component {
    Component { dir: String::new(), ecosystem, manifest: String::new(), name, frameworks, commands }
}

fn manifest_rank(p: &Path) -> Option<u8> {
    Some(match p.file_name()?.to_str()? {
        "Cargo.toml" | "package.json" | "pyproject.toml" | "go.mod" | "CMakeLists.txt" | "pubspec.yaml"
        | "build.gradle" | "build.gradle.kts" | "project.godot" | "project.pbxproj" | "ProjectVersion.txt" => 0,
        n if n.ends_with(".uproject") => 0,
        "setup.py" | "Makefile" => 1,
        "requirements.txt" => 2,
        _ => return None,
    })
}

fn ecosystem_of(manifest: &str) -> &'static str {
    match manifest {
        "Cargo.toml" => "rust",
        "package.json" => "node",
        "pyproject.toml" | "setup.py" | "requirements.txt" => "python",
        "go.mod" => "go",
        "CMakeLists.txt" => "cmake",
        "pubspec.yaml" => "flutter",
        "build.gradle" | "build.gradle.kts" => "gradle",
        "project.godot" => "godot",
        "ProjectVersion.txt" => "unity",
        n if n.ends_with(".uproject") => "unreal",
        "project.pbxproj" => "xcode",
        _ => "make",
    }
}

pub fn detect(root: &Path) -> ProjectMap {
    let files = walk(root);
    let mut languages = BTreeMap::new();
    let (mut entrypoints, mut test_files, mut count) = (vec![], vec![], 0);
    for f in &files {
        let Some(lang) = language(f) else { continue };
        count += 1;
        *languages.entry(lang).or_insert(0) += 1;
        let r = rel(root, f);
        if is_test(&r) {
            test_files.push(r);
        } else if f.file_name().and_then(|n| n.to_str()).is_some_and(|n| ENTRYPOINTS.contains(&n)) {
            entrypoints.push(r);
        }
    }

    let mut manifests: Vec<&PathBuf> = files.iter().filter(|f| manifest_rank(f).is_some()).collect();
    manifests.sort_by_key(|f| (f.components().count(), manifest_rank(f)));
    let mut components: Vec<Component> = vec![];
    for m in manifests {
        let name = m.file_name().and_then(|n| n.to_str()).unwrap_or("");
        // Foo.xcodeproj/project.pbxproj belongs to the directory holding Foo.xcodeproj
        // Foo.xcodeproj/project.pbxproj and Unity's ProjectSettings/ProjectVersion.txt belong to the dir above
        let up = name == "project.pbxproj" || (name == "ProjectVersion.txt" && m.parent().is_some_and(|p| p.ends_with("ProjectSettings")));
        let dir = if up { m.parent().and_then(Path::parent) } else { m.parent() }.unwrap_or(root);
        let eco = ecosystem_of(name);
        let reld = rel(root, dir);
        // ponytail: one component per (dir, ecosystem); a cargo/cmake/gradle root swallows nested manifests
        // (workspaces). Upgrade: read workspace member lists when non-workspace nested crates matter.
        let nested = matches!(eco, "rust" | "cmake" | "gradle");
        if components.iter().any(|c| c.ecosystem == eco && (c.dir == reld || (nested && is_ancestor(&c.dir, &reld)))) {
            continue;
        }
        let text = fs::read_to_string(m).unwrap_or_default();
        let c = match eco {
            "rust" => Some(cargo(dir, &text)),
            "node" => node(dir, &text),
            "python" => Some(python(dir)),
            "go" => Some(go(dir, &text)),
            "cmake" => Some(cmake(dir, &text)),
            "flutter" => Some(flutter(dir, &text)),
            "gradle" => Some(gradle(dir, &text)),
            "godot" => Some(godot(dir, &text)),
            "xcode" => xcode(dir, m),
            "unity" if m.parent().is_some_and(|p| p.ends_with("ProjectSettings")) => Some(unity(dir, &text)),
            "unity" => None,
            "unreal" => Some(unreal(dir, m, &text)),
            _ => make(dir, &text),
        };
        if let Some(mut c) = c {
            c.dir = reld;
            c.manifest = rel(root, m);
            components.push(c);
        }
    }

    ProjectMap { root: root.display().to_string(), files: count, languages, components, entrypoints, test_files }
}

fn cargo(dir: &Path, t: &str) -> Component {
    let dep = |n: &str| Regex::new(&format!(r#"(?m)^\s*"?{}"?\s*="#, regex::escape(n))).unwrap().is_match(t);
    let fw = found(&["tokio", "axum", "actix-web", "tauri", "bevy", "leptos", "dioxus", "egui", "clap", "serde"], dep);
    let mut cmds = vec![
        cmd(dir, "build", "cargo", "cargo build --workspace --all-targets"),
        cmd(dir, "typecheck", "cargo", "cargo check --workspace --all-targets --message-format=short"),
        cmd(dir, "lint", "cargo-clippy", "cargo clippy --workspace --all-targets --message-format=short -- -D warnings"),
        cmd(dir, "test", "cargo", "cargo test --workspace"),
    ];
    if dir.join("src/main.rs").is_file() {
        cmds.push(cmd(dir, "run", "cargo", "cargo run"));
    }
    comp("rust", cap(r#"(?m)^name\s*=\s*"([^"]+)""#, t), fw, cmds)
}

fn node(dir: &Path, t: &str) -> Option<Component> {
    let v: Value = serde_json::from_str(t).ok()?;
    let pm = [("pnpm-lock.yaml", "pnpm"), ("bun.lock", "bun"), ("bun.lockb", "bun"), ("yarn.lock", "yarn")]
        .iter()
        .find(|(f, _)| dir.join(f).exists())
        .map_or("npm", |(_, p)| *p);
    let scripts = v["scripts"].as_object();
    let has = |k: &str| {
        scripts.and_then(|s| s.get(k)).and_then(Value::as_str).is_some_and(|s| !s.contains("no test specified"))
    };
    let mut cmds = vec![cmd(dir, "install", pm, format!("{pm} install"))];
    let kinds: [(&'static str, &[&str]); 5] = [
        ("build", &["build"]),
        ("typecheck", &["typecheck", "type-check", "check-types", "tsc", "types"]),
        ("lint", &["lint"]),
        ("test", &["test", "test:unit"]),
        ("run", &["dev", "start"]),
    ];
    for (kind, keys) in kinds {
        if let Some(k) = keys.iter().find(|k| has(k)) {
            cmds.push(cmd(dir, kind, pm, format!("{pm} run {k}")));
        }
    }
    if !cmds.iter().any(|c| c.kind == "typecheck") && dir.join("tsconfig.json").exists() {
        cmds.push(cmd(dir, "typecheck", "node_modules/.bin/tsc", "node_modules/.bin/tsc --noEmit"));
    }
    let deps: Vec<&String> = ["dependencies", "devDependencies", "peerDependencies"]
        .iter()
        .filter_map(|k| v[*k].as_object())
        .flat_map(|o| o.keys())
        .collect();
    let fw = found(
        &[
            "react", "next", "vue", "nuxt", "svelte", "@sveltejs/kit", "vite", "express", "fastify", "hono", "electron",
            "react-native", "expo", "@tauri-apps/api", "vitest", "jest", "@playwright/test", "typescript",
            "tailwindcss", "prisma", "drizzle-orm", "three", "@react-three/fiber", "babylonjs", "@babylonjs/core", "phaser", "pixi.js",
            "playcanvas", "cannon-es", "@dimforge/rapier3d-compat",
        ],
        |n| deps.iter().any(|d| d.as_str() == n),
    );
    Some(comp("node", v["name"].as_str().map(String::from), fw, cmds))
}

fn python(dir: &Path) -> Component {
    let t = ["pyproject.toml", "setup.py", "setup.cfg", "requirements.txt", "requirements-dev.txt"]
        .iter()
        .filter_map(|f| fs::read_to_string(dir.join(f)).ok())
        .collect::<Vec<_>>()
        .join("\n");
    let tl = t.to_lowercase();
    let py = if dir.join(".venv/bin/python").is_file() { ".venv/bin/python" } else { "python3" };
    let mut cmds = vec![if dir.join("requirements.txt").is_file() {
        cmd(dir, "install", py, format!("{py} -m pip install -r requirements.txt"))
    } else {
        cmd(dir, "install", py, format!("{py} -m pip install -e ."))
    }];
    // ponytail: pytest is the only runner we drive (it also runs unittest suites).
    if tl.contains("pytest") || dir.join("tests").is_dir() || dir.join("conftest.py").is_file() {
        cmds.push(cmd(dir, "test", py, format!("{py} -m pytest -q")));
    }
    if tl.contains("ruff") {
        let ruff = if dir.join(".venv/bin/ruff").is_file() { ".venv/bin/ruff" } else { "ruff" };
        cmds.push(cmd(dir, "lint", ruff, format!("{ruff} check --output-format=concise .")));
    }
    if tl.contains("mypy") {
        cmds.push(cmd(dir, "typecheck", py, format!("{py} -m mypy .")));
    } else if tl.contains("pyright") {
        cmds.push(cmd(dir, "typecheck", "pyright", "pyright"));
    }
    let fw = found(
        &["django", "fastapi", "flask", "pydantic", "sqlalchemy", "pandas", "numpy", "torch", "streamlit", "pytest"],
        |n| tl.contains(n),
    );
    comp("python", cap(r#"(?m)^name\s*=\s*["']([^"']+)["']"#, &t), fw, cmds)
}

fn go(dir: &Path, t: &str) -> Component {
    let fw = found(&["gin-gonic/gin", "labstack/echo", "gofiber/fiber", "spf13/cobra", "gorm.io"], |n| t.contains(n));
    let cmds = vec![
        cmd(dir, "build", "go", "go build ./..."),
        cmd(dir, "lint", "go", "go vet ./..."),
        cmd(dir, "test", "go", "go test ./..."),
    ];
    comp("go", cap(r"(?m)^module\s+(\S+)", t), fw, cmds)
}

fn cmake(dir: &Path, t: &str) -> Component {
    let fw = found(&["Qt5", "Qt6", "SDL2", "Boost", "GTest"], |n| t.contains(n));
    let cmds = vec![
        cmd(dir, "build", "cmake", "cmake -S . -B build && cmake --build build"),
        // build first: ctest alone runs stale binaries, and exits 0 when there are no tests
        cmd(dir, "test", "ctest", "cmake -S . -B build && cmake --build build && ctest --test-dir build --output-on-failure --no-tests=error"),
    ];
    comp("cmake", cap(r"project\(\s*([A-Za-z0-9_-]+)", t), fw, cmds)
}

fn flutter(dir: &Path, t: &str) -> Component {
    let cmds = vec![
        cmd(dir, "install", "flutter", "flutter pub get"),
        cmd(dir, "typecheck", "flutter", "flutter analyze"),
        cmd(dir, "test", "flutter", "flutter test"),
    ];
    comp("flutter", cap(r"(?m)^name:\s*(\S+)", t), vec!["flutter".into()], cmds)
}

fn gradle(dir: &Path, t: &str) -> Component {
    let g = if dir.join("gradlew").is_file() { "./gradlew" } else { "gradle" };
    let cmds = vec![cmd(dir, "build", g, format!("{g} build -x test")), cmd(dir, "test", g, format!("{g} test"))];
    let fw = found(&["com.android", "org.springframework.boot", "io.ktor"], |n| t.contains(n));
    comp("gradle", None, fw, cmds)
}

fn godot(dir: &Path, t: &str) -> Component {
    // headless test runners: scripts extending SceneTree under qa/, test/ or tests/ (run with -s)
    let mut runners: Vec<String> = ["qa", "test", "tests"]
        .iter()
        .flat_map(|d| fs::read_dir(dir.join(d)).into_iter().flatten().flatten().map(move |e| (d, e.path())))
        .filter(|(_, p)| p.extension().is_some_and(|e| e == "gd"))
        .filter(|(_, p)| fs::read_to_string(p).is_ok_and(|t| t.contains("extends SceneTree")))
        .map(|(d, p)| format!("res://{d}/{}", p.file_name().unwrap().to_string_lossy()))
        .collect();
    runners.sort();
    let mut cmds = vec![
        cmd(dir, "typecheck", "godot", "godot --headless --path . --import"),
        cmd(dir, "run", "godot", "godot --path ."),
    ];
    if !runners.is_empty() {
        let run = runners.iter().map(|r| format!("godot --headless --path . -s {r}")).collect::<Vec<_>>().join(" && ");
        cmds.push(cmd(dir, "test", "godot", run));
    }
    let fw = found(&["Forward Plus", "Mobile", "GL Compatibility", "C#"], |n| t.contains(n));
    comp("godot", cap(r#"(?m)^config/name="([^"]+)""#, t), fw, cmds)
}

/// Unity editor for the project's exact version: UNITY_PATH, then Unity Hub's install layout.
fn unity_editor(version: &str) -> String {
    if let Ok(p) = std::env::var("UNITY_PATH") {
        return p;
    }
    let home = std::env::var("HOME").unwrap_or_default();
    [
        format!("/Applications/Unity/Hub/Editor/{version}/Unity.app/Contents/MacOS/Unity"),
        format!("{home}/Unity/Hub/Editor/{version}/Editor/Unity"),
        format!("C:/Program Files/Unity/Hub/Editor/{version}/Editor/Unity.exe"),
    ]
    .into_iter()
    .find(|p| Path::new(p).is_file())
    .unwrap_or_else(|| format!("/Applications/Unity/Hub/Editor/{version}/Unity.app/Contents/MacOS/Unity"))
}

fn unity(dir: &Path, t: &str) -> Component {
    let version = cap(r"m_EditorVersion:\s*(\S+)", t).unwrap_or_default();
    let u = unity_editor(&version);
    let manifest = fs::read_to_string(dir.join("Packages/manifest.json")).unwrap_or_default();
    let fw = found(
        &["com.unity.render-pipelines.universal", "com.unity.render-pipelines.high-definition", "com.unity.inputsystem", "com.unity.netcode.gameobjects", "com.unity.test-framework", "com.unity.addressables"],
        |n| manifest.contains(n),
    );
    // opening the project in batch mode compiles every script: compile errors are the typecheck
    let base = format!("'{u}' -batchmode -nographics -projectPath . -logFile -");
    let cmds = vec![
        cmd(dir, "typecheck", &u, format!("{base} -quit")),
        cmd(dir, "test", &u, format!("{base} -runTests -testPlatform EditMode -testResults .looka/unity-editmode.xml")),
    ];
    comp("unity", (!version.is_empty()).then(|| format!("Unity {version}")), fw, cmds)
}

/// Unreal engine root for the project's EngineAssociation: UE_ROOT, then the Epic launcher's layout.
fn unreal_root(assoc: &str) -> String {
    std::env::var("UE_ROOT").unwrap_or_else(|_| format!("/Users/Shared/Epic Games/UE_{assoc}"))
}

fn unreal(dir: &Path, uproject: &Path, t: &str) -> Component {
    let v: Value = serde_json::from_str(t).unwrap_or(Value::Null);
    let name = uproject.file_stem().and_then(|s| s.to_str()).unwrap_or("Game").to_string();
    let ue = unreal_root(v["EngineAssociation"].as_str().unwrap_or("5.4"));
    let (platform, editor) = if cfg!(target_os = "macos") {
        ("Mac", format!("{ue}/Engine/Binaries/Mac/UnrealEditor.app/Contents/MacOS/UnrealEditor"))
    } else {
        ("Linux", format!("{ue}/Engine/Binaries/Linux/UnrealEditor"))
    };
    let build = format!("{ue}/Engine/Build/BatchFiles/{platform}/Build.sh");
    let fw: Vec<String> = v["Plugins"].as_array().into_iter().flatten().filter(|p| p["Enabled"] == true).filter_map(|p| p["Name"].as_str().map(String::from)).collect();
    let file = uproject.file_name().and_then(|s| s.to_str()).unwrap_or("Game.uproject");
    let mut cmds = vec![];
    if dir.join("Source").is_dir() {
        cmds.push(cmd(dir, "build", &build, format!("'{build}' {name}Editor {platform} Development -Project=\"$PWD/{file}\" -WaitMutex")));
    }
    cmds.push(cmd(dir, "test", &editor, format!("'{editor}' \"$PWD/{file}\" -ExecCmds=\"Automation RunTests Project;Quit\" -unattended -nopause -NullRHI -nosplash -stdout -FullStdOutLogOutput")));
    comp("unreal", Some(name), fw, cmds)
}

fn xcode(dir: &Path, pbx: &Path) -> Option<Component> {
    let proj = pbx.parent()?.file_name()?.to_str()?.to_string();
    let scheme = proj.trim_end_matches(".xcodeproj").to_string();
    let base = format!("xcodebuild -project '{proj}' -scheme '{scheme}' -destination 'generic/platform=iOS Simulator'");
    let cmds = vec![
        cmd(dir, "build", "xcodebuild", format!("{base} build CODE_SIGNING_ALLOWED=NO")),
        // first available iPhone simulator at run time, not a hard-coded device name
        cmd(dir, "test", "xcodebuild", format!("xcodebuild -project '{proj}' -scheme '{scheme}' -destination \"platform=iOS Simulator,id=$(xcrun simctl list devices available | grep -m1 -oE 'iPhone[^(]*\\(([0-9A-F-]{{36}})' | grep -oE '[0-9A-F-]{{36}}')\" test CODE_SIGNING_ALLOWED=NO")),
    ];
    Some(comp("xcode", Some(scheme), vec![], cmds))
}

fn make(dir: &Path, t: &str) -> Option<Component> {
    let targets: Vec<String> = Regex::new(r"(?m)^([A-Za-z0-9_.-]+)\s*:(?:[^=]|$)")
        .unwrap()
        .captures_iter(t)
        .map(|c| c[1].to_string())
        .collect();
    let mut cmds = vec![];
    for (target, kind) in [("build", "build"), ("check", "typecheck"), ("typecheck", "typecheck"), ("lint", "lint"), ("test", "test")] {
        if targets.iter().any(|x| x == target) && !cmds.iter().any(|c: &Cmd| c.kind == kind) {
            cmds.push(cmd(dir, kind, "make", format!("make {target}")));
        }
    }
    (!cmds.is_empty()).then(|| comp("make", None, vec![], cmds))
}

pub fn render(m: &ProjectMap) -> String {
    let langs: Vec<String> = m.languages.iter().map(|(l, n)| format!("{l} {n}")).collect();
    let mut s = format!("# Project map: {}\n{} source files · {}\n", m.root, m.files, langs.join(", "));
    if m.components.is_empty() {
        s += "\nNo build manifest found (Cargo.toml, package.json, pyproject.toml, go.mod, CMakeLists.txt, …).\n";
    }
    for c in &m.components {
        let name = c.name.as_deref().map(|n| format!(" `{n}`")).unwrap_or_default();
        let fw = if c.frameworks.is_empty() { String::new() } else { format!(" · {}", c.frameworks.join(", ")) };
        s += &format!("\n## {} — {}{}{} ({})\n", c.dir, c.ecosystem, name, fw, c.manifest);
        for k in &c.commands {
            s += &format!("- {}: `{}`{}\n", k.kind, k.run, if k.available { "" } else { "  ⚠ tool not on PATH" });
        }
    }
    if !m.entrypoints.is_empty() {
        s += &format!("\nentrypoints: {}\n", m.entrypoints.join(", "));
    }
    s += &format!("test files: {}\n", m.test_files.len());
    s
}
