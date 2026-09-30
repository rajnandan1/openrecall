use crate::index::{self, Candidate, Memory};
use crate::turn::{self, Event, Turn};
use crate::{git, record, scan};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::cell::OnceCell;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime};

const QUIET: Duration = Duration::from_secs(30 * 60);
const STRIKES: u32 = 3;
/// Ticket 26: the top 5 holds 89% of what the top 10 holds.
const CANDIDATES: usize = 5;
/// Built-in files are never replaced, so the dedupe call reads only their start (ticket 26).
const BUILTIN_CHARS: usize = 1200;
const TOOL_CHARS: usize = 200;
const TOOL_KEYS: [&str; 8] = [
    "command",
    "file_path",
    "path",
    "pattern",
    "url",
    "query",
    "description",
    "prompt",
];
const MARK: &str = "=== NEW TURNS ===";

/// Ticket 21's `SYSTEM`, without the `scope` field the binary now sets.
const EXTRACT: &str = r#"You extract lasting facts from one Claude Code session, for a memory store that later sessions on the same repository search.

The transcript is condensed. [user] lines are the person's prompts. [assistant] lines are Claude's replies. [tool] lines are one line per tool call; tool results are left out.

Write only facts that a later session would still need after this session is gone. Each fact has one of five types:
- decision: what was decided and why, and who decided it (the user or Claude). Always keep the reason.
- preference: how the user wants work done.
- pointer: where something lives, as a repo-relative path plus a symbol name. Never a line number.
- state: the status or next step of a ticket or PR. Name the ticket or PR.
- gotcha: surprising behavior someone discovered, with the condition that triggers it.

Never write:
- a summary of what the session did ("we fixed X", "the session explored Y")
- the task's own instructions restated as a fact
- things only true inside this session: the current branch, open files, a test run's output
- secrets, keys, tokens or passwords, or anything marked [REDACTED:...]
- general knowledge that any engineer already has

Keep specifics verbatim: file paths, symbol names, ticket IDs, PR numbers, commands, numbers, config keys. A later session finds a fact by its identifiers, so a fact without them is lost.

A preference that is really a standing rule for every session in this repository (a coding standard, a workflow rule) goes in claude_md_rules instead of facts, written as one CLAUDE.md line.

Fields of a fact:
- type: one of the five types.
- name: kebab-case, 2 to 6 words, naming the subject. It becomes the file name, so it is unique within your answer.
- description: one line of at most 150 characters that states the fact itself with its main identifier. Write "X uses Y because Z", not "About X".
- body: the fact in plain sentences. Keep it under 200 characters unless the reason or the identifiers need more.

Prefer a few strong facts over many weak ones. An empty facts list is the right answer for a session with nothing lasting.

Answer with one JSON object and nothing else, matching this JSON schema:
"#;

const EXTRACT_SCHEMA: &str = r#"{"type": "object", "additionalProperties": false, "required": ["facts", "claude_md_rules"], "properties": {"facts": {"type": "array", "items": {"type": "object", "additionalProperties": false, "required": ["type", "name", "description", "body"], "properties": {"type": {"type": "string", "enum": ["decision", "preference", "pointer", "state", "gotcha"]}, "name": {"type": "string"}, "description": {"type": "string"}, "body": {"type": "string"}}}}, "claude_md_rules": {"type": "array", "items": {"type": "string"}}}}"#;

/// Ticket 26's `MERGE_SYSTEM`.
const MERGE: &str = r#"You keep a memory store free of near-copies. You get new facts just extracted from one Claude Code session, and the existing memories of the same repository that look most like them. Decide each new fact:

- skip: an existing memory already states it, so a later session that read that memory would learn nothing new from the fact.
- replace: the new fact is about the same thing as one existing memory whose address does not start with builtin/, and it changes that memory or adds to it. Give that memory's address, and write the replacement: a description and a body that hold everything in the old memory that is still true plus everything in the new fact. Where they disagree, the new fact wins.
- new: no existing memory states it, or the only memory about the same thing has an address that starts with builtin/. Leave address, description and body empty.

Memories whose address starts with builtin/ are never replaced. If two new facts change the same memory, put both into one replacement and skip the other. When unsure, choose new.

A replacement follows the store's rules: identifiers verbatim (paths, symbols, ticket IDs, PR numbers, commands), never a line number, a description of at most 150 characters that states the fact itself, a body in plain sentences.

Answer with one JSON object and nothing else, one decision per new fact, matching this JSON schema:
"#;

