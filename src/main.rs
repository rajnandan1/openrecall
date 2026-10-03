mod extract;
mod git;
mod index;
mod mcp;
mod record;
mod scan;
mod turn;

use record::Record;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

const INSTALL: &str =
    "cargo install --locked --git https://github.com/rajnandan1/openrecall --root ~/.local";

/// Every path exits 0: exit 2 on UserPromptSubmit would erase the user's prompt (ticket 18).
fn main() {
    let started = now_ms();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let _ = std::panic::catch_unwind(|| run(&args, started));
    std::process::exit(0);
}

fn run(args: &[String], started: u128) {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    if args == ["mcp"] {
        if let Err(e) = mcp::serve() {
            log(json!({"event": "error", "cmd": "mcp", "error": e.to_string()}));
        }
        return;
    }
    if session_skipped() {
        return;
    }
    let result = match args.as_slice() {
        ["handoff"] => handoff(),
        ["recall"] => recall(started),
        ["capture"] => capture_hook(),
        ["capture", "--job"] => capture_job(),
        ["extract"] => extract_hook(),
        ["extract", "--job"] => extract::work(),
        _ => return,
    };
    if let Err(e) = result {
        log(json!({"event": "error", "cmd": args.join(" "), "error": e.to_string()}));
    }
}

/// Ticket 15: an opted-out, headless or scratchpad session writes nothing anywhere.
fn session_skipped() -> bool {
    let var = |k| std::env::var(k).unwrap_or_default();
    let project = var("CLAUDE_PROJECT_DIR");
    var("OPENRECALL") == "0"
        || var("CLAUDE_CODE_ENTRYPOINT") == "sdk-cli"
        || ["/private/tmp/claude-", "/tmp/claude-"]
            .iter()
            .any(|p| project.starts_with(p))
}

/// SessionStart: record where the session starts, warn on a version mismatch, retire idle records.
fn handoff() -> Result<()> {
    let input = read_input()?;
    let sid = session_id(&input)?;
    if let Some(warning) = version_mismatch() {
        println!("{}", json!({"systemMessage": warning}));
    }
    let source = input["source"].as_str().unwrap_or("startup");
    let branch = git::Repo::find(input["cwd"].as_str().unwrap_or(""))
        .and_then(|r| r.branch())
        .unwrap_or_default();
    let (_lock, mut s) = load_session(&sid)?;
    match source {
        "compact" => s["ledger"] = json!([]),
        "resume" | "fork" => {}
        _ => s = json!({"branch": branch, "turns": 0, "ledger": []}),
    }
    s["source"] = json!(source);
    save_session(&sid, &s)?;
    sweep();
    if source == "resume" {
        extract::mark_ended(&sid, false)?;
    }
    detach(&["extract", "--job"], Stdio::null())?;
    Ok(())
}

fn version_mismatch() -> Option<String> {
    let root = std::env::var_os("CLAUDE_PLUGIN_ROOT")?;
    let manifest: Value = serde_json::from_str(
        &fs::read_to_string(Path::new(&root).join(".claude-plugin/plugin.json")).ok()?,
    )
    .ok()?;
    let plugin = manifest["version"].as_str()?;
    let binary = env!("CARGO_PKG_VERSION");
    (plugin != binary).then(|| {
        format!(
            "OpenRecall binary {binary} does not match plugin {plugin}. Update: {INSTALL} --force"
        )
    })
}

