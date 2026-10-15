//! Just enough YAML for CI workflow files: block maps and sequences, plain/quoted scalars, `|`/`>` block
//! scalars, one-level flow `[a, b]` / `{a: b}`, comments. Everything becomes a JSON value (scalars stay strings).
// ponytail: no anchors, aliases, tags, multi-document or nested flow; workflows rarely use them, and a
// failed parse only means `lkc --ci` finds less to replicate. Upgrade path: a YAML crate.

use serde_json::{Map, Value};

struct Line<'a> {
    ind: usize,
    text: String, // without indentation and trailing comment
    raw: &'a str,
}

fn strip_comment(s: &str) -> &str {
    let (mut sq, mut dq) = (false, false);
    let b = s.as_bytes();
    for (i, c) in s.char_indices() {
        match c {
            '\'' if !dq => sq = !sq,
            '"' if !sq => dq = !dq,
            '#' if !sq && !dq && (i == 0 || b[i - 1] == b' ' || b[i - 1] == b'\t') => return s[..i].trim_end(),
            _ => {}
        }
    }
    s.trim_end()
}

pub fn parse(src: &str) -> Value {
    let lines: Vec<Line> = src
        .lines()
        .map(|raw| {
            let ind = raw.len() - raw.trim_start().len();
            Line { ind, text: strip_comment(raw.trim_start()).to_string(), raw }
        })
        .collect();
    let mut i = 0;
    skip_blank(&lines, &mut i);
    if i >= lines.len() {
        return Value::Null;
    }
    let ind = lines[i].ind;
    block(&lines, &mut i, ind)
}

fn skip_blank(lines: &[Line], i: &mut usize) {
    while *i < lines.len() && (lines[*i].text.is_empty() || lines[*i].text == "---") {
        *i += 1;
    }
}

fn block(lines: &[Line], i: &mut usize, ind: usize) -> Value {
    skip_blank(lines, i);
    if *i < lines.len() && is_item(&lines[*i].text) {
        seq(lines, i, ind)
    } else {
        map(lines, i, ind, None)
    }
}

fn is_item(t: &str) -> bool {
    t == "-" || t.starts_with("- ")
}

fn seq(lines: &[Line], i: &mut usize, ind: usize) -> Value {
    let mut out = vec![];
    loop {
        skip_blank(lines, i);
        if *i >= lines.len() || lines[*i].ind != ind || !is_item(&lines[*i].text) {
            break;
        }
        let rest = lines[*i].text[1..].trim_start().to_string();
        let item_ind = ind + (lines[*i].text.len() - rest.len());
        if rest.is_empty() {
            *i += 1;
            skip_blank(lines, i);
            let child = lines.get(*i).map_or(ind + 2, |l| l.ind);
            out.push(if child > ind { block(lines, i, child) } else { Value::Null });
        } else if key_value(&rest).is_some() {
            // `- key: value` opens a map whose further keys sit at the column of `key`
            out.push(map(lines, i, item_ind, Some(rest)));
        } else {
            *i += 1;
            out.push(scalar(&rest));
        }
    }
    Value::Array(out)
}

fn key_value(t: &str) -> Option<(String, String)> {
    let (k, rest) = if let Some(q) = t.strip_prefix('"').and_then(|s| s.split_once('"')) {
        (q.0.to_string(), q.1)
    } else if let Some(q) = t.strip_prefix('\'').and_then(|s| s.split_once('\'')) {
        (q.0.to_string(), q.1)
    } else {
        let at = t.find(": ").or_else(|| t.strip_suffix(':').map(|s| s.len()))?;
        (t[..at].to_string(), &t[at..])
    };
    let v = rest.strip_prefix(':')?;
    (v.is_empty() || v.starts_with(' ')).then(|| (k.trim().to_string(), v.trim().to_string()))
}