/// `fact` is the new fact's number: as a string, Sonnet wrote the fact's name in 9 of 37 runs (ticket 26).
const MERGE_SCHEMA: &str = r#"{"type": "object", "additionalProperties": false, "required": ["decisions"], "properties": {"decisions": {"type": "array", "items": {"type": "object", "additionalProperties": false, "required": ["fact", "action", "address", "description", "body"], "properties": {"fact": {"type": "integer"}, "action": {"type": "string", "enum": ["new", "skip", "replace"]}, "address": {"type": "string"}, "description": {"type": "string"}, "body": {"type": "string"}}}}}}"#;

/// A session's place in the extraction queue (ticket 17): one file per session under `extract/`.
#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
struct Entry {
    transcript: String,
    identity: String,
    /// The main checkout, whose built-in memory files are dedupe candidates; the worktree may be gone by then.
    main: String,
    /// Bytes of the transcript already extracted.
    cursor: u64,
    ended: bool,
    strikes: u32,
    failed: bool,
}

fn entry_path(sid: &str) -> PathBuf {
    crate::home().join("extract").join(format!("{sid}.json"))
}

fn load(path: &Path) -> Option<Entry> {
    serde_json::from_str(&fs::read_to_string(path).ok()?).ok()
}

fn save(path: &Path, e: &Entry) -> crate::Result<()> {
    Ok(crate::write_atomic(path, &serde_json::to_string(e)?)?)
}

/// Ticket 17: only the capture writer creates a session's entry, so a skipped session never enters the queue. Facts
/// are always scoped to a repo (ticket 21), so a session outside one has nowhere to put them.
pub fn enqueue(sid: &str, transcript: &str, repo: &git::Repo) -> crate::Result<()> {
    let path = entry_path(sid);
    if path.exists() || transcript.is_empty() {
        return Ok(());
    }
    save(
        &path,
        &Entry {
            transcript: transcript.into(),
            identity: repo.identity.clone(),
            main: repo
                .main_checkout()
                .map(|m| m.display().to_string())
                .unwrap_or_default(),
            ..Default::default()
        },
    )
}

/// SessionEnd sets `ended`; a resumed session clears it, so it waits for its end or a quiet half hour again.
pub fn mark_ended(sid: &str, ended: bool) -> crate::Result<()> {
    let path = entry_path(sid);
    match load(&path) {
        Some(mut e) if e.ended != ended => {
            e.ended = ended;
            save(&path, &e)
        }
        _ => Ok(()),
    }
}

/// `openrecall extract --job`: one worker at a time takes every due session, oldest first, then exits (ticket 17).
pub fn work() -> crate::Result<()> {
    let dir = crate::home().join("extract");
    fs::create_dir_all(&dir)?;
    let lock = fs::File::create(dir.join("lock"))?;
    if lock.try_lock().is_err() {
        return Ok(());
    }
    let settings = match Settings::load(&crate::home()) {
        Ok(s) => s,
        Err(why) => {
            crate::log(json!({"event": "extract", "off": why}));
            return Ok(());
        }
    };
    let scanner = OnceCell::new();
    let mut tried = HashSet::new();
    while let Some((path, sid, mut e)) = next_due(&dir, &tried) {
        tried.insert(sid.clone());
        let started = crate::now_ms();
        match extract(&sid, &e, &settings, scanner.get_or_init(scan::Scanner::new)) {
            Ok(mut detail) => {
                e.cursor = detail["to"].as_u64().unwrap_or(e.cursor);
                e.strikes = 0;
                detail["ms"] = json!(crate::now_ms() - started);
                crate::log(detail);
            }
            Err(Fail::Strike(why)) => {
                e.strikes += 1;
                e.failed = e.strikes >= STRIKES;
                crate::log(json!({"event": "extract", "session": sid, "strike": why,
                                  "strikes": e.strikes, "failed": e.failed}));
            }
            Err(Fail::Stop(why)) => {
                crate::log(json!({"event": "extract", "session": sid, "stop": why}));
                break;
            }
        }
        e.ended = load(&path).map_or(e.ended, |fresh| fresh.ended);
        save(&path, &e)?;
    }
    Ok(())
}

/// The oldest session whose transcript grew past its cursor and that ended or went quiet for 30 minutes. An entry
/// whose transcript is gone is deleted: its cursor is no longer needed (ticket 17).
fn next_due(dir: &Path, tried: &HashSet<String>) -> Option<(PathBuf, String, Entry)> {
    let mut due = vec![];
    for item in fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = item.path();
        let Some(sid) = path
            .file_stem()
            .and_then(|s| s.to_str())
            .map(str::to_string)
        else {
            continue;
        };
        if path.extension().is_none_or(|x| x != "json") || tried.contains(&sid) {
            continue;
        }
        let Some(e) = load(&path).filter(|e| !e.failed) else {
            continue;
        };
        let meta = match fs::metadata(&e.transcript) {
            Ok(m) => m,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                let _ = fs::remove_file(&path);
                continue;
            }
            Err(_) => continue,
        };
        let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        let quiet = modified.elapsed().is_ok_and(|age| age >= QUIET);
        if meta.len() > e.cursor && (e.ended || quiet) {
            due.push((modified, path, sid, e));
        }
    }
    due.into_iter()
        .min_by_key(|d| d.0)
        .map(|(_, path, sid, e)| (path, sid, e))
}

