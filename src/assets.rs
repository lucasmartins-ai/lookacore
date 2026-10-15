//! Asset intelligence for games and 3D web: what assets exist, how heavy they are for the target engine,
//! who uses each one, and what is referenced but missing. Formats are read from their headers/JSON only.

use crate::detect::{self, ProjectMap};
use regex::Regex;
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::sync::LazyLock;

const MODEL: &[&str] = &["glb", "gltf", "fbx", "obj", "blend", "usd", "usdz", "usdc", "dae", "3ds", "stl", "ply"];
const TEXTURE: &[&str] = &["png", "jpg", "jpeg", "webp", "ktx2", "basis", "dds", "tga", "exr", "hdr", "avif", "psd", "bmp"];
const AUDIO: &[&str] = &["wav", "ogg", "mp3", "flac", "m4a", "aac", "opus"];
const SCENE: &[&str] = &["tscn", "scn", "tres", "res", "prefab", "unity", "mat", "asset", "uasset", "umap", "anim", "controller"];
const FONT: &[&str] = &["ttf", "otf", "woff", "woff2", "fnt"];
/// Files whose references are documentation or tooling, not something the running game loads.
fn non_runtime(rel: &str) -> bool {
    let l = rel.to_lowercase();
    l.ends_with(".md")
        || detect::is_test(rel)
        || ["docs/", "doc/", "tools/", "scripts/", "audit", "e2e/", "fixtures/", "examples/"].iter().any(|d| l.starts_with(d) || l.contains(&format!("/{d}")))
}

/// Text files that can reference assets.
const REFERRERS: &[&str] = &[
    "js", "mjs", "cjs", "ts", "tsx", "jsx", "vue", "svelte", "html", "css", "scss", "json", "gd", "tscn", "tres", "cs", "cpp",
    "h", "py", "lua", "prefab", "unity", "mat", "asset", "godot", "md", "yaml", "yml", "gltf", "rs",
];

static REF: LazyLock<Regex> = LazyLock::new(|| {
    let exts = [MODEL, TEXTURE, AUDIO, &["tscn", "tres", "scn"]].concat().join("|");
    // quoted (or CSS url(...)) paths only: `t.obj` in code is a property, not a model
    Regex::new(&format!(r#"(?:["'`]|url\()((?:res://|\.{{0,2}}/)?[A-Za-z0-9_@./ ${{}}-]*[A-Za-z0-9_}}]\.(?:{exts}))["'`)]"#)).unwrap()
});
static PLACEHOLDER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\{[^}]*\}").unwrap());
static GUID_REF: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"guid: ([0-9a-f]{32})").unwrap());

#[derive(Serialize, Clone, Debug)]
pub struct Issue {
    pub severity: &'static str,
    pub message: String,
}

#[derive(Serialize, Default, Clone, Debug)]
pub struct Facts {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub triangles: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vertices: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub materials: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub textures: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub animations: Option<usize>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub compression: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_texture: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seconds: Option<f64>,
}

#[derive(Serialize, Clone, Debug)]
pub struct Asset {
    pub path: String,
    pub kind: &'static str,
    pub bytes: u64,
    pub facts: Facts,
    pub issues: Vec<Issue>,
    pub used_by: Vec<String>,
}

#[derive(Serialize, Debug)]
pub struct Missing {
    pub referenced: String,
    pub from: String,
}

#[derive(Serialize, Debug)]
pub struct Report {
    pub ok: bool,
    pub profile: String,
    pub totals: BTreeMap<&'static str, (usize, u64)>,
    pub assets: Vec<Asset>,
    pub missing: Vec<Missing>,
    pub unreferenced: Vec<String>,
}

/// Per-target budgets. Web = three.js / babylon / plain WebGL; engines import far heavier content.
struct Budget {
    max_texture: u32,
    max_triangles: u64,
    max_model_bytes: u64,
    max_wav_bytes: u64,
}

fn budget(profile: &str) -> Budget {
    match profile {
        "godot" => Budget { max_texture: 4096, max_triangles: 300_000, max_model_bytes: 50 << 20, max_wav_bytes: 20 << 20 },
        "unity" | "unreal" => Budget { max_texture: 8192, max_triangles: 1_000_000, max_model_bytes: 200 << 20, max_wav_bytes: 50 << 20 },
        _ => Budget { max_texture: 2048, max_triangles: 100_000, max_model_bytes: 5 << 20, max_wav_bytes: 1 << 20 },
    }
}

