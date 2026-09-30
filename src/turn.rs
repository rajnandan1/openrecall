use regex::Regex;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::LazyLock;

/// One turn of a transcript: the prompt that opened it and what the assistant did after it.
pub struct Turn {
    /// The prompt as the UserPromptSubmit hook sees it; a slash command is its name plus arguments.
    pub ask: String,
    /// False for a task notification, which is not a prompt.
    pub real: bool,
    pub events: Vec<Event>,
}

pub enum Event {
    Text(String),
    Tool(String, Value),
    /// A Bash command and the first 4000 characters of its stdout.
    Output(String, String),
}

/// Identifiers found in a turn, each list newest first.
#[derive(Default)]
pub struct Found {
    pub aliases: Vec<String>,
    pub tickets: Vec<String>,
    pub prs: Vec<String>,
    pub commits: Vec<String>,
    pub paths: Vec<String>,
    pub commands: Vec<String>,
}

const NOT_A_PROMPT: [&str; 5] = [
    "<local-command-stdout>",
    "<local-command-caveat>",
    "<system-reminder>",
    "<bash-input>",
    "This session is being continued from a previous conversation",
];
const CMD_VERBS: [&str; 25] = [
    "git", "gh", "pnpm", "npm", "yarn", "bun", "cargo", "make", "pytest", "python", "python3",
    "uv", "go", "docker", "kubectl", "helm", "orca", "ruff", "mypy", "tsc", "node", "deno", "just",
    "railway", "linear",
];

pub(crate) static TICKET: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b[A-Z]{2,5}-\d{2,5}\b").unwrap());
static PR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(\bPR\s*#?|pull/|#)(\d{2,6})\b").unwrap());
static GH_PR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\bgh pr \w+ (\d{2,6})\b").unwrap());
static PULL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"pull/(\d+)").unwrap());
static WORD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[\w-]+").unwrap());
static PATH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?:~/|/)?(?:[\w.@-]+/)+[\w.@-]*[A-Za-z][\w.@-]*\.[A-Za-z]\w{0,7}(?::\d+(?:-\d+)?)?",
    )
    .unwrap()
});
pub(crate) static LINE_SUFFIX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r":\d+(?:-\d+)?$").unwrap());

pub fn is_notification(prompt: &str) -> bool {
    prompt.contains("<task-notification>")
}

pub fn tickets(text: &str) -> Vec<String> {
    TICKET
        .find_iter(text)
        .map(|m| m.as_str().to_string())
        .collect()
}

/// The prompt a transcript line carries, or None when it is not a prompt line.
fn prompt_of(line: &Value) -> Option<String> {
    if line["type"] != "user" || line["isMeta"] == true {
        return None;
    }
    let text = match &line["message"]["content"] {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => {
            if blocks.iter().any(|b| b["type"] == "tool_result") {
                return None;
            }
            let texts: Vec<&str> = blocks
                .iter()
                .filter(|b| b["type"] == "text")
                .filter_map(|b| b["text"].as_str())
                .collect();
            texts.join("\n")
        }
        _ => return None,
    };
    if text.trim().is_empty() {
        return None;
    }
    if text.contains("<command-name>") {
        let name = between(&text, "<command-name>", "</command-name>").unwrap_or("/?");
        let args = between(&text, "<command-args>", "</command-args>").unwrap_or("");
        return Some(
            format!("{} {}", name.trim(), args.trim())
                .trim()
                .to_string(),
        );
    }
    if !is_notification(&text)
        && NOT_A_PROMPT
            .iter()
            .any(|p| text.trim_start().starts_with(p))
    {
        return None;
    }
    Some(text)
}

fn between<'a>(text: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let start = text.find(open)? + open.len();
    Some(&text[start..start + text[start..].find(close)?])
}