/// Ticket 24's two failure classes: a strike is the session's fault; a stop is not (auth, network, money, a retired
/// model), so the run ends with every cursor where it was.
#[derive(Debug, PartialEq)]
enum Fail {
    Strike(String),
    Stop(String),
}

impl From<std::io::Error> for Fail {
    fn from(e: std::io::Error) -> Fail {
        Fail::Stop(e.to_string())
    }
}

impl From<rusqlite::Error> for Fail {
    fn from(e: rusqlite::Error) -> Fail {
        Fail::Stop(e.to_string())
    }
}

struct Fact {
    kind: String,
    name: String,
    description: String,
    body: String,
}

struct Decision {
    fact: usize,
    action: String,
    address: String,
    description: String,
    body: String,
}

/// One run over one session: the extraction call, then the dedupe call, and only when both succeed, the writes.
fn extract(sid: &str, e: &Entry, s: &Settings, scanner: &scan::Scanner) -> Result<Value, Fail> {
    let plain = !e.identity.is_empty()
        && Path::new(&e.identity)
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)));
    if !plain {
        return Err(Fail::Strike("no repo identity".into()));
    }
    let mut data =
        fs::read(&e.transcript).map_err(|err| Fail::Strike(format!("transcript: {err}")))?;
    data.truncate(data.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1));
    let turns = turn::turns(&data);
    let split = turns
        .iter()
        .position(|(_, end)| *end as u64 > e.cursor)
        .unwrap_or(turns.len());
    let mut detail = json!({"event": "extract", "session": sid, "from": e.cursor, "to": data.len(), "model": s.model});
    if !turns[split..].iter().any(|(t, _)| t.real) {
        detail["proposed"] = json!(0);
        return Ok(detail);
    }
    let (text, marked) = condense(&turns, split);
    let (text, hits) = scanner.redact(&text);
    for rule in hits {
        crate::log(
            json!({"event": "redacted", "session": sid, "rule": rule, "section": "extract"}),
        );
    }
    let date = session_date(&e.transcript);
    let note = if marked {
        format!(
            "The part before the line \"{MARK}\" was already extracted in an earlier run. Read it as context only. \
             Write facts only from the turns after that line, and do not repeat facts that come from the part before it.\n"
        )
    } else {
        String::new()
    };
    let user = format!(
        "Repository: {}\nSession date: {date}\n{note}\nTranscript:\n{text}",
        e.identity
    );
    let mut usage = Usage::default();
    let (proposed, rules) = ask(
        s,
        &format!("{EXTRACT}{EXTRACT_SCHEMA}"),
        &user,
        ("facts", EXTRACT_SCHEMA),
        &mut usage,
        extracted,
    )?;

    let mut dropped: HashMap<&str, usize> = HashMap::new();
    let mut secrets = vec![];
    let mut normalized = 0;
    let mut facts = vec![];
    for v in &proposed {
        let mut f = match fact_of(v) {
            Ok(f) => f,
            Err(why) => {
                *dropped.entry(why).or_default() += 1;
                continue;
            }
        };
        let (_, hits) = scanner.redact(&format!("{}\n{}\n{}", f.name, f.description, f.body));
        if !hits.is_empty() {
            *dropped.entry("secret").or_default() += 1;
            secrets.extend(hits);
            continue;
        }
        let name = index::name_of(&f.name);
        normalized += usize::from(name != f.name);
        f.name = name;
        facts.push(f);
    }

    let home = crate::home();
    let dir = home.join("repos").join(&e.identity);
    let own = format!("{}/", e.identity);
    let main = (!e.main.is_empty()).then(|| PathBuf::from(&e.main));
    let scopes = index::scopes(&home, &crate::user_home(), Some(&e.identity), main);
    let mut ix = index::Index::open(&home)?;
    ix.sync(&scopes)?;
    let found = facts
        .iter()
        .map(|f| {
            let query = format!("{} {} {}", f.name.replace('-', " "), f.description, f.body);
            ix.search(&index::terms(&query), &scopes, CANDIDATES)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let stat = |address: &str| {
        fs::metadata(home.join("repos").join(format!("{address}.md")))
            .ok()
            .map(|m| (m.len(), m.modified().ok()))
    };
    let before: HashMap<&str, _> = found
        .iter()
        .flatten()
        .filter(|c| c.address.starts_with(&own))
        .map(|c| (c.address.as_str(), stat(&c.address)))
        .collect();
    let decided = if found.iter().any(|c| !c.is_empty()) {
        ask(
            s,
            &format!("{MERGE}{MERGE_SCHEMA}"),
            &merge_input(&e.identity, &facts, &found),
            ("decisions", MERGE_SCHEMA),
            &mut usage,
            decisions,
        )?
    } else {
        vec![]
    };

    let now = crate::now_ms();
    let secs = (now / 1000) as u64;
    let stamp = format!("{sid} {}", record::iso_ms(now));
    let suggested = suggest(&dir, &rules, sid, &date, scanner, &mut secrets)?;
    let mut actions: HashMap<&str, usize> = HashMap::new();
    let mut replaced: HashSet<&str> = HashSet::new();
    for (i, (f, cands)) in facts.iter().zip(&found).enumerate() {
        let mut line = json!({"event": "dedupe", "session": sid, "name": f.name,
            "candidates": cands.iter().map(|c| json!({"address": c.address, "score": (c.score * 100.0).round() / 100.0})).collect::<Vec<_>>()});
        let d = decided.iter().find(|d| d.fact == i);
        let action = match d {
            Some(d) if d.action == "skip" => {
                line["address"] = json!(d.address);
                "skip"
            }
            Some(d) if d.action == "replace" => {
                let why = if !cands.iter().any(|c| c.address == d.address) {
                    Some("not a candidate")
                } else if !d.address.starts_with(&own) {
                    Some("not own")
                } else if replaced.contains(d.address.as_str()) {
                    Some("replaced twice")
                } else if d.description.is_empty() || d.body.is_empty() {
                    Some("empty")
                } else if before.get(d.address.as_str()) != Some(&stat(&d.address)) {
                    Some("changed")
                } else {
                    None
                };
                let hits = || scanner.redact(&format!("{}\n{}", d.description, d.body)).1;
                if let Some(why) = why {
                    line["why"] = json!(why);
                    line["address"] = json!(write_new(&dir, &e.identity, f, &stamp, secs)?);
                    "new"
                } else if let hits @ [rule, ..] = hits().as_slice() {
                    line["address"] = json!(d.address);
                    line["rule"] = json!(rule);
                    *dropped.entry("secret").or_default() += 1;
                    secrets.extend_from_slice(hits);
                    "dropped"
                } else {
                    let path = home.join("repos").join(format!("{}.md", d.address));
                    line["copy"] = json!(replace(&path, &e.identity, f, d, &stamp, secs)?);
                    line["address"] = json!(d.address);
                    replaced.insert(&d.address);
                    "replace"
                }
            }
            _ => {
                line["address"] = json!(write_new(&dir, &e.identity, f, &stamp, secs)?);
                "new"
            }
        };
        line["action"] = json!(action);
        crate::log(line);
        *actions.entry(action).or_default() += 1;
    }
    for (key, value) in [
        ("proposed", json!(proposed.len())),
        ("new", json!(actions.get("new").unwrap_or(&0))),
        ("replaced", json!(actions.get("replace").unwrap_or(&0))),
        ("skipped", json!(actions.get("skip").unwrap_or(&0))),
        ("dropped", json!(dropped)),
        ("secret_rules", json!(secrets)),
        ("normalized", json!(normalized)),
        ("suggestions", json!(suggested)),
        ("tokens_in", json!(usage.tokens_in)),
        ("tokens_out", json!(usage.tokens_out)),
        ("reasoning", json!(usage.reasoning)),
        ("cost", json!(usage.cost)),
        ("provider", json!(usage.provider)),
    ] {
        detail[key] = value;
    }
    Ok(detail)
}

/// Ticket 17's input: real prompts, assistant text and one line per tool call, tool results left out, with the
/// marker before the first turn the last run did not read. Returns the text and whether the marker is in it.
fn condense(turns: &[(Turn, usize)], split: usize) -> (String, bool) {
    let mut out: Vec<String> = vec![];
    let (mut past, mut marked) = (false, false);
    for (i, (t, _)) in turns.iter().enumerate() {
        if !t.real {
            continue;
        }
        if i >= split && !past {
            past = true;
            if !out.is_empty() {
                out.push(MARK.into());
                marked = true;
            }
        }
        out.push(format!("[user] {}", t.ask));
        for event in &t.events {
            match event {
                Event::Text(text) => out.push(format!("[assistant] {text}")),
                Event::Tool(name, input) => out.push(format!("[tool] {}", tool_line(name, input))),
                Event::Output(..) => {}
            }
        }
    }
    (out.join("\n"), marked)
}

fn tool_line(name: &str, input: &Value) -> String {
    let arg = TOOL_KEYS.iter().find_map(|k| input.get(k)).map_or_else(
        || input.to_string(),
        |v| v.as_str().map_or_else(|| v.to_string(), str::to_string),
    );
    format!(
        "{name} {}",
        arg.chars().take(TOOL_CHARS).collect::<String>()
    )
}

/// The day the session was last active, from its transcript's mtime.
fn session_date(transcript: &str) -> String {
    let secs = fs::metadata(transcript)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map_or(crate::now_secs(), |d| d.as_secs());
    record::iso(secs)[..10].to_string()
}

/// The dedupe call's input: every candidate once, then each new fact with the addresses it looks like (ticket 26).
fn merge_input(identity: &str, facts: &[Fact], found: &[Vec<Candidate>]) -> String {
    let mut seen = HashSet::new();
    let memories: Vec<String> = found
        .iter()
        .flatten()
        .filter(|c| seen.insert(c.address.as_str()))
        .map(|c| {
            let body: String = if c.address.starts_with("builtin/") {
                c.body.chars().take(BUILTIN_CHARS).collect()
            } else {
                c.body.clone()
            };
            format!(
                "{} [{}] {}\n{}\n{body}",
                c.address, c.kind, c.date, c.description
            )
        })
        .collect();
    let new: Vec<String> = facts
        .iter()
        .zip(found)
        .enumerate()
        .map(|(i, (f, cands))| {
            let like: Vec<&str> = cands.iter().map(|c| c.address.as_str()).collect();
            format!(
                "Fact {i} [{}] {}\n{}\n{}\nLooks like: {}",
                f.kind,
                f.name,
                f.description,
                f.body,
                if like.is_empty() {
                    "nothing".into()
                } else {
                    like.join(", ")
                }
            )
        })
        .collect();
    format!(
        "Repository: {identity}\n\nExisting memories:\n\n{}\n\nNew facts:\n\n{}",
        memories.join("\n\n"),
        new.join("\n\n")
    )
}

fn expiry(kind: &str, secs: u64) -> String {
    if kind == "state" {
        index::expires_from(secs)
    } else {
        String::new()
    }
}

fn write_new(
    dir: &Path,
    identity: &str,
    f: &Fact,
    stamp: &str,
    secs: u64,
) -> std::io::Result<String> {
    let stem = index::free_stem(dir, &f.name);
    crate::write_atomic(
        &dir.join(format!("{stem}.md")),
        &index::render(&Memory {
            name: stem.clone(),
            description: f.description.clone(),
            kind: f.kind.clone(),
            scope: identity.into(),
            source: stamp.into(),
            updated: String::new(),
            expires: expiry(&f.kind, secs),
            body: f.body.clone(),
        }),
    )?;
    Ok(format!("{identity}/{stem}"))
}

/// Ticket 26: the old text goes to `replaced/<stem>.<UTC second>.md`, then the file is rewritten in place, so the
/// address stays; `source` stays too, and `updated` records the replace. Returns the copy's file name.
fn replace(
    path: &Path,
    identity: &str,
    f: &Fact,
    d: &Decision,
    stamp: &str,
    secs: u64,
) -> std::io::Result<String> {
    let text = fs::read_to_string(path)?;
    let old = index::parse(&text);
    let stem = path
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let copy = format!("{stem}.{}.md", record::iso(secs).replace(':', ""));
    crate::write_atomic(&path.with_file_name("replaced").join(&copy), &text)?;
    crate::write_atomic(
        path,
        &index::render(&Memory {
            name: if old.name.is_empty() { stem } else { old.name },
            description: d.description.clone(),
            kind: f.kind.clone(),
            scope: if old.scope.is_empty() {
                identity.into()
            } else {
                old.scope
            },
            source: old.source,
            updated: stamp.into(),
            expires: expiry(&f.kind, secs),
            body: d.body.clone(),
        }),
    )?;
    Ok(copy)
}

/// Ticket 21: CLAUDE.md suggestions go to a per-repo file that is never indexed or injected, each line once.
fn suggest(
    dir: &Path,
    rules: &[String],
    sid: &str,
    date: &str,
    scanner: &scan::Scanner,
    secrets: &mut Vec<String>,
) -> std::io::Result<usize> {
    let path = dir.join("claude-md-suggestions.md");
    let mut text = fs::read_to_string(&path).unwrap_or_default();
    let mut added = 0;
    for rule in rules {
        let line = rule.split_whitespace().collect::<Vec<_>>().join(" ");
        if line.is_empty() || text.contains(&format!("- {line} (")) {
            continue;
        }
        let (_, hits) = scanner.redact(&line);
        if !hits.is_empty() {
            secrets.extend(hits);
            continue;
        }
        text += &format!("- {line} ({sid}, {date})\n");
        added += 1;
    }
    if added > 0 {
        crate::write_atomic(&path, &text)?;
    }
    Ok(added)
}

/// The extraction reply's two lists, or None when the reply as a whole is not what the schema asks.
fn extracted(content: &str) -> Option<(Vec<Value>, Vec<String>)> {
    let v: Value = serde_json::from_str(content).ok()?;
    let o = v.as_object().filter(|o| o.len() == 2)?;
    let facts = o.get("facts")?.as_array()?.clone();
    // Sonnet 5.5 through Azure answered 4 of 10 replays of one session with one fact whose texts are all empty.
    let placeholder = |f: &Value| {
        ["name", "description", "body"]
            .iter()
            .all(|k| f[*k].as_str().is_some_and(|t| t.trim().is_empty()))
    };
    if facts.iter().any(placeholder) {
        return None;
    }
    let rules = o
        .get("claude_md_rules")?
        .as_array()?
        .iter()
        .map(|r| r.as_str().map(str::to_string))
        .collect::<Option<_>>()?;
    Some((facts, rules))
}

/// One proposed fact, or why it is dropped (ticket 21). A bad name is normalized later, never a reason.
fn fact_of(v: &Value) -> Result<Fact, &'static str> {
    let o = v.as_object().filter(|o| o.len() == 4).ok_or("shape")?;
    let field = |k: &str| {
        o.get(k)
            .and_then(Value::as_str)
            .map(str::trim)
            .ok_or("shape")
    };
    let (kind, name, description, body) = (
        field("type")?,
        field("name")?,
        field("description")?,
        field("body")?,
    );
    if !index::KINDS.contains(&kind) {
        return Err("type");
    }
    if description.is_empty() || body.is_empty() {
        return Err("empty");
    }
    Ok(Fact {
        kind: kind.into(),
        name: name.into(),
        description: description.into(),
        body: body.into(),
    })
}

