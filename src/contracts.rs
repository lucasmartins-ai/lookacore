//! Cross-stack contracts: the HTTP endpoints each component serves and the calls other components make to them
//! (web client → API, Godot/Unity game → backend, Python worker → service). Broken calls and unused endpoints
//! are where teams working in different stacks miss each other.

use crate::detect::{self, ProjectMap};
use regex::Regex;
use serde::Serialize;
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::sync::LazyLock;

#[derive(Serialize, Clone, Debug)]
pub struct Endpoint {
    pub method: String,
    pub path: String,
    pub file: String,
    pub line: usize,
    pub component: String,
    pub framework: &'static str,
    pub callers: Vec<String>,
}

#[derive(Serialize, Clone, Debug)]
pub struct Call {
    pub method: String,
    pub path: String,
    pub file: String,
    pub line: usize,
    pub component: String,
}

#[derive(Serialize, Debug)]
pub struct Contracts {
    pub ok: bool,
    pub endpoints: Vec<Endpoint>,
    pub broken: Vec<Call>,
    pub unused: Vec<String>,
    pub calls: usize,
}

macro_rules! re {
    ($s:expr) => {
        LazyLock::new(|| Regex::new($s).unwrap())
    };
}
// servers
// `api.get('/x')` / `r.get` are usually axios/ky instances on the client: not listed as server receivers
static EXPRESS: LazyLock<Regex> = re!(r#"\b(?:app|router|server|fastify|hono|routes)\s*\.\s*(get|post|put|patch|delete|all)\s*\(\s*["'`](/[^"'`]*)["'`]"#);
static PY_ROUTE: LazyLock<Regex> = re!(r#"@\w+(?:\.\w+)*\.(get|post|put|patch|delete|route|api_route)\(\s*["'](/[^"']*)["']"#);
static AXUM: LazyLock<Regex> = re!(r#"\.route\(\s*"(/[^"]*)"\s*,\s*([a-z_]+)\("#);
static GO_HTTP: LazyLock<Regex> = re!(r#"\b(?:HandleFunc|Handle|GET|POST|PUT|PATCH|DELETE)\(\s*"(?:(GET|POST|PUT|PATCH|DELETE) )?(/[^"]*)""#);
static NEXT_METHOD: LazyLock<Regex> = re!(r"(?m)^export\s+(?:async\s+)?(?:function|const)\s+(GET|POST|PUT|PATCH|DELETE|HEAD|OPTIONS)\b");
// clients
static FETCH: LazyLock<Regex> = re!(r#"\b(?:fetch|axios(?:\.(get|post|put|patch|delete))?|ky(?:\.(get|post|put|patch|delete))?|\$fetch|useSWR|useFetch)\s*\(\s*(?:[\w.]+\s*\+\s*)?["'`]((?:https?://[^/"'`]+|\$\{[\w.]+\})?/[^"'`]*)["'`](\s*,\s*\{)?"#);
static METHOD_OPT: LazyLock<Regex> = re!(r#"\bmethod\s*:\s*["'](\w+)["']"#);
/// API-looking path literals in files that do HTTP through a variable (`fetch(endpoint)`, `getHttpEndpoint('/api/x')`)
// at least one segment after the prefix: `startsWith('/api/')` is a prefix test, not a call
static API_LITERAL: LazyLock<Regex> = re!(r#"["'`](?:\$\{[\w.]+\})?(/(?:api|v\d+|rpc)/[^"'`\s/][^"'`\s]*|/graphql)["'`]"#);
// an actual HTTP call, not just the word: server files listing route prefixes must not count as clients
static HTTP_CLIENT: LazyLock<Regex> = re!(r"(?:\bfetch|\baxios(?:\.\w+)?|\bky(?:\.\w+)?|\$fetch|sendBeacon)\s*\(|\bnew\s+(?:WebSocket|EventSource|XMLHttpRequest)\b|UnityWebRequest\.|HTTPRequest");
/// `${PLAUSIBLE_API_HOST}/api/event`: a base URL named after a third-party service is not this project's API.
// ponytail: name-based; a project-owned base called e.g. STRIPE_PROXY would be skipped too.
static THIRD_PARTY: LazyLock<Regex> = re!(r"(?i)plausible|posthog|segment|mixpanel|amplitude|sentry|stripe|supabase|firebase|google|openai|anthropic|jina|algolia|cloudinary|mapbox|twilio|sendgrid|resend|hubspot|intercom");
static GODOT_HTTP: LazyLock<Regex> = re!(r#"\.request\(\s*(?:[A-Za-z_]+\s*\+\s*)?"((?:https?://[^/"]+)?/[^"]*)"(?:[^)]*HTTPClient\.METHOD_(\w+))?"#);
static UNITY_HTTP: LazyLock<Regex> = re!(r#"UnityWebRequest(?:\.|\s*\.\s*)(Get|Post|Put|Delete)\(\s*(?:[A-Za-z_.]+\s*\+\s*)?"((?:https?://[^/"]+)?/[^"]*)""#);
static PY_CLIENT: LazyLock<Regex> = re!(r#"\b(?:requests|httpx|client|session)\.(get|post|put|patch|delete)\(\s*f?["']((?:https?://[^/"']+|\{[\w.]+\})?/[^"']*)["']"#);
static TEMPLATE_EXPR: LazyLock<Regex> = re!(r"\$\{[^}]*\}");
static CATCH_ALL: LazyLock<Regex> = re!(r"\[\[?\.\.\.[^\]]+\]\]?|\{[^}]*:path\}|\*\w*$");
static PARAM: LazyLock<Regex> = re!(r"\[[^\]]+\]|:[A-Za-z_]\w*|\{[^}]*\}|\$\{[^}]*\}|<[^>]+>");

/// `/api/foods/[slug]`, `/api/foods/:slug`, `/api/foods/{slug}`, `/api/foods/${s}` → `/api/foods/*`
pub fn normalize(path: &str) -> String {
    // template expressions first: `${data?.id}` holds a `?` that is not a query string
    let templ = TEMPLATE_EXPR.replace_all(path, "{x}");
    let p = templ.split(['?', '#']).next().unwrap_or(&templ);
    // `${base}/api/x` → `/api/x`; an unterminated `${…` (spaces inside the expression) becomes one parameter
    // a leading base (`${API}`, f-string `{BASE}`) is the host, not a path segment
    let p = p.strip_prefix('{').and_then(|r| r.split_once('}')).filter(|(_, rest)| rest.starts_with('/')).map_or(p, |(_, rest)| rest);
    let cut;
    let p = match p.find("${") {
        Some(i) if !p[i..].contains('}') => {
            cut = format!("{}{{x}}", &p[..i]);
            cut.as_str()
        }
        _ => p,
    };
    let p = p.split_once("://").map_or(p, |(_, rest)| rest.find('/').map_or("/", |i| &rest[i..]));
    let p = CATCH_ALL.replace_all(p, "**");
    let p = PARAM.replace_all(&p, "*");
    let p = p.trim_end_matches('/');
    if p.is_empty() { "/".into() } else { p.to_string() }
}

fn line_at(text: &str, off: usize) -> usize {
    text[..off].bytes().filter(|b| *b == b'\n').count() + 1
}

/// Next.js app-router `route.ts` and pages-router `pages/api/**` paths.
fn next_route(rel: &str) -> Option<String> {
    let segs: Vec<&str> = rel.split('/').collect();
    let file = *segs.last()?;
    let stem = file.rsplit_once('.').map_or(file, |(s, _)| s);
    if let Some(i) = segs.iter().position(|s| *s == "app") {
        if stem != "route" {
            return None;
        }
        let parts: Vec<&str> = segs[i + 1..segs.len() - 1].iter().copied().filter(|s| !s.starts_with('(') && !s.starts_with('@')).collect();
        return Some(format!("/{}", parts.join("/")));
    }
    if let Some(i) = segs.iter().position(|s| *s == "pages") {
        if segs.get(i + 1) == Some(&"api") {
            let mut parts: Vec<&str> = segs[i + 1..segs.len() - 1].to_vec();
            if stem != "index" {
                parts.push(stem);
            }
            return Some(format!("/{}", parts.join("/")));
        }
    }
    None
}

fn matches(endpoint: &str, call: &str) -> bool {
    let (e, c): (Vec<&str>, Vec<&str>) = (endpoint.split('/').collect(), call.split('/').collect());
    // a catch-all ([...slug], {p:path}) normalizes to `**` and swallows the rest of the call path
    let catch_all = e.last() == Some(&"**");
    let prefix = if catch_all { &e[..e.len() - 1] } else { &e[..] };
    (e.len() == c.len() || (catch_all && c.len() >= prefix.len())) && prefix.iter().zip(&c).all(|(a, b)| a == b || *a == "*" || *b == "*")
}

pub fn contracts(root: &Path, map: &ProjectMap) -> Contracts {
    let (mut endpoints, mut calls) = (vec![], vec![]);
    for p in detect::walk(root) {
        let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
        if !matches!(ext, "ts" | "tsx" | "js" | "mjs" | "cjs" | "jsx" | "py" | "rs" | "go" | "gd" | "cs" | "vue" | "svelte") {
            continue;
        }
        let rel = detect::rel(root, &p);
        if detect::is_test(&rel) || fs::metadata(&p).map_or(true, |m| m.len() > 512_000) {
            continue;
        }
        let Ok(text) = fs::read_to_string(&p) else { continue };
        let component = map.owner(&rel).map_or(".".to_string(), |c| c.dir.clone());
        let mut ep = |method: &str, path: &str, off: usize, framework: &'static str| {
            endpoints.push(Endpoint {
                method: method.to_uppercase(),
                path: normalize(path),
                file: rel.clone(),
                line: line_at(&text, off),
                component: component.clone(),
                framework,
                callers: vec![],
            })
        };
        if let Some(route) = next_route(&rel) {
            let methods: Vec<_> = NEXT_METHOD.captures_iter(&text).map(|c| (c[1].to_string(), c.get(0).unwrap().start())).collect();
            for (m, off) in methods {
                ep(&m, &route, off, "next");
            }
        }
        for c in EXPRESS.captures_iter(&text) {
            ep(&c[1], &c[2], c.get(0).unwrap().start(), "express");
        }
        for c in PY_ROUTE.captures_iter(&text) {
            let m = if matches!(&c[1], "route" | "api_route") { "ANY" } else { &c[1] };
            ep(m, &c[2], c.get(0).unwrap().start(), "python");
        }
        for c in AXUM.captures_iter(&text) {
            ep(&c[2], &c[1], c.get(0).unwrap().start(), "axum");
        }
        for c in GO_HTTP.captures_iter(&text) {
            ep(c.get(1).map_or("ANY", |m| m.as_str()), &c[2], c.get(0).unwrap().start(), "go");
        }
        let mut call = |method: Option<&str>, path: &str, off: usize| {
            calls.push(Call {
                method: method.unwrap_or("GET").to_uppercase(),
                path: normalize(path),
                file: rel.clone(),
                line: line_at(&text, off),
                component: component.clone(),
            })
        };
        let mut seen_calls: BTreeSet<String> = BTreeSet::new();
        for c in FETCH.captures_iter(&text) {
            // third-party URLs are not this project's contract; localhost and `${base}` hosts are
            let url = &c[3];
            if (url.starts_with("http") && !url.contains("localhost") && !url.contains("127.0.0.1")) || THIRD_PARTY.is_match(url.split('/').next().unwrap_or("")) {
                continue;
            }
            seen_calls.insert(normalize(url));
            // options object: look for `method:` in the next few hundred bytes (headers objects nest braces);
            // an options object with no visible method could be anything
            let m = c.get(1).or(c.get(2)).map(|m| m.as_str().to_string()).or_else(|| {
                c.get(4).map(|o| {
                    let tail = &text[o.end()..];
                    let window = &tail[..tail.char_indices().nth(400).map_or(tail.len(), |(i, _)| i)];
                    METHOD_OPT.captures(window).map_or("ANY".to_string(), |mm| mm[1].to_string())
                })
            });
            call(m.as_deref(), url, c.get(0).unwrap().start());
        }
        let serves = endpoints.iter().any(|e| e.file == rel);
        let name = rel.rsplit('/').next().unwrap_or(&rel);
        let middleware = name.starts_with("middleware.") || name.starts_with("proxy.");
        if HTTP_CLIENT.is_match(&text) && !serves && !middleware && !matches!(ext, "py" | "rs" | "go") {
            for c in API_LITERAL.captures_iter(&text) {
                let start = c.get(0).unwrap().start();
                let line_start = text[..start].rfind('\n').map_or(0, |i| i + 1);
                let line = text[line_start..].lines().next().unwrap_or("").trim_start();
                let commented = line.starts_with("//") || line.starts_with('*') || line.starts_with("/*") || line.starts_with('#');
                let third_party = THIRD_PARTY.is_match(&c[0]);
                if !commented && !third_party && seen_calls.insert(normalize(&c[1])) {
                    call(Some("ANY"), &c[1], start);
                }
            }
        }
        for c in GODOT_HTTP.captures_iter(&text) {
            call(c.get(2).map(|m| m.as_str()), &c[1], c.get(0).unwrap().start());
        }
        for c in UNITY_HTTP.captures_iter(&text) {
            call(Some(&c[1]), &c[2], c.get(0).unwrap().start());
        }
        for c in PY_CLIENT.captures_iter(&text) {
            call(Some(&c[1]), &c[2], c.get(0).unwrap().start());
        }
    }
    let mut broken = vec![];
    for c in &calls {
        let hits: Vec<usize> = endpoints
            .iter()
            .enumerate()
            .filter(|(_, e)| matches(&e.path, &c.path) && (e.method == c.method || ["ANY", "ALL"].contains(&e.method.as_str()) || c.method == "ANY"))
            .map(|(i, _)| i)
            .collect();
        for &i in &hits {
            let who = format!("{}:{}", c.file, c.line);
            if !endpoints[i].callers.contains(&who) {
                endpoints[i].callers.push(who);
            }
        }
        // only calls into a space this project serves can be judged broken (not third-party URLs)
        let served_prefix = c.path.split('/').nth(1).is_some_and(|seg| endpoints.iter().any(|e| e.path.split('/').nth(1) == Some(seg)));
        if hits.is_empty() && served_prefix {
            broken.push(c.clone());
        }
    }
    let unused: Vec<String> = endpoints
        .iter()
        .filter(|e| e.callers.is_empty())
        .map(|e| format!("{} {} ({}:{})", e.method, e.path, e.file, e.line))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    endpoints.sort_by(|a, b| (&a.path, &a.method).cmp(&(&b.path, &b.method)));
    Contracts { ok: broken.is_empty(), calls: calls.len(), endpoints, broken, unused }
}

pub fn render(c: &Contracts) -> String {
    let comps: BTreeSet<&str> = c.endpoints.iter().map(|e| e.component.as_str()).collect();
    let mut s = format!(
        "# contracts: {}\n{} endpoints in {} component(s) · {} client calls · {} broken · {} without callers\n",
        if c.ok { "PASS" } else { "FAIL" },
        c.endpoints.len(),
        comps.len(),
        c.calls,
        c.broken.len(),
        c.unused.len()
    );
    for b in &c.broken {
        s += &format!("  ✗ {} {} called from {}:{} ({}) — no endpoint serves it\n", b.method, b.path, b.file, b.line, b.component);
    }
    for e in c.endpoints.iter().filter(|e| !e.callers.is_empty()).take(40) {
        s += &format!("  {} {} ({}:{}) ← {}\n", e.method, e.path, e.file, e.line, e.callers.join(", "));
    }
    if !c.unused.is_empty() {
        s += &format!("no caller found in this repo ({}; may be called by other repos, webhooks or crons):\n", c.unused.len());
        for u in c.unused.iter().take(15) {
            s += &format!("  {u}\n");
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_params_across_frameworks() {
        for p in ["/api/foods/[slug]", "/api/foods/:slug", "/api/foods/{slug}", "/api/foods/${slug}", "https://x.dev/api/foods/<slug>/"] {
            assert_eq!(normalize(p), "/api/foods/*", "{p}");
        }
        assert_eq!(next_route("app/api/(admin)/users/[id]/route.ts").as_deref(), Some("/api/users/[id]"));
        assert_eq!(next_route("pages/api/health/index.ts").as_deref(), Some("/api/health"));
        assert_eq!(normalize("/api/files/[...path]"), "/api/files/**");
        assert_eq!(normalize("${API_BASE}/api/pair"), "/api/pair");
        assert_eq!(normalize("/api/admin/foods/queue/${data"), "/api/admin/foods/queue/*");
        assert_eq!(normalize("/api/admin/foods/queue/${data?.submissionId}/action"), "/api/admin/foods/queue/*/action");
    }

    #[test]
    fn client_instances_and_options_methods() {
        let root = std::env::temp_dir().join(format!("looka-contracts-{}", std::process::id()));
        std::fs::create_dir_all(root.join("app/api/x")).unwrap();
        std::fs::write(root.join("app/api/x/route.ts"), "export async function POST() {}\n").unwrap();
        // axios instance named `api` is a client, and `method:` after a nested headers object is still found
        std::fs::write(root.join("client.ts"), "const api = axios.create();\napi.get('/api/users');\nfetch('/api/x', { headers: { a: 1 }, method: 'POST' });\nconst s = \"fetch('/api/cardápio/ééééééééééééééééééééééééééééééééééééééééééé')\";\n").unwrap();
        let map = crate::detect::detect(&root);
        let c = contracts(&root, &map);
        assert!(c.endpoints.iter().all(|e| e.file != "client.ts"), "{:?}", c.endpoints);
        assert!(c.broken.iter().all(|b| b.path != "/api/x"), "{:?}", c.broken);
        let _ = std::fs::remove_dir_all(&root);
        assert!(matches("/api/files/**", "/api/files/a/b"));
        assert!(!matches("/api/users/*", "/api/users/1/orders"));
        assert!(!matches("/api/users/*", "/api/orders/1"));
    }
}
