use crate::extract::{self, Fail, Settings};
use crate::{git, index, scan, turn};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;

/// 16,000 tokens at 2.0 characters a token, the system prompt included (spec 3.3).
const MAX_INPUT: usize = 32_000;
const MAX_CALLS: u64 = 10;
/// Past this many memories, a request lists only those that best match the prompt and the newest turn (issue #23).
const MAX_LISTED: usize = 50;
/// `PICK_SYSTEM["strict"]` of ticket 07's prototype, quoted in spec 3.4.
const SYSTEM: &str = "You pick memories for a coding assistant (Claude Code) that is about to act on the user's prompt. \
    You get the list of memories (id, name and description), the session so far, newest turn first, and the prompt. \
    Pick at most one memory, and only one that states a fact, decision, pointer or gotcha that the assistant needs to \
    act on this prompt and that neither the prompt nor the session already contains. A memory about the same project \
    but another task, or one that only repeats what the prompt or the session says, is a wrong pick. Most prompts need \
    no memory: pick none unless you are sure. Return its id, or no id.";
const NO_TURN: &str = "(no earlier turn)";
/// `SYSTEM_FILE` of ticket 05's prototype (spec 4.1, rule 7): the system prompt of a request with named text.
const SYSTEM_FILE: &str = "You pick memories for a coding assistant (Claude Code) that is about to act on the user's \
    prompt. You get the list of memories (id, name and description), the text of the files the prompt names, the \
    session so far, newest turn first, and the prompt. Pick at most one memory, and only one that states a fact, \
    decision, pointer or gotcha that the assistant needs to act on this prompt and that neither the prompt, the named \
    files nor the session already contains. A memory about the same project but another task, or one that only repeats \
    what the prompt, the named files or the session says, is a wrong pick. Most prompts need no memory: pick none \
    unless you are sure. Return its id, or no id.";
/// The named text's cap in a request, after the secret scan (spec 4.1, rule 5).
const NAMED_CHARS: usize = 8000;
const MAX_DOC_BYTES: u64 = 262_144;
const FILE_LED_WORDS: usize = 10;

/// The prompt hook starts the job for a prompt that passed every skip rule, when picks are on, the prompt has a plain
/// `prompt_id` and the session has calls left (spec 3.2). The hook never waits for the job.
pub fn start(input: &Value, s: &Value) -> crate::Result<()> {
    if prompt_id(input).is_none()
        || s["picks"].as_u64().unwrap_or(0) >= MAX_CALLS
        || !extract::picks_on(&crate::home())
    {
        return Ok(());
    }
    crate::detach(&["pick", "--job"], Stdio::piped())?
        .stdin
        .take()
        .ok_or("no stdin")?
        .write_all(input.to_string().as_bytes())?;
    Ok(())
}

/// `openrecall pick --job`: at most one call for one prompt. `.running` marks the job until its result or its end.
pub fn job(started: u128) -> crate::Result<()> {
    let input = crate::read_input()?;
    let sid = crate::session_id(&input)?;
    let id = prompt_id(&input).ok_or("no usable prompt_id")?;
    let dir = dir(&sid);
    fs::create_dir_all(&dir)?;
    let running = dir.join(format!("{id}.running"));
    fs::write(&running, "")?;
    let mut line = json!({"event": "pick", "session": sid, "prompt_id": id, "tokens_in": 0, "tokens_out": 0,
                          "cost": 0.0, "memory": null, "skip": null, "named_docs": 0, "named_chars": 0,
                          "named_sent": 0});
    let result = pick(&input, &sid, &mut line).unwrap_or_else(|e| {
        line["skip"] = json!("error");
        line["error"] = json!(e.to_string());
        None
    });
    if let Some(result) = result {
        let _ = publish(&running, &result);
    }
    let _ = fs::remove_file(&running);
    line["ms"] = json!(crate::now_ms() - started);
    crate::log(line);
    Ok(())
}

