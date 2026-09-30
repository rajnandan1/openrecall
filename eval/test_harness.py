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
        harness.report()
        [path] = glob.glob(os.path.join(harness.EVAL, "runs", "*", "report.md"))
        with open(path) as fh:
            report = fh.read()
        for row in ("| tickets | 100% (1/1,", "| PR numbers | 100% (1/1,", "| commits | 0% (0/1,", "| paths | 0% (0/1,",
                    "| all | 50% (2/4, 15–85%) | 25% (1/4,", "| branch | 1 | 50% (2/4,",
                    "`relevant_memories` attachment: 1: headless.", "lower bound is 40% (2/5, 12–77%)",
                    "Largest of 1 `MEMORY.md` files: %d bytes, 2 lines." % len(INDEX)):
            self.assertIn(row, report)

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
