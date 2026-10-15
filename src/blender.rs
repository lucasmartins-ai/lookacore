//! Blender → engine: inspect .blend scenes and export them with the settings each target needs, headless.
//! three/web: GLB + Draco + WebP; godot: GLB without Draco (Godot 4 can't import it); unity/unreal: FBX with
//! the axis/scale conventions of each engine.

use crate::assets;
use serde::Serialize;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::Duration;

const SCRIPT: &str = r#"
import bpy, json, os, sys
args = json.loads(sys.argv[sys.argv.index('--') + 1])
def stats():
    deps = bpy.context.evaluated_depsgraph_get()
    objs, tris = [], 0
    for o in bpy.context.scene.objects:
        info = {'name': o.name, 'type': o.type}
        if o.type == 'MESH':
            ev = o.evaluated_get(deps)
            m = ev.to_mesh()
            m.calc_loop_triangles()
            info['triangles'] = len(m.loop_triangles)
            info['vertices'] = len(m.vertices)
            tris += len(m.loop_triangles)
            ev.to_mesh_clear()
            info['modifiers'] = [md.type for md in o.modifiers]
            info['materials'] = [s.material.name for s in o.material_slots if s.material]
            info['scale'] = [round(v, 4) for v in o.scale]
        objs.append(info)
    missing = [i.filepath for i in bpy.data.images
               if i.source == 'FILE' and i.filepath and not i.packed_file and not os.path.exists(bpy.path.abspath(i.filepath))]
    images = [{'name': i.name, 'size': list(i.size), 'packed': bool(i.packed_file)} for i in bpy.data.images if i.source in ('FILE', 'GENERATED')]
    return {'blender': bpy.app.version_string, 'objects': objs, 'triangles': tris, 'missing_textures': missing,
            'images': images, 'unit_scale': bpy.context.scene.unit_settings.scale_length,
            'animations': [a.name for a in bpy.data.actions]}
out = {'stats': stats()}
if args['mode'] == 'export':
    target, path = args['target'], args['out']
    os.makedirs(os.path.dirname(path) or '.', exist_ok=True)
    if target in ('three', 'web', 'godot'):
        kw = dict(filepath=path, export_format='GLB', export_apply=True, export_yup=True)
        if target in ('three', 'web'):
            kw.update(export_draco_mesh_compression_enable=True, export_draco_mesh_compression_level=6, export_image_format='WEBP')
        bpy.ops.export_scene.gltf(**kw)
    else:
        kw = dict(filepath=path, use_mesh_modifiers=True, mesh_smooth_type='FACE', add_leaf_bones=False, path_mode='COPY', embed_textures=True)
        if target == 'unity':
            kw.update(apply_unit_scale=True, apply_scale_options='FBX_SCALE_UNITS', bake_space_transform=True, axis_forward='-Z', axis_up='Y')
        else:  # unreal: Z up, centimetres
            kw.update(apply_unit_scale=True, apply_scale_options='FBX_SCALE_ALL', axis_forward='-Y', axis_up='Z')
        bpy.ops.export_scene.fbx(**kw)
    out['exported'] = path
with open(args['result'], 'w') as fh:
    json.dump(out, fh)
"#;

#[derive(Serialize)]
pub struct BlendResult {
    pub ok: bool,
    pub source: String,
    pub target: Option<String>,
    pub exported: Option<String>,
    pub exported_bytes: Option<u64>,
    pub exported_facts: Option<assets::Facts>,
    pub stats: Value,
    pub issues: Vec<String>,
    pub log_tail: String,
}

pub fn binary() -> Option<PathBuf> {
    if let Ok(b) = std::env::var("LOOKA_BLENDER") {
        return Some(PathBuf::from(b)).filter(|p| p.is_file());
    }
    std::env::var_os("PATH")
        .and_then(|p| std::env::split_paths(&p).map(|d| d.join("blender")).find(|f| f.is_file()))
        .or_else(|| Some(PathBuf::from("/Applications/Blender.app/Contents/MacOS/Blender")).filter(|p| p.is_file()))
}

/// Where an export lands by default, following each engine's conventions.
pub fn default_out(root: &Path, src: &Path, target: &str) -> PathBuf {
    let name = src.file_stem().and_then(|s| s.to_str()).unwrap_or("model");
    let (dir, ext) = match target {
        "godot" => ("assets/models", "glb"),
        "unity" => ("Assets/Models", "fbx"),
        "unreal" => ("SourceArt", "fbx"),
        _ if root.join("public").is_dir() => ("public/models", "glb"),
        _ => ("assets/models", "glb"),
    };
    root.join(dir).join(format!("{name}.{ext}"))
}

