use crate::{record, turn};
use regex::Regex;
use rusqlite::{Connection, params};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::UNIX_EPOCH;

/// The threshold: inject a memory only at this score or above (the gate-threshold spec, rule R4).
pub const GATE: f64 = 1.50;
/// The gate stays closed in an index of fewer rows: no floor probe ran below 10 (the gate-threshold spec).
pub const MIN_ROWS: i64 = 10;
pub const MAX_LINES: usize = 3;
/// 400 tokens at 2.6 characters a token, the ratio build step 2 measured.
pub const MAX_CHARS: usize = 1040;
/// Ticket 19's three sentences; the third names the MCP tool by the exact name Claude loads it under.
pub const FRAME: &str = "Recalled memories from earlier sessions (OpenRecall). They reflect what was true when written. \
                         Full text: mcp__plugin_openrecall_openrecall__recall with the address.";
/// Claude Code's own MEMORY.md line rule (ticket 19).
const LINE_TEXT: usize = 200;
const TERMS: usize = 40;
/// Claude Code's own recall reads each memory file up to 4,096 bytes (ticket 03); the body is indexed the same way,
/// identifiers from the whole body.
const BODY_BYTES: usize = 4096;
/// bm25 weights for name, description, body and identifiers (ticket 06).
const WEIGHTS: &str = "2.0, 2.0, 1.0, 4.0";
/// Bumped when the cache's tables change; an older `index.db` is rebuilt.
const SCHEMA: i64 = 2;
pub const KINDS: [&str; 5] = ["decision", "preference", "pointer", "state", "gotcha"];
const NAME_CHARS: usize = 64;
const EXPIRES_SECS: u64 = 14 * 86400;

static URL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"https?://\S+").unwrap());
static BACKTICK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"`([^`\s]{3,})`").unwrap());
static WORD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[\w-]+").unwrap());
static SYMBOL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z_]\w*(::[A-Za-z_]\w*)*$").unwrap());
/// A pointer's symbol is looked up only in these; a config file rarely holds the names a fact mentions beside it.
const SOURCE: [&str; 22] = [
    "rs", "py", "ts", "tsx", "js", "jsx", "mjs", "go", "rb", "java", "kt", "swift", "c", "h", "cc",
    "cpp", "hpp", "cs", "php", "scala", "sh", "sql",
];

/// One directory the search reads: flat `*.md` files, addressed under `prefix` (ticket 19).
pub struct Scope {
    pub dir: PathBuf,
    pub prefix: String,
    pub builtin: bool,
}

pub struct Candidate {
    pub id: i64,
    pub address: String,
    pub bm25: f64,
    pub text_hash: String,
    pub kind: String,
    pub date: String,
    pub source: String,
    pub updated: String,
    pub expires: String,
    pub description: String,
    pub body: String,
}

/// Ticket 11: the repo's facts, the repo's built-in topic files (read-only) and the global memories.
pub fn scopes(home: &Path, user_home: &Path, identity: Option<&str>, main: Option<PathBuf>) -> Vec<Scope> {
    let mut out = vec![];
    if let Some(identity) = identity {
        out.push(Scope {
            dir: home.join("repos").join(identity),
            prefix: format!("{identity}/"),
            builtin: false,
        });
        if let Some(main) = main {
            let slug = slug(&main);
            out.push(Scope {
                dir: user_home
                    .join(".claude/projects")
                    .join(&slug)
                    .join("memory"),
                prefix: format!("builtin/{slug}/"),
                builtin: true,
            });
        }
    }
    out.push(Scope {
        dir: home.join("global"),
        prefix: "global/".into(),
        builtin: false,
    });
    out
}

