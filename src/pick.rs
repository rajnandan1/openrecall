use crate::extract::{self, Fail, Settings};
use crate::{git, index, scan, turn};
use serde_json::{Value, json};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;

/// 16,000 tokens at 2.0 characters a token, the system prompt included (spec 3.3).
const MAX_INPUT: usize = 32_000;
const CAP: u64 = 10;
/// `PICK_SYSTEM["strict"]` of ticket 07's prototype, quoted in spec 3.4.
const SYSTEM: &str = "You pick memories for a coding assistant (Claude Code) that is about to act on the user's prompt. \
    You get the list of memories (id, name and description), the session so far, newest turn first, and the prompt. \
    Pick at most one memory, and only one that states a fact, decision, pointer or gotcha that the assistant needs to \
    act on this prompt and that neither the prompt nor the session already contains. A memory about the same project \
    but another task, or one that only repeats what the prompt or the session says, is a wrong pick. Most prompts need \
    no memory: pick none unless you are sure. Return its id, or no id.";
const NO_TURN: &str = "(no earlier turn)";

/// The prompt hook starts the job for a prompt that passed every skip rule, when picks are on, the prompt has a plain
/// `prompt_id` and the session has calls left (spec 3.2). The hook never waits for the job.
pub fn start(input: &Value, s: &Value) -> crate::Result<()> {
    if prompt_id(input).is_none()
        || s["picks"].as_u64().unwrap_or(0) >= CAP
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
                          "cost": 0.0, "memory": null, "skip": null});
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
    let scopes = index::scopes(
        &home,
        &crate::user_home(),
        repo.as_ref().map(|r| r.identity.as_str()),
        repo.as_ref().and_then(git::Repo::main_checkout),
    );
    let mut ix = index::Index::open(&home)?;
    ix.sync(&scopes)?;
    let memories = ix.all(&scopes)?;
    let Some(user) = message(&memories, input, prompt) else {
        line["skip"] = json!("too_big");
        return Ok(None);
    };
    {
        let (_lock, mut s) = crate::load_session(sid)?;
        let picks = s["picks"].as_u64().unwrap_or(0);
        if picks >= CAP {
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
        "messages": [{"role": "system", "content": SYSTEM}, {"role": "user", "content": user}],
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
    let drop = crate::drop_rule(c, sid, repo.as_ref())
        .or_else(|| index::echo(c, prompt).then_some("echo"));
    if let Some(reason) = drop {
        line["skip"] = json!(reason);
        return Ok(None);
    }
    let text = format!(
        "{}\n{}",
        index::FRAME,
        index::line(c, index::MAX_CHARS - index::FRAME.chars().count() - 1)
    );
    Ok(Some(json!({"address": c.address, "text": text})))
}

/// The memory list, the session's turns newest first while they fit, and the prompt (notes call 6). None when the list,
/// the system prompt and the prompt alone pass `MAX_INPUT` characters.
fn message(memories: &[index::Candidate], input: &Value, prompt: &str) -> Option<String> {
    let scanner = scan::Scanner::new();
    let list: Vec<String> = memories
        .iter()
        .zip(1..)
        .map(|(c, n)| format!("- m{n}: {}: {}", c.name, c.description))
        .collect();
    let list = list.join("\n");
    let asked = scanner.redact(prompt).0;
    let frame = |turns: &str| {
        format!(
            "## Memories\n{list}\n\n## Session so far, newest turn first\n{turns}\n\n## Prompt\n{asked}"
        )
    };
    let fixed = SYSTEM.chars().count() + frame("").chars().count();
    let mut room = MAX_INPUT
        .checked_sub(fixed)
        .filter(|r| *r >= NO_TURN.chars().count())?;
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
    Some(frame(&if kept.is_empty() {
        NO_TURN.into()
    } else {
        kept.join("\n\n")
    }))
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
    let result: Value = serde_json::from_str(&fs::read_to_string(&taken)?)?;
    let address = result["address"].as_str().ok_or("no address in the pick")?;
    let (_lock, mut s) = crate::load_session(&sid)?;
    if crate::seen(&s, address) {
        let _ = fs::remove_file(&taken);
        crate::log(json!({"event": "pick_drop", "session": sid, "prompt_id": id, "memory": address, "reason": "ledger"}));
        return Ok(());
    }
    crate::see(&mut s, address);
    crate::save_session(&sid, &s)?;
    println!(
        "{}",
        json!({"hookSpecificOutput": {"hookEventName": "PostToolUse", "additionalContext": result["text"]}})
    );
    let _ = fs::remove_file(&taken);
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

/// A session's pick results: `<prompt_id>.running` while the job works, then `.json`, then `.taken` in the tool hook.
pub fn dir(sid: &str) -> PathBuf {
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
