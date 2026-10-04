use crate::turn::{Found, TICKET, commits_in, norm_path, paths_in, prs_in};
use std::fs;
use std::path::{Path, PathBuf};

/// 800 tokens at 2.6 characters a token, the median over 727 text-only replies in Claude 5 transcripts here.
const BUDGET_CHARS: usize = 2080;
pub const RETIRE_SECS: u64 = 7 * 86400;

/// A task's handoff record (ticket 14). The file is the writer's only state.
#[derive(Default)]
pub struct Record {
    pub repo: String,
    pub branch: String,
    pub folder: String,
    pub aliases: Vec<String>,
    pub writers: Vec<String>,
    pub updated_at: String,
    /// When the last answer was written. `updated_at` also moves on a partial merge: it is the retirement clock.
    pub answered_at: String,
    pub goal: String,
    /// `## ` sections the writer does not own, written by a person; kept word for word.
    pub kept: String,
    pub tickets: Vec<String>,
    pub prs: Vec<String>,
    pub commits: Vec<String>,
    pub paths: Vec<String>,
    pub commands: Vec<String>,
    pub ask: String,
    pub answer: String,
}

pub fn dir(home: &Path, identity: &str) -> PathBuf {
    home.join("repos").join(identity).join("handoffs")
}

pub fn stem(branch: &str) -> String {
    branch.replace('/', "--")
}

impl Record {
    pub fn parse(text: &str) -> Record {
        let mut r = Record::default();
        let body = match text
            .strip_prefix("---\n")
            .and_then(|t| t.split_once("\n---\n"))
        {
            Some((front, body)) => {
                for (key, value) in front.lines().filter_map(|l| l.split_once(':')) {
                    let value = value.trim().to_string();
                    let list = || split(&value);
                    match key.trim() {
                        "repo" => r.repo = value,
                        "branch" => r.branch = value,
                        "folder" => r.folder = value,
                        "aliases" => r.aliases = list(),
                        "writers" => r.writers = list(),
                        "updated_at" => r.updated_at = value,
                        "answered_at" => r.answered_at = value,
                        _ => {}
                    }
                }
                body
            }
            None => text,
        };
        let mut heading = None;
        let mut content = String::new();
        for line in body.split_inclusive('\n') {
            match line.strip_prefix("## ") {
                Some(h) => {
                    r.section(heading, &content);
                    heading = Some(h.trim_end());
                    content.clear();
                }
                None => content.push_str(line),
            }
        }
        r.section(heading, &content);
        r
    }

    fn section(&mut self, heading: Option<&str>, content: &str) {
        let text = content.trim().to_string();
        match heading {
            None => {}
            Some("Goal") => self.goal = text,
            Some("Last ask") => self.ask = text,
            Some("Last answer") => self.answer = text,
            Some("Identifiers") => {
                for line in text.lines() {
                    if let Some(cmd) = line.strip_prefix("- ") {
                        self.commands.push(cmd.to_string());
                    } else if let Some((key, value)) = line.split_once(':') {
                        let items = split(value);
                        match key {
                            "tickets" => self.tickets = items,
                            "prs" => {
                                self.prs = items
                                    .iter()
                                    .map(|p| p.trim_start_matches('#').to_string())
                                    .collect()
                            }
                            "commits" => self.commits = items,
                            "paths" => self.paths = items,
                            _ => {}
                        }
                    }
                }
            }
            Some(h) => self
                .kept
                .push_str(&format!("## {h}\n{}\n", content.trim_end())),
        }
    }

