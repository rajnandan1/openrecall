use crate::{git, record, turn};
use regex::Regex;
use rusqlite::{Connection, params};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::UNIX_EPOCH;

/// Inject a memory only at this score or above: the eval's threshold sweep picks it (ticket 16).
pub const GATE: f64 = 4.3;
pub const MAX_LINES: usize = 3;
/// 400 tokens at 2.6 characters a token, the ratio build step 2 measured.
pub const MAX_CHARS: usize = 1040;
pub const FRAME: &str =
    "Recalled memories from earlier sessions (OpenRecall). They reflect what was true when written.";
/// Claude Code's own MEMORY.md line rule (ticket 19).
const LINE_TEXT: usize = 200;
const TERMS: usize = 40;
/// Claude Code's own recall reads each memory file up to 4,096 bytes (ticket 03); the body is indexed the same way,
/// identifiers from the whole body.
const BODY_BYTES: usize = 4096;
/// bm25 weights for name, description, body and identifiers (ticket 06).
const WEIGHTS: &str = "2.0, 2.0, 1.0, 4.0";

static URL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"https?://\S+").unwrap());
static BACKTICK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"`([^`\s]{3,})`").unwrap());
static WORD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[\w-]+").unwrap());

/// One directory the search reads: flat `*.md` files, addressed under `prefix` (ticket 19).
pub struct Scope {
    pub dir: PathBuf,
    pub prefix: String,
    pub builtin: bool,
}

pub struct Candidate {
    pub address: String,
    pub score: f64,
    pub text_hash: String,
    pub kind: String,
    pub date: String,
    pub source: String,
    pub updated: String,
    pub description: String,
    pub body: String,
}