/// Target engine from the project map: Godot, Unity, Unreal, else web.
pub fn profile(map: &ProjectMap) -> String {
    if let Ok(p) = std::env::var("LOOKA_ASSET_PROFILE") {
        return p;
    }
    for eco in ["godot", "unity", "unreal"] {
        if map.components.iter().any(|c| c.ecosystem == eco) {
            return eco.into();
        }
    }
    "web".into()
}

pub fn kind_of(path: &Path) -> Option<&'static str> {
    let e = path.extension()?.to_str()?.to_lowercase();
    let e = e.as_str();
    Some(if MODEL.contains(&e) {
        "model"
    } else if TEXTURE.contains(&e) {
        "texture"
    } else if AUDIO.contains(&e) {
        "audio"
    } else if SCENE.contains(&e) {
        "scene"
    } else if FONT.contains(&e) {
        "font"
    } else {
        return None;
    })
}

fn u32be(b: &[u8], i: usize) -> Option<u32> {
    b.get(i..i + 4).map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
}
fn u32le(b: &[u8], i: usize) -> Option<u32> {
    b.get(i..i + 4).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}
fn u16be(b: &[u8], i: usize) -> Option<u32> {
    b.get(i..i + 2).map(|s| u32::from(u16::from_be_bytes([s[0], s[1]])))
}

/// Width and height of a PNG, JPEG, WebP or KTX2 image from its header.
pub fn image_size(b: &[u8]) -> Option<(u32, u32)> {
    if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some((u32be(b, 16)?, u32be(b, 20)?));
    }
    if b.starts_with(&[0xFF, 0xD8]) {
        let mut i = 2;
        while i + 9 < b.len() {
            if b[i] != 0xFF {
                i += 1;
                continue;
            }
            let m = b[i + 1];
            let len = u16be(b, i + 2)? as usize;
            if (0xC0..=0xCF).contains(&m) && ![0xC4, 0xC8, 0xCC].contains(&m) {
                return Some((u16be(b, i + 7)?, u16be(b, i + 5)?));
            }
            i += 2 + len;
        }
        return None;
    }
    if b.starts_with(b"RIFF") && b.get(8..12) == Some(b"WEBP") {
        return match b.get(12..16)? {
            b"VP8X" => Some((1 + (u32le(b, 24)? & 0xFFFFFF), 1 + (u32le(b, 27)? & 0xFFFFFF))),
            b"VP8L" => {
                let v = u32le(b, 21)?;
                Some((1 + (v & 0x3FFF), 1 + ((v >> 14) & 0x3FFF)))
            }
            b"VP8 " => Some((u32::from(u16::from_le_bytes([*b.get(26)?, *b.get(27)?])) & 0x3FFF, u32::from(u16::from_le_bytes([*b.get(28)?, *b.get(29)?])) & 0x3FFF)),
            _ => None,
        };
    }
    if b.starts_with(b"\xABKTX 20\xBB\r\n\x1a\n") {
        return Some((u32le(b, 20)?, u32le(b, 24)?));
    }
    None
}

fn wav_seconds(b: &[u8]) -> Option<f64> {
    if !(b.starts_with(b"RIFF") && b.get(8..12) == Some(b"WAVE")) {
        return None;
    }
    let (mut i, mut byte_rate) = (12, 0u32);
    while i + 8 <= b.len() {
        let id = &b[i..i + 4];
        let len = u32le(b, i + 4)? as usize;
        if id == b"fmt " {
            byte_rate = u32le(b, i + 16)?;
        } else if id == b"data" && byte_rate > 0 {
            return Some(len as f64 / byte_rate as f64);
        }
        i += 8 + len + (len & 1);
    }
    None
}

