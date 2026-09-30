use regex::Regex;
use std::collections::HashMap;

/// gitleaks v8.30.1's default config, byte for byte (MIT, see third_party/gitleaks/LICENSE).
const CONFIG: &str = include_str!("../third_party/gitleaks/gitleaks.toml");

/// The secret scan every stored text goes through (tickets 22 and 23): gitleaks' rules, run the way
/// gitleaks' detect.go runs them, minus the rules that need a file path.
pub struct Scanner {
    rules: Vec<Rule>,
    global: Vec<Allow>,
}

struct Rule {
    id: String,
    re: Regex,
    group: usize,
    entropy: f64,
    keywords: Vec<String>,
    allow: Vec<Allow>,
}

struct Allow {
    regexes: Vec<Regex>,
    stopwords: Vec<String>,
    target: String,
    and: bool,
    has_paths: bool,
}

type Table = HashMap<String, Vec<String>>;

impl Scanner {
    // ken: compiles all 217 rules each run (47 ms, off the prompt path); compile only the rules whose
    // keywords appear if a prompt-path caller ever needs the scan.
    pub fn new() -> Scanner {
        let mut rules: Vec<(Table, Vec<Table>)> = vec![];
        let mut global = vec![];
        for (header, table) in tables(CONFIG) {
            match header.as_str() {
                "rules" => rules.push((table, vec![])),
                "rules.allowlists" => rules
                    .last_mut()
                    .into_iter()
                    .for_each(|r| r.1.push(table.clone())),
                "allowlist" => global.push(allow(&table)),
                _ => {}
            }
        }
        let rules = rules
            .into_iter()
            .filter(|(t, _)| !t.contains_key("path"))
            .filter_map(|(t, allows)| {
                let one = |k: &str| t.get(k).and_then(|v| v.first());
                Some(Rule {
                    id: one("id")?.clone(),
                    re: Regex::new(&go_to_rust(one("regex")?)).ok()?,
                    group: one("secretGroup").and_then(|g| g.parse().ok()).unwrap_or(0),
                    entropy: one("entropy").and_then(|e| e.parse().ok()).unwrap_or(0.0),
                    keywords: t
                        .get("keywords")
                        .into_iter()
                        .flatten()
                        .map(|k| k.to_lowercase())
                        .collect(),
                    allow: allows.iter().map(allow).collect(),
                })
            })
            .collect();
        Scanner { rules, global }
    }

    /// The text with every secret replaced by `[REDACTED:<rule-id>]`, and the ids of the rules that hit.
    pub fn redact(&self, text: &str) -> (String, Vec<String>) {
        let low = text.to_lowercase();
        let mut hits = vec![];
        for r in &self.rules {
            if !r.keywords.is_empty() && !r.keywords.iter().any(|k| low.contains(k.as_str())) {
                continue;
            }
            for c in r.re.captures_iter(text) {
                let m = c.get(0).unwrap();
                let lead = m.as_str().len() - m.as_str().trim_start_matches('\n').len();
                let matched = m.as_str().trim_matches('\n');
                let line_start = text[..m.start()].rfind('\n').map_or(0, |i| i + 1);
                let line_end = text[m.end()..]
                    .find('\n')
                    .map_or(text.len(), |i| m.end() + i);
                let line = &text[line_start..line_end];
                if matched.is_empty() || line.contains("gitleaks:allow") {
                    continue;
                }
                let secret = if r.group > 0 {
                    c.get(r.group).map_or("", |g| g.as_str())
                } else {
                    (1..c.len())
                        .filter_map(|i| c.get(i))
                        .map(|g| g.as_str())
                        .find(|s| !s.is_empty())
                        .unwrap_or(matched)
                };
                if r.entropy > 0.0 && shannon(secret) <= r.entropy {
                    continue;
                }
                if allowed(&self.global, secret, matched, line)
                    || allowed(&r.allow, secret, matched, line)
                {
                    continue;
                }
                hits.push((
                    m.start() + lead,
                    m.start() + lead + matched.len(),
                    r.id.as_str(),
                ));
            }
        }
        hits.sort();
        let (mut out, mut at, mut ids) = (String::new(), 0, Vec::<String>::new());
        for (start, end, id) in hits {
            if !ids.iter().any(|i| i == id) {
                ids.push(id.to_string());
            }
            if start >= at {
                out += &text[at..start];
                out += &format!("[REDACTED:{id}]");
            }
            at = at.max(end);
        }
        out += &text[at..];
        (out, ids)
    }
}