/// The dedupe reply's decisions; a malformed one is left out, and its fact is written new (ticket 26).
fn decisions(content: &str) -> Option<Vec<Decision>> {
    let v: Value = serde_json::from_str(content).ok()?;
    let list = v
        .as_object()
        .filter(|o| o.len() == 1)?
        .get("decisions")?
        .as_array()?;
    Some(
        list.iter()
            .filter_map(|d| {
                let text = |k: &str| d[k].as_str().map(|s| s.trim().to_string());
                Some(Decision {
                    fact: usize::try_from(d["fact"].as_u64()?).ok()?,
                    action: text("action")?,
                    address: text("address")?,
                    description: text("description")?,
                    body: text("body")?,
                })
            })
            .collect(),
    )
}

struct Settings {
    base_url: String,
    model: String,
    key: String,
}

impl Settings {
    /// Ticket 24: `extract.toml` holds `base_url` and `model`, `api-key` the key alone. `Err` says why extraction is off.
    fn load(home: &Path) -> Result<Settings, String> {
        let toml = fs::read_to_string(home.join("extract.toml"))
            .map_err(|_| "no extract.toml".to_string())?;
        let value = |key: &str| {
            toml.lines()
                .filter_map(|l| l.split_once('='))
                .find(|(k, _)| k.trim() == key)
                .map(|(_, v)| v.trim().trim_matches('"').to_string())
                .filter(|v| !v.is_empty())
        };
        let base_url = value("base_url").ok_or("no base_url in extract.toml")?;
        let model = value("model").ok_or("no model in extract.toml")?;
        let path = home.join("api-key");
        let mode = fs::metadata(&path)
            .map_err(|_| "no api-key")?
            .permissions()
            .mode();
        if mode & 0o077 != 0 {
            return Err("api-key is readable by group or others: chmod 600 it".into());
        }
        let key = fs::read_to_string(&path).map_err(|e| e.to_string())?;
        let key = key.trim();
        if key.is_empty() || !key.chars().all(|c| c.is_ascii_graphic()) {
            return Err("api-key is not one key on one line".into());
        }
        Ok(Settings {
            base_url: base_url.trim_end_matches('/').into(),
            model,
            key: key.into(),
        })
    }
}