/// The last turn in a transcript's bytes. `last_message` is the Stop hook's `last_assistant_message`,
/// which the transcript may not hold yet because Claude Code writes it asynchronously.
pub fn last_turn(transcript: &[u8], last_message: &str) -> Option<Turn> {
    let mut tail = vec![];
    for raw in transcript.split(|&b| b == b'\n').rev() {
        let Ok(line) = serde_json::from_slice::<Value>(raw) else {
            continue;
        };
        if line["isSidechain"] == true {
            continue;
        }
        let Some(ask) = prompt_of(&line) else {
            tail.push(line);
            continue;
        };
        let mut turn = Turn {
            real: !is_notification(&ask),
            ask,
            events: vec![],
        };
        let mut tools: HashMap<String, (String, Value)> = HashMap::new();
        for line in tail.iter().rev() {
            let Some(blocks) = line["message"]["content"].as_array() else {
                continue;
            };
            if line["type"] == "assistant" && line["isApiErrorMessage"] != true {
                for b in blocks {
                    if b["type"] == "text" && b["text"].as_str().is_some_and(|t| !t.is_empty()) {
                        turn.events
                            .push(Event::Text(b["text"].as_str().unwrap().to_string()));
                    } else if b["type"] == "tool_use" {
                        let (name, input) = (
                            b["name"].as_str().unwrap_or("").to_string(),
                            b["input"].clone(),
                        );
                        tools.insert(
                            b["id"].as_str().unwrap_or("").to_string(),
                            (name.clone(), input.clone()),
                        );
                        turn.events.push(Event::Tool(name, input));
                    }
                }
            } else if line["type"] == "user" {
                for b in blocks.iter().filter(|b| b["type"] == "tool_result") {
                    let Some((name, input)) =
                        b["tool_use_id"].as_str().and_then(|id| tools.get(id))
                    else {
                        continue;
                    };
                    let result = &line["toolUseResult"];
                    let out = result["stdout"].as_str().or(result.as_str()).unwrap_or("");
                    if name == "Bash" && !out.is_empty() {
                        let cmd = input["command"].as_str().unwrap_or("").to_string();
                        turn.events
                            .push(Event::Output(cmd, out.chars().take(4000).collect()));
                    }
                }
            }
        }
        let last = last_message.trim();
        if !last.is_empty()
            && !turn
                .events
                .iter()
                .any(|e| matches!(e, Event::Text(t) if t.trim() == last))
        {
            turn.events.push(Event::Text(last.to_string()));
        }
        return Some(turn);
    }
    None
}

impl Turn {
    /// The last text block, or the last two when the last is too short to carry the state.
    pub fn answer(&self) -> String {
        let texts: Vec<&str> = self
            .events
            .iter()
            .filter_map(|e| {
                if let Event::Text(t) = e {
                    Some(t.as_str())
                } else {
                    None
                }
            })
            .collect();
        match texts.as_slice() {
            [] => String::new(),
            [.., last] if last.chars().count() > 40 => last.to_string(),
            [.., before, last] => format!("{before}\n{last}"),
            [only] => only.to_string(),
        }
    }

    pub fn mine(&self, folder: &str, home: &str) -> Found {
        let mut f = Found::default();
        if self.real {
            f.aliases = tickets(&self.ask);
        }
        let path = |p: &str| norm_path(p, folder, home);
        for e in &self.events {
            match e {
                Event::Tool(name, input) => {
                    f.paths.extend(
                        ["file_path", "notebook_path"]
                            .iter()
                            .filter_map(|k| input[*k].as_str())
                            .filter_map(path),
                    );
                    if name != "Bash" {
                        continue;
                    }
                    let cmd = input["command"].as_str().unwrap_or("");
                    f.paths.extend(paths_in(cmd).into_iter().filter_map(path));
                    f.prs.extend(
                        GH_PR
                            .captures_iter(cmd)
                            .chain(PULL.captures_iter(cmd))
                            .map(|c| c[1].to_string()),
                    );
                    f.commits.extend(commits_in(cmd).map(str::to_string));
                    let first = cmd.trim().lines().next().unwrap_or("");
                    let verb = first
                        .split(' ')
                        .next()
                        .unwrap_or("")
                        .rsplit('/')
                        .next()
                        .unwrap_or("");
                    if CMD_VERBS.contains(&verb) {
                        f.commands.push(first.to_string());
                    }
                }
                Event::Output(cmd, out) => {
                    if cmd.contains("gh pr") || cmd.contains("git push") {
                        f.prs
                            .extend(PULL.captures_iter(out).map(|c| c[1].to_string()));
                    }
                    if cmd.starts_with("git commit")
                        || cmd.contains("git rev-parse")
                        || cmd.starts_with("git log")
                    {
                        f.commits
                            .extend(commits_in(out).take(3).map(str::to_string));
                    }
                }
                Event::Text(text) => {
                    f.tickets.extend(tickets(text));
                    f.prs.extend(prs_in(text).into_iter().map(|(_, p)| p));
                    f.commits.extend(commits_in(text).map(str::to_string));
                    f.paths.extend(paths_in(text).into_iter().filter_map(path));
                }
            }
        }
        for list in [
            &mut f.aliases,
            &mut f.tickets,
            &mut f.prs,
            &mut f.commits,
            &mut f.paths,
            &mut f.commands,
        ] {
            newest_first(list);
        }
        f
    }
}