/// Ticket 08: a record idle for 7 days retires; session and status files that old are deleted.
fn sweep() {
    let now = now_secs();
    let repos = home().join("repos");
    let mut dirs = vec![];
    handoff_dirs(&repos, &mut dirs);
    for dir in dirs {
        for (path, rec) in record::live(&dir) {
            if rec.stale(now) && record::retire(&path, now).is_ok() {
                let identity = dir
                    .parent()
                    .and_then(|p| p.strip_prefix(&repos).ok())
                    .unwrap_or(Path::new(""));
                log(
                    json!({"event": "retired", "address": address_of(&identity.to_string_lossy(), &path)}),
                );
            }
        }
    }
    for sub in ["sessions", "status"] {
        for entry in fs::read_dir(home().join(sub))
            .into_iter()
            .flatten()
            .flatten()
        {
            let age = entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok());
            if age.is_some_and(|a| a.as_secs() > record::RETIRE_SECS) {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
}

fn handoff_dirs(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        if !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        match entry.file_name().to_str() {
            Some("handoffs") => out.push(entry.path()),
            Some("replaced") => {}
            _ => handoff_dirs(&entry.path(), out),
        }
    }
}

/// UserPromptSubmit: push the task's handoff record once the prompt proves the task (ticket 12), then
/// search the level-2 memories for the prompt (ticket 15's order).
fn recall(started: u128) -> Result<()> {
    let input = read_input()?;
    let sid = session_id(&input)?;
    let prompt = input["prompt"].as_str().unwrap_or("");
    let skip = if !input["agent_id"].is_null() {
        Some("subagent")
    } else if turn::is_notification(prompt) {
        Some("notification")
    } else {
        None
    };
    let (_lock, mut s) = load_session(&sid)?;
    let total = s["injections"].as_u64().unwrap_or(0);
    if let Some(reason) = skip {
        log(
            json!({"event": "skipped", "session": sid, "reason": reason, "prompt_hash": fnv(prompt)}),
        );
        return status(&sid, &format!("recall {total} · skipped"));
    }
    let repo = git::Repo::find(input["cwd"].as_str().unwrap_or(""));
    let mut context = vec![];
    let mut injected = vec![];
    if let Some(repo) = &repo
        && let Some((path, rec, how)) = pick(repo, &s, &sid, prompt)
    {
        let address = address_of(&repo.identity, &path);
        let text = fs::read_to_string(&path)?;
        let body = text.split_once("\n---\n").map_or(text.as_str(), |(_, b)| b);
        let written = if rec.answered_at.is_empty() { &rec.updated_at } else { &rec.answered_at };
        context.push(format!(
            "Handoff record for branch {}, last written {} by an earlier session on this task ({address}). \
             It reflects what was true then; check the working tree before acting on it.\n\n{body}",
            rec.branch, written
        ));
        see(&mut s, &address);
        log(json!({"event": "pushed", "session": sid, "address": address, "how": how}));
        injected.push(address);
    }
    let mut searched = json!({});
    match index::query_of(prompt) {
        Err(reason) => log(
            json!({"event": "skipped", "session": sid, "reason": reason, "prompt_hash": fnv(prompt)}),
        ),
        Ok(query) => match level2(query, repo.as_ref(), &sid, &s) {
            Ok((lines, detail)) => {
                if !lines.is_empty() {
                    let block: Vec<&str> = lines.iter().map(|(_, l)| l.as_str()).collect();
                    context.push(format!("{}\n{}", index::FRAME, block.join("\n")));
                }
                for (address, _) in lines {
                    see(&mut s, &address);
                    injected.push(address);
                }
                searched = detail;
            }
            Err(e) => log(json!({"event": "error", "cmd": "recall search", "error": e.to_string()})),
        },
    }
    if !context.is_empty() {
        println!(
            "{}",
            json!({"hookSpecificOutput": {"hookEventName": "UserPromptSubmit", "additionalContext": context.join("\n\n")}})
        );
    }
    let total = total + injected.len() as u64;
    s["injections"] = json!(total);
    save_session(&sid, &s)?;
    let ended = now_ms();
    let mut line = json!({"event": "recall", "session": sid, "started_at": started, "ended_at": ended,
                          "prompt_hash": fnv(prompt), "injected": injected});
    if let (Some(line), Some(searched)) = (line.as_object_mut(), searched.as_object()) {
        line.extend(searched.clone());
    }
    log(line);
    status(&sid, &format!("recall {total} · {} ms", ended - started))
}

/// Ticket 15's last steps: search the scope, drop what must not be injected (the ledger, this session's own facts,
/// a state fact past its `expires`, a pointer whose path is gone: ticket 09), keep at most 3 lines above the gate within the 400-token budget. Returns the lines with their addresses, and the log detail.
/// The lines are picked first; then each, best first, takes its whole memory if that fits in the room they leave.
fn level2(
    query: &str,
    repo: Option<&git::Repo>,
    sid: &str,
    s: &Value,
) -> Result<(Vec<(String, String)>, Value)> {
    let terms = index::terms(query);
    let scopes = index::scopes(
        &home(),
        &user_home(),
        repo.map(|r| r.identity.as_str()),
        repo.and_then(git::Repo::main_checkout),
    );
    let today = &record::iso(now_secs())[..10];
    let mut ix = index::Index::open(&home())?;
    ix.sync(&scopes)?;
    let own = |stamp: &str| stamp.split_whitespace().next() == Some(sid);
    let mut dropped: HashMap<&str, usize> = HashMap::new();
    let mut kept = vec![];
    for c in ix.search(&terms, &scopes, 10)? {
        let reason = if seen(s, &c.address) {
            Some("ledger")
        } else if own(&c.source) || own(&c.updated) {
            Some("own")
        } else if !c.expires.is_empty() && c.expires.as_str() <= today {
            Some("expired")
        } else if c.kind == "pointer"
            && index::rotted(&c.body, repo.map(|r| r.folder.as_path()), &user_home())
        {
            Some("rot")
        } else {
            None
        };
        match reason {
            Some(r) => *dropped.entry(r).or_default() += 1,
            None => kept.push(c),
        }
    }
    kept.truncate(5);
    let (size, scores) = ix.scores(&terms, &kept)?;
    let mut lines = vec![];
    let mut chars = index::FRAME.chars().count();
    for (c, _) in kept
        .iter()
        .zip(&scores)
        .filter(|&(_, &score)| size >= index::MIN_ROWS && score >= index::GATE)
        .take(index::MAX_LINES)
    {
        let line = index::line(c, 0);
        if chars + line.chars().count() + 1 > index::MAX_CHARS {
            break;
        }
        chars += line.chars().count() + 1;
        lines.push((c, line));
    }
    for (c, line) in &mut lines {
        let whole = index::line(c, index::MAX_CHARS - chars + line.chars().count());
        chars = chars - line.chars().count() + whole.chars().count();
        *line = whole;
    }
    let lines: Vec<(String, String)> = lines
        .into_iter()
        .map(|(c, line)| (c.address.clone(), line))
        .collect();
    let candidates: Vec<Value> = kept
        .iter()
        .zip(&scores)
        .map(|(c, score)| json!({"address": c.address, "score": (score * 1000.0).round() / 1000.0,
                                 "bm25": (c.bm25 * 100.0).round() / 100.0, "text_hash": c.text_hash}))
        .collect();
    Ok((
        lines,
        json!({"terms": terms.len(), "index_size": size, "candidates": candidates, "dropped": dropped}),
    ))
}

/// The record to push, if any: an alias the prompt names first (several records may share a ticket: the newest
/// wins, and the one written from this folder breaks a tie, ticket 12), then the settled branch's record.
fn pick(
    repo: &git::Repo,
    s: &Value,
    sid: &str,
    prompt: &str,
) -> Option<(PathBuf, Record, &'static str)> {
    let source = s["source"].as_str().unwrap_or("startup");
    if matches!(source, "resume" | "fork") {
        return None;
    }
    let now = now_secs();
    let usable = |path: &Path, r: &Record| {
        let own = !r.writers.is_empty() && r.writers.iter().all(|w| w == sid);
        !r.stale(now)
            && (!own || matches!(source, "clear" | "compact"))
            && !seen(s, &address_of(&repo.identity, path))
    };
    let dir = record::dir(&home(), &repo.identity);
    let named = turn::tickets(prompt);
    if !named.is_empty() {
        let hit = record::live(&dir)
            .into_iter()
            .filter(|(p, r)| usable(p, r) && r.aliases.iter().any(|a| named.contains(a)))
            .max_by_key(|(_, r)| {
                (
                    r.updated_at.clone(),
                    r.folder == repo.folder.to_string_lossy(),
                )
            });
        if let Some((path, rec)) = hit {
            return Some((path, rec, "alias"));
        }
    }
    let branch = repo.branch().filter(|b| git::is_task(b))?;
    let settled = source == "compact"
        || (s["branch"] == branch.as_str() && s["turns"].as_u64().unwrap_or(0) >= 1);
    let path = dir.join(format!("{}.md", record::stem(&branch)));
    let rec = Record::parse(&fs::read_to_string(&path).ok()?);
    let foreign = named.iter().any(|t| !rec.aliases.contains(t));
    (settled && !foreign && usable(&path, &rec)).then_some((
        path,
        rec,
        if source == "compact" {
            "compact"
        } else {
            "settle"
        },
    ))
}

/// Stop: hand the turn to a detached writer and return at once (tickets 01 and 06).
fn capture_hook() -> Result<()> {
    let mut job = read_input()?;
    session_id(&job)?;
    let transcript = job["transcript_path"].as_str().unwrap_or("").to_string();
    job["transcript_len"] = json!(fs::metadata(&transcript).map_or(0, |m| m.len()));
    detach(&["capture", "--job"], Stdio::piped())?
        .stdin
        .take()
        .ok_or("no stdin")?
        .write_all(job.to_string().as_bytes())?;
    Ok(())
}

/// SessionEnd: mark the session ended and start the extraction worker, then return (ticket 17).
fn extract_hook() -> Result<()> {
    let input = read_input()?;
    extract::mark_ended(&session_id(&input)?, true)?;
    detach(&["extract", "--job"], Stdio::null())?;
    Ok(())
}

/// A child in its own process group with null output, started through the binary's own path (ticket 18).
fn detach(args: &[&str], stdin: Stdio) -> Result<std::process::Child> {
    Ok(Command::new(std::env::current_exe()?)
        .args(args)
        .stdin(stdin)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()?)
}

/// The detached writer: settle bookkeeping for the session, then the turn into its task's record. The secret scan
/// compiles before the session lock, so a prompt that waits for the lock does not wait for the compile.
fn capture_job() -> Result<()> {
    let job = read_input()?;
    let sid = session_id(&job)?;
    // ken: reads the whole transcript (p90 5 MB, max 12 MB here); read the tail if transcripts grow past ~100 MB.
    let mut data = fs::read(job["transcript_path"].as_str().unwrap_or(""))?;
    data.truncate(
        job["transcript_len"]
            .as_u64()
            .map_or(data.len(), |n| n as usize),
    );
    let Some(turn) = turn::last_turn(&data, job["last_assistant_message"].as_str().unwrap_or(""))
    else {
        return Ok(());
    };
    let repo = git::Repo::find(job["cwd"].as_str().unwrap_or(""));
    if let Some(repo) = &repo {
        extract::enqueue(&sid, job["transcript_path"].as_str().unwrap_or(""), repo)?;
    }
    let branch = repo.as_ref().and_then(|r| r.branch()).unwrap_or_default();
    let scanner = git::is_task(&branch).then(scan::Scanner::new);
    let (_lock, mut s) = load_session(&sid)?;
    let last = s["branch"].as_str().map(str::to_string);
    if last.as_deref() != Some(branch.as_str()) {
        s["turns"] = json!(0);
    }
    if let (Some(old), Some(repo)) = (last.filter(|o| !o.is_empty() && *o != branch), &repo) {
        let renamed = git::is_task(&old) && git::is_task(&branch) && !repo.has_branch(&old);
        log(json!({"event": "hop", "session": sid, "from": old, "to": branch, "renamed": renamed}));
        if renamed {
            rename(repo, &old, &branch, &mut s)?;
        }
    }
    s["branch"] = json!(branch);
    if turn.real && job["stop_hook_active"] != true {
        let turns = s["turns"].as_u64().unwrap_or(0) + 1;
        s["turns"] = json!(turns);
        if turns == 1 && git::is_task(&branch) {
            log(json!({"event": "settled", "session": sid, "branch": branch}));
        }
    }
    if let (Some(repo), Some(scanner)) = (&repo, &scanner) {
        write_record(repo, &branch, &sid, &turn, scanner, &mut s)?;
    }
    save_session(&sid, &s)
}

/// A context window that has not seen an existing record (neither pushed nor created it) merges partially,
/// so the record's last ask and answer stay the earlier session's until the push.
fn write_record(
    repo: &git::Repo,
    branch: &str,
    sid: &str,
    turn: &turn::Turn,
    scanner: &scan::Scanner,
    s: &mut Value,
) -> Result<()> {
    let path = record::dir(&home(), &repo.identity).join(format!("{}.md", record::stem(branch)));
    let address = address_of(&repo.identity, &path);
    let existing = fs::read_to_string(&path).ok();
    let full = existing.is_none() || seen(s, &address);
    let mut rec = match &existing {
        Some(text) => Record::parse(text),
        None => Record {
            repo: repo.identity.clone(),
            branch: branch.to_string(),
            folder: repo.folder.display().to_string(),
            ..Default::default()
        },
    };
    let clean = |text: &str, section: &str| {
        let (out, rules) = scanner.redact(text);
        for rule in rules {
            log(json!({"event": "redacted", "session": sid, "rule": rule, "section": section}));
        }
        out
    };
    let mut found = turn.mine(
        &repo.folder.to_string_lossy(),
        &user_home().to_string_lossy(),
    );
    found.commands = found.commands.iter().map(|c| clean(c, "command")).collect();
    let (ask, answer) = if full && turn.real {
        (
            clean(&turn.ask, "last_ask"),
            clean(&turn.answer(), "last_answer"),
        )
    } else {
        (String::new(), String::new())
    };
    rec.merge(&found, &ask, &answer, full, sid, &record::iso_ms(now_ms()));
    write_atomic(&path, &rec.render())?;
    if existing.is_none() {
        see(s, &address);
    }
    Ok(())
}

/// Ticket 12: the old ref is gone, so the branch was renamed and its record follows it.
fn rename(repo: &git::Repo, old: &str, new: &str, s: &mut Value) -> Result<()> {
    let dir = record::dir(&home(), &repo.identity);
    let (from, to) = (
        dir.join(format!("{}.md", record::stem(old))),
        dir.join(format!("{}.md", record::stem(new))),
    );
    if !from.is_file() || to.exists() {
        return Ok(());
    }
    let mut rec = Record::parse(&fs::read_to_string(&from)?);
    rec.branch = new.to_string();
    write_atomic(&to, &rec.render())?;
    fs::remove_file(&from)?;
    let (old_address, new_address) = (
        address_of(&repo.identity, &from),
        address_of(&repo.identity, &to),
    );
    if let Some(ledger) = s["ledger"].as_array_mut() {
        ledger
            .iter_mut()
            .filter(|a| **a == old_address.as_str())
            .for_each(|a| *a = json!(new_address));
    }
    Ok(())
}

/// Ticket 19: a record's address is its file's path under the repos root, without `.md`.
fn address_of(identity: &str, path: &Path) -> String {
    format!(
        "{identity}/handoffs/{}",
        path.file_stem().unwrap_or_default().to_string_lossy()
    )
}

/// The session ledger: addresses this context window was pushed or created, reset on clear and compact.
fn seen(s: &Value, address: &str) -> bool {
    s["ledger"]
        .as_array()
        .is_some_and(|l| l.iter().any(|a| a == address))
}

fn see(s: &mut Value, address: &str) {
    match s["ledger"].as_array_mut() {
        Some(ledger) => ledger.push(json!(address)),
        None => s["ledger"] = json!([address]),
    }
}

fn home() -> PathBuf {
    match std::env::var_os("OPENRECALL_HOME") {
        Some(h) if !h.is_empty() => PathBuf::from(h),
        _ => user_home().join(".openrecall"),
    }
}

fn user_home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
}