#[derive(Default)]
struct Usage {
    /// The upstream a router served the call from, when it says (OpenRouter's `provider`).
    provider: String,
    tokens_in: u64,
    tokens_out: u64,
    reasoning: u64,
    cost: f64,
}

/// One strict-schema request, sent again once when the reply is not what the schema asks (ticket 24).
fn ask<T>(
    s: &Settings,
    system: &str,
    user: &str,
    (name, schema): (&str, &str),
    usage: &mut Usage,
    parse: impl Fn(&str) -> Option<T>,
) -> Result<T, Fail> {
    let schema: Value = serde_json::from_str(schema).expect("the schemas are tested");
    let body = json!({
        "model": s.model,
        "messages": [{"role": "system", "content": system}, {"role": "user", "content": user}],
        "response_format": {"type": "json_schema", "json_schema": {"name": name, "strict": true, "schema": schema}},
    });
    for _ in 0..2 {
        let (content, reply) = post(s, &body)?;
        let used = &reply["usage"];
        if let Some(provider) = reply["provider"].as_str() {
            usage.provider = provider.into();
        }
        usage.tokens_in += used["prompt_tokens"].as_u64().unwrap_or(0);
        usage.tokens_out += used["completion_tokens"].as_u64().unwrap_or(0);
        usage.reasoning += used["completion_tokens_details"]["reasoning_tokens"]
            .as_u64()
            .unwrap_or(0);
        usage.cost += used["cost"].as_f64().unwrap_or(0.0);
        if let Some(parsed) = parse(&content) {
            return Ok(parsed);
        }
    }
    Err(Fail::Strike("invalid json".into()))
}