/// Claude Code's project slug: every character that is not ASCII alphanumeric becomes `-`.
// ken: no 200-character cap; add Claude Code's hash suffix when a main checkout path passes 200 characters.
pub fn slug(path: &Path) -> String {
    path.to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// A memory file: Claude Code's frontmatter (`name`, `description`, `metadata.type`) plus OpenRecall's flat fields.
#[derive(Default)]
pub struct Memory {
    pub name: String,
    pub description: String,
    pub kind: String,
    pub scope: String,
    pub source: String,
    pub updated: String,
    pub expires: String,
    pub body: String,
}

/// Flat frontmatter by hand: `key: value` lines, with `type` also read from under `metadata:`.
pub fn parse(text: &str) -> Memory {
    let mut m = Memory::default();
    let Some((front, body)) = text
        .strip_prefix("---\n")
        .and_then(|rest| rest.split_once("\n---"))
    else {
        m.body = text.trim().to_string();
        return m;
    };
    for line in front.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim().trim_matches(['"', '\'']).to_string();
        match key.trim() {
            "name" => m.name = value,
            "description" => m.description = value,
            "type" => m.kind = value,
            "scope" => m.scope = value,
            "source" => m.source = value,
            "updated" => m.updated = value,
            "expires" => m.expires = value,
            _ => {}
        }
    }
    m.body = body.trim().to_string();
    m
}

/// The one writer of a fact file's text: `remember` and extraction both go through it (tickets 21 and 26).
pub fn render(m: &Memory) -> String {
    let mut s = format!(
        "---\nname: {}\ndescription: {}\nmetadata:\n  type: {}\nscope: {}\nsource: {}\n",
        m.name,
        squash(&m.description),
        m.kind,
        m.scope,
        m.source
    );
    for (key, value) in [("updated", &m.updated), ("expires", &m.expires)] {
        if !value.is_empty() {
            s += &format!("{key}: {value}\n");
        }
    }
    s + &format!("---\n\n{}\n", m.body.trim())
}

/// Ticket 21's name rule: lowercase, each run of other characters becomes `-`, trimmed, cut at 64 characters.
pub fn name_of(text: &str) -> String {
    let mut out = String::new();
    let mut n = 0;
    for c in text.chars().flat_map(char::to_lowercase) {
        if c.is_alphanumeric() {
            out.push(c);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        } else {
            continue;
        }
        n += 1;
        if n >= NAME_CHARS {
            break;
        }
    }
    let out = out.trim_end_matches('-');
    if out.is_empty() {
        "memory".into()
    } else {
        out.into()
    }
}

/// Ticket 26: a name already taken on disk gets `-2`, `-3`.
pub fn free_stem(dir: &Path, base: &str) -> String {
    let mut stem = base.to_string();
    let mut n = 1;
    while dir.join(format!("{stem}.md")).exists() {
        n += 1;
        stem = format!("{base}-{n}");
    }
    stem
}

/// A state fact's `expires`: 14 days after it was written or replaced, as YYYY-MM-DD (ticket 21).
pub fn expires_from(secs: u64) -> String {
    record::iso(secs + EXPIRES_SECS)[..10].to_string()
}

/// The date in `<session id> <date>`, as YYYY-MM-DD.
fn date_of(stamp: &str) -> Option<String> {
    let date = stamp.split_whitespace().nth(1)?;
    (date.len() >= 10).then(|| date[..10].to_string())
}

pub struct Index {
    conn: Connection,
}

