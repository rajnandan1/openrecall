use serde_json::{Value, json};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// UTC+05:30 all year, so a line at 20:00 UTC counts on the next local day.
const TZ: &str = "Asia/Kolkata";

const FIELDS: [&str; 36] = [
    "day", "written_at", "versions", "sessions", "prompts", "searched", "prompt_skips", "injections",
    "identifier_rule", "echoes", "used_signal", "hook_ms", "index_size", "pick_jobs", "picks", "pick_skips",
    "picks_delivered", "pick_drops", "picks_used", "pick_cost", "pick_ms", "pushes", "right_pushes", "late_pushes",
    "extraction_runs", "facts", "suggestions", "strikes", "failed_sessions", "stops", "extraction_cost",
    "extraction_ms", "redactions", "own_tool_calls", "update_check", "errors",
];

/// A made-up user home: `HOME` is `root`, the OpenRecall home is `root/.openrecall`.
struct Home {
    root: PathBuf,
}

impl Home {
    fn new(name: &str) -> Home {
        let root = std::env::temp_dir().join(format!("openrecall-stats-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join(".openrecall/log")).unwrap();
        Home { root }
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    fn append(&self, rel: &str, lines: &[Value]) {
        let path = self.path(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut f = fs::OpenOptions::new().create(true).append(true).open(path).unwrap();
        for l in lines {
            writeln!(f, "{l}").unwrap();
        }
    }

    fn log(&self, lines: &[Value]) {
        self.append(".openrecall/log/openrecall.jsonl", lines);
    }

    fn transcript(&self, sid: &str, lines: &[Value]) {
        self.append(&format!(".claude/projects/-w-app/{sid}.jsonl"), lines);
    }

    fn put(&self, rel: &str, text: impl AsRef<[u8]>) {
        let path = self.path(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn job(&self) {
        let status = Command::new(env!("CARGO_BIN_EXE_openrecall"))
            .args(["stats", "--job"])
            .env_clear()
            .env("HOME", &self.root)
            .env("OPENRECALL_HOME", self.path(".openrecall"))
            .env("TZ", TZ)
            .status()
            .unwrap();
        assert!(status.success());
    }

    /// Lets the next job run inside the 24 hours.
    fn again(&self) {
        let _ = fs::remove_file(self.path(".openrecall/stats/state.json"));
    }

    fn text(&self) -> String {
        fs::read_to_string(self.path(".openrecall/stats/daily.jsonl")).unwrap_or_default()
    }

    fn rows(&self) -> Vec<Value> {
        self.text().lines().map(|l| serde_json::from_str(l).unwrap()).collect()
    }

    fn row(&self, day: &str) -> Value {
        self.rows().into_iter().find(|r| r["day"] == day).unwrap_or_else(|| panic!("no row for {day}"))
    }

    /// The `stats` tool's text for these arguments, from `openrecall mcp` over stdio.
    fn stats_text(&self, args: Value) -> String {
        let mut child = Command::new(env!("CARGO_BIN_EXE_openrecall"))
            .arg("mcp")
            .env_clear()
            .env("HOME", &self.root)
            .env("OPENRECALL_HOME", self.path(".openrecall"))
            .env("TZ", TZ)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        let mut out = BufReader::new(child.stdout.take().unwrap()).lines();
        let mut reply = |id: u64| {
            out.by_ref().map(|l| serde_json::from_str::<Value>(&l.unwrap()).unwrap()).find(|v| v["id"] == id).unwrap()
        };
        let hello = json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "t", "version": "0"}});
        writeln!(stdin, "{}", json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": hello})).unwrap();
        reply(1);
        writeln!(stdin, "{}", json!({"jsonrpc": "2.0", "method": "notifications/initialized"})).unwrap();
        let call = json!({"name": "stats", "arguments": args});
        writeln!(stdin, "{}", json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": call})).unwrap();
        let text = reply(2)["result"]["content"][0]["text"].as_str().unwrap().to_string();
        drop(stdin);
        child.wait().unwrap();
        text
    }
}

/// `2026-01-10T08:00:00Z` or `2026-01-10T08:00:00.250Z` in ms since 1970.
fn ms(iso: &str) -> u64 {
    let n = |r: std::ops::Range<usize>| iso[r].parse::<i64>().unwrap();
    let (y, m, d) = (n(0..4), n(5..7), n(8..10));
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let days = era * 146097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719468;
    let frac = iso.get(20..23).and_then(|f| f.parse::<i64>().ok()).unwrap_or(0);
    ((days * 86400 + n(11..13) * 3600 + n(14..16) * 60 + n(17..19)) * 1000 + frac) as u64
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64
}

fn with(mut line: Value, extra: Value) -> Value {
    line.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    line
}

fn at(iso: &str, line: Value) -> Value {
    with(line, json!({"at": ms(iso)}))
}

/// A `recall` line of a prompt that started at `iso` and took `took` ms, with `extra` fields.
fn recall(iso: &str, sid: &str, took: u64, extra: Value) -> Value {
    recall_at(ms(iso), sid, took, extra)
}

fn recall_at(start: u64, sid: &str, took: u64, extra: Value) -> Value {
    let line = json!({"event": "recall", "session": sid, "started_at": start, "ended_at": start + took,
                      "prompt_hash": "0", "injected": [], "at": start + took + 1});
    with(line, extra)
}

/// The fields of a gate injection of `address` on a prompt with this text.
fn injected(prompt: &str, address: &str, extra: Value) -> Value {
    with(json!({"prompt_hash": fnv(prompt), "candidates": [{"address": address}], "injected": [address]}), extra)
}

fn fnv(text: &str) -> String {
    let h = text.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3));
    format!("{h:016x}")
}

fn user(iso: &str, text: &str, extra: Value) -> Value {
    with(json!({"type": "user", "timestamp": iso, "message": {"content": text}}), extra)
}

fn said(iso: &str, text: &str) -> Value {
    json!({"type": "assistant", "timestamp": iso, "message": {"content": [{"type": "text", "text": text}]}})
}

fn tool(iso: &str, name: &str, input: Value) -> Value {
    json!({"type": "assistant", "timestamp": iso,
           "message": {"content": [{"type": "tool_use", "id": iso, "name": name, "input": input}]}})
}

fn without_written_at(mut row: Value) -> Value {
    row.as_object_mut().unwrap().remove("written_at");
    row
}

#[test]
fn each_local_day_with_log_lines_gets_one_row_of_counts() {
    let h = Home::new("counts");
    h.log(&[
        at("2026-01-08T10:00:00Z", json!({"event": "skipped", "session": "D", "reason": "notification"})),
        at("2026-01-09T19:00:00Z", json!({"event": "update", "step": "binary", "result": "updated", "from": "0.8.0", "to": "0.9.0", "reason": ""})),
        at("2026-01-09T19:00:05Z", json!({"event": "update", "step": "plugin", "result": "updated", "from": "0.8.0", "to": "0.9.0", "reason": ""})),
        at("2026-01-09T20:00:00Z", json!({"event": "skipped", "session": "A", "reason": "notification"})),
        at("2026-01-09T20:00:01Z", json!({"event": "skipped", "session": "A", "reason": "notification"})),
        recall("2026-01-09T20:01:00Z", "A", 40, json!({"candidates": [], "index_size": 100, "via": {}, "echoes": []})),
        at("2026-01-09T20:01:01Z", json!({"event": "pushed", "session": "A", "address": "app/handoffs/feat--x", "how": "settle"})),
        at("2026-01-09T20:01:02Z", json!({"event": "pushed", "session": "A", "address": "lib/handoffs/feat--y", "how": "sibling"})),
        recall("2026-01-09T20:02:00Z", "A", 60, json!({"candidates": [{"address": "app/m1"}, {"address": "app/m2"}],
            "injected": ["app/m1"], "index_size": 120, "via": {"app/m1": "identifier"}, "echoes": ["app/m2"]})),
        at("2026-01-09T20:03:00Z", json!({"event": "skipped", "session": "B", "reason": "short"})),
        recall("2026-01-09T20:03:00Z", "B", 10, json!({})),
        at("2026-01-09T20:04:00Z", json!({"event": "skipped", "session": "B", "reason": "empty-args"})),
        at("2026-01-09T20:05:00Z", json!({"event": "pick", "session": "A", "prompt_id": "p1", "cost": 0.25, "memory": "app/m1", "skip": null, "ms": 3000})),
        at("2026-01-09T20:05:01Z", json!({"event": "pick", "session": "A", "prompt_id": "p2", "cost": 0.5, "memory": null, "skip": "too_big", "ms": 1000})),
        at("2026-01-09T20:05:02Z", json!({"event": "pick", "session": "A", "prompt_id": "p3", "cost": 0.125, "memory": "app/m3", "skip": "rot", "ms": 2000})),
        at("2026-01-09T20:05:03Z", json!({"event": "deliver", "session": "A", "prompt_id": "p1", "ms": 1, "memory": "app/m1"})),
        at("2026-01-09T20:05:04Z", json!({"event": "pick_drop", "session": "A", "prompt_id": "p2", "memory": null, "reason": "late"})),
        at("2026-01-09T20:05:05Z", json!({"event": "pick_drop", "session": "A", "prompt_id": "p4", "memory": "app/m1", "reason": "ledger"})),
        at("2026-01-09T20:06:00Z", json!({"event": "extract", "session": "A", "from": 0, "to": 100, "proposed": 6, "new": 3, "replaced": 1,
            "skipped": 2, "dropped": {"secret": 1}, "suggestions": 1, "cost": 0.5, "ms": 20000})),
        at("2026-01-09T20:06:01Z", json!({"event": "dedupe", "session": "A", "action": "new"})),
        at("2026-01-09T20:06:02Z", json!({"event": "extract", "session": "A", "strike": "invalid json", "strikes": 1, "failed": false})),
        at("2026-01-09T20:06:03Z", json!({"event": "extract", "session": "A", "strike": "invalid json", "strikes": 2, "failed": true})),
        at("2026-01-09T20:06:04Z", json!({"event": "extract", "session": "A", "stop": "curl exit 6"})),
        at("2026-01-09T20:06:05Z", json!({"event": "extract", "off": "no extract.toml"})),
        at("2026-01-09T20:07:00Z", json!({"event": "redacted", "session": "A", "rule": "generic-api-key", "section": "extract"})),
        at("2026-01-09T20:08:00Z", json!({"event": "mcp", "tool": "recall", "session": "A"})),
        at("2026-01-09T20:08:01Z", json!({"event": "mcp", "tool": "recall", "session": "A"})),
        at("2026-01-09T20:08:02Z", json!({"event": "mcp", "tool": "forget", "session": "A"})),
        at("2026-01-09T20:09:00Z", json!({"event": "settled", "session": "C", "branch": "feat/z"})),
        at("2026-01-09T20:09:01Z", json!({"event": "retired", "address": "app/handoffs/old"})),
        at("2026-01-09T20:10:00Z", json!({"event": "error", "cmd": "mcp", "error": "x"})),
        recall("2026-01-12T06:00:00Z", "A", 25, json!({})),
    ]);
    let before = now_ms();
    h.job();
    let after = now_ms();

    let rows = h.rows();
    let days: Vec<&str> = rows.iter().map(|r| r["day"].as_str().unwrap()).collect();
    assert_eq!(days, ["2026-01-08", "2026-01-10", "2026-01-12"], "a day with no log lines gets no row");
    for line in h.text().lines() {
        let at: Vec<usize> = FIELDS.iter().map(|f| line.find(&format!("\"{f}\":")).unwrap()).collect();
        assert!(at.is_sorted(), "the 36 fields in order: {line}");
    }
    let written = rows[1]["written_at"].as_u64().unwrap();
    assert!((before..=after).contains(&written));
    assert_eq!(
        without_written_at(rows[1].clone()),
        json!({"day": "2026-01-10", "versions": ["0.8.0", "0.9.0"], "sessions": 3, "prompts": 3, "searched": 2,
               "prompt_skips": {"notification": 2, "short": 1, "empty_args": 1}, "injections": 1,
               "identifier_rule": 1, "echoes": 1, "used_signal": {"yes": 0, "judged": 0},
               "hook_ms": {"median": 40, "p95": 60}, "index_size": 120, "pick_jobs": 3, "picks": 2,
               "pick_skips": {"too_big": 1, "rot": 1}, "picks_delivered": 1, "pick_drops": {"late": 1, "ledger": 1},
               "picks_used": {"yes": 0, "judged": 0}, "pick_cost": 0.875, "pick_ms": 2000,
               "pushes": {"settle": 1, "sibling": 1}, "right_pushes": {"right": 0, "wrong": 0, "unjudged": 0},
               "late_pushes": {"late": 0, "judged": 0}, "extraction_runs": 1,
               "facts": {"new": 3, "replaced": 1, "reconciled": 0, "skipped": 2, "dropped": 1}, "suggestions": 1,
               "strikes": 2,
               "failed_sessions": 1, "stops": 1, "extraction_cost": 0.5, "extraction_ms": 20000, "redactions": 1,
               "own_tool_calls": {"recall": 2, "forget": 1}, "update_check": {"binary_updated": 1, "plugin_updated": 1},
               "errors": 1})
    );
    assert_eq!(
        without_written_at(rows[0].clone()),
        json!({"day": "2026-01-08", "versions": [], "sessions": 1, "prompts": 0, "searched": 0,
               "prompt_skips": {"notification": 1}, "injections": 0, "identifier_rule": 0, "echoes": 0,
               "used_signal": {"yes": 0, "judged": 0}, "hook_ms": {"median": 0, "p95": 0}, "index_size": 0,
               "pick_jobs": 0, "picks": 0, "pick_skips": {}, "picks_delivered": 0, "pick_drops": {},
               "picks_used": {"yes": 0, "judged": 0}, "pick_cost": 0.0, "pick_ms": 0, "pushes": {},
               "right_pushes": {"right": 0, "wrong": 0, "unjudged": 0}, "late_pushes": {"late": 0, "judged": 0},
               "extraction_runs": 0, "facts": {"new": 0, "replaced": 0, "reconciled": 0, "skipped": 0, "dropped": 0},
               "suggestions": 0, "strikes": 0, "failed_sessions": 0, "stops": 0, "extraction_cost": 0.0,
               "extraction_ms": 0, "redactions": 0, "own_tool_calls": {}, "update_check": {}, "errors": 0}),
        "a day before any update line has no version"
    );
    assert_eq!(rows[2]["versions"], json!(["0.9.0"]), "a day with no update line takes the last version before it");
    assert_eq!(rows[2]["hook_ms"], json!({"median": 25, "p95": 25}));
}

/// Spec test 13 (ticket 12): an `extract` line's applied reconciles reach the row, and the `stats` tool shows them in a
/// column of their own after the new and the replaced facts.
#[test]
fn an_extract_lines_reconciles_count_in_the_row_and_in_the_stats_text() {
    let h = Home::new("reconciled");
    h.log(&[at("2026-01-10T06:00:00Z", json!({"event": "extract", "session": "A", "from": 0, "to": 100, "proposed": 4,
        "new": 3, "replaced": 1, "reconciled": 2, "skipped": 0, "dropped": {}, "cost": 0.5}))]);
    let text = h.stats_text(json!({"from": "2026-01-10", "to": "2026-01-10"}));
    assert_eq!(h.row("2026-01-10")["facts"], json!({"new": 3, "replaced": 1, "reconciled": 2, "skipped": 0, "dropped": 0}));
    let lines: Vec<Vec<&str>> = text.lines().skip(1).map(|l| l.split(' ').collect()).collect();
    let (header, row) = (&lines[0], &lines[1]);
    assert_eq!(header.len(), 19, "{text}");
    let at = header.iter().position(|c| *c == "facts_new").unwrap();
    assert_eq!(header[at..at + 3], ["facts_new", "facts_replaced", "facts_reconciled"], "{text}");
    assert_eq!(row[at..at + 3], ["3", "1", "2"], "{text}");
}

/// Spec tests 4 and 5: the used signal reads the line's identifiers, else the memory file. No transcript, no turn or
/// no file leaves an injection unjudged.
#[test]
fn an_injection_is_used_when_its_turn_repeats_an_identifier_the_prompt_lacks() {
    let h = Home::new("used");
    h.put(
        ".openrecall/repos/app/m2.md",
        "---\ndescription: flaky test\n---\nThe flaky test lives in tests/flaky_test.py and needs a retry.\n",
    );
    let ids = |address: &str, list: Value| json!({"identifiers": {address: list}});
    h.log(&[
        recall("2026-01-10T05:00:00Z", "S1", 30, injected("fix ABC-12 exports", "app/m1", ids("app/m1", json!(["ABC-12", "src/export.py"])))),
        recall("2026-01-10T05:10:00Z", "S2", 30, injected("why does CI fail", "app/m2", json!({"prompt_hash": "0"}))),
        recall("2026-01-10T05:20:00Z", "S3", 30, injected("what next", "app/m3", ids("app/m3", json!(["XYZ-99"])))),
        recall("2026-01-10T05:30:00Z", "S4", 30, injected("no transcript", "app/m1", ids("app/m1", json!(["ABC-12"])))),
        recall("2026-01-10T05:40:00Z", "S5", 30, injected("gone file", "app/gone", json!({}))),
        recall("2026-01-10T05:50:00Z", "S6", 30, with(injected("far", "app/m1", ids("app/m1", json!(["ABC-12"]))), json!({"prompt_hash": "0"}))),
        recall("2026-01-10T06:00:00Z", "S7", 30, injected("no reply", "app/m1", ids("app/m1", json!(["ABC-12"])))),
    ]);
    h.transcript("S7", &[user("2026-01-10T06:00:01.000Z", "no reply", json!({}))]);
    h.transcript("S1", &[
        user("2026-01-10T05:00:02.000Z", "fix ABC-12 exports", json!({})),
        said("2026-01-10T05:00:09.000Z", "ABC-12 again: looking at src/export.py now."),
    ]);
    h.transcript("S2", &[
        user("2026-01-10T05:10:03.000Z", "why does CI fail?", json!({})),
        tool("2026-01-10T05:10:09.000Z", "Read", json!({"file_path": "/w/app/tests/flaky_test.py"})),
    ]);
    h.transcript("S3", &[
        user("2026-01-10T05:20:01.000Z", "what next", json!({})),
        said("2026-01-10T05:20:09.000Z", "ABC-12 is done."),
    ]);
    h.transcript("S5", &[
        user("2026-01-10T05:40:01.000Z", "gone file", json!({})),
        said("2026-01-10T05:40:09.000Z", "app/gone"),
    ]);
    h.transcript("S6", &[
        user("2026-01-10T05:50:16.000Z", "far?", json!({})),
        said("2026-01-10T05:50:19.000Z", "ABC-12"),
    ]);
    h.job();
    assert_eq!(h.row("2026-01-10")["used_signal"], json!({"yes": 2, "judged": 3}));
}

/// Spec section 5.4: a delivered pick joins its prompt by `promptId` and its pick line by `prompt_id`.
#[test]
fn a_delivered_pick_is_used_when_its_turn_repeats_an_identifier() {
    let h = Home::new("picks");
    h.put(".openrecall/repos/app/m2.md", "---\ndescription: release\n---\nThe steps are in docs/release.md.\n");
    let pick = |iso: &str, id: &str, memory: &str, extra: Value| {
        let line = json!({"event": "pick", "session": "S1", "prompt_id": id, "memory": memory, "skip": null, "cost": 0.0, "ms": 10});
        at(iso, with(line, extra))
    };
    let deliver = |iso: &str, sid: &str, id: &str, memory: &str| {
        at(iso, json!({"event": "deliver", "session": sid, "prompt_id": id, "ms": 1, "memory": memory}))
    };
    h.log(&[
        pick("2026-01-10T05:00:00Z", "q1", "app/m1", json!({"identifiers": ["CORE-7"]})),
        deliver("2026-01-10T05:00:01Z", "S1", "q1", "app/m1"),
        pick("2026-01-10T05:10:00Z", "q2", "app/m2", json!({})),
        deliver("2026-01-10T05:10:01Z", "S1", "q2", "app/m2"),
        pick("2026-01-10T05:20:00Z", "q3", "app/m3", json!({"identifiers": ["ZZZ-10"]})),
        deliver("2026-01-10T05:20:01Z", "S1", "q3", "app/m3"),
        deliver("2026-01-10T05:30:01Z", "S1", "q4", "app/m1"),
        deliver("2026-01-10T05:40:01Z", "S9", "q5", "app/m1"),
    ]);
    h.transcript("S1", &[
        user("2026-01-10T04:59:59Z", "go", json!({"promptId": "q1"})),
        tool("2026-01-10T05:00:02Z", "Bash", json!({"command": "grep -rn CORE-7 ."})),
        user("2026-01-10T05:09:59Z", "go on", json!({"promptId": "q2"})),
        said("2026-01-10T05:10:02Z", "Following docs/release.md."),
        user("2026-01-10T05:19:59Z", "and then", json!({"promptId": "q3"})),
        said("2026-01-10T05:20:02Z", "Done."),
    ]);
    h.job();
    let row = h.row("2026-01-10");
    assert_eq!((&row["picks_delivered"], &row["picks_used"]), (&json!(5), &json!({"yes": 2, "judged": 3})));
}

/// Spec sections 5.5 to 5.8: the verdict and the late test of each push whose turn is found, the record from the
/// line, else from its file.
#[test]
fn a_push_is_right_wrong_or_unjudged_and_late_after_an_edit_on_its_branch() {
    let h = Home::new("pushes");
    h.put(".openrecall/repos/app/handoffs/retired/feat--y.2026-01-01.md", "---\nbranch: feat/old-y\naliases: ABC-12\n---\n");
    h.put(".openrecall/repos/app/handoffs/retired/feat--y.2026-01-05.md", "---\nrepo: app\nbranch: feat/y\naliases: ABC-12, ABC-13\n---\n");
    let pushed = |sid: &str, prompt: &str, iso: &str, extra: Value| {
        let line = json!({"event": "pushed", "session": sid, "at": ms(iso) + 10});
        [recall(iso, sid, 50, json!({"prompt_hash": fnv(prompt)})), with(line, extra)]
    };
    let on = |line: Value, branch: &str| with(line, json!({"gitBranch": branch}));
    let mut log = vec![];
    log.extend(pushed("P1", "start", "2026-01-10T06:00:00Z", json!({"how": "settle", "branch": "feat/x", "aliases": []})));
    log.extend(pushed("P2", "continue", "2026-01-10T06:10:00Z", json!({"how": "identifier", "address": "app/handoffs/feat--y"})));
    log.extend(pushed("P3", "fix it", "2026-01-10T06:20:00Z", json!({"how": "alias", "branch": "feat/z", "aliases": ["QQ-11"]})));
    log.extend(pushed("P4", "hello", "2026-01-10T06:30:00Z", json!({"how": "settle", "branch": "feat/w", "aliases": []})));
    log.extend(pushed("P5", "next", "2026-01-10T06:40:00Z", json!({"how": "sibling", "branch": "feat/s", "aliases": []})));
    log.extend(pushed("P6", "lost", "2026-01-10T06:50:00Z", json!({"how": "settle", "branch": "feat/x", "aliases": []})));
    log.push(at("2026-01-10T07:00:00Z", json!({"event": "pushed", "session": "P7", "how": "settle", "branch": "feat/v", "aliases": []})));
    log.extend(pushed("P8", "gone", "2026-01-10T07:10:00Z", json!({"how": "settle", "address": "app/handoffs/feat--gone"})));
    h.log(&log);
    h.transcript("P1", &[
        user("2026-01-10T06:00:01Z", "start", json!({"gitBranch": "main"})),
        on(said("2026-01-10T06:00:05Z", "ok"), "feat/x"),
        user("2026-01-10T06:05:00Z", "more", json!({"gitBranch": "feat/x"})),
        on(said("2026-01-10T06:05:05Z", "done"), "feat/x"),
    ]);
    h.transcript("P2", &[
        user("2026-01-10T06:01:00Z", "work on abc-12", json!({"gitBranch": "feat/y"})),
        on(tool("2026-01-10T06:01:05Z", "Edit", json!({"file_path": "/w/app/a.py"})), "feat/y"),
        user("2026-01-10T06:10:01Z", "continue", json!({"gitBranch": "feat/y"})),
        on(said("2026-01-10T06:10:05Z", "ok"), "feat/y"),
    ]);
    h.transcript("P3", &[
        user("2026-01-10T06:20:01Z", "fix it", json!({"gitBranch": "main"})),
        on(said("2026-01-10T06:20:05Z", "ok"), "main"),
        user("2026-01-10T06:25:00Z", "again", json!({"gitBranch": "main"})),
        on(said("2026-01-10T06:25:05Z", "ok"), "main"),
    ]);
    h.transcript("P4", &[user("2026-01-10T06:30:01Z", "hello", json!({"gitBranch": "main"})), said("2026-01-10T06:30:05Z", "hi")]);
    h.transcript("P5", &[
        user("2026-01-10T06:35:00Z", "commit it", json!({"gitBranch": "feat/s"})),
        on(tool("2026-01-10T06:35:05Z", "Bash", json!({"command": "git -C /w/lib commit -m x"})), "feat/s"),
        user("2026-01-10T06:40:01Z", "next", json!({"gitBranch": "feat/s"})),
        on(said("2026-01-10T06:40:05Z", "ok"), "feat/s"),
    ]);
    h.transcript("P7", &[
        user("2026-01-10T06:59:00Z", "one", json!({"gitBranch": "feat/v"})),
        on(said("2026-01-10T06:59:05Z", "ok"), "feat/v"),
        user("2026-01-10T07:00:00Z", "two", json!({"gitBranch": "feat/v"})),
        on(said("2026-01-10T07:00:05Z", "ok"), "feat/v"),
    ]);
    h.transcript("P8", &[user("2026-01-10T07:10:01Z", "gone", json!({"gitBranch": "main"})), said("2026-01-10T07:10:05Z", "ok")]);
    h.job();
    let row = h.row("2026-01-10");
    assert_eq!(row["right_pushes"], json!({"right": 2, "wrong": 1, "unjudged": 1}));
    assert_eq!(row["late_pushes"], json!({"late": 2, "judged": 5}));
}

/// Spec tests 1 and 4, against the harness's `daily` on the same made-up home: some of the home's lines lack the 0.10.0
/// fields, so their checks take the fallback and read the home's made-up memory and record files.
#[test]
fn the_shared_made_up_home_gives_the_expected_rows_and_a_line_without_the_new_fields_reads_the_made_up_files() {
    let root = std::env::temp_dir().join(format!("openrecall-stats-shared-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let status = Command::new("cp").arg("-R").arg(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/daily")).arg(&root).status().unwrap();
    assert!(status.success());
    let home = root.join("home");
    let status = Command::new(env!("CARGO_BIN_EXE_openrecall"))
        .args(["stats", "--job"])
        .env_clear()
        .env("HOME", &home)
        .env("OPENRECALL_HOME", home.join(".openrecall"))
        .env("TZ", "IST-5:30")
        .status()
        .unwrap();
    assert!(status.success());
    let got = fs::read_to_string(home.join(".openrecall/stats/daily.jsonl")).unwrap();
    assert_eq!(lines_without_written_at(&got), fs::read_to_string(root.join("expected.jsonl")).unwrap());
}

/// Each row's `"written_at":<ms>,`, the one field `expected.jsonl` leaves out, taken out.
fn lines_without_written_at(rows: &str) -> String {
    rows.lines()
        .map(|l| {
            let start = l.find(",\"written_at\":").unwrap();
            let end = start + 1 + l[start + 1..].find(',').unwrap();
            format!("{}{}\n", &l[..start], &l[end..])
        })
        .collect()
}

/// A day without `update` lines takes the version its last update line left, a repeat included.
#[test]
fn a_day_without_update_lines_takes_the_last_version_logged_before_it() {
    let h = Home::new("versions");
    let update = |iso: &str, step: &str, result: &str, from: &str, to: &str| {
        at(iso, json!({"event": "update", "step": step, "result": result, "from": from, "to": to, "reason": ""}))
    };
    h.log(&[
        update("2026-01-10T06:00:00Z", "binary", "current", "0.9.0", ""),
        update("2026-01-10T07:00:00Z", "binary", "updated", "0.9.0", "0.10.0"),
        update("2026-01-10T08:00:00Z", "binary", "current", "0.9.0", ""),
        recall("2026-01-11T06:00:00Z", "A", 5, json!({})),
    ]);
    h.job();
    assert_eq!(h.row("2026-01-10")["versions"], json!(["0.9.0", "0.10.0"]));
    assert_eq!(h.row("2026-01-11")["versions"], json!(["0.9.0"]));
}

/// Spec test 2: the job copies a final row's line as it is, fields it does not know, a `\r` and bytes that are not
/// UTF-8 included.
#[test]
fn a_final_row_survives_byte_for_byte() {
    let h = Home::new("final-bytes");
    h.log(&[recall("2026-01-09T06:00:00Z", "A", 5, json!({})), recall("2026-01-10T06:00:00Z", "A", 5, json!({}))]);
    let head = format!(r#"{{"day":"2026-01-09","written_at":{},"versions":[],"a_later_field":{{"x":[1,2]}},"#, ms("2026-01-20T00:00:00Z"));
    let kept = [head.as_bytes(), b"\"note\":\"\xff\xfe\",\"prompts\":99}\r\n"].concat();
    h.put(".openrecall/stats/daily.jsonl", &kept);
    h.job();
    let bytes = fs::read(h.path(".openrecall/stats/daily.jsonl")).unwrap();
    assert_eq!(&bytes[..kept.len()], kept);
    let next: Value = serde_json::from_slice(&bytes[kept.len()..]).unwrap();
    assert_eq!((&next["day"], &next["prompts"]), (&json!("2026-01-10"), &json!(1)));
}

/// Spec test 6: a row written during its own day is open, so the next run counts the day's later lines.
#[test]
fn a_row_written_during_its_day_changes_with_the_days_later_lines() {
    let h = Home::new("open-today");
    let start = now_ms() - 60_000;
    h.log(&[recall_at(start, "A", 5, json!({}))]);
    h.job();
    let first = h.rows().pop().unwrap();
    assert_eq!(first["prompts"], 1);
    h.log(&[recall_at(start, "A", 7, json!({}))]);
    h.again();
    h.job();
    let second = h.rows().pop().unwrap();
    assert_eq!((&second["day"], &second["prompts"]), (&first["day"], &json!(2)));
    assert!(second["written_at"].as_u64() > first["written_at"].as_u64());
}

/// Spec test 7: a push stays unjudged until its session's second real prompt, while its day's row is open.
#[test]
fn an_unjudged_push_becomes_right_after_the_sessions_second_real_prompt() {
    let h = Home::new("open-push");
    let start = now_ms() - 600_000;
    h.log(&[
        recall_at(start, "R", 50, json!({"prompt_hash": fnv("start")})),
        json!({"event": "pushed", "session": "R", "how": "settle", "branch": "feat/r", "aliases": [], "at": start + 10}),
    ]);
    let on = |line: Value| with(line, json!({"gitBranch": "feat/r"}));
    h.transcript("R", &[on(user("2026-01-10T06:00:01Z", "start", json!({}))), on(said("2026-01-10T06:00:05Z", "ok"))]);
    h.job();
    assert_eq!(h.rows().pop().unwrap()["right_pushes"], json!({"right": 0, "wrong": 0, "unjudged": 1}));
    h.transcript("R", &[on(user("2026-01-10T06:05:00Z", "more", json!({}))), on(said("2026-01-10T06:05:05Z", "done"))]);
    h.again();
    h.job();
    assert_eq!(h.rows().pop().unwrap()["right_pushes"], json!({"right": 1, "wrong": 0, "unjudged": 0}));
}

/// Spec test 8: nothing computes a final row again.
#[test]
fn a_final_row_stays_when_its_days_transcript_changes() {
    let h = Home::new("final-transcript");
    h.log(&[recall("2026-01-10T06:00:00Z", "A", 30, injected("go", "app/m1", json!({"identifiers": {"app/m1": ["KEY-42"]}})))]);
    h.transcript("A", &[user("2026-01-10T06:00:01Z", "go", json!({})), said("2026-01-10T06:00:05Z", "ok")]);
    h.job();
    let before = h.text();
    assert_eq!(h.row("2026-01-10")["used_signal"], json!({"yes": 0, "judged": 1}));
    h.transcript("A", &[said("2026-01-10T06:00:09Z", "KEY-42 it is")]);
    h.again();
    h.job();
    assert_eq!(h.text(), before);
}

/// Spec test 9, the job's half: a second job within 24 hours of `attempted_at` changes nothing.
#[test]
fn a_second_job_within_24_hours_changes_nothing() {
    let h = Home::new("daily");
    h.log(&[recall("2026-01-10T06:00:00Z", "A", 5, json!({}))]);
    h.job();
    let (rows, state) = (h.text(), fs::read_to_string(h.path(".openrecall/stats/state.json")).unwrap());
    h.log(&[recall("2026-01-12T06:00:00Z", "A", 5, json!({}))]);
    h.job();
    assert_eq!(h.text(), rows);
    assert_eq!(fs::read_to_string(h.path(".openrecall/stats/state.json")).unwrap(), state);
}

/// Spec test 10, the job's half: the job tries the lock once and exits while another process holds it.
#[test]
fn the_job_exits_at_once_while_the_lock_is_held() {
    let h = Home::new("locked");
    h.log(&[recall("2026-01-10T06:00:00Z", "A", 5, json!({}))]);
    h.put(".openrecall/stats/lock", "");
    let held = fs::File::create(h.path(".openrecall/stats/lock")).unwrap();
    held.try_lock().unwrap();
    let started = std::time::Instant::now();
    h.job();
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
    assert!(!h.path(".openrecall/stats/state.json").exists() && h.text().is_empty());
    drop(held);
    h.job();
    assert_eq!(h.rows().len(), 1);
}

/// Spec section 7.7: a failed run keeps the old rows and leaves an `error` line with `cmd` `stats`.
#[test]
fn a_failed_run_keeps_the_old_rows_and_logs_an_error() {
    let h = Home::new("failed");
    h.log(&[recall("2026-01-10T06:00:00Z", "A", 5, json!({}))]);
    fs::create_dir_all(h.path(".openrecall/stats/daily.jsonl")).unwrap();
    h.job();
    assert!(h.path(".openrecall/stats/daily.jsonl").is_dir());
    let log = fs::read_to_string(h.path(".openrecall/log/openrecall.jsonl")).unwrap();
    let error: Value = serde_json::from_str(log.lines().last().unwrap()).unwrap();
    assert_eq!((&error["event"], &error["cmd"]), (&json!("error"), &json!("stats")));
    let left: Vec<_> = fs::read_dir(h.path(".openrecall/stats")).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert!(!left.iter().any(|n| n.to_string_lossy().contains("tmp")), "{left:?}");
}

/// Spec section 7.7: a lock file the job cannot make is a failure, not a lock another process holds.
#[test]
fn a_lock_file_the_job_cannot_make_logs_an_error() {
    let h = Home::new("lock-failed");
    h.log(&[recall("2026-01-10T06:00:00Z", "A", 5, json!({}))]);
    fs::create_dir_all(h.path(".openrecall/stats/lock")).unwrap();
    h.job();
    let log = fs::read_to_string(h.path(".openrecall/log/openrecall.jsonl")).unwrap();
    let error: Value = serde_json::from_str(log.lines().last().unwrap()).unwrap();
    assert_eq!((&error["event"], &error["cmd"]), (&json!("error"), &json!("stats")));
    assert!(h.text().is_empty());
}

/// Spec section 7.2: SessionStart starts the job detached.
#[test]
fn session_start_starts_the_job() {
    let h = Home::new("session-start");
    h.log(&[recall("2026-01-10T06:00:00Z", "A", 5, json!({}))]);
    let mut child = Command::new(env!("CARGO_BIN_EXE_openrecall"))
        .arg("handoff")
        .env_clear()
        .env("HOME", &h.root)
        .env("OPENRECALL_HOME", h.path(".openrecall"))
        .env("TZ", TZ)
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(br#"{"session_id": "S", "source": "startup", "cwd": "/nowhere"}"#).unwrap();
    assert!(child.wait().unwrap().success());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while h.text().is_empty() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert_eq!(h.rows()[0]["day"], "2026-01-10");
}