    /// Folds one captured turn in. A partial merge is for a session whose context window has not seen this
    /// record yet: it adds aliases and identifiers behind the record's own and leaves goal, ask and answer alone.
    pub fn merge(
        &mut self,
        found: &Found,
        ask: &str,
        answer: &str,
        full: bool,
        session: &str,
        now: &str,
    ) {
        if self.answered_at.is_empty() && !self.answer.is_empty() {
            self.answered_at = self.updated_at.clone();
        }
        for a in &found.aliases {
            if !self.aliases.contains(a) {
                self.aliases.push(a.clone());
            }
        }
        if full {
            if self.sets_goal(ask) {
                self.goal = cut(ask, 300);
            }
            if !ask.is_empty() {
                self.ask = cut(ask, 400);
            }
            if !answer.is_empty() {
                self.answer = cut_answer(answer, 1600);
                self.answered_at = now.to_string();
            }
        }
        let fold = |new: &Vec<String>, old: &Vec<String>, cap| {
            if full {
                newest(&[new, old], cap)
            } else {
                newest(&[old, new], cap)
            }
        };
        self.tickets = newest(
            &[
                &self.aliases,
                &fold(&found.tickets, &self.tickets, usize::MAX),
            ],
            20,
        );
        self.prs = fold(&found.prs, &self.prs, 10);
        self.commits = shortest_forms(fold(&found.commits, &self.commits, usize::MAX), 6);
        let commands: Vec<String> = found
            .commands
            .iter()
            .map(|c| c.chars().take(120).collect())
            .collect();
        self.commands = fold(&commands, &self.commands, 8);
        self.paths = fold(&found.paths, &self.paths, 20);
        if !self.writers.iter().any(|w| w == session) {
            self.writers.push(session.to_string());
        }
        self.updated_at = now.to_string();
        self.fit();
    }

    /// The Goal is the first ask that names a ticket or has 40 or more characters. A Goal that is a question and
    /// names no ticket gives way to the next such ask: a side question came before the task (build step 7).
    fn sets_goal(&self, ask: &str) -> bool {
        let sets_a_task = ask.chars().count() >= 40 || TICKET.is_match(ask);
        let side_question = !TICKET.is_match(&self.goal) && self.goal.trim_end().ends_with('?');
        sets_a_task && (self.goal.is_empty() || side_question)
    }

    /// Ticket 13's cut ladder: the generous caps above hold until the body passes 800 tokens, then
    /// commands, paths and the answer shrink first, the goal and the ask last.
    fn fit(&mut self) {
        let ladder = [
            ("commands", 4),
            ("paths", 12),
            ("answer", 1000),
            ("commands", 2),
            ("paths", 8),
            ("answer", 600),
            ("goal", 150),
            ("commands", 0),
            ("paths", 6),
            ("ask", 200),
            ("answer", 400),
        ];
        for (field, cap) in ladder {
            if self.sections().chars().count() <= BUDGET_CHARS {
                return;
            }
            match field {
                "commands" => self.commands.truncate(cap),
                "paths" => self.paths.truncate(cap),
                "answer" => self.answer = cut_answer(&self.answer, cap),
                "goal" => self.goal = cut(&self.goal, cap),
                _ => self.ask = cut(&self.ask, cap),
            }
        }
    }

    /// What a push injects: the file without its frontmatter, at most 800 tokens.
    pub fn body(&self) -> String {
        let b = self.sections();
        if b.chars().count() <= BUDGET_CHARS {
            return b;
        }
        let h = take_chars(&b, BUDGET_CHARS - 6);
        let at = h.rfind("\n\n").or_else(|| h.rfind('\n')).unwrap_or(h.len());
        format!("{}\n[...]\n", &h[..at])
    }

