#!/usr/bin/env python3
import glob
import json
import os
import random
import tempfile
import unittest

import harness


def user(ts, text, branch, cwd="/w/app", entry="cli"):
    return dict(type="user", timestamp=ts, cwd=cwd, gitBranch=branch, entrypoint=entry,
                message=dict(role="user", content=text))


def said(ts, text, branch, mid):
    return dict(type="assistant", timestamp=ts, gitBranch=branch,
                message=dict(id=mid, role="assistant", content=[dict(type="text", text=text)]))


def attached(ts, **attachment):
    return dict(type="attachment", timestamp=ts, attachment=attachment)


EARLIER = [
    user("2026-01-05T10:00:00Z", "Build ABC-12 so exports stop failing on empty rows", "feat-a"),
    said("2026-01-05T10:01:00Z", "Plan: edit src/export.py first.", "feat-a", "m1"),
    user("2026-01-05T10:10:00Z", "run the tests and open the PR please", "feat-a"),
    said("2026-01-05T10:11:00Z", "Opened PR #345 at commit abc1234def; touched /w/app/src/export.py. Next: ABC-12 review.",
         "feat-a", "m2"),
    user("2026-01-05T10:12:00Z", "<command-name>/effort</command-name>\n<command-args>max</command-args>", "feat-a"),
    user("2026-01-05T10:12:01Z", "<local-command-stdout>Set effort level to max</local-command-stdout>", "feat-a"),
]
NEW = [
    attached("2026-01-05T10:30:00Z", type="instructions",
             files=[dict(path="/m/MEMORY.md", type="AutoMem", content="- ABC-12 exports: see PR #345")]),
    user("2026-01-05T10:30:01Z", "<command-name>/implement</command-name>\n<command-args>continue the export work</command-args>",
         "feat-a"),
    said("2026-01-05T10:30:05Z", "On it.", "feat-a", "m3"),
    user("2026-01-05T10:40:00Z", "check src/export.py again", "feat-a"),
    said("2026-01-05T10:41:00Z", "Done.", "feat-a", "m4"),
    user("2026-01-05T10:50:00Z", "<task-notification><task-id>t1</task-id></task-notification>", "feat-a"),
    said("2026-01-05T10:51:00Z", "Noted.", "feat-a", "m5"),
]
HOP = [
    user("2026-01-06T09:00:00Z", "fix the flaky export test", "feat-a"),
    said("2026-01-06T09:01:00Z", "Fixed.", "feat-a", "m6"),
    user("2026-01-06T09:10:00Z", "ok", "feat-a"),
    said("2026-01-06T09:11:00Z", "Switched.", "feat-b", "m7"),
    user("2026-01-06T09:20:00Z", "now work on the imports for feat-b", "feat-b"),
    said("2026-01-06T09:21:00Z", "Working on src/imports.py.", "feat-b", "m8"),
]
LATER = [
    user("2026-01-07T09:00:00Z", "keep going on the export fix", "feat-a"),
    said("2026-01-07T09:01:00Z", "Going.", "feat-a", "m10"),
    user("2026-01-07T09:10:00Z", "add a test for src/export.py", "feat-a"),
    said("2026-01-07T09:11:00Z", "Added.", "feat-a", "m11"),
]
HEADLESS = [
    user("2026-01-05T11:00:00Z", "Use the judge skill with the scope battery", "", "/private/tmp/claude-1/x/scratchpad",
         "sdk-cli"),
    attached("2026-01-05T11:00:01Z", type="relevant_memories", memories=[]),
    said("2026-01-05T11:00:02Z", "Verdict: fine.", "", "m9"),
]
ELIGIBLE = {("earlier", "2026-01-05T10:00:00Z"), ("earlier", "2026-01-05T10:10:00Z"), ("new", "2026-01-05T10:30:01Z"),
            ("new", "2026-01-05T10:40:00Z"), ("hop", "2026-01-06T09:00:00Z"), ("hop", "2026-01-06T09:20:00Z"),
            ("later", "2026-01-07T09:00:00Z"), ("later", "2026-01-07T09:10:00Z")}
