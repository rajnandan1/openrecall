use serde_json::{Value, json};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// macOS sets close-on-exec on a new pipe in a second step, so a parallel test's spawn can inherit it.
static SPAWN: Mutex<()> = Mutex::new(());

struct World {
    root: PathBuf,
    home: PathBuf,
    folder: PathBuf,
    entrypoint: &'static str,
}

impl World {
    fn new(name: &str) -> World {
        let root = std::env::temp_dir().join(format!("openrecall-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let folder = root.join("w");
        fs::create_dir_all(folder.join(".git/refs/heads")).unwrap();
        fs::write(
            folder.join(".git/config"),
            "[remote \"origin\"]\n\turl = git@github.com:someone/app.git\n",
        )
        .unwrap();
        World {
            home: root.join("home"),
            root,
            folder,
            entrypoint: "cli",
        }
    }

    fn branch(&self, name: &str) {
        fs::write(
            self.folder.join(".git/HEAD"),
            format!("ref: refs/heads/{name}\n"),
        )
        .unwrap();
        let r = self.folder.join(".git/refs/heads").join(name);
        fs::create_dir_all(r.parent().unwrap()).unwrap();
        fs::write(r, "0000000\n").unwrap();
    }

    fn spawn(&self, args: &[&str]) -> Child {
        let guard = SPAWN.lock().unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_openrecall"))
            .args(args)
            .env_clear()
            .env("HOME", &self.home)
            .env("OPENRECALL_HOME", self.home.join(".openrecall"))
            .env("CLAUDE_PROJECT_DIR", &self.folder)
            .env("CLAUDE_CODE_ENTRYPOINT", self.entrypoint)
            .env("CLAUDE_CODE_SESSION_ID", "M")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        drop(guard);
        child
    }

    fn run(&self, args: &[&str], input: &str) -> (String, i32) {
        let mut child = self.spawn(args);
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        (
            String::from_utf8(out.stdout).unwrap(),
            out.status.code().unwrap_or(-1),
        )
    }

    fn hook(&self, args: &[&str], sid: &str, extra: Value) -> String {
        let mut input = json!({"session_id": sid, "cwd": self.folder.join("src")});
        input
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let (out, code) = self.run(args, &input.to_string());
        assert_eq!(code, 0);
        out
    }

    /// Writes a session's transcript and captures its last turn the way the detached writer does.
    fn turn(&self, sid: &str, lines: &[Value], last: &str) {
        let path = self.root.join(format!("{sid}.jsonl"));
        let mut text = fs::read_to_string(&path).unwrap_or_default();
        text += &lines
            .iter()
            .map(|l| l.to_string() + "\n")
            .collect::<String>();
        fs::write(&path, text).unwrap();
        self.hook(
            &["capture", "--job"],
            sid,
            json!({"transcript_path": path, "last_assistant_message": last}),
        );
    }

    fn record(&self, stem: &str) -> Option<String> {
        fs::read_to_string(
            self.home
                .join(".openrecall/repos/github.com/someone/app/handoffs")
                .join(format!("{stem}.md")),
        )
        .ok()
    }

    fn memory(&self, dir: &Path, stem: &str, kind: &str, source: &str, description: &str, body: &str) {
        fs::create_dir_all(dir).unwrap();
        fs::write(
            dir.join(format!("{stem}.md")),
            format!("---\nname: {stem}\ndescription: {description}\nmetadata:\n  type: {kind}\n{source}---\n\n{body}\n"),
        )
        .unwrap();
    }

    /// Forty unrelated memories, so a test's subject words are rare enough to score above the gate (bm25 idf).
    fn fillers(&self, dir: &Path) {
        let bank = [
            "deploy", "standup", "linter", "wildcard", "review", "staging", "billing", "release", "titles",
            "browser", "retries", "runner", "branch", "slug", "rota", "monday", "cache", "queue", "worker",
            "metric", "alert", "quota", "invoice", "tenant",
        ];
        for i in 0..40 {
            let word = |k: usize| bank[(i * 7 + k * 5) % bank.len()];
            let text = format!("The {} {} runs the {} before the {} step.", word(0), word(1), word(2), word(3));
            self.memory(dir, &format!("note-{i}"), "project", "", &text, &text);
        }
    }

    fn builtin_dir(&self) -> PathBuf {
        let slug: String = self
            .folder
            .to_string_lossy()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        self.home.join(".claude/projects").join(slug).join("memory")
    }

    fn pushed(&self, sid: &str, prompt: &str) -> Option<String> {
        let out = self.hook(&["recall"], sid, json!({"prompt": prompt}));
        let v: Value = serde_json::from_str(out.trim()).ok()?;
        Some(
            v["hookSpecificOutput"]["additionalContext"]
                .as_str()?
                .to_string(),
        )
    }
}

fn user(text: &str) -> Value {
    json!({"type": "user", "message": {"role": "user", "content": text}})
}

fn said(text: &str, tool: Option<Value>) -> Value {
    let mut content = vec![json!({"type": "text", "text": text})];
    content.extend(tool);
    json!({"type": "assistant", "message": {"role": "assistant", "content": content}})
}

#[test]
fn a_new_session_gets_the_earlier_sessions_state() {
    let w = World::new("handoff");
    w.branch("feat/x");
    w.hook(&["handoff"], "E", json!({"source": "startup"}));
    let edit = json!({"type": "tool_use", "id": "t1", "name": "Edit", "input": {"file_path": w.folder.join("src/export.py")}});
    w.turn(
        "E",
        &[
            user("Build ABC-12 so exports stop failing on empty rows"),
            said("Plan: edit it.", Some(edit)),
        ],
        "",
    );
    w.turn(
        "E",
        &[
            user("open the PR"),
            said("Opened PR #345 at abc1234def. Next: ask for review.", None),
        ],
        "",
    );
    assert!(
        w.pushed("E", "and then?").is_none(),
        "a session is never pushed its own record"
    );

    let updated_at = |w: &World| {
        let rec = w.record("feat--x").unwrap();
        let line = rec.lines().find(|l| l.starts_with("updated_at: ")).unwrap();
        line["updated_at: ".len()..].to_string()
    };
    let e_wrote = updated_at(&w);
    w.hook(&["handoff"], "S", json!({"source": "startup"}));
    assert!(
        w.pushed("S", "continue please").is_none(),
        "not settled before one completed turn"
    );
    w.turn(
        "S",
        &[
            user("continue please"),
            said("Reading src/other.py first.", None),
        ],
        "",
    );
    assert_ne!(updated_at(&w), e_wrote, "S's identifiers-only write still moves the retirement clock");
    let ctx = w.pushed("S", "what is left").expect("pushed once settled");
    assert!(
        ctx.starts_with(&format!("Handoff record for branch feat/x, last written {e_wrote} by an earlier session")),
        "the header shows E's last write, not S's own: {ctx}"
    );
    assert!(ctx.contains("(github.com/someone/app/handoffs/feat--x)"));
    assert!(ctx.contains("## Goal\nBuild ABC-12 so exports stop failing on empty rows\n"));
    assert!(
        ctx.contains("## Last ask\nopen the PR\n## Last answer\nOpened PR #345 at abc1234def. Next: ask for review.\n")
    );
    assert!(ctx.contains(
        "tickets: ABC-12\nprs: #345\ncommits: abc1234def\npaths: src/export.py, src/other.py\n"
    ));
    assert!(!ctx.contains("---"), "frontmatter is never injected");
    assert!(
        w.pushed("S", "and now?").is_none(),
        "never twice in one context window"
    );
    let status = fs::read_to_string(w.home.join(".openrecall/status/S")).unwrap();
    assert!(status.starts_with("recall 0 · "), "{status}");

    w.turn(
        "S",
        &[user("fix the flaky test"), said("Fixed it.", None)],
        "",
    );
    let rec = w.record("feat--x").unwrap();
    assert!(
        rec.contains("writers: E, S\n") && rec.contains("## Last ask\nfix the flaky test\n"),
        "after the push S writes in full"
    );

    w.hook(&["handoff"], "S", json!({"source": "compact"}));
    assert!(
        w.pushed("S", "where were we")
            .unwrap()
            .contains("fix the flaky test"),
        "compact re-pushes at once"
    );

    w.branch("main");
    w.hook(&["handoff"], "T", json!({"source": "startup"}));
    assert!(
        w.pushed("T", "status of ABC-12?").is_some(),
        "an alias pushes from trunk at prompt 1"
    );
    w.hook(&["handoff"], "U", json!({"source": "startup"}));
    assert!(w.pushed("U", "status of XYZ-99?").is_none());

    let log = fs::read_to_string(w.home.join(".openrecall/log/openrecall.jsonl")).unwrap();
    for event in [
        "\"pushed\"",
        "\"settled\"",
        "\"how\":\"alias\"",
        "\"how\":\"compact\"",
        "\"started_at\"",
    ] {
        assert!(log.contains(event), "{event}");
    }
}

#[test]
fn keys_are_redacted_and_renames_move_the_record() {
    let w = World::new("rename");
    w.branch("orca/tmp");
    w.hook(&["handoff"], "E", json!({"source": "startup"}));
    let token = "ghp_Zq8Xw3Kp9Lm2Nv7Bc4Rt6Yh1Jd5Fg0Sa3Ew8";
    w.turn(
        "E",
        &[
            user(&format!("use {token} to push the release branch now")),
            said("Pushed.", None),
        ],
        "",
    );
    let rec = w.record("orca--tmp").unwrap();
    assert!(rec.contains("[REDACTED:github-pat]") && !rec.contains(token));

    fs::remove_file(w.folder.join(".git/refs/heads/orca/tmp")).unwrap();
    w.branch("feat/y");
    w.turn("E", &[user("next"), said("Done.", None)], "");
    assert!(w.record("orca--tmp").is_none());
    assert!(w.record("feat--y").unwrap().contains("branch: feat/y\n"));

    w.branch("feat/z");
    w.turn("E", &[user("switch"), said("Switched.", None)], "");
    assert!(
        w.record("feat--y").is_some() && w.record("feat--z").is_some(),
        "a switch keeps the old record"
    );
}

#[test]
fn idle_records_retire_at_session_start() {
    let w = World::new("retire");
    w.branch("feat/old");
    let dir = w
        .home
        .join(".openrecall/repos/github.com/someone/app/handoffs");
    fs::create_dir_all(&dir).unwrap();
    let old = "---\nrepo: github.com/someone/app\nbranch: feat/old\nfolder: /w\naliases: \nwriters: E\nupdated_at: 2020-01-01T00:00:00Z\n---\n## Last ask\nx\n";
    fs::write(dir.join("feat--old.md"), old).unwrap();
    w.hook(&["handoff"], "S", json!({"source": "startup"}));
    let retired: Vec<_> = fs::read_dir(dir.join("retired"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(retired.len(), 1);
    assert!(
        fs::read_to_string(&retired[0])
            .unwrap()
            .contains("updated_at: 2020-01-01T00:00:00Z\nretired_at: ")
    );
    assert!(w.record("feat--old").is_none());
}

#[test]
fn skipped_sessions_write_nothing_and_every_exit_is_zero() {
    let mut w = World::new("skip");
    w.branch("feat/x");
    w.entrypoint = "sdk-cli";
    w.hook(&["handoff"], "H", json!({"source": "startup"}));
    w.turn(
        "H",
        &[
            user("Build ABC-12 so exports stop failing on empty rows"),
            said("ok", None),
        ],
        "",
    );
    assert!(w.pushed("H", "ABC-12").is_none());
    assert!(
        !w.home.exists(),
        "a headless session writes nothing anywhere"
    );

    w.entrypoint = "cli";
    for (args, input) in [
        (&["recall"][..], "not json"),
        (&["bogus", "--flag"][..], "{}"),
        (&["capture", "--x"][..], "{}"),
        (&[][..], ""),
    ] {
        assert_eq!(w.run(args, input).1, 0);
    }
    let note = w.hook(
        &["recall"],
        "N",
        json!({"prompt": "<task-notification>done</task-notification>"}),
    );
    assert!(note.is_empty());
    assert_eq!(
        fs::read_to_string(w.home.join(".openrecall/status/N")).unwrap(),
        "recall skipped\n"
    );
}

#[test]
fn the_stop_hook_returns_before_the_writer_finishes() {
    let w = World::new("detach");
    w.branch("feat/x");
    let path = w.root.join("D.jsonl");
    fs::write(
        &path,
        format!(
            "{}\n{}\n",
            user("Build ABC-12 so exports stop failing on empty rows"),
            said("ok", None)
        ),
    )
    .unwrap();
    let started = Instant::now();
    w.hook(
        &["capture"],
        "D",
        json!({"transcript_path": path, "last_assistant_message": "ok"}),
    );
    assert!(
        w.record("feat--x").is_none(),
        "the hook waited for the writer"
    );
    while w.record("feat--x").is_none() {
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the detached writer never wrote the record"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn level_two_recalls_the_repos_memories_above_the_gate() {
    let w = World::new("level2");
    w.branch("feat/x");
    fs::create_dir_all(w.folder.join("src")).unwrap();
    fs::write(w.folder.join("src/export.py"), "").unwrap();
    let builtin = w.builtin_dir();
    let repo = w.home.join(".openrecall/repos/github.com/someone/app");
    w.fillers(&builtin);
    fs::write(builtin.join("MEMORY.md"), "- ABC-12 exports: src/export.py\n").unwrap();
    let long = format!(
        "The export job wrote empty rows because the null check in src/export.py ran after the batch was \
         sealed; ABC-12 tracks it and PR #345 moved the check ahead of the seal. {}",
        "More detail on the batch sealing order and its retries. ".repeat(4)
    );
    w.memory(&builtin, "export-empty-rows", "project", "", "Exports fail on empty rows until the null check moves", &long);
    w.memory(&repo, "old-export-notes", "pointer", "source: E 2026-01-02T03:04:05Z\n", "Empty rows and ABC-12 in src/gone.py", "Empty rows: ABC-12 was first seen in src/gone.py before the export moved.");
    w.memory(&repo, "export-null-rows", "gotcha", "source: E 2026-01-02T03:04:05Z\n", "Null rows", "Exports fail on empty rows: ABC-12 needs a null check in src/export.py before the batch seals.");
    w.memory(&repo, "own-export-note", "state", "source: S 2026-01-03T00:00:00Z\n", "Own", "ABC-12 export empty rows: this session's own fact about src/export.py.");
    fs::write(repo.join("claude-md-suggestions.md"), "- ABC-12 exports empty rows src/export.py\n").unwrap();
    w.memory(&w.home.join(".openrecall/global"), "empty-rows-rule", "preference", "source: G 2026-01-01T00:00:00Z\n", "Rule", "Treat empty rows as a bug in every export, never as data (ABC-12 style).");

    let ctx = w.pushed("S", "exports fail on empty rows in ABC-12").expect("level 2 injects");
    assert!(ctx.starts_with("Recalled memories from earlier sessions (OpenRecall). They reflect what was true when written. Full text: mcp__plugin_openrecall_openrecall__recall with the address.\n- "), "{ctx}");
    let lines: Vec<&str> = ctx.lines().skip(1).collect();
    assert!(lines.len() <= 3 && ctx.chars().count() <= 1040, "{ctx}");
    let whole = long.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        lines.iter().any(|l| l.starts_with("- project 20")
            && l.contains("builtin/") && l.ends_with(&format!("/export-empty-rows: {whole}"))),
        "{ctx}"
    );
    assert!(
        lines.contains(&"- gotcha 2026-01-02 github.com/someone/app/export-null-rows: Exports fail on empty rows: ABC-12 needs a null check in src/export.py before the batch seals."),
        "{ctx}"
    );
    for gone in ["own-export-note", "old-export-notes", "claude-md-suggestions", "MEMORY"] {
        assert!(!ctx.contains(gone), "{gone} must not be injected: {ctx}");
    }
    assert!(fs::read_to_string(w.home.join(".openrecall/status/S")).unwrap().starts_with(&format!("recall {} · ", lines.len())));
    let again = w.pushed("S", "exports fail on empty rows in ABC-12").unwrap_or_default();
    assert!(
        !again.contains("export-empty-rows") && !again.contains("export-null-rows"),
        "the ledger stops a repeat: {again}"
    );

    w.hook(&["recall"], "S", json!({"prompt": "fix it now"}));
    w.hook(&["recall"], "S", json!({"prompt": "/implement"}));
    let ctx = w.pushed("T", "/implement ABC-12 export empty rows").expect("slash arguments are the query");
    assert!(ctx.contains("own-export-note"), "another session may recall S's fact: {ctx}");

    let path = builtin.join("export-empty-rows.md");
    let edited = fs::read_to_string(&path).unwrap().replace("moved the check ahead of the seal", "moved the seal after the check");
    fs::write(&path, edited).unwrap();
    fs::remove_file(repo.join("export-null-rows.md")).unwrap();
    let ctx = w.pushed("U", "exports fail on empty rows in ABC-12").unwrap();
    assert!(ctx.contains("moved the seal after the check") && !ctx.contains("export-null-rows"), "the index follows the files: {ctx}");

    let log = fs::read_to_string(w.home.join(".openrecall/log/openrecall.jsonl")).unwrap();
    for event in ["\"reason\":\"short\"", "\"reason\":\"empty-args\"", "\"candidates\":[{\"address\":\"", "\"text_hash\":\"", "\"rot\":1", "\"own\":1", "\"ledger\":"] {
        assert!(log.contains(event), "{event}\n{log}");
    }
}

#[test]
fn a_memory_goes_in_whole_only_in_the_room_the_other_lines_leave() {
    let w = World::new("whole");
    w.branch("feat/y");
    let repo = w.home.join(".openrecall/repos/github.com/someone/app");
    w.fillers(&repo);
    let source = "source: E 2026-01-02T03:04:05Z\n";
    let history = "Ledger totals round half-even since ABC-77, because half-up rounding drifted one cent per thousand \
                   postings. The rule lives in `ledger::round_total`; every caller passes the minor unit of its \
                   currency. Settlement exports keep half-even too, since the bank file expects it. Older ledger notes \
                   that say half-up are wrong now, and ledger rounding tests pin the cents.";
    w.memory(&repo, "ledger-rounding-history", "decision", source, "Ledger totals round half-even since ABC-77", history);
    w.memory(&repo, "ledger-rounding-test", "gotcha", source, "The rounding test needs its fixture first",
        "The ledger rounding test needs the ABC-77 fixture loaded first, or it compares against half-up totals and \
         fails on the third posting. Load it in the test setup, never by hand.");
    w.memory(&repo, "ledger-cents-column", "decision", source, "The cents column stays an integer",
        "The ledger cents column stays an integer; ABC-77 rounding happens before the write, so the database never \
         sees a fraction of a cent. Reports divide by a hundred only when they print.");
    let head = "- decision 2026-01-02 github.com/someone/app/ledger-rounding-history: ";

    let ctx = w.pushed("S", "ledger rounding ABC-77 cents").expect("level 2 injects");
    let lines: Vec<&str> = ctx.lines().skip(1).collect();
    assert_eq!(lines.len(), 3, "a whole memory never pushes a line out: {ctx}");
    let short = format!("{head}Ledger totals round half-even since ABC-77 (ledger::round_total)");
    assert!(lines.contains(&short.as_str()), "{ctx}");

    fs::remove_file(repo.join("ledger-rounding-test.md")).unwrap();
    fs::remove_file(repo.join("ledger-cents-column.md")).unwrap();
    let ctx = w.pushed("T", "ledger rounding ABC-77 cents").expect("level 2 injects");
    let whole = format!("{head}{}", history.split_whitespace().collect::<Vec<_>>().join(" "));
    assert_eq!(ctx.lines().skip(1).collect::<Vec<_>>(), [whole], "alone, it goes in whole: {ctx}");
}

/// One JSON-RPC client over the server's pipes: each call reads lines until the reply with its id.
struct Mcp {
    child: Child,
    out: BufReader<std::process::ChildStdout>,
    next: u64,
}

impl Mcp {
    fn start(w: &World) -> Mcp {
        let mut child = w.spawn(&["mcp"]);
        let out = BufReader::new(child.stdout.take().unwrap());
        let mut m = Mcp { child, out, next: 0 };
        m.call("initialize", json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "t", "version": "0"}}));
        m.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        m
    }

    fn send(&mut self, msg: Value) {
        let stdin = self.child.stdin.as_mut().unwrap();
        stdin.write_all(format!("{msg}\n").as_bytes()).unwrap();
        stdin.flush().unwrap();
    }

    fn call(&mut self, method: &str, params: Value) -> Value {
        self.next += 1;
        let id = self.next;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        loop {
            let mut line = String::new();
            assert!(self.out.read_line(&mut line).unwrap() > 0, "server closed before replying to {method}");
            let v: Value = serde_json::from_str(&line).unwrap();
            if v["id"] == id {
                return v;
            }
        }
    }

    /// The tool's text and whether it reported an error.
    fn tool(&mut self, name: &str, args: Value) -> (String, bool) {
        let r = self.call("tools/call", json!({"name": name, "arguments": args}));
        let result = &r["result"];
        assert!(!result.is_null(), "{r}");
        (
            result["content"][0]["text"].as_str().unwrap_or("").to_string(),
            result["isError"] == true,
        )
    }

    /// Closing stdin must end the process (spec: no leftover servers).
    fn close(mut self) {
        drop(self.child.stdin.take());
        let start = Instant::now();
        while self.child.try_wait().unwrap().is_none() {
            assert!(start.elapsed() < Duration::from_secs(10), "server outlived its stdin");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

#[test]
fn mcp_tools_remember_recall_and_forget() {
    let w = World::new("mcp");
    w.branch("feat/x");
    let repo = w.home.join(".openrecall/repos/github.com/someone/app");
    let builtin = w.builtin_dir();
    w.memory(&builtin, "vendor-quirks", "project", "", "The vendor API rejects unknown slugs", "The vendor API rejects unknown slugs, so every slug is checked first.");
    fs::create_dir_all(repo.join("replaced")).unwrap();
    fs::write(repo.join("replaced/asana-slug.2026-01-01T00:00:00Z.md"), "old text\n").unwrap();
    fs::write(repo.join("replaced/asana-slug-2.2026-01-01T00:00:00Z.md"), "other fact\n").unwrap();

    let mut m = Mcp::start(&w);
    let tools = m.call("tools/list", json!({}));
    let mut names: Vec<&str> = tools["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    names.sort();
    assert_eq!(names, ["forget", "recall", "remember"]);

    let (text, err) = m.tool("remember", json!({"text": "The asana slug is hard-coded in src/slugs.py (ABC-1234): the vendor API rejects anything else.", "type": "decision", "scope": "repo", "name": "Asana slug"}));
    assert!(!err && text.starts_with("Stored as github.com/someone/app/asana-slug ("), "{text}");
    let file = fs::read_to_string(repo.join("asana-slug.md")).unwrap();
    assert!(file.starts_with("---\nname: asana-slug\ndescription: The asana slug is hard-coded in src/slugs.py (ABC-1234): the vendor API rejects anything else.\nmetadata:\n  type: decision\nscope: github.com/someone/app\nsource: M 20"), "{file}");
    assert!(file.ends_with("---\n\nThe asana slug is hard-coded in src/slugs.py (ABC-1234): the vendor API rejects anything else.\n") && !file.contains("expires"), "{file}");
    let (text, _) = m.tool("remember", json!({"text": "ABC-1234 waits on the vendor's slug list; next: ask them.", "type": "state", "scope": "global"}));
    assert!(text.starts_with("Stored as global/abc-1234-waits-on-the-vendor-s-slug-list-next-ask-them ("), "{text}");
    assert!(fs::read_to_string(w.home.join(".openrecall/global/abc-1234-waits-on-the-vendor-s-slug-list-next-ask-them.md")).unwrap().contains("\nexpires: 20"));
    let (text, _) = m.tool("remember", json!({"text": "A second asana note.", "type": "gotcha", "scope": "repo", "name": "asana-slug"}));
    assert!(text.starts_with("Stored as github.com/someone/app/asana-slug-2 ("), "a taken name gets -2: {text}");

    let token = format!("ghp_{}", "Zq8Xw3Kp9Lm2Nv7Bc4Rt6Yh1Jd5Fg0Sa3Ew8");
    let (text, err) = m.tool("remember", json!({"text": format!("The CI token is {token}"), "type": "gotcha", "scope": "repo"}));
    assert!(err && text.contains("github-pat"), "{text}");
    assert!(fs::read_dir(&repo).unwrap().flatten().all(|e| !fs::read_to_string(e.path()).unwrap_or_default().contains(&token)));
    for (args, what) in [
        (json!({"text": "x", "type": "fact", "scope": "repo"}), "type must be one of"),
        (json!({"text": "x", "type": "gotcha", "scope": "team"}), "scope must be"),
        (json!({"text": "  ", "type": "gotcha", "scope": "repo"}), "text is empty"),
    ] {
        let (text, err) = m.tool("remember", args);
        assert!(err && text.contains(what), "{text}");
    }

    let (text, err) = m.tool("recall", json!({"query": "why is the asana slug hard-coded", "k": 2}));
    assert!(!err, "{text}");
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2, "{text}");
    assert!(lines[0].starts_with("- decision 20") && lines[0].contains(" github.com/someone/app/asana-slug: The asana slug is hard-coded in src/slugs.py (ABC-1234)"), "{text}");
    let (text, _) = m.tool("recall", json!({"query": "vendor slugs rejected"}));
    assert!(text.contains("builtin/") && text.contains("/vendor-quirks: The vendor API rejects unknown slugs"), "{text}");
    let (text, _) = m.tool("recall", json!({"query": "github.com/someone/app/asana-slug"}));
    assert!(text.starts_with(&format!("{}\n\n---\nname: asana-slug\n", repo.join("asana-slug.md").display())), "{text}");
    let (text, _) = m.tool("recall", json!({"query": "zzzz qqqq"}));
    assert_eq!(text, "No memory matches.");

    let (text, err) = m.tool("forget", json!({"id": "github.com/someone/app/asana-slug"}));
    assert!(!err && text.ends_with("and 1 replaced copies"), "{text}");
    assert!(!repo.join("asana-slug.md").exists() && !repo.join("replaced/asana-slug.2026-01-01T00:00:00Z.md").exists());
    assert!(repo.join("replaced/asana-slug-2.2026-01-01T00:00:00Z.md").exists() && repo.join("asana-slug-2.md").exists());
    for (id, what) in [
        ("github.com/someone/app/asana-slug", "no memory at"),
        ("github.com/someone/app/handoffs/feat--x", "is a handoff record"),
        ("builtin/-w/vendor-quirks", "is a Claude Code memory file"),
        ("../../etc/passwd", "is not a memory address"),
        ("nope", "is not a memory address"),
    ] {
        let (text, err) = m.tool("forget", json!({"id": id}));
        assert!(err && text.contains(what), "{id}: {text}");
    }
    assert!(builtin.join("vendor-quirks.md").exists());
    m.close();

    let log = fs::read_to_string(w.home.join(".openrecall/log/openrecall.jsonl")).unwrap();
    for event in [
        "\"address\":\"github.com/someone/app/asana-slug\",\"at\":",
        "\"event\":\"mcp\",\"session\":\"M\",\"tool\":\"remember\"",
        "\"dropped\":\"github-pat\"",
        "\"query_hash\":\"",
        "\"replaced\":1,\"session\":\"M\",\"tool\":\"forget\"",
    ] {
        assert!(log.contains(event), "{event}\n{log}");
    }
    assert!(!log.contains(&token));
}

/// A stand-in for an OpenAI-compatible provider on localhost: answers each request with the next canned reply and
/// keeps each request's headers and body.
struct Provider {
    port: u16,
    seen: Arc<Mutex<Vec<(String, Value)>>>,
}

impl Provider {
    fn start(replies: Vec<(u16, Value)>) -> Provider {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = Arc::new(Mutex::new(vec![]));
        let kept = seen.clone();
        std::thread::spawn(move || {
            for (status, reply) in replies {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let (mut head, mut len) = (String::new(), 0);
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line.trim().is_empty() {
                        break;
                    }
                    let low = line.to_ascii_lowercase();
                    if let Some(n) = low.strip_prefix("content-length:") {
                        len = n.trim().parse().unwrap();
                    }
                    if low.starts_with("expect: 100-continue") {
                        stream.write_all(b"HTTP/1.1 100 Continue\r\n\r\n").unwrap();
                    }
                    head += &line;
                }
                let mut body = vec![0; len];
                reader.read_exact(&mut body).unwrap();
                kept.lock().unwrap().push((head, serde_json::from_slice(&body).unwrap()));
                let text = reply.to_string();
                write!(stream, "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}", text.len()).unwrap();
            }
        });
        Provider { port, seen }
    }

    fn requests(&self) -> Vec<(String, Value)> {
        self.seen.lock().unwrap().clone()
    }
}

fn answer(content: Value) -> (u16, Value) {
    (
        200,
        json!({"choices": [{"finish_reason": "stop", "message": {"content": content.to_string()}}],
               "usage": {"prompt_tokens": 1000, "completion_tokens": 200, "completion_tokens_details": {"reasoning_tokens": 50}, "cost": 0.01}}),
    )
}

fn wait_for(path: &Path, needle: &str) -> String {
    let started = Instant::now();
    loop {
        let text = fs::read_to_string(path).unwrap_or_default();
        if text.contains(needle) {
            return text;
        }
        assert!(started.elapsed() < Duration::from_secs(20), "no {needle} in {text}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn extraction_writes_dedupes_and_moves_the_cursor() {
    let w = World::new("extract");
    w.branch("feat/x");
    let home = w.home.join(".openrecall");
    let repo = home.join("repos/github.com/someone/app");
    let log = home.join("log/openrecall.jsonl");
    w.memory(&repo, "export-empty-rows", "gotcha", "source: E0 2026-01-01T00:00:00.000Z\n", "Exports fail on empty rows", "Exports fail on empty rows in src/export.py.");
    w.memory(&repo, "cargo-fmt-rule", "preference", "source: E0 2026-01-01T00:00:00.000Z\n", "Run cargo fmt", "Run cargo fmt before pushing.");
    let fmt_rule = fs::read_to_string(repo.join("cargo-fmt-rule.md")).unwrap();
    let token = format!("ghp_{}", "Zq8Xw3Kp9Lm2Nv7Bc4Rt6Yh1Jd5Fg0Sa3Ew8");
    let facts = json!({"facts": [
        {"type": "gotcha", "name": "Export Empty Rows", "description": "Exports fail on empty rows unless src/export.py skips them",
         "body": "src/export.py now skips empty rows before writing; exports failed on them."},
        {"type": "state", "name": "pr-345-review", "description": "PR #345 waits for review", "body": "PR #345 for ABC-12 is open and waits for review."},
        {"type": "decision", "name": "leaky", "description": "The CI token", "body": format!("CI pushes with {token}.")},
        {"type": "chat", "name": "x", "description": "d", "body": "b"},
        {"type": "preference", "name": "fmt-before-commit", "description": "Run cargo fmt before each commit", "body": "The user wants cargo fmt run before each commit."}],
        "claude_md_rules": ["Always run cargo fmt before committing."]});
    let merged = json!({"decisions": [
        {"fact": 0, "action": "replace", "address": "github.com/someone/app/export-empty-rows",
         "description": "Exports fail on empty rows unless src/export.py skips them",
         "body": "Exports failed on empty rows. src/export.py now skips empty rows before writing."},
        {"fact": 1, "action": "new", "address": "", "description": "", "body": ""},
        {"fact": 2, "action": "replace", "address": "github.com/someone/app/cargo-fmt-rule",
         "description": "Run cargo fmt before each commit", "body": format!("Run cargo fmt before each commit; CI pushes with {token}.")}]});
    let p = Provider::start(vec![
        answer(facts),
        answer(merged),
        answer(json!({"facts": [], "claude_md_rules": []})),
        (400, json!({"error": {"code": 400, "message": "too long", "metadata": {"error_type": "context_length_exceeded"}}})),
    ]);
    fs::write(home.join("extract.toml"), format!("base_url = \"http://127.0.0.1:{}/v1/\"\nmodel = \"test/model\"\n", p.port)).unwrap();
    let key = home.join("api-key");
    fs::write(&key, "sk-test-123\n").unwrap();

    let edit = json!({"type": "tool_use", "id": "t1", "name": "Edit", "input": {"file_path": w.folder.join("src/export.py")}});
    w.turn("S1", &[user(&format!("Build ABC-12 so exports stop failing on empty rows; CI uses {token}")), said("Fixed src/export.py; PR #345 is open.", Some(edit))], "");
    let entry = home.join("extract/S1.json");
    assert!(entry.exists(), "capture queues the session");

    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&key, fs::Permissions::from_mode(0o644)).unwrap();
    w.run(&["extract", "--job"], "");
    assert!(fs::read_to_string(&log).unwrap().contains("\"off\":\"api-key is readable by group or others: chmod 600 it\""));
    fs::set_permissions(&key, fs::Permissions::from_mode(0o600)).unwrap();
    w.run(&["extract", "--job"], "");
    assert!(p.requests().is_empty(), "a live session is left alone until it ends or goes quiet");

    w.hook(&["extract"], "S1", json!({"hook_event_name": "SessionEnd"}));
    let text = wait_for(&log, "\"event\":\"extract\",\"from\":0");
    let reqs = p.requests();
    assert_eq!(reqs.len(), 2);
    assert!(reqs[0].0.starts_with("POST /v1/chat/completions ") && reqs[0].0.contains("Authorization: Bearer sk-test-123\r\n"));
    let body = &reqs[0].1;
    assert_eq!((body["model"].as_str(), body["response_format"]["json_schema"]["strict"].as_bool()), (Some("test/model"), Some(true)));
    let input = body["messages"][1]["content"].as_str().unwrap();
    assert!(input.starts_with("Repository: github.com/someone/app\nSession date: 20") && input.contains("\n\nTranscript:\n[user] Build ABC-12"), "{input}");
    assert!(input.contains("[REDACTED:github-pat]") && !input.contains(&token) && input.contains("[tool] Edit ") && !input.contains("NEW TURNS"));
    let dedupe = reqs[1].1["messages"][1]["content"].as_str().unwrap();
    assert!(dedupe.contains("\n\ngithub.com/someone/app/export-empty-rows [gotcha] 2026-01-01\nExports fail on empty rows\n"), "{dedupe}");
    assert!(dedupe.contains("\n\nFact 0 [gotcha] export-empty-rows\n") && dedupe.contains("Looks like: github.com/someone/app/export-empty-rows"), "{dedupe}");

    let fact = fs::read_to_string(repo.join("export-empty-rows.md")).unwrap();
    assert!(fact.contains("\nsource: E0 2026-01-01T00:00:00.000Z\nupdated: S1 20") && fact.ends_with("src/export.py now skips empty rows before writing.\n"), "{fact}");
    let copies: Vec<PathBuf> = fs::read_dir(repo.join("replaced")).unwrap().map(|e| e.unwrap().path()).collect();
    assert_eq!(copies.len(), 1);
    assert!(fs::read_to_string(&copies[0]).unwrap().contains("Exports fail on empty rows in src/export.py."));
    let state = fs::read_to_string(repo.join("pr-345-review.md")).unwrap();
    assert!(state.contains("\nscope: github.com/someone/app\nsource: S1 20") && state.contains("\nexpires: 20"), "{state}");
    assert!(!repo.join("leaky.md").exists() && !repo.join("export-empty-rows-2.md").exists());
    assert_eq!(fs::read_to_string(repo.join("cargo-fmt-rule.md")).unwrap(), fmt_rule, "a merge holding a key leaves the old fact as it was");
    assert!(!repo.join("fmt-before-commit.md").exists());
    let suggestions = fs::read_to_string(repo.join("claude-md-suggestions.md")).unwrap();
    assert!(suggestions.starts_with("- Always run cargo fmt before committing. (S1, 20") && suggestions.lines().count() == 1);
    for event in [
        "\"dropped\":{\"secret\":2,\"type\":1}",
        "\"action\":\"dropped\",\"address\":\"github.com/someone/app/cargo-fmt-rule\"",
        "\"secret_rules\":[\"github-pat\",\"github-pat\"]",
        "\"normalized\":1",
        "\"cost\":0.02",
        "\"action\":\"replace\",\"address\":\"github.com/someone/app/export-empty-rows\"",
        "\"action\":\"new\",\"address\":\"github.com/someone/app/pr-345-review\"",
        "\"rule\":\"github-pat\",\"section\":\"extract\"",
    ] {
        assert!(text.contains(event), "{event}\n{text}");
    }
    assert!(!text.contains(&token) && !text.contains("skips empty rows"), "the log holds no text");
    let transcript = w.root.join("S1.jsonl");
    let queued: Value = serde_json::from_str(&fs::read_to_string(&entry).unwrap()).unwrap();
    assert_eq!((queued["cursor"].as_u64(), queued["ended"].as_bool()), (Some(fs::metadata(&transcript).unwrap().len()), Some(true)));

    w.run(&["extract", "--job"], "");
    assert_eq!(p.requests().len(), 2, "nothing new past the cursor, no call");
    w.hook(&["handoff"], "S1", json!({"source": "resume"}));
    w.turn("S1", &[user("also document the export fix in the README"), said("Documented.", None)], "");
    w.hook(&["extract"], "S1", json!({}));
    let text = wait_for(&log, &format!("\"to\":{},\"tokens_in\"", fs::metadata(&transcript).unwrap().len()));
    let resumed = p.requests()[2].1["messages"][1]["content"].as_str().unwrap().to_string();
    assert!(resumed.contains("was already extracted in an earlier run") && resumed.contains("=== NEW TURNS ===\n[user] also document"), "{resumed}");
    assert!(resumed.find("[user] Build ABC-12").unwrap() < resumed.find("\n=== NEW TURNS ===\n").unwrap());
    assert_eq!(p.requests().len(), 3, "no candidates, no dedupe call: {text}");

    w.turn("S3", &[user("A session far too long for the model's context window"), said("ok", None)], "");
    w.hook(&["extract"], "S3", json!({}));
    wait_for(&log, "\"strike\":\"context length\",\"strikes\":1");
    let queued: Value = serde_json::from_str(&fs::read_to_string(home.join("extract/S3.json")).unwrap()).unwrap();
    assert_eq!((queued["cursor"].as_u64(), queued["failed"].as_bool()), (Some(0), Some(false)));

    let mut past = fs::read_to_string(repo.join("pr-345-review.md")).unwrap();
    let at = past.find("\nexpires: ").unwrap() + 10;
    past.replace_range(at..at + 10, "2020-01-01");
    fs::write(repo.join("pr-345-review.md"), past).unwrap();
    w.hook(&["recall"], "S2", json!({"prompt": "what is the state of the PR #345 review for ABC-12"}));
    assert!(fs::read_to_string(&log).unwrap().lines().last().unwrap().contains("\"expired\":1"), "an expired state fact is never injected");
}

#[test]
fn dedupe_sees_a_built_in_memory_past_the_top_five_and_past_its_start() {
    let w = World::new("dedupe-builtin");
    w.branch("feat/x");
    let home = w.home.join(".openrecall");
    let repo = home.join("repos/github.com/someone/app");
    for i in 0..6 {
        let text = format!("The search tool score gate note {i}: k slots and weak matches.");
        w.memory(&repo, &format!("search-note-{i}"), "gotcha", "source: E0 2026-01-01T00:00:00.000Z\n", &text, &text);
    }
    let stated = "The search tool applies no gate and k defaults to 5.";
    let long = format!("{}{stated}", "The build notes list the deploy order, the release rota and the staging checks. ".repeat(18));
    assert!(long.find(stated).unwrap() > 1200);
    w.memory(&w.builtin_dir(), "tools-build", "project", "", "Build notes for the tools", &long);
    let p = Provider::start(vec![
        answer(json!({"facts": [{"type": "gotcha", "name": "search-tool-fills-k",
            "description": "The search tool has no score gate and k defaults to 5, so weak matches fill the slots",
            "body": "The search tool has no score gate and k defaults to 5, so weak matches fill the slots."}], "claude_md_rules": []})),
        answer(json!({"decisions": [{"fact": 0, "action": "skip", "address": "", "description": "", "body": ""}]})),
    ]);
    fs::write(home.join("extract.toml"), format!("base_url = \"http://127.0.0.1:{}/v1\"\nmodel = \"test/model\"\n", p.port)).unwrap();
    fs::write(home.join("api-key"), "sk-test-123\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(home.join("api-key"), fs::Permissions::from_mode(0o600)).unwrap();
    w.turn("S1", &[user("Why does the search tool return weak matches for a short query?"), said("It has no gate.", None)], "");
    w.hook(&["extract"], "S1", json!({"hook_event_name": "SessionEnd"}));
    let log = wait_for(&home.join("log/openrecall.jsonl"), "\"event\":\"extract\",\"from\":0");
    let dedupe = p.requests()[1].1["messages"][1]["content"].as_str().unwrap().to_string();
    assert!(dedupe.contains("/tools-build [project] ") && dedupe.contains(stated), "{dedupe}");
    assert!(log.contains("\"action\":\"skip\"") && log.contains("\"skipped\":1") && !repo.join("search-tool-fills-k.md").exists(), "{log}");
}