    fn sections(&self) -> String {
        let mut b = String::new();
        if !self.goal.is_empty() {
            b += &format!("## Goal\n{}\n", demote(&self.goal));
        }
        b += &self.kept;
        let line = |name: &str, items: &[String], mark: &str| {
            if items.is_empty() {
                String::new()
            } else {
                format!(
                    "{name}: {}\n",
                    items
                        .iter()
                        .map(|i| format!("{mark}{i}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
        };
        let mut ids = line("tickets", &self.tickets, "") + &line("prs", &self.prs, "#");
        ids += &(line("commits", &self.commits, "") + &line("paths", &self.paths, ""));
        if !self.commands.is_empty() {
            ids += "commands:\n";
            ids += &self
                .commands
                .iter()
                .map(|c| format!("- {c}\n"))
                .collect::<String>();
        }
        if !ids.is_empty() {
            b += &format!("## Identifiers\n{ids}");
        }
        if !self.ask.is_empty() {
            b += &format!("## Last ask\n{}\n", demote(&self.ask));
        }
        if !self.answer.is_empty() {
            b += &format!("## Last answer\n{}\n", demote(&self.answer));
        }
        b
    }

    pub fn render(&self) -> String {
        format!(
            "---\nrepo: {}\nbranch: {}\nfolder: {}\naliases: {}\nwriters: {}\nupdated_at: {}\nanswered_at: {}\n---\n{}",
            self.repo,
            self.branch,
            self.folder,
            self.aliases.join(", "),
            self.writers.join(", "),
            self.updated_at,
            self.answered_at,
            self.body()
        )
    }

    pub fn stale(&self, now: u64) -> bool {
        parse_iso(&self.updated_at).is_none_or(|t| now.saturating_sub(t) > RETIRE_SECS)
    }

    /// The text names one of the record's paths (an `@` mention too), PR numbers or commits; a commit matches by
    /// prefix either way round, over 7 or more characters (issue 9).
    pub fn named_in(&self, text: &str, folder: &str, home: &str) -> bool {
        paths_in(text)
            .into_iter()
            .filter_map(|p| norm_path(p.strip_prefix('@').unwrap_or(p), folder, home))
            .any(|p| self.paths.contains(&p))
            || prs_in(text).iter().any(|(_, n)| self.prs.contains(n))
            || commits_in(text).any(|c| {
                self.commits
                    .iter()
                    .any(|k| k.len() >= 7 && (k.starts_with(c) || c.starts_with(k.as_str())))
            })
    }
}

/// The live records in a handoffs directory, with their files.
pub fn live(dir: &Path) -> Vec<(PathBuf, Record)> {
    let Ok(entries) = fs::read_dir(dir) else {
        return vec![];
    };
    entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "md"))
        .filter_map(|p| Some((p.clone(), Record::parse(&fs::read_to_string(&p).ok()?))))
        .collect()
}

/// Sets `retired_at` and moves the file to `retired/<stem>.<date>.md`, so a returning task never collides with it.
pub fn retire(path: &Path, now: u64) -> std::io::Result<PathBuf> {
    let text = fs::read_to_string(path)?;
    let stamped = text.replacen("\n---\n", &format!("\nretired_at: {}\n---\n", iso(now)), 1);
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    let dest = path
        .with_file_name("retired")
        .join(format!("{stem}.{}.md", &iso(now)[..10]));
    crate::write_atomic(&dest, &stamped)?;
    fs::remove_file(path)?;
    Ok(dest)
}

fn split(value: &str) -> Vec<String> {
    value
        .split(", ")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// Lists merged newest first: earlier lists win, each item once, at most `cap`.
fn newest(lists: &[&Vec<String>], cap: usize) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for item in lists.iter().flat_map(|l| l.iter()) {
        if !out.contains(item) {
            out.push(item.clone());
        }
    }
    out.truncate(cap);
    out
}

/// One form per commit, at its newest position: a hash that another one extends is kept in its shorter form.
fn shortest_forms(commits: Vec<String>, cap: usize) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for c in commits {
        match out
            .iter_mut()
            .find(|k| k.starts_with(&c) || c.starts_with(k.as_str()))
        {
            Some(k) if c.len() < k.len() => *k = c,
            Some(_) => {}
            None => out.push(c),
        }
    }
    out.truncate(cap);
    out
}

/// A `## ` line inside owned text would read as a new section on the next parse.
fn demote(text: &str) -> String {
    text.split_inclusive('\n')
        .map(|l| {
            if l.starts_with("## ") {
                format!("#{l}")
            } else {
                l.to_string()
            }
        })
        .collect()
}

fn take_chars(s: &str, n: usize) -> &str {
    s.char_indices().nth(n).map_or(s, |(i, _)| &s[..i])
}

/// The start of `text`, at most `cap` characters, ended after a line, a sentence or a word when one ends past halfway.
fn head(text: &str, cap: usize) -> &str {
    if text.chars().count() <= cap {
        return text;
    }
    let h = take_chars(text, cap);
    let past_half = |i: &usize| *i > h.len() / 2;
    let at = h
        .rfind('\n')
        .filter(past_half)
        .or_else(|| h.rfind(". ").filter(past_half).map(|i| i + 1))
        .or_else(|| h.rfind(' ').filter(past_half))
        .unwrap_or(h.len());
    h[..at].trim_end()
}

fn cut(text: &str, cap: usize) -> String {
    if text.chars().count() <= cap {
        text.to_string()
    } else {
        format!("{} [...]", head(text, cap - 6))
    }
}

/// Ticket 14: the head plus the last paragraph (at most 400 characters, less under a tight cap), joined by `[...]`.
fn cut_answer(text: &str, cap: usize) -> String {
    let text = text.trim();
    let n = text.chars().count();
    if n <= cap {
        return text.to_string();
    }
    let tail_cap = (cap * 2 / 5).min(400);
    let last = text
        .rsplit("\n\n")
        .find(|p| !p.trim().is_empty())
        .unwrap_or(text)
        .trim();
    let tail = if last.chars().count() <= tail_cap {
        last
    } else {
        let t = &text[text.char_indices().nth(n - tail_cap).map_or(0, |(i, _)| i)..];
        let half = t.len() / 2;
        let start = t
            .find(". ")
            .filter(|&i| i < half)
            .map(|i| i + 2)
            .or_else(|| t.find(' ').map(|i| i + 1))
            .unwrap_or(0);
        &t[start..]
    };
    format!(
        "{}\n[...]\n{tail}",
        head(text, cap - tail.chars().count() - 7)
    )
}

/// `updated_at` to the millisecond: an alias push takes the newest of several records sharing a ticket, and two
/// writes in one second are common when sessions are replayed (build step 3).
pub fn iso_ms(ms: u128) -> String {
    let s = iso((ms / 1000) as u64);
    format!("{}.{:03}Z", &s[..s.len() - 1], ms % 1000)
}

pub fn iso(secs: u64) -> String {
    let (y, m, d) = civil_from_days((secs / 86400) as i64);
    let s = secs % 86400;
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        s / 3600,
        s % 3600 / 60,
        s % 60
    )
}