fn allow(t: &Table) -> Allow {
    let one = |k: &str| {
        t.get(k)
            .and_then(|v| v.first())
            .cloned()
            .unwrap_or_default()
    };
    Allow {
        regexes: t
            .get("regexes")
            .into_iter()
            .flatten()
            .filter_map(|r| Regex::new(&go_to_rust(r)).ok())
            .collect(),
        stopwords: t
            .get("stopwords")
            .into_iter()
            .flatten()
            .map(|w| w.to_lowercase())
            .collect(),
        target: one("regexTarget"),
        and: one("condition").eq_ignore_ascii_case("and"),
        has_paths: t.contains_key("paths") || t.contains_key("commits"),
    }
}

fn allowed(lists: &[Allow], secret: &str, matched: &str, line: &str) -> bool {
    let low = secret.to_lowercase();
    lists.iter().any(|a| {
        let target = match a.target.as_str() {
            "match" => matched,
            "line" => line,
            _ => secret,
        };
        let rx = a.regexes.iter().any(|r| r.is_match(target));
        let sw = a.stopwords.iter().any(|w| low.contains(w.as_str()));
        if !a.and {
            return rx || sw;
        }
        let checks = [
            a.has_paths.then_some(false),
            (!a.regexes.is_empty()).then_some(rx),
            (!a.stopwords.is_empty()).then_some(sw),
        ];
        let checks: Vec<bool> = checks.into_iter().flatten().collect();
        !checks.is_empty() && checks.iter().all(|&c| c)
    })
}

fn shannon(s: &str) -> f64 {
    let mut counts: HashMap<char, f64> = HashMap::new();
    for c in s.chars() {
        *counts.entry(c).or_default() += 1.0;
    }
    let n = s.chars().count() as f64;
    counts.values().map(|c| -(c / n) * (c / n).log2()).sum()
}

/// Go RE2 meaning in Rust syntax: `\w \d \s \b` are ASCII in Go and Unicode in Rust (which also
/// blows the 10 MB size limit), and Go reads a `{` that starts no repeat as a literal.
fn go_to_rust(p: &str) -> String {
    let c: Vec<char> = p.chars().collect();
    let (mut out, mut i, mut class) = (String::new(), 0, false);
    while i < c.len() {
        match c[i] {
            '\\' if i + 1 < c.len() => {
                let ascii = match (c[i + 1], class) {
                    ('w', false) => "[0-9A-Za-z_]",
                    ('w', true) => "0-9A-Za-z_",
                    ('d', false) => "[0-9]",
                    ('d', true) => "0-9",
                    ('s', false) => r"[\t\n\f\r ]",
                    ('s', true) => r"\t\n\f\r ",
                    ('b', false) => r"(?-u:\b)",
                    _ => "",
                };
                if ascii.is_empty() {
                    out.extend([c[i], c[i + 1]]);
                } else {
                    out += ascii;
                }
                i += 1;
            }
            '[' if class && c.get(i + 1) == Some(&':') => {
                while i < c.len() && !(c[i] == ']' && c[i - 1] == ':') {
                    out.push(c[i]);
                    i += 1;
                }
                out.push(']');
            }
            '[' if !class => {
                class = true;
                out.push('[');
                if c.get(i + 1) == Some(&'^') {
                    out.push('^');
                    i += 1;
                }
                if c.get(i + 1) == Some(&']') {
                    out += r"\]";
                    i += 1;
                }
            }
            ']' if class => {
                class = false;
                out.push(']');
            }
            '{' if !class && !starts_repeat(&c[i..]) => out += r"\{",
            ch => out.push(ch),
        }
        i += 1;
    }
    out
}