impl Index {
    pub fn open(home: &Path) -> rusqlite::Result<Index> {
        let _ = fs::create_dir_all(home);
        let conn = Connection::open(home.join("index.db"))?;
        conn.busy_timeout(std::time::Duration::from_millis(200))?;
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version != SCHEMA {
            conn.execute_batch("DROP TABLE IF EXISTS files; DROP TABLE IF EXISTS ft;")?;
        }
        conn.execute_batch(
            "PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL;
             CREATE TABLE IF NOT EXISTS files (id INTEGER PRIMARY KEY, path TEXT UNIQUE, dir TEXT, mtime INTEGER,
                 size INTEGER, address TEXT, kind TEXT, date TEXT, source TEXT, updated TEXT, expires TEXT, hash TEXT);
             CREATE INDEX IF NOT EXISTS files_dir ON files(dir);
             CREATE VIRTUAL TABLE IF NOT EXISTS ft USING fts5(name, description, body, idents,
                 tokenize = \"unicode61 remove_diacritics 0 tokenchars '_'\");",
        )?;
        conn.pragma_update(None, "user_version", SCHEMA)?;
        Ok(Index { conn })
    }

    /// Ticket 10: the index is a cache. Reindex files whose mtime or size changed, drop rows for files that are gone.
    pub fn sync(&mut self, scopes: &[Scope]) -> rusqlite::Result<()> {
        let tx = self.conn.transaction()?;
        for scope in scopes {
            let dir = scope.dir.to_string_lossy().into_owned();
            let mut known: HashMap<String, (i64, i64, i64)> = tx
                .prepare("SELECT id, path, mtime, size FROM files WHERE dir = ?1")?
                .query_map([&dir], |r| {
                    Ok((r.get::<_, String>(1)?, (r.get(0)?, r.get(2)?, r.get(3)?)))
                })?
                .collect::<Result<_, _>>()?;
            for (path, mtime, size) in files(&scope.dir, scope.builtin) {
                let key = path.to_string_lossy().into_owned();
                if known.remove(&key).is_some_and(|(_, m, s)| m == mtime && s == size) {
                    continue;
                }
                let Ok(text) = fs::read_to_string(&path) else {
                    continue;
                };
                let m = parse(&text);
                let stem = path.file_stem().unwrap_or_default().to_string_lossy();
                let date = if scope.builtin {
                    record::iso((mtime / 1_000_000_000) as u64)[..10].to_string()
                } else {
                    date_of(&m.updated)
                        .or_else(|| date_of(&m.source))
                        .unwrap_or_default()
                };
                let kind = if m.kind.is_empty() { "memory" } else { &m.kind };
                let id: i64 = tx.query_row(
                    "INSERT INTO files (path, dir, mtime, size, address, kind, date, source, updated, expires, hash)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                     ON CONFLICT(path) DO UPDATE SET mtime = excluded.mtime, size = excluded.size,
                         address = excluded.address, kind = excluded.kind, date = excluded.date,
                         source = excluded.source, updated = excluded.updated, expires = excluded.expires,
                         hash = excluded.hash
                     RETURNING id",
                    params![key, dir, mtime, size, format!("{}{stem}", scope.prefix), kind, date, m.source, m.updated, m.expires, crate::fnv(&text)],
                    |r| r.get(0),
                )?;
                tx.execute("DELETE FROM ft WHERE rowid = ?1", [id])?;
                let name = if m.name.is_empty() { stem.to_string() } else { m.name };
                tx.execute(
                    "INSERT INTO ft (rowid, name, description, body, idents) VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![id, name, m.description, head(&m.body, BODY_BYTES), idents(&m.body).join(" ")],
                )?;
            }
            for (id, _, _) in known.values() {
                tx.execute("DELETE FROM ft WHERE rowid = ?1", [id])?;
                tx.execute("DELETE FROM files WHERE id = ?1", [id])?;
            }
        }
        tx.commit()
    }

    /// The best matches for the query terms inside the scopes, best first by `bm25`: the weighted bm25 sum (ticket 06)
    /// divided by the square root of the term count (build step 3). It orders the candidates; the gate reads `scores`.
    pub fn search(
        &self,
        terms: &[String],
        scopes: &[Scope],
        limit: usize,
    ) -> rusqlite::Result<Vec<Candidate>> {
        if terms.is_empty() {
            return Ok(vec![]);
        }
        let query = terms
            .iter()
            .map(|t| phrase(t))
            .collect::<Vec<_>>()
            .join(" OR ");
        let dir = |i: usize| {
            scopes
                .get(i)
                .map_or(String::new(), |s| s.dir.to_string_lossy().into_owned())
        };
        let sql = format!(
            "SELECT m.id, f.address, m.bm25, f.hash, f.kind, f.date, f.source, f.updated, f.expires, m.description, m.body
             FROM (SELECT rowid AS id, -bm25(ft, {WEIGHTS}) / ?6 AS bm25, description, body FROM ft WHERE ft MATCH ?1) m
             JOIN files f ON f.id = m.id
             WHERE f.dir IN (?2, ?3, ?4)
             ORDER BY m.bm25 DESC, f.date DESC LIMIT ?5"
        );
        self.conn
            .prepare(&sql)?
            .query_map(
                params![query, dir(0), dir(1), dir(2), limit as i64, (terms.len() as f64).sqrt()],
                |r| {
                    Ok(Candidate {
                        id: r.get(0)?,
                        address: r.get(1)?,
                        bm25: r.get(2)?,
                        text_hash: r.get(3)?,
                        kind: r.get(4)?,
                        date: r.get(5)?,
                        source: r.get(6)?,
                        updated: r.get(7)?,
                        expires: r.get(8)?,
                        description: r.get(9)?,
                        body: r.get(10)?,
                    })
                },
            )?
            .collect()
    }

    /// The index size N, rows of every scope, and each candidate's score (rule R4): the bm25 sum with each term's idf
    /// replaced by ln((N + 0.5) / n) / ln(N + 1), n the rows that hold the term, over the square root of the term count.
    // ken: one query per term over every row that holds it; compute bm25 for candidates only if recall p95 moves.
    pub fn scores(
        &self,
        terms: &[String],
        candidates: &[Candidate],
    ) -> rusqlite::Result<(i64, Vec<f64>)> {
        let size: i64 = self
            .conn
            .query_row("SELECT count(*) FROM ft", [], |r| r.get(0))?;
        let mut sums = vec![0.0; candidates.len()];
        if candidates.is_empty() {
            return Ok((size, sums));
        }
        let mut each = self.conn.prepare(&format!(
            "SELECT rowid, -bm25(ft, {WEIGHTS}) FROM ft WHERE ft MATCH ?1"
        ))?;
        let rows = size as f64;
        for t in terms {
            let mut holding = 0.0;
            let mut found = vec![];
            let mut hits = each.query([phrase(t)])?;
            while let Some(r) = hits.next()? {
                holding += 1.0;
                let id: i64 = r.get(0)?;
                if let Some(k) = candidates.iter().position(|c| c.id == id) {
                    found.push((k, r.get::<_, f64>(1)?));
                }
            }
            if found.is_empty() {
                continue;
            }
            // FTS5's own idf, which fts5_aux.c raises to 1e-6 when it is not positive; a one-term bm25 is this idf
            // times the term's frequency part.
            let idf = ((rows - holding + 0.5) / (holding + 0.5)).ln();
            let idf = if idf <= 0.0 { 1e-6 } else { idf };
            let weight = ((rows + 0.5) / holding).ln() / (rows + 1.0).ln();
            for (k, bm25) in found {
                sums[k] += weight * bm25 / idf;
            }
        }
        let root = (terms.len() as f64).sqrt();
        Ok((size, sums.into_iter().map(|s| s / root).collect()))
    }
}

fn phrase(term: &str) -> String {
    format!("\"{}\"", term.replace('"', ""))
}

/// The scope's memory files with their mtime in nanoseconds and size. `MEMORY.md` is already in the context
/// window (ticket 11) and `claude-md-suggestions.md` is never a memory (ticket 21).
fn files(dir: &Path, builtin: bool) -> Vec<(PathBuf, i64, i64)> {
    let mut out = vec![];
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        let skip = if builtin {
            name == "MEMORY.md"
        } else {
            name == "claude-md-suggestions.md"
        };
        if skip || path.extension().is_none_or(|e| e != "md") {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_nanos() as i64);
        out.push((path, mtime, meta.len() as i64));
    }
    out
}