pub fn parse_iso(s: &str) -> Option<u64> {
    let n = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let days = days_from_civil(n(0..4)?, n(5..7)?, n(8..10)?);
    u64::try_from(days * 86400 + n(11..13)? * 3600 + n(14..16)? * 60 + n(17..19)?).ok()
}

// Howard Hinnant's civil calendar algorithms, http://howardhinnant.github.io/date_algorithms.html
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    era * 146097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let (d, m) = (
        doy - (153 * mp + 2) / 5 + 1,
        if mp < 10 { mp + 3 } else { mp - 9 },
    );
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_round_trip() {
        assert_eq!(iso(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso(1_790_763_307), "2026-09-30T10:15:07Z");
        assert_eq!(iso_ms(1_790_763_307_042), "2026-09-30T10:15:07.042Z");
        assert_eq!(parse_iso("2026-09-30T10:15:07.042Z"), Some(1_790_763_307));
        assert_eq!(parse_iso("2026-09-30T10:15:07Z"), Some(1_790_763_307));
        assert_eq!(
            parse_iso("2024-02-29T00:00:00Z").map(iso).as_deref(),
            Some("2024-02-29T00:00:00Z")
        );
    }

    #[test]
    fn merge_render_parse() {
        let mut r = Record {
            repo: "github.com/o/r".into(),
            branch: "feat/x".into(),
            folder: "/w".into(),
            ..Default::default()
        };
        let found = Found {
            aliases: vec!["ABC-12".into()],
            tickets: vec!["ABC-99".into()],
            prs: vec!["345".into()],
            commits: vec!["abc1234def".into(), "abc1234".into()],
            paths: vec!["src/a.py".into()],
            commands: vec!["git push origin feat/x".into()],
        };
        let ask = "Build ABC-12 so exports stop failing on empty rows";
        r.merge(
            &found,
            ask,
            "## Summary\nDone. Next: review.",
            true,
            "s1",
            "2026-09-30T10:00:00Z",
        );
        let text = r.render();
        assert!(
            text.contains("## Goal\nBuild ABC-12")
                && text.contains("### Summary")
                && text.contains("prs: #345\n")
        );
        assert!(text.contains("commits: abc1234\n") && text.contains("- git push origin feat/x\n"));
        let back = Record::parse(&text);
        assert_eq!(back.answer, "### Summary\nDone. Next: review.");
        assert_eq!(
            (back.tickets.clone(), back.writers.clone()),
            (vec!["ABC-12".into(), "ABC-99".into()], vec!["s1".into()])
        );

        let hand = text.replace(
            "## Identifiers",
            "## Done\nShipped the parser.\n## Identifiers",
        );
        let mut r2 = Record::parse(&hand);
        let later = Found {
            paths: vec!["src/b.py".into()],
            ..Default::default()
        };
        r2.merge(
            &later,
            "another ask that is long enough to be a goal",
            "new answer",
            false,
            "s2",
            "2026-09-30T11:00:00Z",
        );
        assert_eq!(r2.goal, back.goal);
        assert_eq!(r2.ask, back.ask);
        assert_eq!(
            r2.paths,
            ["src/a.py", "src/b.py"],
            "a partial merge adds behind the record's own"
        );
        assert_eq!(r2.writers, ["s1", "s2"]);
        assert_eq!(
            (r2.updated_at.as_str(), r2.answered_at.as_str()),
            ("2026-09-30T11:00:00Z", "2026-09-30T10:00:00Z"),
            "a partial merge moves the retirement clock, not the answer's time"
        );
        assert!(r2.render().contains(
            "## Goal\nBuild ABC-12 so exports stop failing on empty rows\n## Done\nShipped the parser.\n## Identifiers"
        ));

        let mut old_record = Record::parse(&text.replace("answered_at: 2026-09-30T10:00:00Z\n", ""));
        assert!(old_record.answered_at.is_empty());
        old_record.merge(&later, "", "", false, "s2", "2026-09-30T11:00:00Z");
        assert_eq!(
            old_record.answered_at, "2026-09-30T10:00:00Z",
            "a record from before `answered_at` keeps its last write as the answer's time"
        );
    }

    #[test]
    fn the_goal_is_the_ask_that_sets_the_task() {
        let none = Found::default();
        let ask = |r: &mut Record, text: &str| r.merge(&none, text, "ok", true, "s", "2026-09-30T10:00:00Z");

        let mut r = Record::default();
        for text in ["go", "/implement ABC-12", "/pr-comments https://example.com/someone/app/pull/345"] {
            ask(&mut r, text);
        }
        assert_eq!(r.goal, "/implement ABC-12", "a short ask that names a ticket is a Goal");
        ask(&mut r, &"and a far longer follow-up about the review ".repeat(4));
        assert_eq!(r.goal, "/implement ABC-12", "a Goal that names a ticket stays");

        let mut r = Record::default();
        ask(&mut r, "Is the cleanup setting still on in the settings file?");
        let work = "Read src/export.py and src/batch.py and explain where the null check runs, without editing.";
        ask(&mut r, work);
        assert_eq!(r.goal, work, "a side question gives way to the next ask that sets a task");
        ask(&mut r, &format!("{work} Then say whether the export tests cover it?"));
        assert_eq!(r.goal, work, "a Goal that is not a question stays");
        r.merge(&none, &"x".repeat(400), "ok", false, "s2", "2026-09-30T11:00:00Z");
        assert_eq!(r.goal, work, "a partial merge never sets the Goal");

        let mut r = Record::default();
        ask(&mut r, "Why does ABC-12 fail on empty rows?");
        ask(&mut r, work);
        assert_eq!(r.goal, "Why does ABC-12 fail on empty rows?", "a question that names a ticket is the task");
    }

    #[test]
    fn cuts() {
        let long = format!("{}\n\nNext: open the PR.", "word ".repeat(300));
        let a = cut_answer(&long, 1000);
        assert!(a.chars().count() <= 1000 && a.ends_with("\n[...]\nNext: open the PR."));
        let paths = (0..20)
            .map(|i| format!("src/deep/module_{i}/file.rs"))
            .collect();
        let answer = cut_answer(
            &format!("{}\n\nNext: open the PR.", "word ".repeat(400)),
            1600,
        );
        let mut big = Record {
            goal: "g".repeat(300),
            answer,
            paths,
            ..Default::default()
        };
        big.commands = (0..8)
            .map(|i| {
                format!(
                    "cargo test --package p{i} -- --nocapture {}",
                    "x".repeat(60)
                )
            })
            .collect();
        big.fit();
        assert!(
            big.body().chars().count() <= BUDGET_CHARS
                && big.answer.ends_with("Next: open the PR.")
        );
        assert_eq!(
            (
                big.commands.len(),
                big.paths.len(),
                big.answer.chars().count() <= 1000
            ),
            (2, 12, true),
            "commands, then paths, then the answer shrink, in the ladder's order"
        );
        assert_eq!(cut(&"x".repeat(50), 40).chars().count(), 40);
        let huge = Record {
            answer: "y ".repeat(5000),
            ask: "z".into(),
            ..Default::default()
        };
        assert!(huge.body().chars().count() <= BUDGET_CHARS);
        assert_eq!(
            shortest_forms(
                vec!["abc1234def".into(), "fff0000".into(), "abc1234".into()],
                6
            ),
            ["abc1234", "fff0000"]
        );
    }
}