fn starts_repeat(c: &[char]) -> bool {
    let Some(close) = c.iter().position(|&ch| ch == '}') else {
        return false;
    };
    let inner: String = c[1..close].iter().collect();
    let (min, max) = inner
        .split_once(',')
        .map_or((inner.as_str(), ""), |(a, b)| (a, b));
    !min.is_empty()
        && min.chars().all(|d| d.is_ascii_digit())
        && max.chars().all(|d| d.is_ascii_digit())
}

/// The config as (header, table) pairs in file order. Enough TOML for this one frozen file:
/// headers, `key = value`, `'''literal'''` and `"basic"` strings without escapes, numbers, arrays.
fn tables(toml: &str) -> Vec<(String, Table)> {
    let mut out = vec![(String::new(), Table::new())];
    let mut lines = toml.lines();
    while let Some(line) = lines.next() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') && !line.contains(" = ") {
            out.push((
                line.trim_matches(|c| c == '[' || c == ']').to_string(),
                Table::new(),
            ));
            continue;
        }
        let Some((key, value)) = line.split_once(" = ") else {
            continue;
        };
        let mut value = value.to_string();
        if value.starts_with('[') && !value.ends_with(']') {
            for l in lines.by_ref() {
                value += "\n";
                value += l;
                if l.trim() == "]" {
                    break;
                }
            }
        }
        let items = if value.starts_with(['[', '\'', '"']) {
            strings(&value)
        } else {
            vec![value]
        };
        out.last_mut().unwrap().1.insert(key.to_string(), items);
    }
    out
}

fn strings(s: &str) -> Vec<String> {
    let (mut out, mut rest) = (vec![], s);
    loop {
        let (lit, basic) = (rest.find("'''"), rest.find('"'));
        let (open, delim) = match (lit, basic) {
            (Some(l), b) if b.is_none_or(|b| l < b) => (l, "'''"),
            (_, Some(b)) => (b, "\""),
            _ => return out,
        };
        let body = &rest[open + delim.len()..];
        let Some(end) = body.find(delim) else {
            return out;
        };
        out.push(body[..end].to_string());
        rest = &body[end + delim.len()..];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_rule_loads_and_compiles() {
        let t = tables(CONFIG);
        assert_eq!(t.iter().filter(|(h, _)| h == "rules").count(), 222);
        assert_eq!(
            t.iter().filter(|(h, _)| h == "rules.allowlists").count(),
            13
        );
        for (header, table) in &t {
            for r in table
                .get("regex")
                .into_iter()
                .chain(table.get("regexes"))
                .flatten()
            {
                assert!(Regex::new(&go_to_rust(r)).is_ok(), "{header}: {r}");
            }
        }
        let s = Scanner::new();
        assert_eq!(s.rules.len(), 217);
        assert_eq!(s.global[0].stopwords.len(), 2);
        assert!(
            s.rules
                .iter()
                .any(|r| r.id == "generic-api-key" && r.entropy == 3.5 && r.allow.len() == 4)
        );
        assert!(
            s.rules
                .iter()
                .any(|r| r.id == "curl-auth-user" && r.allow[0].regexes.len() == 6)
        );
    }

    #[test]
    fn redacts_keys_not_hashes() {
        let s = Scanner::new();
        let token = format!("ghp_{}", "Zq8Xw3Kp9Lm2Nv7Bc4Rt6Yh1Jd5Fg0Sa3Ew8");
        let (out, ids) = s.redact(&format!(
            "set GH token {token} then commit 9fceb02d0ae598e95dc970b74767f19372d61af8"
        ));
        assert_eq!(
            out,
            "set GH token [REDACTED:github-pat] then commit 9fceb02d0ae598e95dc970b74767f19372d61af8"
        );
        assert_eq!(ids, ["github-pat"]);
        let clean =
            "session 3e80859b-c1a7-496e-b442-4bc915c22b07 at abc1234def, see src/api/key_store.rs";
        assert_eq!(s.redact(clean), (clean.to_string(), vec![]));
        assert_eq!(
            go_to_rust(r"^\$(?:\d+|{\d+})$"),
            r"^\$(?:[0-9]+|\{[0-9]+})$"
        );
        assert_eq!(
            go_to_rust(r"[\w.-]{0,50}[[:alnum:]]\b"),
            r"[0-9A-Za-z_.-]{0,50}[[:alnum:]](?-u:\b)"
        );
    }
}
