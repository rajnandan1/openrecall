use crate::index::IDENT;
use crate::turn::{self, Event, Turn};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::sync::LazyLock;

const DAY_SECS: u64 = 24 * 60 * 60;
/// A row written this long after its day ended is final (ticket 05).
const FINAL_MS: u64 = 72 * 60 * 60 * 1000;
/// The harness's `turn_of`: a transcript writes a prompt's line up to about 10 s after the hook runs.
const NEAR_MS: i64 = 15_000;
/// The harness's `line_idents`: the identifiers an injected line of at most this many characters carries.
const LINE_CHARS: usize = 200;
const EDITS: [&str; 4] = ["Edit", "Write", "NotebookEdit", "MultiEdit"];

static GIT_COMMIT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\bgit(?:\s+-C\s+\S+)?\s+commit\b").unwrap());

type Counts = BTreeMap<String, u64>;

#[derive(Serialize, Default)]
struct Used {
    yes: u64,
    judged: u64,
}

#[derive(Serialize, Default)]
struct Spread {
    median: i64,
    p95: i64,
}

#[derive(Serialize, Default)]
struct Verdicts {
    right: u64,
    wrong: u64,
    unjudged: u64,
}

#[derive(Serialize, Default)]
struct Late {
    late: u64,
    judged: u64,
}

#[derive(Serialize, Default)]
struct Facts {
    new: u64,
    replaced: u64,
    skipped: u64,
    dropped: u64,
}