/// Ticket 11: the repo's facts, the repo's built-in topic files (read-only) and the global memories.
pub fn scopes(home: &Path, user_home: &Path, repo: Option<&git::Repo>) -> Vec<Scope> {
    let mut out = vec![];
    if let Some(r) = repo {
        out.push(Scope {
            dir: home.join("repos").join(&r.identity),
            prefix: format!("{}/", r.identity),
            builtin: false,
        });
        if let Some(main) = r.main_checkout() {
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

#[derive(Default)]
struct Memory {
    name: String,
    description: String,
    kind: String,
    source: String,
    updated: String,
    body: String,
}

/// Flat frontmatter by hand: `key: value` lines, with `type` also read from under `metadata:`.
fn parse(text: &str) -> Memory {
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
            "source" => m.source = value,
            "updated" => m.updated = value,
            _ => {}
        }
    }
    m.body = body.trim().to_string();
    m
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
        conn.execute_batch(
            "PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL;
             CREATE TABLE IF NOT EXISTS files (id INTEGER PRIMARY KEY, path TEXT UNIQUE, dir TEXT, mtime INTEGER,
                 size INTEGER, address TEXT, kind TEXT, date TEXT, source TEXT, updated TEXT, hash TEXT);
             CREATE INDEX IF NOT EXISTS files_dir ON files(dir);
             CREATE VIRTUAL TABLE IF NOT EXISTS ft USING fts5(name, description, body, idents,
                 tokenize = \"unicode61 remove_diacritics 0 tokenchars '-_'\");",
        )?;
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
                    "INSERT INTO files (path, dir, mtime, size, address, kind, date, source, updated, hash)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                     ON CONFLICT(path) DO UPDATE SET mtime = excluded.mtime, size = excluded.size,
                         address = excluded.address, kind = excluded.kind, date = excluded.date,
                         source = excluded.source, updated = excluded.updated, hash = excluded.hash
                     RETURNING id",
                    params![key, dir, mtime, size, format!("{}{stem}", scope.prefix), kind, date, m.source, m.updated, crate::fnv(&text)],
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

    /// The best matches for the query terms inside the scopes, best first. The score is the weighted bm25 sum
    /// (ticket 06) divided by the square root of the term count, so a long prompt cannot lift every memory over
    /// the gate: the only recipe of the six the eval swept that met the precision goal (build step 3).
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
            .map(|t| format!("\"{}\"", t.replace('"', "")))
            .collect::<Vec<_>>()
            .join(" OR ");
        let dir = |i: usize| {
            scopes
                .get(i)
                .map_or(String::new(), |s| s.dir.to_string_lossy().into_owned())
        };
        let sql = format!(
            "SELECT f.address, m.score, f.hash, f.kind, f.date, f.source, f.updated, m.description, m.body
             FROM (SELECT rowid AS id, -bm25(ft, {WEIGHTS}) / ?6 AS score, description, body FROM ft WHERE ft MATCH ?1) m
             JOIN files f ON f.id = m.id
             WHERE f.dir IN (?2, ?3, ?4)
             ORDER BY m.score DESC, f.date DESC LIMIT ?5"
        );
        self.conn
            .prepare(&sql)?
            .query_map(
                params![query, dir(0), dir(1), dir(2), limit as i64, (terms.len() as f64).sqrt()],
                |r| {
                    Ok(Candidate {
                        address: r.get(0)?,
                        score: r.get(1)?,
                        text_hash: r.get(2)?,
                        kind: r.get(3)?,
                        date: r.get(4)?,
                        source: r.get(5)?,
                        updated: r.get(6)?,
                        description: r.get(7)?,
                        body: r.get(8)?,
                    })
                },
            )?
            .collect()
    }
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

/// Search terms: identifiers first, then every other word, each once. Paths stay whole so they match as phrases.
// ken: the first 40 terms; weigh terms by rarity if long pasted prompts show up as false injections.
pub fn terms(query: &str) -> Vec<String> {
    let mut out: Vec<String> = idents(query)
        .into_iter()
        .map(|i| i.trim_start_matches('#').to_string())
        .collect();
    for w in WORD.find_iter(&URL.replace_all(query, " ")) {
        let w = w.as_str().trim_matches(['-', '_']).to_lowercase();
        if w.chars().count() >= 2 && !out.iter().any(|o| o.eq_ignore_ascii_case(&w)) {
            out.push(w);
        }
    }
    out.truncate(TERMS);
    out
}

/// Ticket 09: a pointer whose cited path no longer exists is never injected.
pub fn rotted(body: &str, folder: Option<&Path>, home: &Path) -> bool {
    let text = URL.replace_all(body, " ");
    let folder_s = folder.map_or(String::new(), |f| f.to_string_lossy().into_owned());
    let home_s = home.to_string_lossy();
    turn::paths_in(&text)
        .into_iter()
        .filter_map(|p| turn::norm_path(p, &folder_s, &home_s))
        .any(|p| {
            let full = if let Some(rest) = p.strip_prefix("~/") {
                home.join(rest)
            } else if p.starts_with('/') {
                PathBuf::from(&p)
            } else if let Some(f) = folder {
                f.join(&p)
            } else {
                return false;
            };
            !full.exists()
        })
}

/// Tickets 19 and 21: `- <type> <date> <address>: <text>`, the body when it fits in 200 characters, else the
/// description plus the body's identifiers the description lacks, in parentheses, up to 200 characters.
pub fn line(c: &Candidate) -> String {
    let body = squash(&c.body);
    let text = if body.chars().count() <= LINE_TEXT {
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
    format!("- {} {} {}: {text}", c.kind, c.date, c.address)
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
            address: "global/x".into(),
            score: 1.0,
            text_hash: String::new(),
            kind: "gotcha".into(),
            date: "2026-01-02".into(),
            source: String::new(),
            updated: String::new(),
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
    fn lines() {
        let short = candidate("d", "PR #345 is merged.");
        assert_eq!(line(&short), "- gotcha 2026-01-02 global/x: PR #345 is merged.");
        let long_body = format!("See src/export.py and ABC-12 at abc1234def. {}", "x ".repeat(120));
        let long = candidate("Exports fail on empty rows", &long_body);
        assert_eq!(
            line(&long),
            "- gotcha 2026-01-02 global/x: Exports fail on empty rows (src/export.py, ABC-12, abc1234def)"
        );
        let wide = candidate(&"w".repeat(250), &long_body);
        assert_eq!(line(&wide).chars().count(), "- gotcha 2026-01-02 global/x: ".len() + 200);
        assert!(line(&wide).ends_with('…'));
        assert_eq!(idents("`a_b` then a/b.md:3 and https://h/x.md and ABC-12"), ["a_b", "a/b.md", "ABC-12"]);
    }

    #[test]
    fn rot() {
        let tmp = std::env::temp_dir().join(format!("openrecall-rot-{}", std::process::id()));
        fs::create_dir_all(tmp.join("src")).unwrap();
        fs::write(tmp.join("src/a.py"), "").unwrap();
        assert!(!rotted("see src/a.py and https://x/y.md", Some(&tmp), &tmp));
        assert!(rotted("see src/gone.py", Some(&tmp), &tmp));
        assert!(!rotted("see src/gone.py", None, &tmp));
        assert!(rotted("see ~/notes/gone.md", None, &tmp));
        fs::remove_dir_all(tmp).unwrap();
    }
}