/// Identifiers in body order: tickets, PR numbers, commits, paths and backticked symbols, URLs left out.
pub fn idents(text: &str) -> Vec<String> {
    let text = URL.replace_all(text, " ");
    let at = |s: &str| s.as_ptr() as usize - text.as_ptr() as usize;
    let mut found: Vec<(usize, String)> = vec![];
    found.extend(turn::TICKET.find_iter(&text).map(|m| (m.start(), m.as_str().to_string())));
    found.extend(turn::prs_in(&text).into_iter().map(|(i, p)| (i, format!("#{p}"))));
    found.extend(turn::commits_in(&text).map(|c| (at(c), c.to_string())));
    found.extend(
        turn::paths_in(&text)
            .into_iter()
            .map(|p| (at(p), turn::LINE_SUFFIX.replace(p, "").into_owned())),
    );
    found.extend(BACKTICK.captures_iter(&text).map(|c| (c.get(1).unwrap().start(), c[1].to_string())));
    found.sort_by_key(|(i, _)| *i);
    let mut out: Vec<String> = vec![];
    for (_, id) in found {
        if !out.contains(&id) {
            out.push(id);
        }
    }
    out
}

pub fn has_identifier(text: &str) -> bool {
    !idents(text).is_empty() || URL.is_match(text)
}