fn read_input() -> Result<Value> {
    let mut text = String::new();
    std::io::stdin().read_to_string(&mut text)?;
    Ok(serde_json::from_str(&text)?)
}

/// Session ids name files, so only a plain id is accepted.
fn session_id(input: &Value) -> Result<String> {
    let sid = input["session_id"].as_str().unwrap_or("");
    let plain = !sid.is_empty()
        && sid
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if plain {
        Ok(sid.to_string())
    } else {
        Err("no usable session_id".into())
    }
}

/// The session state, locked until the returned file drops: the hooks and the detached writer each read, change
/// and save it whole, so each keeps the lock from this read to its save.
fn load_session(sid: &str) -> Result<(fs::File, Value)> {
    let dir = home().join("sessions");
    fs::create_dir_all(&dir)?;
    let lock = fs::File::create(dir.join(format!("{sid}.lock")))?;
    lock.lock()?;
    let text = fs::read_to_string(dir.join(format!("{sid}.json"))).unwrap_or_default();
    let s = serde_json::from_str::<Value>(&text)
        .ok()
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}));
    Ok((lock, s))
}

fn save_session(sid: &str, s: &Value) -> Result<()> {
    Ok(write_atomic(
        &home().join("sessions").join(format!("{sid}.json")),
        &s.to_string(),
    )?)
}