/// Reverses a list in the order found and keeps each item's latest occurrence.
fn newest_first(list: &mut Vec<String>) {
    list.reverse();
    let mut seen = std::collections::HashSet::new();
    list.retain(|x| seen.insert(x.clone()));
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

pub(crate) fn prs_in(text: &str) -> Vec<(usize, String)> {
    let mut out = vec![];
    for c in PR.captures_iter(text).chain(GH_PR.captures_iter(text)) {
        let whole = c.get(0).unwrap();
        let bare_hash = c.get(2).is_some() && &c[1] == "#";
        if bare_hash
            && text[..whole.start()]
                .chars()
                .next_back()
                .is_some_and(|b| is_word(b) || b == '/')
        {
            continue;
        }
        out.push((whole.start(), c[c.len() - 1].to_string()));
    }
    out
}

/// Tokens of 7 to 40 hex characters with at least one letter and one digit, not part of a longer word.
pub(crate) fn commits_in(text: &str) -> impl Iterator<Item = &str> {
    WORD.find_iter(text).map(|m| m.as_str()).filter(|t| {
        let hex = |b: u8| b.is_ascii_digit() || (b'a'..=b'f').contains(&b);
        (7..=40).contains(&t.len())
            && t.bytes().all(hex)
            && t.bytes().any(|b| b.is_ascii_digit())
            && t.bytes().any(|b| b.is_ascii_alphabetic())
    })
}

/// Path-like tokens: not after a word character, `/`, `:`, `@` or `$`, and not followed by a word character or `/`.
pub(crate) fn paths_in(text: &str) -> Vec<&str> {
    let mut out = vec![];
    let mut at = 0;
    while let Some(m) = PATH.find_at(text, at) {
        let next = |s: &str| text[m.start() + s.len()..].chars().next();
        let free_after = |s: &str| next(s).is_none_or(|c| !is_word(c) && c != '/');
        let mut s = m.as_str();
        if !free_after(s) && LINE_SUFFIX.is_match(s) {
            s = &s[..LINE_SUFFIX.find(s).unwrap().start()];
        }
        let free_before = text[..m.start()]
            .chars()
            .next_back()
            .is_none_or(|c| !is_word(c) && !"/:@$".contains(c));
        if free_before && free_after(s) {
            out.push(s);
            at = m.start() + s.len();
        } else {
            at = m.start() + text[m.start()..].chars().next().map_or(1, char::len_utf8);
        }
    }
    out
}

/// A path relative to the worktree root, `~/`-relative under home, or None for noise.
pub(crate) fn norm_path(raw: &str, folder: &str, home: &str) -> Option<String> {
    let p = raw.trim().trim_matches(|c| "`'\"(),;:".contains(c));
    let p = LINE_SUFFIX.replace(p, "");
    let noise = [
        "://",
        "node_modules",
        "/tmp/",
        "/.claude/",
        "scratchpad",
        "..",
    ];
    if noise.iter().any(|x| p.contains(x)) || p.starts_with("origin/") {
        return None;
    }
    let mut p = match p.strip_prefix("~/") {
        Some(rest) => format!("{home}/{rest}"),
        None => p.to_string(),
    };
    if let Some(rest) = p
        .strip_prefix(folder)
        .and_then(|r| r.strip_prefix('/'))
        .filter(|_| !folder.is_empty())
    {
        p = rest.to_string();
    } else if let Some(rest) = p
        .strip_prefix(home)
        .and_then(|r| r.strip_prefix('/'))
        .filter(|_| !home.is_empty())
    {
        p = format!("~/{rest}");
    }
    let p = p.strip_prefix("./").unwrap_or(&p);
    (!p.is_empty()).then(|| p.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn lines(v: &[Value]) -> Vec<u8> {
        v.iter()
            .map(|l| l.to_string() + "\n")
            .collect::<String>()
            .into_bytes()
    }

    #[test]
    fn miners() {
        let text = "PR #345 at abc1234 in /w/app/src/export.py:12, see $HOME/x/y.py and a#99 or deadbeefcafe0000000000000000000000000000000";
        assert_eq!(prs_in(text), [(0, "345".to_string())]);
        assert_eq!(commits_in(text).collect::<Vec<_>>(), ["abc1234"]);
        assert_eq!(paths_in(text), ["/w/app/src/export.py:12"]);
        assert_eq!(
            norm_path("/w/app/src/export.py:12", "/w/app", "/h").as_deref(),
            Some("src/export.py")
        );
        assert_eq!(
            norm_path("~/notes/a.md", "/w/app", "/h").as_deref(),
            Some("~/notes/a.md")
        );
        assert_eq!(norm_path("../up/a.py", "/w/app", "/h"), None);
        assert_eq!(
            paths_in("see a/b.c:12x and `docs/x.md`."),
            ["a/b.c", "docs/x.md"]
        );
        assert_eq!(tickets("ABC-12 and abc-12 and ABCDEF-1"), ["ABC-12"]);
    }

    #[test]
    fn last_turn_and_mining() {
        let data = lines(&[
            json!({"type": "user", "message": {"content": "Build ABC-12 so exports stop failing on empty rows"}}),
            json!({"type": "assistant", "message": {"id": "m1", "content": [{"type": "text", "text": "old turn"}]}}),
            json!({"type": "user", "message": {"content": "<command-name>/implement</command-name>\n<command-args>ABC-12 next step</command-args>"}}),
            json!({"type": "assistant", "message": {"id": "m2", "content": [
                {"type": "text", "text": "Opening PR #345; see src/export.py."},
                {"type": "tool_use", "id": "t1", "name": "Bash", "input": {"command": "gh pr create --fill\nmore"}}]}}),
            json!({"type": "user", "toolUseResult": {"stdout": "https://github.com/o/r/pull/346"},
                   "message": {"content": [{"type": "tool_result", "tool_use_id": "t1"}]}}),
            json!({"type": "assistant", "message": {"id": "m3", "content": [
                {"type": "tool_use", "id": "t2", "name": "Edit", "input": {"file_path": "/w/app/src/b.py"}}]}}),
            json!({"type": "assistant", "isSidechain": true, "message": {"content": [{"type": "text", "text": "src/side.py"}]}}),
        ]);
        let turn = last_turn(&data, "Done: commit abc1234def pushed. Next: review.").unwrap();
        assert_eq!(turn.ask, "/implement ABC-12 next step");
        assert!(turn.real);
        assert_eq!(
            turn.answer(),
            "Done: commit abc1234def pushed. Next: review."
        );
        let f = turn.mine("/w/app", "/h");
        assert_eq!(f.aliases, ["ABC-12"]);
        assert_eq!(f.prs, ["346", "345"]);
        assert_eq!(f.commits, ["abc1234def"]);
        assert_eq!(f.paths, ["src/b.py", "src/export.py"]);
        assert_eq!(f.commands, ["gh pr create --fill"]);

        let note = lines(&[
            json!({"type": "user", "message": {"content": "<task-notification><task-id>t</task-id></task-notification>"}}),
        ]);
        let turn = last_turn(&note, "").unwrap();
        assert!(!turn.real && turn.mine("/w", "/h").aliases.is_empty());
    }
}
