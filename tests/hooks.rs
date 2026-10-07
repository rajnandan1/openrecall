use serde_json::{Value, json};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// macOS sets close-on-exec on a new pipe in a second step, so a parallel test's spawn can inherit it.
static SPAWN: Mutex<()> = Mutex::new(());

#[derive(Clone)]
struct World {
    root: PathBuf,
    home: PathBuf,
    folder: PathBuf,
    entrypoint: &'static str,
    plugin: Option<PathBuf>,
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
            plugin: None,
        }
    }

    /// The same world seen from another folder: a repo with this origin, or with no remote.
    fn repo(&self, name: &str, origin: Option<&str>) -> World {
        let folder = self.root.join(name);
        fs::create_dir_all(folder.join(".git/refs/heads")).unwrap();
        let remote = origin.map_or(String::new(), |url| format!("[remote \"origin\"]\n\turl = {url}\n"));
        fs::write(folder.join(".git/config"), remote).unwrap();
        World { folder, ..self.clone() }
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
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_openrecall"));
        cmd.args(args)
            .env_clear()
            .env("HOME", &self.home)
            .env("OPENRECALL_HOME", self.home.join(".openrecall"))
            .env("CLAUDE_PROJECT_DIR", &self.folder)
            .env("CLAUDE_CODE_ENTRYPOINT", self.entrypoint)
            .env("CLAUDE_CODE_SESSION_ID", "M")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped());
        if let Some(plugin) = &self.plugin {
            cmd.env("CLAUDE_PLUGIN_ROOT", plugin);
        }
        let guard = SPAWN.lock().unwrap();
        let child = cmd.spawn().unwrap();
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
        self.record_in("github.com/someone/app", stem)
    }

    fn record_in(&self, identity: &str, stem: &str) -> Option<String> {
        fs::read_to_string(
            self.home
                .join(".openrecall/repos")
                .join(identity)
                .join("handoffs")
                .join(format!("{stem}.md")),
        )
        .ok()
    }

    /// The route of each push to the session, in order, from the log.
    fn routes(&self, sid: &str) -> Vec<String> {
        fs::read_to_string(self.home.join(".openrecall/log/openrecall.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|e| e["event"] == "pushed" && e["session"] == sid)
            .map(|e| e["how"].as_str().unwrap_or("").to_string())
            .collect()
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
    assert!(status.starts_with("recall 1 · "), "the total keeps the record pushed one prompt earlier: {status}");

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
fn a_prompt_that_names_the_branchs_record_gets_it_before_the_branch_settles() {
    let w = World::new("identifier");
    let edit = |p: &str| json!({"type": "tool_use", "id": "t1", "name": "Edit", "input": {"file_path": w.folder.join(p)}});
    w.branch("feat/y");
    w.hook(&["handoff"], "Y", json!({"source": "startup"}));
    w.turn("Y", &[user("Write the other doc"), said("Editing.", Some(edit("docs/y.md")))], "");
    w.branch("feat/x");
    w.hook(&["handoff"], "E", json!({"source": "startup"}));
    w.turn("E", &[user("Build ABC-12 in the docs"), said("Editing.", Some(edit("docs/x.md")))], "");
    w.turn("E", &[user("open the PR"), said("Opened PR #345 at abc1234def.", None)], "");
    let first = |sid: &str, prompt: &str| {
        w.hook(&["handoff"], sid, json!({"source": "startup"}));
        w.pushed(sid, prompt)
    };
    for (sid, prompt) in [
        ("A", "/implement docs/x.md"),
        ("B", "@docs/x.md"),
        ("C", "is #345 green?"),
        ("D", "is abc1234 in?"),
    ] {
        let ctx = first(sid, prompt).unwrap_or_default();
        assert!(ctx.starts_with("Handoff record for branch feat/x"), "{prompt}: {ctx}");
        assert_eq!(w.routes(sid), ["identifier"], "{prompt}");
    }
    let pasted = format!("docs/x.md {}", "pasted ".repeat(300));
    for (sid, prompt, why) in [
        ("F", "check docs/y.md", "only another branch's record lists it"),
        ("G", pasted.as_str(), "2,000 characters or more"),
        ("H", "docs/x.md for XYZ-99", "a ticket the record does not list"),
        ("I", "is abc123 in?", "a 6-character prefix"),
    ] {
        assert!(first(sid, prompt).is_none(), "{why}");
    }

    w.hook(&["handoff"], "S", json!({"source": "startup"}));
    w.turn("S", &[user("continue please"), said("Reading.", None)], "");
    assert!(w.pushed("S", "check docs/x.md").is_some());
    assert!(w.pushed("S", "docs/x.md again").is_none(), "never twice in one context window");
    assert_eq!(w.routes("S"), ["settle"]);
}

fn start(w: &World, sid: &str) {
    w.hook(&["handoff"], sid, json!({"source": "startup"}));
}

fn turn(w: &World, sid: &str, prompt: &str, answer: &str) {
    w.turn(sid, &[user(prompt), said(answer, None)], "");
}

#[test]
fn a_session_gets_the_record_of_a_sibling_repo_that_names_the_alias() {
    let w = World::new("sibling");
    let api = w.repo("api", Some("git@github.com:acme/api.git"));
    let web = w.repo("web", Some("https://github.com/acme/web"));
    let other = w.repo("other", Some("git@github.com:other/web.git"));
    let (one, two) = (w.repo("loose/one", None), w.repo("loose/two", None));
    let nowhere = World { folder: w.root.join("no-repo"), ..w.clone() };
    for r in [&api, &web, &other, &one, &two] {
        r.branch("feat/x");
    }
    let has = |ctx: Option<String>, address: &str| ctx.is_some_and(|c| c.contains(&format!("({address})")));
    let (api_record, web_record) = ("github.com/acme/api/handoffs/feat--x", "github.com/acme/web/handoffs/feat--x");

    start(&api, "E");
    turn(&api, "E", "Plan ABC-12 for the web repo: the export button calls the new endpoint", "Planned.");
    turn(&api, "E", "write the handoff", "Wrote docs/handoff-abc-12.md. Next: build the button in acme/web.");
    assert!(w.record_in("github.com/acme/api", "feat--x").unwrap().contains("\naliases: ABC-12\n"));

    start(&web, "S");
    let ctx = web.pushed("S", "start ABC-12").expect("1. the sibling repo's record");
    let (head, body) = ctx.split_once("\n\n").unwrap();
    assert!(
        head.starts_with("Handoff record for branch feat/x, last written 20")
            && head.ends_with(&format!(
                "by an earlier session on this task ({api_record}). It reflects what was true then in another repo, \
                 github.com/acme/api, in the folder {}. Its paths are paths of that repo, not of this one; check them \
                 in that folder before acting on it.",
                api.folder.display()
            )),
        "1. the header names the repo and its folder: {head}"
    );
    assert!(
        body.starts_with("## Goal\nPlan ABC-12 for the web repo")
            && body.ends_with("## Last answer\nWrote docs/handoff-abc-12.md. Next: build the button in acme/web.\n")
            && !ctx.contains("---"),
        "2. the record's body, no frontmatter: {ctx}"
    );
    assert_eq!(w.routes("S"), ["sibling"], "3. the log names the route");
    assert!(web.pushed("S", "and ABC-12 again").is_none(), "4. once in a context window");
    start(&web, "S2");
    assert!(web.pushed("S2", "what is next?").is_none(), "5. a prompt without the alias");

    turn(&web, "S", "start ABC-12", "Built the button. Next: wire it to the endpoint.");
    let own = w.record_in("github.com/acme/web", "feat--x").unwrap();
    assert!(own.contains("\naliases: ABC-12\nwriters: S\n"), "{own}");
    assert!(
        has(api.pushed("E", "is ABC-12 done on the web side?"), web_record),
        "6. the way back: E's own record is not usable to it"
    );
    assert_eq!(w.routes("E"), ["sibling"], "6");
    assert!(
        api.pushed("E", "ABC-12 again").is_none() && web.pushed("S", "ABC-12 once more").is_none(),
        "7. no loop"
    );

    start(&api, "E2");
    assert!(has(api.pushed("E2", "is ABC-12 done?"), api_record), "8. the own repo first");
    assert_eq!(w.routes("E2"), ["alias"], "8");
    start(&api, "E3");
    turn(&api, "E3", "unrelated work on the api", "Done.");
    start(&web, "S4");
    assert!(
        has(web.pushed("S4", "ABC-12 next step"), web_record),
        "9. the own repo's record wins over a newer record of a sibling repo"
    );
    assert_eq!(w.routes("S4"), ["alias"], "9");

    start(&other, "O");
    assert!(other.pushed("O", "start ABC-12").is_none(), "10. another repo owner");
    start(&one, "L");
    turn(&one, "L", "Plan XYZ-34 in the loose repo", "Planned.");
    start(&two, "L2");
    assert!(two.pushed("L2", "start XYZ-34").is_none(), "11. a repo with no remote has no repo owner");
    start(&web, "S3");
    assert!(web.pushed("S3", "start XYZ-34").is_none(), "12. a repo with no remote gives nothing");
    start(&nowhere, "N");
    assert!(nowhere.pushed("N", "start ABC-12").is_none(), "13. a session in no repo");

    start(&api, "E5");
    turn(&api, "E5", "Plan KLM-90 for the web repo", "Planned.");
    web.branch("main");
    start(&web, "T");
    assert!(has(web.pushed("T", "status of KLM-90?"), api_record), "14. a session on a trunk branch");
    assert_eq!(w.routes("T"), ["sibling"], "14");

    let mut m = Mcp::start(&web);
    let (text, err) = m.tool("recall", json!({"query": api_record}));
    assert!(
        !err && text.contains("\n\n---\nrepo: github.com/acme/api\n")
            && text.ends_with("## Last answer\nWrote docs/handoff-abc-12.md. Next: build the button in acme/web.\n"),
        "the MCP tool reads a sibling record by its address: {text}"
    );
    m.close();
}

#[test]
fn a_lower_case_ticket_in_a_file_name_finds_and_learns_the_alias() {
    let w = World::new("lower");
    let api = w.repo("api", Some("git@github.com:acme/api.git"));
    let web = w.repo("web", Some("https://github.com/acme/web"));
    api.branch("feat/x");
    web.branch("feat/z");
    let from_api = |ctx: Option<String>| ctx.is_some_and(|c| c.contains("(github.com/acme/api/handoffs/feat--x)"));
    let aliases = || {
        let rec = w.record_in("github.com/acme/web", "feat--z").unwrap();
        rec.lines().find(|l| l.starts_with("aliases: ")).unwrap().to_string()
    };

    start(&api, "E");
    turn(&api, "E", "Plan QRS-56 for the web repo", "Wrote docs/handoff-qrs-56.md.");
    start(&web, "F");
    let file = "read ~/Code/api/docs/handoff-qrs-56.md and go";
    assert!(from_api(web.pushed("F", file)), "16. a lower-case form in a file name finds the record");
    assert_eq!(w.routes("F"), ["sibling"], "16");
    turn(&web, "F", file, "Building it.");
    assert_eq!(aliases(), "aliases: QRS-56", "17. capture learns the alias in upper case");

    start(&web, "G");
    let shapes = "see notes/standup-oct-05.md and utf-16 and sha-256";
    assert!(web.pushed("G", shapes).is_none(), "18. false shapes find nothing");
    turn(&web, "G", shapes, "Seen.");
    assert_eq!(aliases(), "aliases: QRS-56", "19. a false shape adds no alias");

    start(&api, "E2");
    turn(&api, "E2", "Plan TUV-78 for the web repo", "Planned.");
    start(&web, "H");
    turn(&web, "H", "continue the button work", "Continued.");
    assert!(from_api(web.pushed("H", "look at ~/Code/api/docs/handoff-tuv-78.md")));
    assert!(web.pushed("H", "go on").is_some());
    assert_eq!(
        w.routes("H"),
        ["sibling", "settle"],
        "20. on a settled branch, the branch's own record comes one prompt after the sibling record"
    );
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
        assert_eq!(w.run(args, input), (String::new(), 0), "{args:?}");
    }
    let note = w.hook(
        &["recall"],
        "N",
        json!({"prompt": "<task-notification>done</task-notification>"}),
    );
    assert!(note.is_empty());
    assert_eq!(
        fs::read_to_string(w.home.join(".openrecall/status/N")).unwrap(),
        "recall 0 · skipped\n"
    );
}

#[test]
fn version_answers_in_a_skipped_session_and_marks_a_source_build() {
    let mut w = World::new("version");
    w.entrypoint = "sdk-cli";
    let (out, code) = w.run(&["version"], "");
    assert_eq!((out.as_str(), code), (format!("{} (source build)\n", env!("CARGO_PKG_VERSION")).as_str(), 0));
    assert!(!w.home.exists());
}

#[test]
fn session_start_shows_a_waiting_message_once_beside_the_version_warning() {
    let mut w = World::new("message");
    w.branch("feat/x");
    let state = w.home.join(".openrecall/update/state.json");
    fs::create_dir_all(state.parent().unwrap()).unwrap();
    fs::write(&state, r#"{"attempted_at":1,"failures":4,"message":"OpenRecall could not update itself: x."}"#).unwrap();
    fs::create_dir_all(w.root.join("plugin/.claude-plugin")).unwrap();
    fs::write(w.root.join("plugin/.claude-plugin/plugin.json"), r#"{"version": "99.0.0"}"#).unwrap();
    w.plugin = Some(w.root.join("plugin"));
    let shown = |w: &World| {
        let out = w.hook(&["handoff"], "S", json!({"source": "startup"}));
        serde_json::from_str::<Value>(&out).ok().map(|v| v["systemMessage"].as_str().unwrap().to_string())
    };
    let warning = format!(
        "OpenRecall binary {} is older than plugin 99.0.0. Update the binary: curl -fsSL https://raw.githubusercontent.com/rajnandan1/openrecall/main/install.sh | sh",
        env!("CARGO_PKG_VERSION")
    );
    assert_eq!(shown(&w), Some(format!("OpenRecall could not update itself: x.\n{warning}")));
    assert_eq!(shown(&w), Some(warning), "the message shows once, and a source build still warns");
    assert_eq!(fs::read_to_string(&state).unwrap(), r#"{"attempted_at":1,"failures":4}"#);
    w.plugin = None;
    assert_eq!(shown(&w), None);
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap().map(Result::unwrap) {
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &to.join(entry.file_name()));
        } else {
            fs::copy(entry.path(), to.join(entry.file_name())).unwrap();
        }
    }
}

/// Every later release must read a home in the formats of 0.2.0. A release that adds a field adds a file that holds
/// it. The record's date lies in the future, so it never retires.
#[test]
fn a_home_from_0_2_0_still_reads() {
    let w = World::new("compat");
    w.branch("feat/x");
    copy_dir(Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/home-0.2.0")), &w.home.join(".openrecall"));
    let out = w.hook(&["handoff"], "NEW", json!({"source": "startup"}));
    assert!(out.contains("OpenRecall updated from 0.1.0 to 0.2.0. Sessions that were open"), "the update state: {out}");
    let ctx = w.pushed("NEW", "status of ABC-12?").expect("the handoff record");
    assert!(ctx.contains("tickets: ABC-12\nprs: #345\n") && ctx.ends_with("## Last answer\nOpened PR #345. Next: ask for review.\n"), "{ctx}");
    assert!(w.pushed("OLD", "status of ABC-12?").is_none(), "the session state's ledger");
    let status = fs::read_to_string(w.home.join(".openrecall/status/OLD")).unwrap();
    assert!(status.starts_with("recall 2 · "), "the session state's total: {status}");
    let mut m = Mcp::start(&w);
    let (text, _) = m.tool("recall", json!({"query": "export queue retries"}));
    assert!(
        text.starts_with("- decision 2026-10-03 github.com/someone/app/export-retry-limit: The export queue retries a failed row 3 times"),
        "the memory file: {text}"
    );
    m.close();
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

#[test]
fn the_gate_opens_at_ten_rows_from_every_scope() {
    let w = World::new("small");
    w.branch("feat/z");
    let repo = w.home.join(".openrecall/repos/github.com/someone/app");
    let source = "source: E 2026-01-02T03:04:05Z\n";
    w.memory(&repo, "ledger-rounding", "decision", source, "Ledger totals round half-even since ABC-77",
        "Ledger totals round half-even since ABC-77; the cents column stays an integer.");
    for i in 0..8 {
        w.memory(&repo, &format!("note-{i}"), "project", source, "Unrelated", &format!("Note {i} about the deploy runner."));
    }
    assert_eq!(w.pushed("S", "ledger rounding ABC-77 cents"), None, "9 rows: the gate stays closed");
    let log = fs::read_to_string(w.home.join(".openrecall/log/openrecall.jsonl")).unwrap();
    let recall: Value = log
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find(|e| e["event"] == "recall" && e["session"] == "S")
        .unwrap();
    let top = &recall["candidates"][0];
    assert!(
        recall["index_size"] == 9
            && top["address"] == "github.com/someone/app/ledger-rounding"
            && top["score"].as_f64().unwrap() >= 1.5
            && top["bm25"].is_f64(),
        "a score above the threshold, logged beside the order's bm25: {recall}"
    );

    let config = |name: &str| {
        fs::write(w.folder.join(".git/config"), format!("[remote \"origin\"]\n\turl = git@github.com:someone/{name}.git\n")).unwrap()
    };
    config("other");
    w.memory(&w.home.join(".openrecall/repos/github.com/someone/other"), "other-note", "project", source, "Other", "A note of another repository.");
    w.hook(&["recall"], "O", json!({"prompt": "a prompt in the other repository"}));
    config("app");
    let ctx = w.pushed("T", "ledger rounding ABC-77 cents").expect("the other repository's row makes 10");
    assert!(ctx.contains("github.com/someone/app/ledger-rounding"), "{ctx}");
}

#[test]
fn a_memory_whose_description_names_the_prompts_ticket_passes_under_the_threshold() {
    let w = World::new("ident");
    w.branch("feat/x");
    let repo = w.home.join(".openrecall/repos/github.com/someone/app");
    w.fillers(&repo);
    let source = "source: E 2026-01-02T03:04:05Z\n";
    w.memory(&repo, "ledger-rounding", "decision", source, "Ledger totals round half-even since ABC-77",
        "Totals round half-even; the cents column stays an integer.");
    w.memory(&repo, "rounding-history", "decision", source, "Half-up rounding drifted a cent",
        "Half-up rounding drifted one cent per thousand postings before ABC-77 moved it.");

    let ctx = w.pushed("S", "ABC-77 why does this keep failing for the team today").expect("the identifier rule injects");
    let lines: Vec<&str> = ctx.lines().skip(1).collect();
    assert_eq!(lines.len(), 1, "a ticket in the body alone does not count: {ctx}");
    assert!(lines[0].contains("github.com/someone/app/ledger-rounding: "), "{ctx}");
    let log = fs::read_to_string(w.home.join(".openrecall/log/openrecall.jsonl")).unwrap();
    let recall: Value = log
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find(|e| e["event"] == "recall" && e["session"] == "S")
        .unwrap();
    let scores: Vec<f64> = recall["candidates"].as_array().unwrap().iter().map(|c| c["score"].as_f64().unwrap()).collect();
    assert!(scores.iter().all(|&s| s < 1.5), "no candidate passes by score: {recall}");
    assert_eq!(recall["via"], json!({"github.com/someone/app/ledger-rounding": "identifier"}), "{recall}");
}

#[test]
fn an_echo_frees_its_line_and_stays_out_of_the_ledger() {
    let w = World::new("echo");
    w.branch("feat/x");
    fs::create_dir_all(w.folder.join(".scratch/x")).unwrap();
    fs::write(w.folder.join(".scratch/x/map.md"), "").unwrap();
    let repo = w.home.join(".openrecall/repos/github.com/someone/app");
    w.fillers(&repo);
    let source = "source: E 2026-01-02T03:04:05Z\n";
    let address = "github.com/someone/app/walrus-migration-map";
    w.memory(&repo, "walrus-migration-map", "pointer", source, "The walrus migration map",
        "The walrus migration map lives in .scratch/x/map.md for the whole team");
    for i in 0..3 {
        w.memory(&repo, &format!("walrus-step-{i}"), "decision", source, &format!("Step {i} of WAL-12"),
            &format!("Step {i} of the walrus migration runs after the map is read."));
    }
    let recall = |sid: &str| -> Value {
        fs::read_to_string(w.home.join(".openrecall/log/openrecall.jsonl"))
            .unwrap()
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|e| e["event"] == "recall" && e["session"] == sid)
            .last()
            .unwrap()
    };

    let ctx = w.pushed("S", "read .scratch/x/map.md and plan the walrus migration map for WAL-12").expect("the steps pass");
    let lines: Vec<&str> = ctx.lines().skip(1).collect();
    assert!(!ctx.contains(address), "the prompt names the pointer's path: {ctx}");
    assert_eq!(lines.len(), 3, "the echo frees its line for the next candidate: {ctx}");
    let log = recall("S");
    assert_eq!((&log["dropped"]["echo"], &log["echoes"]), (&json!(1), &json!([address])), "{log}");

    let ctx = w.pushed("S", "walrus migration map lives where").expect("the pointer passes");
    assert!(ctx.contains(address), "an echo never enters the session ledger: {ctx}");
    assert_eq!(recall("S")["dropped"].get("echo"), None);
}

#[test]
fn the_status_line_totals_the_sessions_injections() {
    let w = World::new("total");
    w.branch("feat/x");
    let repo = w.home.join(".openrecall/repos/github.com/someone/app");
    w.fillers(&repo);
    let source = "source: E 2026-01-02T03:04:05Z\n";
    w.memory(&repo, "kiwi-checksum", "gotcha", source, "The kiwi importer drops rows without a checksum",
        "The kiwi importer drops every row that has no checksum.");
    w.memory(&repo, "walrus-export-encoding", "gotcha", source, "The walrus export writes UTF-16",
        "The walrus export writes UTF-16 files, so every reader decodes them first.");
    w.memory(&repo, "walrus-export-header", "decision", source, "The walrus export puts the header row first",
        "The walrus export puts the header row first, before any data row.");
    let status = || fs::read_to_string(w.home.join(".openrecall/status/S")).unwrap();
    let send = |prompt: &str| {
        let injected = w.pushed("S", prompt).unwrap_or_default().lines().filter(|l| l.starts_with("- ")).count();
        let line = status();
        let total = line
            .strip_suffix(" ms\n")
            .and_then(|l| l.rsplit_once(" · "))
            .filter(|(_, ms)| ms.parse::<u64>().is_ok())
            .map(|(total, _)| total.to_string());
        (injected, total.unwrap_or(line))
    };
    let kiwi = "kiwi importer drops rows";
    let walrus = "walrus export header encoding";

    w.hook(&["handoff"], "S", json!({"source": "startup"}));
    assert_eq!(send(kiwi), (1, "recall 1".into()));
    assert_eq!(send("please tidy the formatting everywhere"), (0, "recall 1".into()));
    assert_eq!(send(walrus), (2, "recall 3".into()));
    w.hook(&["recall"], "S", json!({"prompt": "<task-notification>done</task-notification>"}));
    assert_eq!(status(), "recall 3 · skipped\n");
    w.hook(&["recall"], "S", json!({"prompt": walrus, "agent_id": "a1"}));
    assert_eq!(status(), "recall 3 · skipped\n", "a subagent prompt keeps the total");

    w.hook(&["handoff"], "S", json!({"source": "compact"}));
    assert_eq!(send(kiwi), (1, "recall 4".into()), "a compaction keeps the total");
    w.hook(&["handoff"], "S", json!({"source": "startup"}));
    assert_eq!(send(kiwi), (1, "recall 1".into()), "a new start counts from 0");
}

#[test]
fn a_prompt_during_the_capture_writer_keeps_both_updates() {
    let w = World::new("lock");
    w.branch("feat/x");
    let repo = w.home.join(".openrecall/repos/github.com/someone/app");
    w.fillers(&repo);
    let source = "source: E 2026-01-02T03:04:05Z\n";
    w.memory(&repo, "walrus-export-encoding", "gotcha", source, "The walrus export writes UTF-16",
        "The walrus export writes UTF-16 files, so every reader decodes them first.");
    w.memory(&repo, "walrus-export-header", "decision", source, "The walrus export puts the header row first",
        "The walrus export puts the header row first, before any data row.");
    w.hook(&["handoff"], "S", json!({"source": "startup"}));
    let transcript = w.root.join("S.jsonl");
    fs::write(&transcript, format!("{}\n{}\n", user("Build ABC-12 so exports stop failing on empty rows"), said("ok", None))).unwrap();
    let start = |args: &[&str], input: Value| {
        let mut child = w.spawn(args);
        child.stdin.take().unwrap().write_all(input.to_string().as_bytes()).unwrap();
        child
    };
    let cwd = w.folder.join("src");

    let lock = fs::File::create(w.home.join(".openrecall/sessions/S.lock")).unwrap();
    lock.lock().unwrap();
    let writer = start(&["capture", "--job"], json!({"session_id": "S", "cwd": cwd, "transcript_path": transcript, "last_assistant_message": "ok"}));
    let prompt = start(&["recall"], json!({"session_id": "S", "cwd": cwd, "prompt": "walrus export header encoding"}));
    std::thread::sleep(Duration::from_millis(300));
    assert!(w.record("feat--x").is_none() && !w.home.join(".openrecall/status/S").exists(), "both wait for the lock");
    drop(lock);
    for child in [writer, prompt] {
        assert!(child.wait_with_output().unwrap().status.success());
    }

    let s: Value = serde_json::from_str(&fs::read_to_string(w.home.join(".openrecall/sessions/S.json")).unwrap()).unwrap();
    let mut ledger: Vec<&str> = s["ledger"].as_array().unwrap().iter().map(|a| a.as_str().unwrap()).collect();
    ledger.sort();
    assert_eq!(ledger, [
        "github.com/someone/app/handoffs/feat--x",
        "github.com/someone/app/walrus-export-encoding",
        "github.com/someone/app/walrus-export-header",
    ], "{s}");
    assert_eq!((s["injections"].as_u64(), s["turns"].as_u64()), (Some(2), Some(1)), "{s}");
    assert!(fs::read_to_string(w.home.join(".openrecall/status/S")).unwrap().starts_with("recall 2 · "));
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

const PICK_SYSTEM: &str = "You pick memories for a coding assistant (Claude Code) that is about to act on the user's prompt. You get the list of memories (id, name and description), the session so far, newest turn first, and the prompt. Pick at most one memory, and only one that states a fact, decision, pointer or gotcha that the assistant needs to act on this prompt and that neither the prompt nor the session already contains. A memory about the same project but another task, or one that only repeats what the prompt or the session says, is a wrong pick. Most prompts need no memory: pick none unless you are sure. Return its id, or no id.";

/// Picks on through the provider stand-in: extraction's settings plus `pick = true`.
fn picks_on(w: &World, p: &Provider) {
    let home = w.home.join(".openrecall");
    fs::create_dir_all(&home).unwrap();
    fs::write(home.join("extract.toml"), format!("base_url = \"http://127.0.0.1:{}/v1\"\nmodel = \"test/model\"\npick = true\n", p.port)).unwrap();
    fs::write(home.join("api-key"), "sk-test-123\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(home.join("api-key"), fs::Permissions::from_mode(0o600)).unwrap();
}

/// The prompt hook with a `prompt_id` and the session's transcript.
fn prompt(w: &World, sid: &str, id: &str, text: &str) -> String {
    w.hook(&["recall"], sid, json!({"prompt": text, "prompt_id": id, "transcript_path": w.root.join(format!("{sid}.jsonl"))}))
}

fn transcript(w: &World, sid: &str, lines: &[Value]) {
    let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
    fs::write(w.root.join(format!("{sid}.jsonl")), text).unwrap();
}

fn log_of(w: &World, event: &str) -> Vec<Value> {
    fs::read_to_string(w.home.join(".openrecall/log/openrecall.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|e| e["event"] == event)
        .collect()
}

/// The `pick` line of one prompt, once the job has logged it.
fn pick_line(w: &World, id: &str) -> Value {
    let started = Instant::now();
    loop {
        if let Some(line) = log_of(w, "pick").into_iter().find(|e| e["prompt_id"] == id) {
            return line;
        }
        assert!(started.elapsed() < Duration::from_secs(20), "no pick line for {id}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn pick_file(w: &World, sid: &str, name: &str) -> PathBuf {
    w.home.join(".openrecall/picks").join(sid).join(name)
}

#[test]
fn a_pick_that_passes_writes_its_result_for_the_tool_hook() {
    let w = World::new("pick");
    w.branch("feat/x");
    let repo = w.home.join(".openrecall/repos/github.com/someone/app");
    let source = "source: E 2026-01-02T03:04:05Z\n";
    w.memory(&repo, "alpha-walrus-encoding", "gotcha", source, "The walrus export writes UTF-16", "The walrus export writes UTF-16 files.");
    w.memory(&repo, "bravo-walrus-header", "decision", source, "The walrus export puts the header row first", "The walrus export puts the header row first, before any data row.");
    w.memory(&repo, "charlie-kiwi-checksum", "gotcha", source, "The kiwi importer drops rows without a checksum", "The kiwi importer drops every row that has no checksum.");
    let p = Provider::start(vec![answer(json!({"ids": ["m2", "m1"]}))]);
    picks_on(&w, &p);
    let token = format!("ghp_{}", "Zq8Xw3Kp9Lm2Nv7Bc4Rt6Yh1Jd5Fg0Sa3Ew8");
    let ask = "why does the walrus export drop the header row today";
    let bash = json!({"type": "tool_use", "id": "t1", "name": "Bash", "input": {"command": "cargo test walrus"}});
    transcript(&w, "S", &[
        user(&format!("set up the walrus export, CI pushes with {token}")),
        said("Set it up.", Some(bash)),
        user(ask),
    ]);

    prompt(&w, "S", "p1", ask);
    let line = pick_line(&w, "p1");
    let address = "github.com/someone/app/bravo-walrus-header";
    assert_eq!(
        (&line["session"], &line["memory"], &line["skip"], &line["tokens_in"], &line["tokens_out"], &line["cost"]),
        (&json!("S"), &json!(address), &Value::Null, &json!(1000), &json!(200), &json!(0.01)),
        "only the first id counts: {line}"
    );
    assert!(line["ms"].is_u64() && line["at"].is_u64(), "{line}");
    let result: Value = serde_json::from_str(&fs::read_to_string(pick_file(&w, "S", "p1.json")).unwrap()).unwrap();
    assert_eq!(result["address"], address);
    assert_eq!(
        result["text"],
        "Recalled memories from earlier sessions (OpenRecall). They reflect what was true when written. Full text: \
         mcp__plugin_openrecall_openrecall__recall with the address.\n- decision 2026-01-02 \
         github.com/someone/app/bravo-walrus-header: The walrus export puts the header row first, before any data row."
    );
    assert!(!pick_file(&w, "S", "p1.running").exists());

    let reqs = p.requests();
    assert_eq!(reqs.len(), 1);
    let body = &reqs[0].1;
    assert_eq!((&body["model"], &body["max_tokens"], &body["reasoning"]), (&json!("test/model"), &json!(500), &json!({"effort": "low"})));
    assert_eq!(
        body["response_format"],
        json!({"type": "json_schema", "json_schema": {"name": "pick", "strict": true, "schema":
            {"type": "object", "properties": {"ids": {"type": "array", "items": {"type": "string"}}}, "required": ["ids"], "additionalProperties": false}}})
    );
    assert_eq!(body["messages"][0], json!({"role": "system", "content": PICK_SYSTEM}));
    let sent = body["messages"][1]["content"].as_str().unwrap();
    assert_eq!(
        sent,
        "## Memories\n\
         - m1: alpha-walrus-encoding: The walrus export writes UTF-16\n\
         - m2: bravo-walrus-header: The walrus export puts the header row first\n\
         - m3: charlie-kiwi-checksum: The kiwi importer drops rows without a checksum\n\n\
         ## Session so far, newest turn first\n\
         [user] set up the walrus export, CI pushes with [REDACTED:github-pat]\n[assistant] Set it up.\n[tool] Bash cargo test walrus\n\n\
         ## Prompt\nwhy does the walrus export drop the header row today",
        "the transcript's copy of the prompt is left out"
    );
    let s: Value = serde_json::from_str(&fs::read_to_string(w.home.join(".openrecall/sessions/S.json")).unwrap()).unwrap();
    assert_eq!(s["picks"], 1);
}

#[test]
fn the_pick_input_fits_32000_characters_and_cuts_the_oldest_turns_first() {
    let w = World::new("pick-size");
    w.branch("feat/x");
    let repo = w.home.join(".openrecall/repos/github.com/someone/app");
    w.memory(&repo, "walrus-header", "decision", "", "The walrus export puts the header row first", "The header row comes first.");
    let p = Provider::start(vec![answer(json!({"ids": []})), answer(json!({"ids": []}))]);
    picks_on(&w, &p);
    let mut lines = vec![];
    for i in 0..40 {
        lines.push(user(&format!("turn-{i:02} asks about the walrus export")));
        lines.push(said(&format!("turn-{i:02} answers. {}", "The walrus export is fine. ".repeat(40)), None));
    }
    transcript(&w, "S", &lines);
    prompt(&w, "S", "p1", "what changed in the walrus export header since yesterday");
    let line = pick_line(&w, "p1");
    assert_eq!((&line["memory"], &line["skip"]), (&Value::Null, &Value::Null), "{line}");
    let body = &p.requests()[0].1;
    let sent = body["messages"][1]["content"].as_str().unwrap();
    let chars = PICK_SYSTEM.chars().count() + sent.chars().count();
    assert!((30_000..=32_000).contains(&chars), "{chars} characters");
    let (newest, older) = (sent.find("[user] turn-39").unwrap(), sent.find("[user] turn-38").unwrap());
    assert!(newest < older, "newest turn first");
    assert!(!sent.contains("turn-00 "), "the oldest turns are cut");

    transcript(&w, "S", &[user("turn-big asks"), said(&"Walrus. ".repeat(5000), None)]);
    prompt(&w, "S", "p3", "what changed in the walrus export header since yesterday");
    pick_line(&w, "p3");
    let sent = p.requests()[1].1["messages"][1]["content"].as_str().unwrap().to_string();
    assert_eq!(PICK_SYSTEM.chars().count() + sent.chars().count(), 32_000, "the newest turn alone is cut to the room left");
    assert!(sent.contains("\n[user] turn-big asks\n[assistant] Walrus. ") && sent.ends_with("\n\n## Prompt\nwhat changed in the walrus export header since yesterday"));

    let w = World::new("pick-big");
    w.branch("feat/x");
    let repo = w.home.join(".openrecall/repos/github.com/someone/app");
    for i in 0..250 {
        w.memory(&repo, &format!("note-{i:03}"), "decision", "", &format!("Note {i:03}: {}", "the walrus export keeps one rule here. ".repeat(3)), "Body.");
    }
    let p = Provider::start(vec![answer(json!({"ids": ["m1"]}))]);
    picks_on(&w, &p);
    prompt(&w, "S", "p2", "what changed in the walrus export header since yesterday");
    let line = pick_line(&w, "p2");
    assert_eq!((&line["skip"], &line["memory"], &line["tokens_in"]), (&json!("too_big"), &Value::Null, &json!(0)), "{line}");
    assert!(p.requests().is_empty(), "no call");
    assert!(!pick_file(&w, "S", "p2.running").exists() && !pick_file(&w, "S", "p2.json").exists());
    let s: Value = serde_json::from_str(&fs::read_to_string(w.home.join(".openrecall/sessions/S.json")).unwrap()).unwrap();
    assert_eq!(s["picks"], Value::Null, "a too_big input does not count");
}

#[test]
fn a_prompt_makes_at_most_one_call_and_a_session_ten() {
    let w = World::new("pick-cap");
    w.branch("feat/x");
    let repo = w.home.join(".openrecall/repos/github.com/someone/app");
    w.memory(&repo, "walrus-header", "decision", "", "The walrus export puts the header row first", "The header row comes first.");
    let not_json = (200, json!({"choices": [{"finish_reason": "stop", "message": {"content": "m1, I think"}}]}));
    let mut replies = vec![(500, json!({"error": {"code": "server_error"}})), not_json, answer(json!({"ids": ["m9"]}))];
    replies.extend((0..8).map(|_| answer(json!({"ids": []}))));
    let p = Provider::start(replies);
    picks_on(&w, &p);
    transcript(&w, "S", &[]);
    for n in 1..=10 {
        let id = format!("p{n}");
        prompt(&w, "S", &id, "what changed in the walrus export header since yesterday");
        let line = pick_line(&w, &id);
        assert_eq!(p.requests().len(), n, "one call for prompt {n}, never a retry: {line}");
        let want = match n {
            1 => json!({"skip": "error", "error": "http 500 server_error", "memory": null}),
            2 => json!({"skip": "error", "error": "invalid json", "memory": null}),
            _ => json!({"skip": null, "memory": null}),
        };
        for (k, v) in want.as_object().unwrap() {
            assert_eq!(&line[k], v, "{k} of prompt {n}: {line}");
        }
        assert!(!pick_file(&w, "S", &format!("{id}.json")).exists(), "no result for prompt {n}");
        assert!(!pick_file(&w, "S", &format!("{id}.running")).exists());
    }
    prompt(&w, "S", "p11", "what changed in the walrus export header since yesterday");
    w.run(&["pick", "--job"], &json!({"session_id": "S", "prompt_id": "p12", "cwd": w.folder, "prompt": "walrus export header"}).to_string());
    assert_eq!(pick_line(&w, "p12")["skip"], "cap", "the job keeps the cap too");
    assert!(log_of(&w, "pick").iter().all(|l| l["prompt_id"] != "p11"), "the 11th prompt starts no job");
    assert_eq!(p.requests().len(), 10);
}

#[test]
fn own_expired_rot_and_echo_drop_a_pick() {
    let w = World::new("pick-drop");
    w.branch("feat/x");
    fs::create_dir_all(w.folder.join(".scratch/x")).unwrap();
    fs::write(w.folder.join(".scratch/x/map.md"), "").unwrap();
    let repo = w.home.join(".openrecall/repos/github.com/someone/app");
    let old = "source: E 2026-01-02T03:04:05Z\n";
    w.memory(&repo, "a-own", "decision", "source: S 2026-01-02T03:04:05Z\n", "This session wrote it", "The walrus export runs nightly.");
    w.memory(&repo, "b-expired", "state", &format!("{old}expires: 2020-01-01\n"), "An old state", "PR #345 waits for review.");
    w.memory(&repo, "c-rot", "pointer", old, "The walrus code", "The walrus export lives in src/gone.py.");
    w.memory(&repo, "d-echo", "pointer", old, "The walrus map", "The walrus migration map lives in .scratch/x/map.md");
    let p = Provider::start(["m1", "m2", "m3", "m4"].iter().map(|m| answer(json!({"ids": [m]}))).collect());
    picks_on(&w, &p);
    transcript(&w, "S", &[]);
    for (n, (stem, reason)) in [("a-own", "own"), ("b-expired", "expired"), ("c-rot", "rot"), ("d-echo", "echo")].iter().enumerate() {
        let id = format!("p{n}");
        prompt(&w, "S", &id, "read .scratch/x/map.md and plan the walrus migration");
        let line = pick_line(&w, &id);
        assert_eq!((&line["memory"], &line["skip"]), (&json!(format!("github.com/someone/app/{stem}")), &json!(reason)), "{line}");
        assert!(!pick_file(&w, "S", &format!("{id}.json")).exists() && !pick_file(&w, "S", &format!("{id}.running")).exists(), "{reason}");
    }
}

#[test]
fn nothing_starts_with_picks_off_or_without_a_plain_prompt_id() {
    let w = World::new("pick-off");
    w.branch("feat/x");
    let repo = w.home.join(".openrecall/repos/github.com/someone/app");
    w.memory(&repo, "walrus-header", "decision", "", "The walrus export puts the header row first", "The header row comes first.");
    let p = Provider::start(vec![answer(json!({"ids": []}))]);
    picks_on(&w, &p);
    transcript(&w, "S", &[]);
    let home = w.home.join(".openrecall");
    let toml = fs::read_to_string(home.join("extract.toml")).unwrap();
    let ask = "what changed in the walrus export header since yesterday";
    for (n, settings) in [toml.replace("pick = true\n", ""), toml.replace("pick = true", "pick = false")].iter().enumerate() {
        fs::write(home.join("extract.toml"), settings).unwrap();
        prompt(&w, "S", &format!("off{n}"), ask);
    }
    fs::write(home.join("extract.toml"), &toml).unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(home.join("api-key"), fs::Permissions::from_mode(0o644)).unwrap();
    prompt(&w, "S", "off2", ask);
    fs::set_permissions(home.join("api-key"), fs::Permissions::from_mode(0o600)).unwrap();
    w.hook(&["recall"], "S", json!({"prompt": ask}));
    prompt(&w, "S", "../off3", ask);
    prompt(&w, "S", "off4", "<task-notification>done</task-notification>");
    prompt(&w, "S", "off5", "fix it now");
    w.hook(&["recall"], "S", json!({"prompt": ask, "prompt_id": "off6", "agent_id": "a1"}));

    prompt(&w, "S", "on", ask);
    pick_line(&w, "on");
    std::thread::sleep(Duration::from_millis(300));
    let started: Vec<Value> = log_of(&w, "pick").iter().map(|l| l["prompt_id"].clone()).collect();
    assert_eq!(started, [json!("on")], "only the prompt with picks on and a plain prompt_id starts a job");
    assert_eq!(p.requests().len(), 1);
}