INDEX = "- ABC-12 exports\n- see PR #345\n"


class HarnessTest(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.mkdtemp()
        harness.PROJECTS, harness.EVAL, harness.CASES = os.path.join(tmp, "projects"), os.path.join(tmp, "eval"), 4
        for sid, lines in (("earlier", EARLIER), ("new", NEW), ("hop", HOP), ("later", LATER), ("headless", HEADLESS)):
            os.makedirs(os.path.join(harness.PROJECTS, "-w-app"), exist_ok=True)
            with open(os.path.join(harness.PROJECTS, "-w-app", sid + ".jsonl"), "w") as fh:
                fh.writelines(json.dumps(o) + "\n" for o in lines)
        os.makedirs(os.path.join(harness.PROJECTS, "-w-app", "memory"))
        with open(os.path.join(harness.PROJECTS, "-w-app", "memory", "MEMORY.md"), "w") as fh:
            fh.write(INDEX)
        os.makedirs(harness.EVAL)
        with open(os.path.join(harness.EVAL, "appendix-a.jsonl"), "w") as fh:
            fh.write(json.dumps(dict(id="A00", session="headless", at="2026-01-05T11:00:00Z")) + "\n")

    def report(self, binary):
        harness.report(binary=binary)
        [path] = glob.glob(os.path.join(harness.EVAL, "runs", "*", "report.md"))
        with open(path) as fh:
            text = fh.read()
        os.remove(path)
        return text

    def test_freeze_then_report(self):
        harness.freeze()
        cases = harness.read(os.path.join(harness.EVAL, "cases.jsonl"))
        self.assertEqual([c["id"] for c in cases], ["A00", "D001", "D002", "D003"])
        self.assertEqual(cases[0]["prompt"], "Use the judge skill with the scope battery")
        self.assertTrue({(c["session"], c["at"]) for c in cases[1:]} <= ELIGIBLE)
        pairs = harness.read(os.path.join(harness.EVAL, "pairs.jsonl"))
        self.assertEqual([(p["earlier"], p["new"], p["rule"]) for p in pairs],
                         [("earlier", "new", "branch"), ("hop", "later", "branch")])
        self.assertEqual(pairs[0]["final_state"], dict(tickets=["ABC-12"], prs=["345"], commits=["abc1234def"],
                                                       paths=["src/export.py"]))
        harness.freeze()
        self.assertEqual(harness.read(os.path.join(harness.EVAL, "cases.jsonl")), cases)
        self.assertEqual(harness.read(os.path.join(harness.EVAL, "pairs.jsonl")), pairs)
        report = self.report("/nonexistent/openrecall")
        for row in ("| tickets | not replayed | 100% (1/1,", "| PR numbers | not replayed | 100% (1/1,",
                    "| commits | not replayed | 0% (0/1,", "| paths | not replayed | 0% (0/1,",
                    "| all | not replayed | 50% (2/4, 15–85%) | not replayed | 25% (1/4,", "| branch | 1 | not replayed | 50% (2/4,",
                    "`relevant_memories` attachment: 1: headless.", "lower bound is 40% (2/5, 12–77%)",
                    "Largest of 1 `MEMORY.md` files: %d bytes, 2 lines." % len(INDEX)):
            self.assertIn(row, report)

    @unittest.skipUnless(os.path.exists(harness.BINARY), "needs cargo build --release")
    def test_replay_through_the_binary(self):
        harness.freeze()
        report = self.report(harness.BINARY)
        for row in ("| all | 100% (4/4, 51–100%) | 50% (2/4, 15–85%) | 100% (4/4,", "| branch | 1 | 100% (4/4,",
                    "read when the new session started (the writer alone, before the push rule): 100% (4/4,",
                    "first push at prompt 1: 0, 2: 1, 3: 0.", "3 pushes in 3 sessions of 4", "Right, by ticket 13's test",
                    "the session shares an alias with the record, or does 2 or more real turns on its branch): 67% (2/3,",
                    "By rule: settle 3.", "At real prompt 1: 0, 2: 3, 3: 0, later: 0.", "Binary errors in the replay: none."):
            self.assertIn(row, report)

    def test_level_two_metrics(self):
        cases = [dict(id="C1", session="new", at="2026-01-05T10:40:00Z", prompt="check src/export.py again", repo="github.com/x/app"),
                 dict(id="C2", session="new", at="2026-01-05T10:30:01Z", prompt="/implement continue the export work", repo=None)]
        mem = os.path.join(harness.EVAL, "m.md")
        with open(mem, "w") as fh:
            fh.write("---\nname: m\ndescription: Exports fail on empty rows\nmetadata:\n  type: project\n---\n\n"
                     "See src/export.py and ABC-12 at abc1234def. %s\n" % ("x " * 120))
        cand = lambda a, s: dict(address=a, score=s, text_hash="h", rot=None)
        results = [dict(case="C1", ms=1.0, terms=3, blind=False, skipped=None, errors=[], dropped={},
                        candidates=[cand("builtin/-w-app/m", 6.0), cand("builtin/-w-app/n", 5.0), cand("builtin/-w-app/o", 1.0)],
                        injected=["builtin/-w-app/m", "builtin/-w-app/n"], memories={"builtin/-w-app/m": mem}),
                   dict(case="C2", ms=2.0, terms=2, blind=False, skipped=None, errors=[], dropped={"rot": 1},
                        candidates=[cand("builtin/-w-app/n", 5.5)], injected=["builtin/-w-app/n"], memories={})]
        labels = harness.label_candidates(cases, results, {}, lambda c, todo, memories, earlier: [
            dict(case=c["id"], address=x["address"], text_hash=x["text_hash"], judge="test", at="", note={},
                 label="useful" if x["address"].endswith("/m") else "noise") for x in todo])
        self.assertEqual(labels[("C1", "builtin/-w-app/m", "h")], "useful")
        self.assertEqual(len(harness.read(os.path.join(harness.EVAL, "labels.jsonl"))), 4)
        self.assertEqual(harness.label_candidates(cases, results, {}, None), labels)
        m = harness.gate_metrics(cases, results, labels)
        self.assertEqual((m["injections"], m["useful"], m["misses"], m["false"], m["cases"]), (3, 1, 0, 1, 2))
        self.assertEqual(dict(m["kinds"]), {"builtin project": 1, "builtin ?": 2})
        m = harness.gate_metrics(cases, results, labels, gate=5.5)
        self.assertEqual((m["injections"], m["useful"], m["misses"], m["false"]), (2, 1, 0, 1))
        rows, (pick, best), met = harness.sweep(cases, results, labels)
        self.assertFalse(met)
        self.assertEqual(pick, 6.0)
        self.assertEqual((best["injections"], best["useful"]), (1, 1))
        self.assertEqual(harness.line_idents(mem), ["src/export.py", "ABC-12", "abc1234def"])
        self.assertEqual(harness.rot_of(mem, "/nonexistent"), "all")
        self.assertEqual(harness.slug("/Users/x/Code/app.v2"), "-Users-x-Code-app-v2")
        sessions, _ = harness.load()
        by_sid = {s["sid"]: s for s in sessions}
        self.assertTrue(harness.used_signal(cases[0], by_sid["new"], mem) is False)
        lines, pick = harness.level2_lines(cases, results, labels, by_sid, 5.5)
        text = "\n".join(lines)
        for row in ("Searched: 2.", "Candidates: 4 over 2 cases", "Labels: useful 1, partly 0, noise 3, unlabeled 0.",
                    "At the binary's gate (5.5 and above), injections 3 on 2 cases: precision 33% (1/3, 6–79%), misses 0, "
                    "false injections 33% (1/3, 6–79%)", "| builtin project | 1 | 1 | 100% (1/1, 21–100%) |",
                    "| 6.0 (pick) | 1 | 1 | 100% (1/1, 21–100%) | 0 | 0 |", "dropped in this replay: 1",
                    "p50 2.0 ms, p95 2.0 ms"):
            self.assertIn(row, text)

    @unittest.skipUnless(os.path.exists(harness.BINARY), "needs cargo build --release")
    def test_replay_cases_through_the_binary(self):
        tmp = tempfile.mkdtemp(dir="/var/tmp")
        os.makedirs(os.path.join(tmp, ".git"))
        with open(os.path.join(tmp, ".git", "config"), "w") as fh:
            fh.write('[remote "origin"]\n\turl = git@github.com:someone/app.git\n')
        os.makedirs(os.path.join(tmp, "src"))
        open(os.path.join(tmp, "src", "export.py"), "w").close()
        mem = os.path.join(harness.PROJECTS, harness.slug(tmp), "memory")
        os.makedirs(mem)
        with open(os.path.join(mem, "export-empty-rows.md"), "w") as fh:
            fh.write("---\nname: export-empty-rows\ndescription: Exports fail on empty rows until the null check moves\n"
                     "metadata:\n  type: project\n---\n\nThe export job wrote empty rows because the null check in "
                     "src/export.py ran late; ABC-12 tracks it.\n")
        bank = ["deploy", "standup", "linter", "wildcard", "review", "staging", "billing", "release", "titles", "browser",
                "retries", "runner", "branch", "slug", "rota", "monday", "cache", "queue", "worker", "metric", "alert"]
        for i in range(40):
            text = "The %s %s runs the %s before the %s step." % tuple(bank[(i * 7 + k * 5) % len(bank)] for k in range(4))
            with open(os.path.join(mem, "filler-%d.md" % i), "w") as fh:
                fh.write("---\nname: filler-%d\ndescription: %s\nmetadata:\n  type: project\n---\n\n%s\n" % (i, text, text))
        later = dict(id="L1", session="new", at="2030-01-01T00:00:00Z", prompt="why do exports fail on empty rows in ABC-12",
                     repo="github.com/someone/app", branch="feat-a", folder=tmp)
        before = dict(later, id="L2", at="2000-01-01T00:00:00Z")
        gone = dict(later, id="L3", folder="/nonexistent/app")
        short = dict(later, id="L4", prompt="/implement")
        results = harness.replay_cases(harness.BINARY, [later, before, gone, short], harness.mains_of([later, before, gone, short]))
        self.assertEqual(results[0]["candidates"][0]["address"], "builtin/%s/export-empty-rows" % harness.slug(tmp))
        self.assertEqual(results[0]["injected"], ["builtin/%s/export-empty-rows" % harness.slug(tmp)])
        self.assertEqual(results[0]["memories"]["builtin/%s/export-empty-rows" % harness.slug(tmp)],
                         os.path.join(mem, "export-empty-rows.md"))
        self.assertIsNone(results[0]["candidates"][0]["rot"])
        self.assertEqual(results[1]["candidates"], [], "a memory born after the prompt is not there")
        self.assertEqual(results[2]["candidates"][0]["address"], results[0]["candidates"][0]["address"],
                         "a gone folder still finds the repo's memory directory through another case")
        self.assertEqual(results[3]["skipped"], "empty-args")
        self.assertFalse(any(r["errors"] for r in results))

    def test_rules(self):
        lo, hi = harness.wilson(67, 100)
        self.assertEqual((round(lo, 2), round(hi, 2)), (0.57, 0.75))
        found = harness.mine("PR #345 at abc1234 in /w/app/src/export.py, line 346", "/w/app")
        self.assertTrue(harness.present("commits", "abc1234def", found))
        self.assertTrue(harness.present("paths", "src/export.py", found))
        self.assertFalse(harness.present("prs", "346", found))
        for url in ("git@github.com:someone/app.git", "https://github.com/someone/app", "ssh://git@github.com:22/someone/app/"):
            self.assertEqual(harness.normalize(url), "github.com/someone/app")
        pool = [("big", i) for i in range(8)] + [("small", i) for i in range(2)]
        self.assertEqual(sorted(r for r, _ in harness.draw(pool, 5, random.Random(1))), ["big"] * 4 + ["small"])


if __name__ == "__main__":
    unittest.main()