/// glTF facts: triangles from index/position accessors, materials, textures, animations, compression extensions,
/// largest embedded or external texture, and external files the model needs.
pub fn gltf_facts(path: &Path, bytes: &[u8]) -> (Facts, Vec<Issue>) {
    let (mut f, mut issues) = (Facts::default(), vec![]);
    let (json, bin): (Option<Value>, &[u8]) = if bytes.starts_with(b"glTF") {
        let jlen = u32le(bytes, 12).unwrap_or(0) as usize;
        let j = bytes.get(20..20 + jlen).and_then(|s| serde_json::from_slice(s).ok());
        let bstart = 20 + jlen + 8;
        (j, bytes.get(bstart..).unwrap_or(&[]))
    } else {
        (serde_json::from_slice(bytes).ok(), &[])
    };
    let Some(g) = json else {
        issues.push(Issue { severity: "error", message: "not a readable glTF".into() });
        return (f, issues);
    };
    let acc = |i: &Value| i.as_u64().and_then(|i| g["accessors"].get(i as usize)).and_then(|a| a["count"].as_u64()).unwrap_or(0);
    let (mut tris, mut verts) = (0u64, 0u64);
    for m in g["meshes"].as_array().into_iter().flatten() {
        for p in m["primitives"].as_array().into_iter().flatten() {
            let mode = p["mode"].as_u64().unwrap_or(4);
            let pos = acc(&p["attributes"]["POSITION"]);
            verts += pos;
            let n = if p["indices"].is_null() { pos } else { acc(&p["indices"]) };
            tris += match mode {
                4 => n / 3,
                5 | 6 => n.saturating_sub(2),
                _ => 0,
            };
        }
    }
    f.triangles = Some(tris);
    f.vertices = Some(verts);
    f.materials = g["materials"].as_array().map(Vec::len);
    f.textures = g["textures"].as_array().map(Vec::len);
    f.animations = g["animations"].as_array().map(Vec::len).filter(|n| *n > 0);
    f.compression = g["extensionsUsed"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|e| ["KHR_draco_mesh_compression", "EXT_meshopt_compression", "KHR_texture_basisu", "EXT_texture_webp", "KHR_mesh_quantization"].contains(e))
        .map(String::from)
        .collect();
    let dir = path.parent().unwrap_or(Path::new("."));
    let mut max_tex = 0;
    for img in g["images"].as_array().into_iter().flatten() {
        if let Some(uri) = img["uri"].as_str() {
            if uri.starts_with("data:") {
                continue;
            }
            let p = dir.join(percent(uri));
            match fs::read(&p) {
                Ok(b) => max_tex = max_tex.max(image_size(&b).map_or(0, |(w, h)| w.max(h))),
                Err(_) => issues.push(Issue { severity: "error", message: format!("missing texture file `{uri}`") }),
            }
        } else if let Some(bv) = img["bufferView"].as_u64().and_then(|i| g["bufferViews"].get(i as usize)) {
            let off = bv["byteOffset"].as_u64().unwrap_or(0) as usize;
            if let Some(slice) = bin.get(off..off + 64.min(bin.len().saturating_sub(off))) {
                // headers live in the first bytes; JPEG may need more, so fall back to the whole view
                let len = bv["byteLength"].as_u64().unwrap_or(0) as usize;
                let size = image_size(slice).or_else(|| off.checked_add(len).and_then(|e| bin.get(off..e)).and_then(image_size));
                max_tex = max_tex.max(size.map_or(0, |(w, h)| w.max(h)));
            }
        }
    }
    for b in g["buffers"].as_array().into_iter().flatten() {
        if let Some(uri) = b["uri"].as_str().filter(|u| !u.starts_with("data:")) {
            if !dir.join(percent(uri)).is_file() {
                issues.push(Issue { severity: "error", message: format!("missing buffer file `{uri}`") });
            }
        }
    }
    f.max_texture = (max_tex > 0).then_some(max_tex);
    (f, issues)
}

fn percent(s: &str) -> String {
    s.replace("%20", " ")
}

fn obj_facts(text: &str) -> Facts {
    let (mut tris, mut verts) = (0u64, 0u64);
    for l in text.lines() {
        if let Some(rest) = l.strip_prefix("f ") {
            tris += rest.split_whitespace().count().saturating_sub(2) as u64;
        } else if l.starts_with("v ") {
            verts += 1;
        }
    }
    Facts { triangles: Some(tris), vertices: Some(verts), ..Default::default() }
}