/// One local day's row (spec section 4): every field, in this order.
#[derive(Serialize, Default)]
struct Row {
    day: String,
    written_at: u64,
    versions: Vec<String>,
    sessions: usize,
    prompts: u64,
    searched: u64,
    prompt_skips: Counts,
    injections: u64,
    identifier_rule: u64,
    echoes: u64,
    used_signal: Used,
    hook_ms: Spread,
    index_size: u64,
    pick_jobs: u64,
    picks: u64,
    pick_skips: Counts,
    picks_delivered: u64,
    pick_drops: Counts,
    picks_used: Used,
    pick_cost: f64,
    pick_ms: i64,
    pushes: Counts,
    right_pushes: Verdicts,
    late_pushes: Late,
    extraction_runs: u64,
    facts: Facts,
    suggestions: u64,
    strikes: u64,
    failed_sessions: u64,
    stops: u64,
    extraction_cost: f64,
    extraction_ms: i64,
    redactions: u64,
    own_tool_calls: Counts,
    update_check: Counts,
    errors: u64,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct State {
    attempted_at: u64,
}

/// What the job reads of a row it keeps: a final row's line is copied byte for byte, never parsed whole.
#[derive(Deserialize)]
struct Head {
    day: String,
    written_at: u64,
}

fn dir() -> PathBuf {
    crate::home().join("stats")
}

pub fn rows_file() -> PathBuf {
    dir().join("daily.jsonl")
}

/// The stats lock of the job and the `stats` tool, held until the file drops; None while another process holds it.
pub fn lock() -> Option<fs::File> {
    fs::create_dir_all(dir()).ok()?;
    let file = fs::File::create(dir().join("lock")).ok()?;
    file.try_lock().ok()?;
    Some(file)
}

/// `openrecall stats --job`: at most once in 24 hours, and not while another process holds the lock. A failure keeps
/// the old rows file and leaves an `error` line, which the next row counts.
pub fn job() -> crate::Result<()> {
    let Some(_lock) = lock() else {
        return Ok(());
    };
    let path = dir().join("state.json");
    let mut state: State = fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    let now = crate::now_secs();
    if now.abs_diff(state.attempted_at) < DAY_SECS {
        return Ok(());
    }
    state.attempted_at = now;
    let done = crate::write_atomic(&path, &serde_json::to_string(&state)?)
        .map_err(Into::into)
        .and_then(|()| recompute());
    if let Err(e) = done {
        crate::log(json!({"event": "error", "cmd": "stats", "error": e.to_string()}));
    }
    Ok(())
}

/// Computes every open day's row and replaces the rows file in one write; the caller holds `lock()`. An open day has
/// log lines and either no row or a row written less than 72 hours after the day ended.
pub fn recompute() -> crate::Result<()> {
    let now = crate::now_ms() as u64;
    let db = rusqlite::Connection::open_in_memory()?;
    let mut local_day = db.prepare("SELECT date(?1, 'unixepoch', 'localtime')")?;
    let mut day_end = db.prepare("SELECT unixepoch(?1, '+1 day', 'utc')")?;
    // ken: whole-log read each run; an offset index when the read is measured over 1 s
    let log = read_or_none(&crate::home().join("log").join("openrecall.jsonl"))?;
    let mut lines: Vec<(String, Value)> = vec![];
    for raw in log.split(|&b| b == b'\n') {
        let Ok(e) = serde_json::from_slice::<Value>(raw) else {
            continue;
        };
        let Some(at) = e["at"].as_u64() else {
            continue;
        };
        let day: String = local_day.query_row([(at / 1000) as i64], |r| r.get(0))?;
        lines.push((day, e));
    }

    let mut rows: BTreeMap<String, String> = BTreeMap::new();
    let mut written: HashMap<String, u64> = HashMap::new();
    for line in String::from_utf8_lossy(&read_or_none(&rows_file())?).lines() {
        if let Ok(head) = serde_json::from_str::<Head>(line) {
            written.insert(head.day.clone(), head.written_at);
            rows.insert(head.day, line.to_string());
        }
    }
    let mut open: BTreeMap<&str, Vec<&Value>> = BTreeMap::new();
    for (day, e) in &lines {
        open.entry(day.as_str()).or_default().push(e);
    }
    let mut finals = HashSet::new();
    for day in open.keys() {
        if let Some(w) = written.get(*day) {
            let end: i64 = day_end.query_row([*day], |r| r.get(0))?;
            if *w >= end as u64 * 1000 + FINAL_MS {
                finals.insert(*day);
            }
        }
    }
    let versions = versions_by_day(&lines);
    open.retain(|day, _| !finals.contains(day));

    let mut fresh: BTreeMap<&str, Row> = open
        .iter()
        .map(|(day, day_lines)| {
            let mut row = count(day_lines);
            row.day = day.to_string();
            row.written_at = now;
            row.versions = versions[day].clone();
            (*day, row)
        })
        .collect();
    judge(&lines, &mut fresh);
    for (day, row) in fresh {
        rows.insert(day.to_string(), serde_json::to_string(&row)?);
    }
    let text: String = rows.values().map(|l| format!("{l}\n")).collect();
    Ok(crate::write_atomic(&rows_file(), &text)?)
}

/// A missing file reads as empty; any other read error fails the run, so a final row is never overwritten unread.
fn read_or_none(path: &std::path::Path) -> std::io::Result<Vec<u8>> {
    match fs::read(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(vec![]),
        read => read,
    }
}

/// Each day's versions from its `update` lines (the `from` of a binary line, the `to` of a swap or a plugin line), in
/// log order and without repeats; a day without them takes the last version logged before it.
fn versions_by_day(lines: &[(String, Value)]) -> HashMap<&str, Vec<String>> {
    let mut seen: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (day, e) in lines {
        let list = seen.entry(day.as_str()).or_default();
        if e["event"] == "update" {
            let from = (e["step"] == "binary").then(|| &e["from"]);
            let to = (e["step"] == "plugin" || e["result"] == "updated").then(|| &e["to"]);
            list.extend([from, to].into_iter().flatten().filter_map(Value::as_str));
        }
    }
    let mut last: Option<&str> = None;
    let mut out = HashMap::new();
    for (day, list) in seen {
        let mut versions: Vec<String> = vec![];
        for v in &list {
            if !versions.iter().any(|x| x == v) {
                versions.push(v.to_string());
            }
        }
        if versions.is_empty() {
            versions.extend(last.map(str::to_string));
        }
        last = list.last().copied().or(last);
        out.insert(day, versions);
    }
    out
}

/// The harness's `pct`: index ⌊q·n⌋ of the sorted values, capped at n−1; 0 without values.
fn pct(values: &mut [i64], q: f64) -> i64 {
    values.sort_unstable();
    let n = values.len();
    if n == 0 { 0 } else { values[((q * n as f64) as usize).min(n - 1)] }
}

fn bump(counts: &mut Counts, key: &Value) {
    let key = key.as_str().map_or_else(|| key.to_string(), str::to_string);
    *counts.entry(key).or_default() += 1;
}

/// The day's numbers that come from the log alone.
fn count(lines: &[&Value]) -> Row {
    let mut r = Row::default();
    let mut sessions = HashSet::new();
    let (mut hook, mut pick_ms, mut extraction_ms) = (vec![], vec![], vec![]);
    let n = |v: &Value| v.as_u64().unwrap_or(0);
    for e in lines {
        if let Some(s) = e["session"].as_str().filter(|s| !s.is_empty()) {
            sessions.insert(s);
        }
        match e["event"].as_str().unwrap_or("") {
            "recall" => {
                r.prompts += 1;
                r.searched += u64::from(e.get("candidates").is_some());
                r.injections += gate_injections(e).len() as u64;
                if let Some(via) = e["via"].as_object() {
                    r.identifier_rule += via.values().filter(|v| *v == "identifier").count() as u64;
                }
                r.echoes += e["echoes"].as_array().map_or(0, Vec::len) as u64;
                if let (Some(a), Some(b)) = (e["started_at"].as_i64(), e["ended_at"].as_i64()) {
                    hook.push(b - a);
                }
                if let Some(size) = e["index_size"].as_u64() {
                    r.index_size = size;
                }
            }
            "skipped" => bump(&mut r.prompt_skips, &json!(e["reason"].as_str().unwrap_or("").replace('-', "_"))),
            "pick" => {
                r.pick_jobs += 1;
                r.picks += u64::from(e["memory"].as_str().is_some_and(|m| !m.is_empty()));
                if e["skip"].as_str().is_some_and(|s| !s.is_empty()) {
                    bump(&mut r.pick_skips, &e["skip"]);
                }
                r.pick_cost += e["cost"].as_f64().unwrap_or(0.0);
                pick_ms.extend(e["ms"].as_i64());
            }
            "deliver" => r.picks_delivered += 1,
            "pick_drop" => bump(&mut r.pick_drops, &e["reason"]),
            "pushed" => bump(&mut r.pushes, &e["how"]),
            "extract" => {
                r.extraction_runs += u64::from(e.get("to").is_some());
                r.strikes += u64::from(e.get("strike").is_some());
                r.failed_sessions += u64::from(e["failed"] == true);
                r.stops += u64::from(e.get("stop").is_some());
                r.facts.new += n(&e["new"]);
                r.facts.replaced += n(&e["replaced"]);
                r.facts.skipped += n(&e["skipped"]);
                if let Some(dropped) = e["dropped"].as_object() {
                    r.facts.dropped += dropped.values().map(n).sum::<u64>();
                }
                r.suggestions += n(&e["suggestions"]);
                r.extraction_cost += e["cost"].as_f64().unwrap_or(0.0);
                extraction_ms.extend(e["ms"].as_i64());
            }
            "redacted" => r.redactions += 1,
            "mcp" => bump(&mut r.own_tool_calls, &e["tool"]),
            "update" => {
                let key = format!("{}_{}", e["step"].as_str().unwrap_or(""), e["result"].as_str().unwrap_or(""));
                bump(&mut r.update_check, &json!(key));
            }
            "error" => r.errors += 1,
            _ => {}
        }
    }
    r.sessions = sessions.len();
    r.hook_ms = Spread { median: pct(&mut hook, 0.5), p95: pct(&mut hook, 0.95) };
    r.pick_ms = pct(&mut pick_ms, 0.5);
    r.extraction_ms = pct(&mut extraction_ms, 0.5);
    r
}

/// The transcript numbers of the open days (spec section 5), the harness's checks over one session's transcript at
/// a time.
fn judge(lines: &[(String, Value)], rows: &mut BTreeMap<&str, Row>) {
    let mut recalls: HashMap<&str, Vec<&Value>> = HashMap::new();
    let mut picks: HashMap<&str, &Value> = HashMap::new();
    let mut checks: BTreeMap<&str, Vec<(&str, &Value)>> = BTreeMap::new();
    for (day, e) in lines {
        let sid = e["session"].as_str().unwrap_or("");
        let event = e["event"].as_str().unwrap_or("");
        match event {
            "recall" => recalls.entry(sid).or_default().push(e),
            "pick" => {
                picks.insert(e["prompt_id"].as_str().unwrap_or(""), e);
            }
            _ => {}
        }
        let check = match event {
            "recall" => !gate_injections(e).is_empty(),
            "pushed" | "deliver" => true,
            _ => false,
        };
        if check && rows.contains_key(day.as_str()) {
            checks.entry(sid).or_default().push((day, e));
        }
    }
    let files = transcripts();
    for (sid, items) in checks {
        let Some(data) = files.get(sid).and_then(|p| fs::read(p).ok()) else {
            continue;
        };
        let s = Session::new(turn::turns(&data).into_iter().map(|(t, _)| t).collect());
        for (day, e) in items {
            let row = rows.get_mut(day).expect("a check only for an open day");
            match e["event"].as_str().unwrap_or("") {
                "recall" => {
                    let Some(t) = s.turn_of(e).filter(|&t| s.turns[t].answered) else { continue };
                    for address in gate_injections(e) {
                        let ids = match e.get("identifiers") {
                            Some(map) => Some(strings(&map[address])),
                            None => memory_idents(address),
                        };
                        if let Some(ids) = ids {
                            row.used_signal.judged += 1;
                            row.used_signal.yes += u64::from(used(&s.turns[t], &ids));
                        }
                    }
                }
                "deliver" => {
                    let id = e["prompt_id"].as_str().unwrap_or("");
                    let turn = s.turns.iter().find(|t| !id.is_empty() && t.prompt_id == id);
                    let Some(t) = turn.filter(|t| t.answered) else { continue };
                    let ids = match picks.get(id).and_then(|p| p.get("identifiers")) {
                        Some(list) => Some(strings(list)),
                        None => memory_idents(e["memory"].as_str().unwrap_or("")),
                    };
                    if let Some(ids) = ids {
                        row.picks_used.judged += 1;
                        row.picks_used.yes += u64::from(used(t, &ids));
                    }
                }
                "pushed" => {
                    let record = match e.get("branch") {
                        Some(branch) => Some((branch.as_str().unwrap_or("").to_string(), strings(&e["aliases"]))),
                        None => record_of(e["address"].as_str().unwrap_or("")),
                    };
                    let at = e["at"].as_i64().unwrap_or(0);
                    let bracket = recalls.get(sid).into_iter().flatten().find(|r| {
                        r["started_at"].as_i64().unwrap_or(0) <= at && at <= r["ended_at"].as_i64().unwrap_or(0)
                    });
                    let (Some((branch, aliases)), Some(t)) = (record, bracket.and_then(|r| s.turn_of(r))) else {
                        continue;
                    };
                    if !matches!(e["how"].as_str(), Some("sibling" | "file")) {
                        match s.verdict(&branch, &aliases) {
                            Verdict::Right => row.right_pushes.right += 1,
                            Verdict::Wrong => row.right_pushes.wrong += 1,
                            Verdict::Unjudged => row.right_pushes.unjudged += 1,
                        }
                    }
                    row.late_pushes.judged += 1;
                    row.late_pushes.late += u64::from(s.late(t, &branch));
                }
                _ => {}
            }
        }
    }
}

/// The addresses the gate injected: injected and a candidate, so not a pushed record.
fn gate_injections(e: &Value) -> Vec<&str> {
    let candidates = candidates(e);
    addresses(&e["injected"]).filter(|a| candidates.contains(a)).collect()
}

fn strings(list: &Value) -> Vec<String> {
    addresses(list).map(str::to_string).collect()
}

/// `~/.claude/projects/*/<session>.jsonl` by session, listed once per run; of two, the later path as the harness's
/// sorted glob keeps.
fn transcripts() -> HashMap<String, PathBuf> {
    let mut out: HashMap<String, PathBuf> = HashMap::new();
    let projects = crate::user_home().join(".claude").join("projects");
    for dir in fs::read_dir(projects).into_iter().flatten().flatten() {
        for file in fs::read_dir(dir.path()).into_iter().flatten().flatten() {
            let path = file.path();
            let Some(sid) = path.file_stem().and_then(|s| s.to_str()).filter(|_| path.extension().is_some_and(|x| x == "jsonl")) else {
                continue;
            };
            if out.get(sid).is_none_or(|p| *p < path) {
                out.insert(sid.to_string(), path.clone());
            }
        }
    }
    out
}

/// One session's turns, as `scan` and `load` of the harness see them.
struct Session {
    turns: Vec<Turn>,
    /// The real turns: no task notification, and no built-in command, which gets no assistant line.
    real: Vec<usize>,
    /// The tickets the real prompts name, a lower-case form in upper case (the harness's `tickets_in`).
    tickets: HashSet<String>,
}

enum Verdict {
    Right,
    Wrong,
    Unjudged,
}

impl Session {
    fn new(turns: Vec<Turn>) -> Session {
        let real: Vec<usize> = (0..turns.len())
            .filter(|&i| turns[i].real && (turns[i].answered || !turns[i].ask.starts_with('/')))
            .collect();
        let tickets = real
            .iter()
            .flat_map(|&i| {
                let ask = &turns[i].ask;
                let lower = turn::LOWER.find_iter(ask).map(|m| m.as_str().to_uppercase());
                turn::tickets(ask).into_iter().chain(lower)
            })
            .collect();
        Session { turns, real, tickets }
    }