/// The result file's JSON, or None; `line` gets the usage, the picked memory and why it was skipped or dropped.
fn pick(input: &Value, sid: &str, line: &mut Value) -> crate::Result<Option<Value>> {
    let home = crate::home();
    let settings = Settings::load(&home)?;
    let prompt = input["prompt"].as_str().unwrap_or("");
    let repo = git::Repo::find(input["cwd"].as_str().unwrap_or(""));
    let (ix, scopes) = index::open_for(&home, &crate::user_home(), repo.as_ref())?;
    let turns = turns(input, prompt);
    let scanner = scan::Scanner::new();
    let folder = repo.as_ref().map_or_else(
        || PathBuf::from(input["cwd"].as_str().unwrap_or("")),
        |r| r.folder.clone(),
    );
    let named = named_text(prompt, &folder, &scanner, line);
    let memories = ix.all(&scopes)?;
    line["memories"] = json!(memories.len());
    let memories = if memories.len() <= MAX_LISTED {
        memories
    } else {
        let newest = turns.last().map(extract::short_form).unwrap_or_default();
        let mut best = ix.search(&index::terms(&format!("{prompt}\n{newest}")), &scopes, MAX_LISTED)?;
        best.sort_by(|a, b| a.address.cmp(&b.address));
        best
    };
    line["listed"] = json!(memories.len());
    let Some((user, sent)) = message(&memories, &turns, prompt, &named, &scanner) else {
        line["skip"] = json!("too_big");
        return Ok(None);
    };
    line["named_sent"] = json!(sent);
    {
        let (_lock, mut s) = crate::load_session(sid)?;
        let picks = s["picks"].as_u64().unwrap_or(0);
        if picks >= MAX_CALLS {
            line["skip"] = json!("cap");
            return Ok(None);
        }
        s["picks"] = json!(picks + 1);
        crate::save_session(sid, &s)?;
    }
    let body = json!({
        "model": settings.model,
        "max_tokens": 500,
        "reasoning": {"effort": "low"},
        "messages": [{"role": "system", "content": if sent > 0 { SYSTEM_FILE } else { SYSTEM }},
                     {"role": "user", "content": user}],
        "response_format": {"type": "json_schema", "json_schema": {"name": "pick", "strict": true, "schema":
            {"type": "object", "properties": {"ids": {"type": "array", "items": {"type": "string"}}},
             "required": ["ids"], "additionalProperties": false}}},
    });
    let mut usage = extract::Usage::default();
    let content = extract::post(&settings, &body, &mut usage);
    line["tokens_in"] = json!(usage.tokens_in);
    line["tokens_out"] = json!(usage.tokens_out);
    line["cost"] = json!(usage.cost);
    let content = content.map_err(|(Fail::Strike(e) | Fail::Stop(e))| e)?;
    let reply: Value = serde_json::from_str(&content).map_err(|_| "invalid json")?;
    let ids = reply["ids"].as_array().ok_or("invalid json")?;
    let Some((c, _)) = ids.first().and_then(Value::as_str).and_then(|id| {
        memories
            .iter()
            .zip(1..)
            .find(|(_, n)| format!("m{n}") == id)
    }) else {
        return Ok(None);
    };
    line["memory"] = json!(c.address);
    let drop = crate::drop_reason(c, sid, repo.as_ref())
        .or_else(|| index::echo(c, prompt).then_some("echo"));
    if let Some(reason) = drop {
        line["skip"] = json!(reason);
        return Ok(None);
    }
    let picked = index::line(c, index::MAX_CHARS - index::FRAME.chars().count() - 1);
    line["identifiers"] = json!(index::identifiers(c, &picked));
    let text = format!("{}\n{picked}", index::FRAME);
    Ok(Some(json!({"address": c.address, "text": text})))
}