fn analyse(root: &Path, p: &Path, kind: &'static str, profile: &str) -> Asset {
    let bytes = fs::metadata(p).map_or(0, |m| m.len());
    let b = budget(profile);
    let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    let mut a = Asset { path: detect::rel(root, p), kind, bytes, facts: Facts::default(), issues: vec![], used_by: vec![] };
    let mut issue = |sev: &'static str, m: String| a.issues.push(Issue { severity: sev, message: m });
    match (kind, ext.as_str()) {
        ("texture", _) => {
            let head = read_head(p, 1 << 16);
            if let Some((w, h)) = image_size(&head) {
                a.facts.width = Some(w);
                a.facts.height = Some(h);
                if w.max(h) > b.max_texture {
                    issue("warning", format!("{w}x{h} exceeds the {profile} budget of {0}px; downscale or ship a {0}px variant", b.max_texture));
                }
                if !w.is_power_of_two() || !h.is_power_of_two() {
                    issue("info", format!("{w}x{h} is not power-of-two: no mipmaps/compression on some GPUs and texture formats"));
                }
            }
        }
        ("audio", "wav") => {
            a.facts.seconds = wav_seconds(&read_head(p, 1 << 16)).map(|s| (s * 100.0).round() / 100.0);
            if bytes > b.max_wav_bytes {
                issue("warning", format!("{} uncompressed WAV; ship OGG/MP3 for {profile}", human(bytes)));
            }
        }
        ("model", "glb" | "gltf") => {
            let all = fs::read(p).unwrap_or_default();
            let (facts, problems) = gltf_facts(p, &all);
            a.facts = facts;
            for x in problems {
                issue(x.severity, x.message);
            }
            if let Some(t) = a.facts.triangles.filter(|t| *t > b.max_triangles) {
                issue("warning", format!("{t} triangles exceeds the {profile} budget of {}; decimate or add LODs", b.max_triangles));
            }
            let draco = a.facts.compression.iter().any(|c| c == "KHR_draco_mesh_compression");
            if profile == "godot" && draco {
                issue("error", "Draco-compressed glTF: Godot 4 cannot import KHR_draco_mesh_compression; re-export without Draco".into());
            }
            if profile == "web" && bytes > b.max_model_bytes && a.facts.compression.is_empty() {
                issue("warning", format!("{} uncompressed model for the web; export with Draco or meshopt and WebP/KTX2 textures", human(bytes)));
            }
            if let Some(t) = a.facts.max_texture.filter(|t| *t > b.max_texture) {
                issue("warning", format!("embedded texture of {t}px exceeds the {profile} budget of {}px", b.max_texture));
            }
        }
        ("model", "obj") => a.facts = obj_facts(&fs::read_to_string(p).unwrap_or_default()),
        ("model", "blend") if profile == "web" => {
            issue("info", "source .blend in a web project: export it (`looka assets export`) and ship the .glb".into())
        }
        ("model", "fbx") if profile == "web" => issue("warning", "FBX is not loadable efficiently on the web; convert to glTF/GLB".into()),
        _ => {}
    }
    a
}

fn read_head(p: &Path, n: usize) -> Vec<u8> {
    use std::io::Read;
    let mut v = Vec::with_capacity(n);
    if let Ok(f) = fs::File::open(p) {
        let _ = f.take(n as u64).read_to_end(&mut v);
    }
    v
}

pub fn human(b: u64) -> String {
    match b {
        b if b >= 1 << 20 => format!("{:.1} MB", b as f64 / (1 << 20) as f64),
        b if b >= 1 << 10 => format!("{:.0} KB", b as f64 / 1024.0),
        b => format!("{b} B"),
    }
}

/// Where a reference written in `from` can point: relative to the file, the project root, `res://`,
/// and the usual static roots that web servers expose at `/`.
fn resolve(root: &Path, from: &Path, r: &str) -> Vec<std::path::PathBuf> {
    if let Some(rest) = r.strip_prefix("res://") {
        // res:// is relative to the directory holding project.godot
        let mut d = from.parent();
        while let Some(dir) = d {
            if dir.join("project.godot").is_file() {
                return vec![dir.join(rest)];
            }
            if dir == root {
                break;
            }
            d = dir.parent();
        }
        return vec![root.join(rest)];
    }
    let rel = r.trim_start_matches("./");
    let mut v = vec![from.parent().unwrap_or(root).join(rel), root.join(rel.trim_start_matches('/'))];
    for s in ["public", "static", "www", "assets", "dist"] {
        v.push(root.join(s).join(rel.trim_start_matches('/')));
    }
    v
}