/// Ticket 15: the query is the prompt, or a slash command's text after the first space. `Err` names the skip.
pub fn query_of(prompt: &str) -> Result<&str, &'static str> {
    let query = if prompt.starts_with('/') {
        prompt.split_once(char::is_whitespace).map_or("", |(_, rest)| rest)
    } else {
        prompt
    };
    let query = query.trim();
    if prompt.starts_with('/') && query.is_empty() {
        Err("empty-args")
    } else if query.split_whitespace().count() < 4 && !has_identifier(query) {
        Err("short")
    } else {
        Ok(query)
    }
}

/// Search terms: identifiers first, then the PR number of a PR URL when it has 3 to 6 digits (issue 13), then every
/// other word, each once. Paths stay whole so they match as phrases.
// ken: the first 40 terms; weigh terms by rarity if long pasted prompts show up as false injections.
pub fn terms(query: &str) -> Vec<String> {
    let mut out: Vec<String> = idents(query)
        .into_iter()
        .map(|i| i.trim_start_matches('#').to_string())
        .collect();
    for t in turn::tickets(query) {
        if !out.contains(&t) {
            out.push(t);
        }
    }
    for (_, p) in turn::prs_in(query) {
        if p.len() >= 3 && !out.contains(&p) {
            out.push(p);
        }
    }
    for w in WORD.find_iter(&URL.replace_all(query, " ")) {
        let w = w.as_str().trim_matches(['-', '_']).to_lowercase();
        if w.chars().count() >= 2 && !out.iter().any(|o| o.eq_ignore_ascii_case(&w)) {
            out.push(w);
        }
    }
    out.truncate(TERMS);
    out
}

/// Ticket 09: a pointer is never injected when a path it cites no longer exists, or when a code-shaped symbol it
/// names in backticks is in none of the source files it cites.
// ken: reads each cited source file whole; cap the size if a pointer to a large generated file moves recall p95.
pub fn rotted(body: &str, folder: Option<&Path>, home: &Path) -> bool {
    let text = URL.replace_all(body, " ");
    let folder_s = folder.map_or(String::new(), |f| f.to_string_lossy().into_owned());
    let home_s = home.to_string_lossy();
    let mut sources = vec![];
    for p in turn::paths_in(&text)
        .into_iter()
        .filter_map(|p| turn::norm_path(p, &folder_s, &home_s))
    {
        let full = if let Some(rest) = p.strip_prefix("~/") {
            home.join(rest)
        } else if p.starts_with('/') {
            PathBuf::from(&p)
        } else if let Some(f) = folder {
            f.join(&p)
        } else {
            continue;
        };
        if !full.exists() {
            return true;
        }
        if full.extension().is_some_and(|e| SOURCE.iter().any(|s| e == *s))
            && let Ok(code) = fs::read_to_string(&full)
        {
            sources.push(code);
        }
    }
    !sources.is_empty()
        && BACKTICK
            .captures_iter(&text)
            .filter_map(|c| symbol(c.get(1).unwrap().as_str()))
            .any(|s| !sources.iter().any(|code| code.contains(s)))
}

/// The name a backticked token looks up, when the token is shaped like code: `snake_case`, `camelCase`, `Type::item`
/// or `call()`. A plain word such as `curl` is often a command, not a symbol, so it is never checked.
fn symbol(token: &str) -> Option<&str> {
    let bare = token.strip_suffix("()").unwrap_or(token);
    let last = bare.rsplit("::").next()?;
    let camel = bare
        .as_bytes()
        .windows(2)
        .any(|w| w[0].is_ascii_lowercase() && w[1].is_ascii_uppercase());
    let code = bare.contains('_') || bare.contains("::") || bare.len() < token.len() || camel;
    (code && SYMBOL.is_match(bare)).then_some(last)
}

/// Tickets 19 and 21: `- <type> <date> <address>: <text>`, the body when it fits in 200 characters or the whole line
/// fits in `room` characters, else the description plus the body's identifiers the description lacks, in
/// parentheses, up to 200 characters.
pub fn line(c: &Candidate, room: usize) -> String {
    let prefix = format!("- {} {} {}: ", c.kind, c.date, c.address);
    let body = squash(&c.body);
    let text = if body.chars().count() <= LINE_TEXT.max(room.saturating_sub(prefix.chars().count())) {
        body
    } else if c.description.trim().is_empty() {
        cut(&body, LINE_TEXT)
    } else {
        let desc = cut(&squash(&c.description), LINE_TEXT);
        let mut extra: Vec<String> = vec![];
        for id in idents(&c.body) {
            if c.description.contains(&id) || extra.contains(&id) {
                continue;
            }
            let longer = desc.chars().count() + extra.iter().map(|e| e.chars().count() + 2).sum::<usize>() + id.chars().count() + 3;
            if longer > LINE_TEXT {
                break;
            }
            extra.push(id);
        }
        if extra.is_empty() {
            desc
        } else {
            format!("{desc} ({})", extra.join(", "))
        }
    };
    prefix + &text
}