/// The session's real turns, oldest first, without the transcript's copy of the prompt.
fn turns(input: &Value, prompt: &str) -> Vec<turn::Turn> {
    // ken: parses the whole transcript (p90 5 MB); read it from the end if pick ms grows with transcript size.
    let data = fs::read(input["transcript_path"].as_str().unwrap_or("")).unwrap_or_default();
    let mut turns: Vec<turn::Turn> = turn::turns(&data)
        .into_iter()
        .map(|(t, _)| t)
        .filter(|t| t.real)
        .collect();
    if turns.last().is_some_and(|t| t.ask == prompt) {
        turns.pop();
    }
    turns
}

/// The text of the documents that a file-led prompt names (spec 4.1): each regular file of 256 KB or less, secrets
/// redacted on the whole file, as `=== {path}` blocks in prompt order. `line` gets its documents and characters.
fn named_text(prompt: &str, folder: &Path, scanner: &scan::Scanner, line: &mut Value) -> String {
    let query = index::query_of(prompt).unwrap_or(prompt);
    let found = turn::named_paths(query);
    let words = found
        .iter()
        .fold(query.to_string(), |q, p| q.replace(p, " "));
    if words.split_whitespace().count() > FILE_LED_WORDS {
        return String::new();
    }
    let mut docs = turn::named_docs(prompt, folder, &crate::user_home());
    let mut seen = HashSet::new();
    docs.retain(|(_, path)| seen.insert(path.clone()));
    let blocks: Vec<String> = docs
        .iter()
        .filter_map(|(typed, path)| {
            fs::metadata(path)
                .ok()
                .filter(|m| m.is_file() && m.len() <= MAX_DOC_BYTES)?;
            let text = fs::read_to_string(path).ok()?;
            Some(format!("=== {typed}\n{}", scanner.redact(&text).0))
        })
        .collect();
    let text = blocks.join("\n\n");
    line["named_docs"] = json!(blocks.len());
    line["named_chars"] = json!(text.chars().count());
    text
}

/// The memory list, the named text, the session's turns newest first while they fit, and the prompt (notes call 6),
/// with the characters of named text sent. None when the list, the system prompt and the prompt alone pass `MAX_INPUT`
/// characters. The named text takes at most `NAMED_CHARS` of the room they leave, and never the room of `NO_TURN`.
fn message(
    memories: &[index::Candidate],
    turns: &[turn::Turn],
    prompt: &str,
    named: &str,
    scanner: &scan::Scanner,
) -> Option<(String, usize)> {
    let list: Vec<String> = memories
        .iter()
        .zip(1..)
        .map(|(c, n)| format!("- m{n}: {}: {}", c.name, c.description))
        .collect();
    let list = list.join("\n");
    let asked = scanner.redact(prompt).0;
    let frame = |files: &str, turns: &str| {
        let files = if files.is_empty() { String::new() } else { format!("## Named files\n{files}\n\n") };
        format!(
            "## Memories\n{list}\n\n{files}## Session so far, newest turn first\n{turns}\n\n## Prompt\n{asked}"
        )
    };
    let room_with = |system: &str, files: &str| {
        MAX_INPUT
            .checked_sub(system.chars().count() + frame(files, "").chars().count())
            .and_then(|r| r.checked_sub(NO_TURN.chars().count()))
    };
    let mut room = room_with(SYSTEM, "")?;
    let cap = room_with(SYSTEM_FILE, " ").map_or(0, |r| (r + 1).min(NAMED_CHARS));
    let files: String = if named.is_empty() { String::new() } else { named.chars().take(cap).collect() };
    let sent = files.chars().count();
    if sent > 0 {
        room = room_with(SYSTEM_FILE, &files)?;
    }
    room += NO_TURN.chars().count();
    let mut kept: Vec<String> = vec![];
    for t in turns.iter().rev() {
        let text = scanner.redact(&extract::short_form(t)).0;
        let need = text.chars().count() + if kept.is_empty() { 0 } else { 2 };
        if need > room {
            if kept.is_empty() {
                kept.push(text.chars().take(room).collect());
            }
            break;
        }
        room -= need;
        kept.push(text);
    }
    let turns = if kept.is_empty() { NO_TURN.into() } else { kept.join("\n\n") };
    Some((frame(&files, &turns), sent))
}