/// One `POST <base_url>/chat/completions` through macOS curl. The key goes in curl's config on stdin, never in `ps`;
/// the body goes in a 0600 file, because stdin cannot carry both (ticket 24).
fn post(s: &Settings, body: &Value) -> Result<(String, Value), Fail> {
    let tmp = crate::home()
        .join("extract")
        .join(format!("request.{}", std::process::id()));
    fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?
        .write_all(body.to_string().as_bytes())?;
    let config = format!(
        "url = \"{}/chat/completions\"\nheader = \"Authorization: Bearer {}\"\n\
         header = \"Content-Type: application/json\"\ndata-binary = \"@{}\"\nmax-time = 300\n\
         silent\nshow-error\nwrite-out = \"\\n%{{http_code}}\"\n",
        quote(&s.base_url),
        quote(&s.key),
        quote(&tmp.to_string_lossy())
    );
    let run = || -> std::io::Result<std::process::Output> {
        let mut child = Command::new("/usr/bin/curl")
            .args(["-q", "-K", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        child
            .stdin
            .take()
            .ok_or_else(|| std::io::Error::other("no stdin"))?
            .write_all(config.as_bytes())?;
        child.wait_with_output()
    };
    let out = run();
    let _ = fs::remove_file(&tmp);
    let out = out?;
    classify(out.status.code(), &String::from_utf8_lossy(&out.stdout))
}

/// curl's result as ticket 24's classes: the reply's content and the whole reply, a strike, or a stop.
fn classify(exit: Option<i32>, stdout: &str) -> Result<(String, Value), Fail> {
    if exit != Some(0) {
        return Err(Fail::Stop(format!("curl exit {}", exit.unwrap_or(-1))));
    }
    let (raw, status) = stdout.rsplit_once('\n').unwrap_or(("", stdout));
    let reply: Value = serde_json::from_str(raw).unwrap_or(Value::Null);
    let error = &reply["error"];
    let code = error["metadata"]["error_type"]
        .as_str()
        .or(error["code"].as_str())
        .unwrap_or("");
    match status.trim() {
        "200" => {}
        "400" if code == "context_length_exceeded" => {
            return Err(Fail::Strike("context length".into()));
        }
        other => return Err(Fail::Stop(format!("http {other} {code}").trim_end().into())),
    }
    let choice = &reply["choices"][0];
    match choice["finish_reason"].as_str() {
        Some("stop") => Ok((
            choice["message"]["content"].as_str().unwrap_or("").into(),
            reply.clone(),
        )),
        Some("length") => Err(Fail::Strike("length".into())),
        // A filtered session would otherwise stop every run after it for good.
        Some("content_filter") => Err(Fail::Strike("content filter".into())),
        other => Err(Fail::Stop(format!("finish {}", other.unwrap_or("none")))),
    }
}

/// A double-quoted value in a curl config file, where `\` escapes.
fn quote(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// OpenAI's strict subset: every object closed and every property required, at every level (ticket 24).
    fn strict(v: &Value) -> bool {
        if v["type"] == "object" {
            let props: Vec<&String> = v["properties"].as_object().unwrap().keys().collect();
            let required: Vec<&str> = v["required"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| r.as_str().unwrap())
                .collect();
            return v["additionalProperties"] == false
                && props.len() == required.len()
                && props.iter().all(|p| required.contains(&p.as_str()))
                && v["properties"].as_object().unwrap().values().all(strict);
        }
        if v["type"] == "array" {
            return strict(&v["items"]);
        }
        true
    }

    #[test]
    fn schemas_are_strict() {
        for schema in [EXTRACT_SCHEMA, MERGE_SCHEMA] {
            assert!(strict(&serde_json::from_str(schema).unwrap()));
        }
        assert!(EXTRACT.ends_with("matching this JSON schema:\n") && !EXTRACT.contains("scope"));
    }

    #[test]
    fn replies_are_classified() {
        let ok = r#"{"choices": [{"finish_reason": "stop", "message": {"content": "{}"}}], "usage": {"cost": 0.01}}"#;
        assert_eq!(
            classify(Some(0), &format!("{ok}\n200")).unwrap().1["usage"]["cost"],
            0.01
        );
        let finish = |r: &str| {
            classify(
                Some(0),
                &format!("{{\"choices\": [{{\"finish_reason\": \"{r}\"}}]}}\n200"),
            )
        };
        assert_eq!(finish("length").unwrap_err(), Fail::Strike("length".into()));
        assert_eq!(
            finish("content_filter").unwrap_err(),
            Fail::Strike("content filter".into())
        );
        assert_eq!(
            finish("error").unwrap_err(),
            Fail::Stop("finish error".into())
        );
        for context in [
            r#"{"error": {"code": 400, "metadata": {"error_type": "context_length_exceeded"}}}"#,
            r#"{"error": {"code": "context_length_exceeded"}}"#,
        ] {
            assert_eq!(
                classify(Some(0), &format!("{context}\n400")).unwrap_err(),
                Fail::Strike("context length".into())
            );
        }
        assert_eq!(
            classify(Some(0), "{\"error\": {\"code\": \"invalid_api_key\"}}\n401").unwrap_err(),
            Fail::Stop("http 401 invalid_api_key".into())
        );
        assert_eq!(
            classify(Some(0), "{}\n400").unwrap_err(),
            Fail::Stop("http 400".into())
        );
        assert_eq!(
            classify(Some(28), "").unwrap_err(),
            Fail::Stop("curl exit 28".into())
        );
        assert_eq!(quote(r#"a"b\c"#), r#"a\"b\\c"#);
    }

    #[test]
    fn replies_are_validated() {
        assert!(extracted("```json\n{\"facts\": [], \"claude_md_rules\": []}\n```").is_none());
        assert!(extracted(r#"{"facts": []}"#).is_none());
        assert!(extracted(r#"{"facts": [], "claude_md_rules": [1]}"#).is_none());
        assert!(
            extracted(r#"{"facts": [{"type": "pointer", "name": "", "description": "", "body": " "}], "claude_md_rules": []}"#)
                .is_none(),
            "a placeholder fact makes the reply invalid"
        );
        let (facts, rules) = extracted(
            r#"{"facts": [
                {"type": "gotcha", "name": "A B", "description": "d", "body": "b"},
                {"type": "chat", "name": "x", "description": "d", "body": "b"},
                {"type": "state", "name": "x", "description": " ", "body": "b"},
                {"type": "state", "name": "x", "description": "d"}],
               "claude_md_rules": ["Run cargo fmt."]}"#,
        )
        .unwrap();
        assert_eq!(rules, ["Run cargo fmt."]);
        let got: Vec<Result<String, &str>> =
            facts.iter().map(|f| fact_of(f).map(|f| f.name)).collect();
        assert_eq!(
            got,
            [Ok("A B".into()), Err("type"), Err("empty"), Err("shape")]
        );
        let d = decisions(
            r#"{"decisions": [{"fact": 1, "action": "replace", "address": " a/b/c ", "description": "d", "body": "b"},
                              {"fact": "f0", "action": "new", "address": "", "description": "", "body": ""}]}"#,
        )
        .unwrap();
        assert_eq!((d.len(), d[0].fact, d[0].address.as_str()), (1, 1, "a/b/c"));
    }

    #[test]
    fn the_input_marks_what_the_last_run_read() {
        let user = |t: &str| json!({"type": "user", "message": {"content": t}});
        let said = |t: &str, tool: Value| json!({"type": "assistant", "message": {"content": [{"type": "text", "text": t}, tool]}});
        let bash = json!({"type": "tool_use", "id": "t1", "name": "Bash", "input": {"command": "cargo test"}});
        let lines = [
            user("first ask"),
            said("first answer", bash),
            user("<task-notification>x</task-notification>"),
            user("second ask"),
            said(
                "second answer",
                json!({"type": "tool_use", "id": "t2", "name": "Mystery", "input": {"n": 1}}),
            ),
        ];
        let data: String = lines.iter().map(|l| format!("{l}\n")).collect();
        let turns = turn::turns(data.as_bytes());
        assert_eq!(turns.len(), 3);
        let whole = "[user] first ask\n[assistant] first answer\n[tool] Bash cargo test\n\
                     [user] second ask\n[assistant] second answer\n[tool] Mystery {\"n\":1}";
        assert_eq!(condense(&turns, 0), (whole.to_string(), false));
        let (text, marked) = condense(&turns, 1);
        assert!(
            marked && text.contains("[tool] Bash cargo test\n=== NEW TURNS ===\n[user] second ask")
        );
        assert!(!condense(&turns, 3).1);
    }
}