/// Ticket 18: one line for the user's own statusline script.
fn status(sid: &str, line: &str) -> Result<()> {
    Ok(write_atomic(
        &home().join("status").join(sid),
        &format!("{line}\n"),
    )?)
}

fn log(mut event: Value) {
    event["at"] = json!(now_ms());
    let path = home().join("log").join("openrecall.jsonl");
    let _ = fs::create_dir_all(home().join("log"));
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = f.write_all(format!("{event}\n").as_bytes());
    }
}

/// Temp file, then rename: two detached writers can race on one record.
pub fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    fs::write(&tmp, text)?;
    fs::rename(&tmp, path)
}

/// FNV-1a 64: the log joins a line to its prompt without holding the prompt's text.
pub fn fnv(text: &str) -> String {
    let h = text.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3)
    });
    format!("{h:016x}")
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis())
}

fn now_secs() -> u64 {
    (now_ms() / 1000) as u64
}

#[cfg(test)]
mod tests {
    #[test]
    fn plugin_version_matches_the_binary() {
        let plugin: serde_json::Value =
            serde_json::from_str(include_str!("../plugin/.claude-plugin/plugin.json")).unwrap();
        assert_eq!(plugin["version"], env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn fnv_is_stable() {
        assert_eq!(super::fnv(""), "cbf29ce484222325");
        assert_eq!(super::fnv("a"), "af63dc4c8601ec8c");
    }
}