/// `openrecall deliver`, the tool hook: the rename to `.taken` lets one of parallel tool calls take the pick (spec 3.7).
pub fn deliver(started: u128) -> crate::Result<()> {
    let input = crate::read_input()?;
    let Some(id) = prompt_id(&input) else {
        return Ok(());
    };
    if !input["agent_id"].is_null() || !extract::picks_on(&crate::home()) {
        return Ok(());
    }
    let sid = crate::session_id(&input)?;
    let dir = dir(&sid);
    let taken = dir.join(format!("{id}.taken"));
    if fs::rename(dir.join(format!("{id}.json")), &taken).is_err() {
        return Ok(());
    }
    let delivered = take(&taken, &sid, id, started);
    let _ = fs::remove_file(&taken);
    delivered
}

/// The taken pick goes to the session, unless the session ledger already holds its memory.
fn take(taken: &Path, sid: &str, id: &str, started: u128) -> crate::Result<()> {
    let result: Value = serde_json::from_str(&fs::read_to_string(taken)?)?;
    let address = result["address"].as_str().ok_or("no address in the pick")?;
    let (_lock, mut s) = crate::load_session(sid)?;
    if crate::seen(&s, address) {
        crate::log(json!({"event": "pick_drop", "session": sid, "prompt_id": id, "memory": address, "reason": "ledger"}));
        return Ok(());
    }
    crate::see(&mut s, address);
    crate::save_session(sid, &s)?;
    println!(
        "{}",
        json!({"hookSpecificOutput": {"hookEventName": "PostToolUse", "additionalContext": result["text"]}})
    );
    crate::log(json!({"event": "deliver", "session": sid, "prompt_id": id, "ms": crate::now_ms() - started,
                      "memory": address}));
    Ok(())
}

/// The Stop hook drops what the turn left (spec 3.8). `.running` goes first: a job that publishes meanwhile leaves a
/// `.json` for the second pass, and a job that publishes later finds no `.running`.
pub fn end_turn(sid: &str) {
    let dir = dir(sid);
    let files = |ext: &str| -> Vec<(String, PathBuf)> {
        fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == ext))
            .map(|p| (p.file_stem().unwrap_or_default().to_string_lossy().into_owned(), p))
            .collect()
    };
    for (id, path) in files("running") {
        if fs::remove_file(&path).is_ok() {
            crate::log(json!({"event": "pick_drop", "session": sid, "prompt_id": id, "memory": null, "reason": "late"}));
        }
    }
    for (id, path) in files("json") {
        let result: Value = fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        if fs::remove_file(&path).is_ok() {
            crate::log(json!({"event": "pick_drop", "session": sid, "prompt_id": id, "memory": result["address"],
                              "reason": "no_tool_call"}));
        }
    }
}

/// The SessionEnd hook deletes the session's picks (spec 3.8).
pub fn end_session(sid: &str) {
    let _ = fs::remove_dir_all(dir(sid));
}

/// A session's pick results: `<prompt_id>.running` while the job works, then `.json`, then `.taken` in the tool hook.
fn dir(sid: &str) -> PathBuf {
    crate::home().join("picks").join(sid)
}

fn prompt_id(input: &Value) -> Option<&str> {
    input["prompt_id"].as_str().filter(|id| crate::plain(id))
}

/// The result goes into `.running`, opened without create, which then becomes `.json`: a `.running` that the Stop or
/// SessionEnd hook removed makes the open or the rename fail, so a late job writes nothing (notes call 7).
fn publish(running: &Path, result: &Value) -> std::io::Result<()> {
    fs::OpenOptions::new()
        .write(true)
        .open(running)?
        .write_all(result.to_string().as_bytes())?;
    fs::rename(running, running.with_extension("json"))
}