/// `first`: the text after `- ` when the map starts on a sequence item's line.
fn map(lines: &[Line], i: &mut usize, ind: usize, mut first: Option<String>) -> Value {
    let mut out = Map::new();
    loop {
        let text = match first.take() {
            Some(t) => t,
            None => {
                skip_blank(lines, i);
                if *i >= lines.len() || lines[*i].ind != ind || is_item(&lines[*i].text) {
                    break;
                }
                lines[*i].text.clone()
            }
        };
        let Some((k, v)) = key_value(&text) else { break };
        *i += 1;
        let value = if v.starts_with('|') || v.starts_with('>') {
            block_scalar(lines, i, ind, v.starts_with('>'))
        } else if v.is_empty() {
            skip_blank(lines, i);
            match lines.get(*i) {
                // GitHub style puts a key's sequence at the key's own indentation
                Some(l) if l.ind > ind || (l.ind == ind && is_item(&l.text)) => block(lines, i, l.ind),
                _ => Value::Null,
            }
        } else {
            scalar(&v)
        };
        out.insert(k, value);
    }
    Value::Object(out)
}

fn block_scalar(lines: &[Line], i: &mut usize, ind: usize, folded: bool) -> Value {
    let mut body: Vec<&str> = vec![];
    let mut base = None;
    while *i < lines.len() && (lines[*i].raw.trim().is_empty() || lines[*i].ind > ind) {
        let raw = lines[*i].raw;
        if !raw.trim().is_empty() {
            let b = *base.get_or_insert(lines[*i].ind);
            body.push(raw.get(b..).unwrap_or(raw.trim_start()));
        } else {
            body.push("");
        }
        *i += 1;
    }
    while body.last() == Some(&"") {
        body.pop();
    }
    Value::String(if folded { body.join(" ") } else { body.join("\n") })
}

fn scalar(v: &str) -> Value {
    let v = v.trim();
    if let Some(inner) = v.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
        return Value::Array(inner.split(',').map(str::trim).filter(|s| !s.is_empty()).map(scalar).collect());
    }
    if let Some(inner) = v.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
        let mut m = Map::new();
        for part in inner.split(',') {
            if let Some((k, val)) = part.split_once(':') {
                m.insert(k.trim().to_string(), scalar(val));
            }
        }
        return Value::Object(m);
    }
    for q in ['"', '\''] {
        if v.len() >= 2 && v.starts_with(q) && v.ends_with(q) {
            return Value::String(v[1..v.len() - 1].to_string());
        }
    }
    Value::String(v.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_workflow_subset() {
        let y = parse(
            "name: CI # pipeline\non:\n  push:\n    branches: [main]\njobs:\n  test:\n    runs-on: ubuntu-latest\n    services:\n      postgres:\n        image: postgres:15-alpine\n        env:\n          POSTGRES_PASSWORD: password\n        ports:\n          - 5432:5432\n        options: >-\n          --health-cmd pg_isready\n          --health-retries 5\n    env:\n      DATABASE_URL: \"postgresql://postgres:password@localhost:5432/db?schema=public\"\n      URL_WITH_HASH: http://x/#frag\n    steps:\n    - uses: actions/checkout@v4\n    - name: Setup DB\n      run: npm run db:test:setup\n    - run: |\n        npm test\n        # comment kept in scripts\n        echo done\n",
        );
        let job = &y["jobs"]["test"];
        assert_eq!(job["services"]["postgres"]["image"], "postgres:15-alpine");
        assert_eq!(job["services"]["postgres"]["ports"][0], "5432:5432");
        assert_eq!(job["services"]["postgres"]["options"], "--health-cmd pg_isready --health-retries 5");
        assert_eq!(job["env"]["DATABASE_URL"], "postgresql://postgres:password@localhost:5432/db?schema=public");
        assert_eq!(job["env"]["URL_WITH_HASH"], "http://x/#frag");
        assert_eq!(job["steps"][1]["name"], "Setup DB");
        assert_eq!(job["steps"][1]["run"], "npm run db:test:setup");
        assert_eq!(job["steps"][2]["run"], "npm test\n# comment kept in scripts\necho done");
        assert_eq!(y["on"]["push"]["branches"][0], "main");
    }
}