    /// The harness's `verdict`: right when the session names an alias of the record or does 2 or more real turns on
    /// its branch, else unjudged with fewer than 2 real prompts, else wrong.
    fn verdict(&self, branch: &str, aliases: &[String]) -> Verdict {
        let on_branch = self.real.iter().filter(|&&i| self.on(i, branch)).count();
        if aliases.iter().any(|a| self.tickets.contains(a)) || on_branch >= 2 {
            Verdict::Right
        } else if self.real.len() < 2 {
            Verdict::Unjudged
        } else {
            Verdict::Wrong
        }
    }

    /// The harness's `is_late`: a turn before the push's turn ended on the record's branch and edited a file or ran
    /// `git commit`.
    fn late(&self, push: usize, branch: &str) -> bool {
        (0..push).any(|i| {
            self.on(i, branch)
                && self.turns[i].events.iter().any(|e| match e {
                    Event::Tool(name, input) => {
                        EDITS.contains(&name.as_str())
                            || name == "Bash" && GIT_COMMIT.is_match(input["command"].as_str().unwrap_or(""))
                    }
                    _ => false,
                })
        })
    }

    fn on(&self, turn: usize, branch: &str) -> bool {
        !branch.is_empty() && self.turns[turn].branch == branch
    }

    /// The harness's `turn_of`: the turn of the line's prompt hash nearest in time, else the turn nearest in time within
    /// 15 s, else none.
    fn turn_of(&self, e: &Value) -> Option<usize> {
        let at = e["started_at"].as_i64().or(e["at"].as_i64());
        let gap = |t: &Turn| at.map_or(0, |a| (t.at - a).abs());
        let hash = e["prompt_hash"].as_str();
        let all = self.turns.iter().enumerate();
        let same = all.clone().filter(|(_, t)| hash == Some(crate::fnv(&t.ask).as_str())).min_by_key(|(_, t)| gap(t));
        if same.is_some() || at.is_none() {
            return same.map(|(i, _)| i);
        }
        let (i, near) = all.min_by_key(|(_, t)| gap(t))?;
        (gap(near) <= NEAR_MS).then_some(i)
    }
}

/// The harness's `used_signal`: the turn's assistant text or tool inputs hold an identifier of the line that the prompt
/// does not hold.
fn used(t: &Turn, ids: &[String]) -> bool {
    let asked: HashSet<&str> = IDENT.find_iter(&t.ask).map(|m| m.as_str()).collect();
    let mut texts = vec![];
    let mut inputs = vec![];
    for e in &t.events {
        match e {
            Event::Text(text) => texts.push(text.clone()),
            Event::Tool(_, input) => inputs.push(input.to_string()),
            Event::Output(..) => {}
        }
    }
    let after = format!("{}\n{}", texts.join("\n"), inputs.join("\n"));
    ids.iter().any(|i| !asked.contains(i.as_str()) && after.contains(i.as_str()))
}

/// The fallback for a line from before 0.10.0: the identifiers of the memory's file as it stands now, by the harness's
/// `line_idents`. None when the file is gone.
fn memory_idents(address: &str) -> Option<Vec<String>> {
    let (path, _) = crate::mcp::file_of(address, &crate::home(), &crate::user_home())?;
    let text = String::from_utf8_lossy(&fs::read(path).ok()?).into_owned();
    let all = |s: &str| IDENT.find_iter(s).map(|m| m.as_str().to_string()).collect::<Vec<_>>();
    let body = text.strip_prefix("---\n").and_then(|t| t.split_once("\n---")).map_or(text.as_str(), |(_, b)| b);
    let body = body.split_whitespace().collect::<Vec<_>>().join(" ");
    if body.chars().count() <= LINE_CHARS {
        return Some(all(&body));
    }
    let desc = front(&text, "description");
    let mut ids = all(&desc);
    let mut room = LINE_CHARS as i64 - desc.chars().count().min(LINE_CHARS) as i64 - 3;
    for i in all(&body) {
        let len = i.chars().count() as i64;
        if !desc.contains(&i) && !ids.contains(&i) && len + 2 <= room {
            room -= len + 2;
            ids.push(i);
        }
    }
    Some(ids)
}

/// The fallback for a `pushed` line from before 0.10.0: the branch and aliases of the record's file as it stands now,
/// else of its newest retired copy. None when both are gone.
fn record_of(address: &str) -> Option<(String, Vec<String>)> {
    let (path, _) = crate::mcp::file_of(address, &crate::home(), &crate::user_home())?;
    let path = if path.exists() {
        path
    } else {
        let stem = path.file_stem()?.to_string_lossy().into_owned();
        let retired = path.with_file_name("retired");
        fs::read_dir(&retired)
            .ok()?
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.len() >= stem.len() + 4 && n.starts_with(&format!("{stem}.")) && n.ends_with(".md"))
            .max()
            .map(|n| retired.join(n))?
    };
    let text = String::from_utf8_lossy(&fs::read(path).ok()?).into_owned();
    let aliases = front(&text, "aliases").split(", ").filter(|a| !a.is_empty()).map(str::to_string).collect();
    Some((front(&text, "branch"), aliases))
}

/// The harness's `front`: one flat front-matter value from the first 4,000 characters.
fn front(text: &str, key: &str) -> String {
    let head: String = text.chars().take(4000).collect();
    Regex::new(&format!(r"(?m)^\s*{key}:\s*(.*)$"))
        .ok()
        .and_then(|re| re.captures(&head).map(|c| c[1].trim().trim_matches(['"', '\'']).to_string()))
        .unwrap_or_default()
}

fn addresses(list: &Value) -> impl Iterator<Item = &str> {
    list.as_array().into_iter().flatten().filter_map(Value::as_str)
}

fn candidates(e: &Value) -> HashSet<&str> {
    e["candidates"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| c["address"].as_str())
        .collect()
}
