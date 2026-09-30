use serde_json::{Value, json};
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Mutex;
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

    fn run(&self, args: &[&str], input: &str) -> (String, i32) {
        let guard = SPAWN.lock().unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_openrecall"))
            .args(args)
            .env_clear()
            .env("HOME", &self.home)
            .env("OPENRECALL_HOME", self.home.join(".openrecall"))
            .env("CLAUDE_PROJECT_DIR", &self.folder)
            .env("CLAUDE_CODE_ENTRYPOINT", self.entrypoint)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        drop(guard);
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
    let ctx = w.pushed("S", "what is left").expect("pushed once settled");
    assert!(ctx.starts_with("Handoff record for branch feat/x, last written "));
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