pub fn run(root: &Path, src: &Path, export: Option<(&str, PathBuf)>) -> BlendResult {
    let mut r = BlendResult {
        ok: false,
        source: crate::detect::rel(root, src),
        target: export.as_ref().map(|(t, _)| t.to_string()),
        exported: None,
        exported_bytes: None,
        exported_facts: None,
        stats: Value::Null,
        issues: vec![],
        log_tail: String::new(),
    };
    let Some(bin) = binary() else {
        r.issues.push("Blender not found: install it or set LOOKA_BLENDER".into());
        return r;
    };
    // result file inside the project's .looka/, created exclusively so nobody can pre-plant it or a symlink
    let dir = root.join(".looka");
    let _ = std::fs::create_dir_all(&dir);
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let result = dir.join(format!("blend-{}-{nanos}.json", std::process::id()));
    if std::fs::OpenOptions::new().write(true).create_new(true).open(&result).is_err() {
        r.issues.push("could not create a private result file under .looka/".into());
        return r;
    }
    let args = match &export {
        Some((t, out)) => serde_json::json!({"mode": "export", "target": t, "out": out, "result": result}),
        None => serde_json::json!({"mode": "inspect", "result": result}),
    };
    let cmd = crate::detect::Cmd {
        kind: "blender",
        run: format!(
            "{} -b {} --python-exit-code 1 --python-expr {} -- {}",
            shell_quote(&bin.display().to_string()),
            shell_quote(&src.display().to_string()),
            shell_quote(SCRIPT),
            shell_quote(&args.to_string())
        ),
        available: true,
    };
    let comp = crate::detect::Component {
        dir: ".".into(),
        ecosystem: "blender",
        manifest: String::new(),
        name: None,
        frameworks: vec![],
        commands: vec![],
    };
    let ev = crate::run::exec(root, &comp, &cmd, Duration::from_secs(600));
    r.log_tail = ev.output_tail.clone();
    let json = std::fs::read_to_string(&result).ok().and_then(|j| serde_json::from_str::<Value>(&j).ok());
    let _ = std::fs::remove_file(&result);
    let Some(out) = json else {
        r.issues.push(format!("Blender did not finish (exit {:?}); see log_tail", ev.exit_code));
        return r;
    };
    r.stats = out["stats"].clone();
    for m in r.stats["missing_textures"].as_array().into_iter().flatten().filter_map(Value::as_str) {
        r.issues.push(format!("missing texture `{m}` (not packed, file not found)"));
    }
    if let Some(s) = r.stats["unit_scale"].as_f64().filter(|s| (*s - 1.0).abs() > 1e-6) {
        r.issues.push(format!("scene unit scale is {s}; engines expect 1.0 (metres)"));
    }
    for o in r.stats["objects"].as_array().into_iter().flatten() {
        let scaled = o["scale"].as_array().is_some_and(|v| v.iter().any(|x| (x.as_f64().unwrap_or(1.0) - 1.0).abs() > 1e-3));
        if scaled {
            r.issues.push(format!("object `{}` has unapplied scale {}; apply it (Ctrl+A) to avoid physics/LOD surprises", o["name"].as_str().unwrap_or("?"), o["scale"]));
        }
    }
    if let Some(p) = out["exported"].as_str() {
        let p = PathBuf::from(p);
        r.exported = Some(crate::detect::rel(root, &p));
        r.exported_bytes = std::fs::metadata(&p).ok().map(|m| m.len());
        if p.extension().is_some_and(|e| e == "glb") {
            r.exported_facts = std::fs::read(&p).ok().map(|b| assets::gltf_facts(&p, &b).0);
        }
    }
    r.ok = ev.exit_code == Some(0) && r.issues.iter().all(|i| !i.starts_with("missing"));
    r
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

pub fn render(r: &BlendResult) -> String {
    let mut s = format!("# blender: {} {}\n", r.source, if r.ok { "OK" } else { "FAIL" });
    if let Some(v) = r.stats["blender"].as_str() {
        s += &format!("Blender {v} · {} triangles (after modifiers)\n", r.stats["triangles"]);
    }
    for o in r.stats["objects"].as_array().into_iter().flatten().filter(|o| o["type"] == "MESH") {
        let mods = o["modifiers"].as_array().map(|m| m.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(",")).unwrap_or_default();
        s += &format!("  {} — {} tris{}\n", o["name"].as_str().unwrap_or("?"), o["triangles"], if mods.is_empty() { String::new() } else { format!(", modifiers: {mods}") });
    }
    if let (Some(t), Some(e)) = (&r.target, &r.exported) {
        let size = r.exported_bytes.map(assets::human).unwrap_or_default();
        let comp = r.exported_facts.as_ref().map(|f| f.compression.join("+")).filter(|c| !c.is_empty()).map(|c| format!(", {c}")).unwrap_or_default();
        s += &format!("exported for {t}: {e} ({size}{comp})\n");
    }
    for i in &r.issues {
        s += &format!("  ⚠ {i}\n");
    }
    if !r.ok && r.stats.is_null() {
        let lines: Vec<&str> = r.log_tail.lines().collect();
        s += &lines[lines.len().saturating_sub(12)..].iter().map(|l| format!("    {l}\n")).collect::<String>();
    }
    s
}