fn head(s: &str, bytes: usize) -> &str {
    let mut end = bytes.min(s.len());
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

fn squash(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn cut(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n - 1).chain(['…']).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(description: &str, body: &str) -> Candidate {
        Candidate {
            id: 0,
            address: "global/x".into(),
            bm25: 1.0,
            text_hash: String::new(),
            kind: "gotcha".into(),
            date: "2026-01-02".into(),
            source: String::new(),
            updated: String::new(),
            expires: String::new(),
            description: description.into(),
            body: body.into(),
        }
    }

    #[test]
    fn frontmatter_and_slug() {
        let m = parse("---\nname: hook-budget\ndescription: \"Hooks share 1.5 s\"\nmetadata:\n  type: gotcha\nsource: S1 2026-01-02T03:04:05Z\n---\n\nThe body.\n");
        assert_eq!(
            (m.name.as_str(), m.description.as_str(), m.kind.as_str(), m.body.as_str()),
            ("hook-budget", "Hooks share 1.5 s", "gotcha", "The body.")
        );
        assert_eq!(date_of(&m.source).as_deref(), Some("2026-01-02"));
        assert_eq!(parse("no frontmatter").body, "no frontmatter");
        assert_eq!(slug(Path::new("/Users/x/Code/app.v2")), "-Users-x-Code-app-v2");
        let fact = Memory {
            name: "pr-345".into(),
            description: "PR #345\nis merged".into(),
            kind: "state".into(),
            scope: "github.com/a/b".into(),
            source: "S1 2026-01-02T03:04:05.000Z".into(),
            updated: "S2 2026-01-03T00:00:00.000Z".into(),
            expires: "2026-01-17".into(),
            body: "PR #345 is merged.\n".into(),
        };
        let text = render(&fact);
        assert!(text.starts_with("---\nname: pr-345\ndescription: PR #345 is merged\nmetadata:\n  type: state\n"));
        let back = parse(&text);
        assert_eq!(
            (back.scope.as_str(), back.updated.as_str(), back.expires.as_str(), back.body.as_str()),
            ("github.com/a/b", "S2 2026-01-03T00:00:00.000Z", "2026-01-17", "PR #345 is merged.")
        );
    }

    #[test]
    fn names() {
        assert_eq!(
            name_of("  The Asana slug: hard-coded (ABC-1234)! "),
            "the-asana-slug-hard-coded-abc-1234"
        );
        assert_eq!(name_of("!!!"), "memory");
        assert_eq!(name_of(&"ab ".repeat(40)).chars().count(), 64);
        assert_eq!(expires_from(0), "1970-01-15");
    }

    #[test]
    fn queries() {
        assert_eq!(query_of("/implement"), Err("empty-args"));
        assert_eq!(query_of("/implement  "), Err("empty-args"));
        assert_eq!(query_of("fix it now"), Err("short"));
        assert_eq!(query_of("fix ABC-12"), Ok("fix ABC-12"));
        assert_eq!(query_of("/probe fix ABC-1234"), Ok("fix ABC-1234"));
        assert_eq!(query_of("please fix the export bug"), Ok("please fix the export bug"));
        assert_eq!(
            terms("Fix ABC-12 in src/export.py: the `run_export` call, see https://x.y/z"),
            ["ABC-12", "src/export.py", "run_export", "fix", "in", "src", "export", "py", "the", "call", "see"]
        );
    }

    #[test]
    fn pr_urls() {
        let pr = "https://github.com/acme/web-app/pull/345";
        assert_eq!(terms(pr), ["345"]);
        let review = query_of("/review https://github.com/acme/web-app/pull/345");
        assert_eq!(review, Ok(pr));
        assert_eq!(terms(review.unwrap()), ["345"]);
        for none in [
            "https://github.com/acme/web-app/pull/7",
            "https://github.com/acme/web-app/pull/12",
            "https://github.com/acme/web-app/issues/345",
        ] {
            assert_eq!(terms(none), Vec::<String>::new(), "{none}");
        }
        assert_eq!(terms("PR #345 is https://github.com/acme/web-app/pull/345"), ["345", "pr", "is"]);
        assert_eq!(terms("see https://github.com/acme/web-app/pull/345 now"), ["345", "see", "now"]);
        assert_eq!(
            terms("https://tracker.example/issue/ABC-123/export-drops-the-currency-column i think the fix is in the exporter"),
            ["ABC-123", "think", "the", "fix", "is", "in", "exporter"]
        );
    }

    #[test]
    fn lines() {
        let short = candidate("d", "PR #345 is merged.");
        assert_eq!(line(&short, 0), "- gotcha 2026-01-02 global/x: PR #345 is merged.");
        let long_body = format!("See src/export.py and ABC-12 at abc1234def. {}", "x ".repeat(120));
        let long = candidate("Exports fail on empty rows", &long_body);
        assert_eq!(
            line(&long, 0),
            "- gotcha 2026-01-02 global/x: Exports fail on empty rows (src/export.py, ABC-12, abc1234def)"
        );
        let whole = format!("- gotcha 2026-01-02 global/x: {}", long_body.trim());
        assert_eq!(line(&long, whole.chars().count()), whole);
        assert_eq!(line(&long, whole.chars().count() - 1), line(&long, 0));
        let wide = candidate(&"w".repeat(250), &long_body);
        assert_eq!(line(&wide, 0).chars().count(), "- gotcha 2026-01-02 global/x: ".len() + 200);
        assert!(line(&wide, 0).ends_with('…'));
        assert_eq!(idents("`a_b` then a/b.md:3 and https://h/x.md and ABC-12"), ["a_b", "a/b.md", "ABC-12"]);
    }

    #[test]
    fn rot() {
        let tmp = std::env::temp_dir().join(format!("openrecall-rot-{}", std::process::id()));
        fs::create_dir_all(tmp.join("src")).unwrap();
        fs::write(tmp.join("src/a.py"), "def run_export():\n    Exporter.flush_rows()\n").unwrap();
        fs::write(tmp.join("src/b.toml"), "base_url = 1\n").unwrap();
        assert!(!rotted("see src/a.py and https://x/y.md", Some(&tmp), &tmp));
        assert!(rotted("see src/gone.py", Some(&tmp), &tmp));
        assert!(!rotted("see src/gone.py", None, &tmp));
        assert!(rotted("see ~/notes/gone.md", None, &tmp));
        assert!(!rotted("`run_export()` in src/a.py calls `Exporter::flush_rows`", Some(&tmp), &tmp));
        assert!(rotted("`run_import` lives in src/a.py", Some(&tmp), &tmp));
        assert!(rotted("`exportAll` lives in src/a.py", Some(&tmp), &tmp));
        assert!(!rotted("run `curl` and `index.db` beside src/a.py", Some(&tmp), &tmp));
        assert!(!rotted("`ANTHROPIC_API_KEY` is never in src/b.toml", Some(&tmp), &tmp));
        assert!(!rotted("`run_import` is named, no file cited", Some(&tmp), &tmp));
        fs::remove_dir_all(tmp).unwrap();
    }

    #[test]
    fn a_schema_number_from_any_other_version_rebuilds_the_index() {
        let tmp = std::env::temp_dir().join(format!("openrecall-schema-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        for other in [SCHEMA - 1, SCHEMA + 1] {
            let ix = Index::open(&tmp).unwrap();
            ix.conn.execute("INSERT INTO files (path) VALUES ('x')", []).unwrap();
            ix.conn.pragma_update(None, "user_version", other).unwrap();
            drop(ix);
            let ix = Index::open(&tmp).unwrap();
            let rows: i64 = ix.conn.query_row("SELECT count(*) FROM files", [], |r| r.get(0)).unwrap();
            assert_eq!(rows, 0, "schema {other}");
        }
        fs::remove_dir_all(tmp).unwrap();
    }

    #[test]
    fn hyphens_split_in_the_index() {
        let tmp = std::env::temp_dir().join(format!("openrecall-ix-{}", std::process::id()));
        fs::create_dir_all(tmp.join("global")).unwrap();
        let fact = |name: &str, body: &str| {
            format!("---\nname: {name}\ndescription: d\nmetadata:\n  type: decision\nsource: S 2026-01-02\n---\n\n{body}\n")
        };
        fs::write(tmp.join("global/a.md"), fact("export-queue-retry-limits", "Limits live there.")).unwrap();
        fs::write(tmp.join("global/b.md"), fact("abc-1234-slug-list", "Hard-code the slug.")).unwrap();
        let scopes = scopes(&tmp, &tmp, None, None);
        let mut ix = Index::open(&tmp).unwrap();
        ix.sync(&scopes).unwrap();
        let top = |q: &str| ix.search(&terms(q), &scopes, 5).unwrap().first().map(|c| c.address.clone());
        assert_eq!(top("change the export queue limits").as_deref(), Some("global/a"));
        assert_eq!(top("https://tracker.example/issue/ABC-1234/slug").as_deref(), Some("global/b"));
        assert_eq!(top("queue-retry-limits").as_deref(), Some("global/a"));
        assert_eq!(top("limits-queue"), None);
        fs::remove_dir_all(tmp).unwrap();
    }

    #[test]
    fn a_pr_url_finds_the_fact_that_names_its_pr() {
        let tmp = std::env::temp_dir().join(format!("openrecall-pr-{}", std::process::id()));
        fs::create_dir_all(tmp.join("global")).unwrap();
        let fact = |name: &str, body: &str| {
            format!("---\nname: {name}\ndescription: d\nmetadata:\n  type: state\nsource: S 2026-01-02\n---\n\n{body}\n")
        };
        fs::write(tmp.join("global/a.md"), fact("web-app-pull-requests", "The acme web app reviews each pull request.")).unwrap();
        fs::write(tmp.join("global/b.md"), fact("export-null-rows", "PR #345 fixes the null rows.")).unwrap();
        let scopes = scopes(&tmp, &tmp, None, None);
        let mut ix = Index::open(&tmp).unwrap();
        ix.sync(&scopes).unwrap();
        let top = |q: &str| ix.search(&terms(q), &scopes, 5).unwrap().first().map(|c| c.address.clone());
        assert_eq!(top("https://github.com/acme/web-app/pull/345").as_deref(), Some("global/b"));
        fs::remove_dir_all(tmp).unwrap();
    }

    #[test]
    fn scores_count_the_rows_of_every_scope() {
        let tmp = std::env::temp_dir().join(format!("openrecall-score-{}", std::process::id()));
        let fact = |body: &str| {
            format!("---\nname: n\ndescription: d\nmetadata:\n  type: decision\n---\n\n{body}\n")
        };
        fs::create_dir_all(tmp.join("global")).unwrap();
        fs::create_dir_all(tmp.join("repos/other")).unwrap();
        for i in 0..9 {
            fs::write(
                tmp.join(format!("global/f{i}.md")),
                fact(if i == 0 { "zebra" } else { "plain" }),
            )
            .unwrap();
        }
        fs::write(tmp.join("repos/other/o.md"), fact("plain")).unwrap();
        let global = scopes(&tmp, &tmp, None, None);
        let mut ix = Index::open(&tmp).unwrap();
        ix.sync(&global).unwrap();
        ix.sync(&[Scope {
            dir: tmp.join("repos/other"),
            prefix: "other/".into(),
            builtin: false,
        }])
        .unwrap();
        let terms = vec!["zebra".to_string(), "plain".to_string()];
        let found = ix.search(&terms, &global, 2).unwrap();
        assert_eq!(found[0].address, "global/f0");
        let (size, scores) = ix.scores(&terms, &found).unwrap();
        assert_eq!(size, 10);
        // Each row holds one token per column, so a term's frequency part is 1 (bm25 with k1 1.2, b 0.75, weight 1).
        let expect = |n: f64| (10.5 / n).ln() / 11f64.ln() / 2f64.sqrt();
        assert!((scores[0] - expect(1.0)).abs() < 1e-9, "{scores:?}");
        assert!(
            (scores[1] - expect(9.0)).abs() < 1e-9,
            "a term in most rows, where FTS5's idf is 1e-6: {scores:?}"
        );
        fs::remove_dir_all(tmp).unwrap();
    }
}