pub fn report(root: &Path, map: &ProjectMap) -> Report {
    let profile = profile(map);
    let files = detect::walk(root);
    let mut assets: Vec<Asset> = files
        .iter()
        .filter_map(|p| kind_of(p).map(|k| (p, k)))
        .filter(|(p, _)| !p.components().any(|c| c.as_os_str() == ".godot" || c.as_os_str() == ".import"))
        .map(|(p, k)| analyse(root, p, k, &profile))
        .collect();
    let index: BTreeMap<String, usize> = assets.iter().enumerate().map(|(i, a)| (a.path.clone(), i)).collect();

    // Unity: .meta GUIDs let scenes/prefabs/materials reference assets without paths
    let mut guid_of: BTreeMap<String, usize> = BTreeMap::new();
    for (i, a) in assets.iter().enumerate() {
        if let Ok(meta) = fs::read_to_string(root.join(format!("{}.meta", a.path))) {
            if let Some(g) = GUID_REF.captures(&meta) {
                guid_of.insert(g[1].to_string(), i);
            }
        }
    }

    // every GUID any .meta declares (scripts, folders, assets): a scene/prefab GUID outside this set is a dangling
    // reference — Unity shows "Missing Prefab"/"Missing Script". Built-in resources use all-zero-prefixed GUIDs.
    let package_cache = map.components.iter().any(|c| c.ecosystem == "unity" && root.join(&c.dir).join("Library/PackageCache").is_dir());
    let cache_metas: Vec<std::path::PathBuf> = map
        .components
        .iter()
        .filter(|c| c.ecosystem == "unity")
        .flat_map(|c| detect::walk_all(&root.join(&c.dir).join("Library/PackageCache")))
        .collect();
    let declared: BTreeSet<String> = files
        .iter()
        .chain(cache_metas.iter())
        .filter(|f| f.extension().is_some_and(|e| e == "meta"))
        .filter_map(|f| fs::read_to_string(f).ok())
        .filter_map(|t| GUID_REF.captures(&t).map(|c| c[1].to_string()))
        .collect();
    let mut missing = vec![];
    let mut used: Vec<BTreeSet<String>> = vec![BTreeSet::new(); assets.len()];
    let canon_root = fs::canonicalize(root).unwrap_or(root.to_path_buf());
    for f in &files {
        let ext = f.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
        if !REFERRERS.contains(&ext.as_str()) || fs::metadata(f).map_or(true, |m| m.len() > 2 << 20) {
            continue;
        }
        let Ok(text) = fs::read_to_string(f) else { continue };
        let from = detect::rel(root, f);
        let runtime = !non_runtime(&from);
        for c in REF.captures_iter(&text) {
            let r = c[1].trim();
            if r.starts_with("//") {
                continue;
            }
            // built at runtime (`audio/${name}-${i}.m4a`, f"{x}.wav"): every asset matching the pattern is used
            if r.contains('{') {
                let pat = regex::escape(r).replace(r"\$", "").replace(r"\{", "{").replace(r"\}", "}");
                let pat = PLACEHOLDER.replace_all(&pat, "[^/]*").into_owned();
                if let Ok(re) = Regex::new(&format!("(^|/){}$", pat.trim_start_matches("\\./").trim_start_matches('/'))) {
                    for (i, a) in assets.iter().enumerate() {
                        if re.is_match(&a.path) {
                            used[i].insert(from.clone());
                        }
                    }
                }
                continue;
            }
            let hit = resolve(root, f, r)
                .into_iter()
                .filter_map(|p| fs::canonicalize(&p).ok())
                .find_map(|p| p.strip_prefix(&canon_root).ok().and_then(|x| index.get(&x.to_string_lossy().replace('\\', "/")).copied()));
            match hit {
                Some(i) if assets[i].path != from => {
                    used[i].insert(from.clone());
                }
                Some(_) => {}
                // a bare file name may be joined to a base path at runtime (loader.setPath): match by name
                None => match assets.iter().position(|a| a.path.rsplit('/').next() == r.rsplit('/').next() && !r.contains('/')) {
                    Some(i) => {
                        used[i].insert(from.clone());
                    }
                    None if runtime && !r.contains(' ') => missing.push(Missing { referenced: r.to_string(), from: from.clone() }),
                    None => {}
                },
            }
        }
        if matches!(ext.as_str(), "prefab" | "unity" | "mat" | "asset" | "controller" | "anim") {
            for g in GUID_REF.captures_iter(&text) {
                if let Some(&i) = guid_of.get(&g[1]) {
                    used[i].insert(from.clone());
                // package GUIDs (TextMeshPro, URP…) live in Library/PackageCache, absent in clean clones and CI:
                // only judge dangling references when that cache is present
                } else if package_cache && !declared.contains(&g[1]) && !g[1].starts_with("0000000000000000") {
                    missing.push(Missing { referenced: format!("guid:{}", &g[1]), from: from.clone() });
                }
            }
        }
    }
    missing.sort_by(|a, b| (&a.from, &a.referenced).cmp(&(&b.from, &b.referenced)));
    missing.dedup_by(|a, b| a.from == b.from && a.referenced == b.referenced);
    for (a, u) in assets.iter_mut().zip(used) {
        a.used_by = u.into_iter().collect();
    }
    // budgets apply to what ships: referenced assets, static web roots, or anything inside an engine project
    let engine = profile != "web";
    for a in assets.iter_mut() {
        let static_root = ["public/", "static/", "www/"].iter().any(|d| a.path.starts_with(d) || a.path.contains(&format!("/{d}")));
        let loaded = a.used_by.iter().any(|u| !non_runtime(u));
        let shipped = loaded || static_root || (engine && !non_runtime(&a.path));
        if !shipped {
            a.issues.clear();
        }
    }
    let unreferenced = assets
        .iter()
        .filter(|a| a.used_by.iter().all(|u| non_runtime(u)) && matches!(a.kind, "model" | "texture" | "audio") && !a.path.contains("icon") && !non_runtime(&a.path))
        .map(|a| a.path.clone())
        .collect();
    let mut totals: BTreeMap<&'static str, (usize, u64)> = BTreeMap::new();
    for a in &assets {
        let t = totals.entry(a.kind).or_insert((0, 0));
        t.0 += 1;
        t.1 += a.bytes;
    }
    let ok = missing.is_empty() && !assets.iter().any(|a| a.issues.iter().any(|i| i.severity == "error"));
    Report { ok, profile, totals, assets, missing, unreferenced }
}

pub fn render(r: &Report) -> String {
    let mut s = format!("# assets ({} budget): {}\n", r.profile, if r.ok { "PASS" } else { "FAIL" });
    let t: Vec<String> = r.totals.iter().map(|(k, (n, b))| format!("{n} {k} ({})", human(*b))).collect();
    s += &format!("{}\n", if t.is_empty() { "no assets found".into() } else { t.join(" · ") });
    for m in &r.missing {
        s += &format!("  ✗ missing `{}` referenced from {}\n", m.referenced, m.from);
    }
    let mut flagged: Vec<&Asset> = r.assets.iter().filter(|a| a.issues.iter().any(|i| i.severity != "info")).collect();
    flagged.sort_by_key(|a| std::cmp::Reverse(a.bytes));
    for a in flagged.iter().take(40) {
        for i in a.issues.iter().filter(|i| i.severity != "info") {
            s += &format!("  {} {} ({}) — {}\n", if i.severity == "error" { "✗" } else { "⚠" }, a.path, human(a.bytes), i.message);
        }
    }
    let mut models: Vec<&Asset> = r.assets.iter().filter(|a| a.kind == "model").collect();
    models.sort_by_key(|a| std::cmp::Reverse(a.facts.triangles.unwrap_or(0)));
    if !models.is_empty() {
        s += "models:\n";
        for a in models.iter().take(15) {
            let tri = a.facts.triangles.map(|t| format!("{t} tris, ")).unwrap_or_default();
            let comp = if a.facts.compression.is_empty() { String::new() } else { format!(", {}", a.facts.compression.join("+")) };
            let users = if a.used_by.is_empty() { "no static reference".into() } else { format!("used by {}", a.used_by.join(", ")) };
            s += &format!("  {} — {tri}{}{comp} — {users}\n", a.path, human(a.bytes));
        }
    }
    if !r.unreferenced.is_empty() {
        s += &format!("no static reference found ({}; may be loaded dynamically): {}\n", r.unreferenced.len(), r.unreferenced.iter().take(12).cloned().collect::<Vec<_>>().join(", "));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_headers() {
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        png.extend_from_slice(&3000u32.to_be_bytes());
        png.extend_from_slice(&2000u32.to_be_bytes());
        assert_eq!(image_size(&png), Some((3000, 2000)));
        let jpg = [0xFF, 0xD8, 0xFF, 0xE0, 0, 4, 0, 0, 0xFF, 0xC0, 0, 17, 8, 0x01, 0x00, 0x02, 0x00, 3, 0, 0, 0, 0];
        assert_eq!(image_size(&jpg), Some((512, 256)));
    }

    #[test]
    fn gltf_counts_triangles_and_flags_draco() {
        let g = br#"{"asset":{"version":"2.0"},"extensionsUsed":["KHR_draco_mesh_compression"],
            "accessors":[{"count":300},{"count":900}],"meshes":[{"primitives":[{"attributes":{"POSITION":0},"indices":1}]}],
            "materials":[{}]}"#;
        let (f, _) = gltf_facts(Path::new("a.gltf"), g);
        assert_eq!((f.triangles, f.vertices, f.materials), (Some(300), Some(300), Some(1)));
        assert_eq!(f.compression, vec!["KHR_draco_mesh_compression"]);
    }
}
