#!/usr/bin/env python3
import glob
import http.server
import json
import os
import random
import re
import tempfile
import threading
import unittest
from datetime import datetime, timezone

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

    def report(self, binary, **kw):
        harness.report(binary=binary, **kw)
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
                    "By route: settle 3.", "Right by route: settle 2 right, 1 wrong, 0 unjudged.",
                    "Each of the 0 sibling pushes shares an alias with its record",
                    "Handoff pairs whose two sessions have different repo identities: 0, 0 of them measured.",
                    "Late pushes, after a turn on the push's branch edited a file or ran `git commit`: 0% (0/3,",
                    "Live pushes by route, in the recall log: none.",
                    "At real prompt 1: 0, 2: 3, 3: 0, later: 0.", "Binary errors in the replay: none."):
            self.assertIn(row, report)
        self.assertNotIn("picks on", report)
        home = os.path.dirname(harness.EVAL)
        with open(os.path.join(home, "extract.toml"), "w") as fh:
            fh.write('base_url = "http://127.0.0.1:9"\nmodel = "made-up"\n')
        with open(os.path.join(home, "api-key"), "w") as fh:
            fh.write("made-up-key")
        report = self.report(harness.BINARY, picks=True)
        for row in ("process start included, with picks on: each prompt starts "
                    "the pick job, against a made-up provider that cannot answer: p50",
                    "Picks with a memory: 0; dropped: none; injected: 0.", "Errors: 4 (4 × curl exit 7).",
                    "Cost of the pick lines: $0.00.",
                    "searched cases, process start included, with picks on: p50"):
            self.assertIn(row, report)

    def test_level_two_metrics(self):
        cases = [dict(id="C1", session="new", at="2026-01-05T10:40:00Z", prompt="check src/export.py again", repo="github.com/x/app"),
                 dict(id="C2", session="new", at="2026-01-05T10:30:01Z", prompt="/implement continue the export work", repo=None)]
        mem = os.path.join(harness.EVAL, "m.md")
        with open(mem, "w") as fh:
            fh.write("---\nname: m\ndescription: Exports fail on empty rows\nmetadata:\n  type: project\n---\n\n"
                     "See src/export.py and ABC-12 at abc1234def. %s\n" % ("x " * 120))
        cand = lambda a, s: dict(address=a, score=s, text_hash="h", rot=None)
        results = [dict(case="C1", ms=1.0, terms=3, index_size=12, blind=False, skipped=None, errors=[], dropped={},
                        candidates=[cand("builtin/-w-app/m", 6.0), cand("builtin/-w-app/n", 5.0), cand("builtin/-w-app/o", 1.0)],
                        injected=["builtin/-w-app/m", "builtin/-w-app/n"], memories={"builtin/-w-app/m": mem}),
                   dict(case="C2", ms=2.0, terms=2, index_size=9, blind=False, skipped=None, errors=[], dropped={"rot": 1},
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
        m = harness.gate_metrics(cases, results, labels, gate=5.5, min_rows=10)
        self.assertEqual((m["injections"], m["useful"], m["misses"], m["false"]), (1, 1, 0, 0), "C2's 9 rows keep the gate closed")
        rows, (best_threshold, best), met = harness.sweep(cases, results, labels, 10)
        self.assertFalse(met)
        self.assertEqual(best_threshold, 6.0)
        self.assertEqual((best["injections"], best["useful"]), (1, 1))
        self.assertEqual(dict(rows)[0.0]["injections"], 3, "only C1's 3 candidates")
        threshold, min_rows = harness.binary_gate()
        self.assertTrue(threshold > 0 and min_rows > 0)
        self.assertEqual(harness.line_idents(mem), ["src/export.py", "ABC-12", "abc1234def"])
        self.assertEqual(harness.rot_of(mem, "/nonexistent"), "all")
        self.assertEqual(harness.slug("/Users/x/Code/app.v2"), "-Users-x-Code-app-v2")
        sessions, _ = harness.load()
        by_sid = {s["sid"]: s for s in sessions}
        self.assertTrue(harness.used_signal(cases[0], by_sid["new"], mem) is False)
        lines, best_threshold = harness.level2_lines(cases, results, labels, by_sid, (5.5, 10))
        text = "\n".join(lines)
        for row in ("Searched: 2.", "Candidates: 4 over 2 cases", "Labels: useful 1, partly 0, noise 3, unlabeled 0.",
                    "At the binary's gate (5.50 and above, in an index of 10 rows or more), injections 3 on 2 cases: "
                    "precision 33% (1/3, 6–79%), misses 0, false injections 33% (1/3, 6–79%)",
                    "| builtin project | 1 | 1 | 100% (1/1, 21–100%) |", "injected in an index of 10 rows or more",
                    "| 6.00 (best threshold) | 1 | 1 | 100% (1/1, 21–100%) | 0 | 0 |", "| 0.00 | 3 | 1 | 33% (1/3,",
                    "dropped in this replay: 1", "p50 2.0 ms, p95 2.0 ms"):
            self.assertIn(row, text)

    def test_live_expansions(self):
        self.assertEqual(harness.fnv("a"), "af63dc4c8601ec8c")
        turn = user("2026-01-08T09:00:00Z", "why did the export fail on ABC-12", "feat-a")
        expand = said("2026-01-08T09:01:00Z", "Reading it.", "feat-a", "m12")
        expand["message"]["content"].append(dict(type="tool_use", id="t1", name=harness.MCP_RECALL,
                                                 input=dict(query="github.com/x/app/export-null-rows")))
        with open(os.path.join(harness.PROJECTS, "-w-app", "live.jsonl"), "w") as fh:
            fh.writelines(json.dumps(o) + "\n" for o in (turn, expand))
        log = os.path.join(os.path.dirname(harness.EVAL), "log")
        os.makedirs(log)
        with open(os.path.join(log, "openrecall.jsonl"), "w") as fh:
            fh.writelines(json.dumps(e) + "\n" for e in (
                dict(event="recall", session="live", prompt_hash=harness.fnv("why did the export fail on ABC-12"),
                     injected=["github.com/x/app/export-null-rows"]),
                dict(event="recall", session="new", prompt_hash=harness.fnv("check src/export.py again"), injected=["global/x"]),
                dict(event="recall", session="new", prompt_hash="0000000000000000", injected=["global/y"]),
                dict(event="recall", session="gone", prompt_hash="0", injected=["global/z"]),
                dict(event="recall", session="live", prompt_hash="0", injected=[])))
        sessions, _ = harness.load()
        self.assertEqual(harness.live_expansions({s["sid"]: s for s in sessions}), (2, 1))

    def test_turn_of(self):
        ms = lambda ts: int(harness.ts_of(ts).timestamp() * 1000)
        turn = lambda ts, ask: dict(start=harness.ts_of(ts), ask=ask)
        turns = [turn("2026-01-08T09:00:01Z", "/mattpocock-skills:wayfinder plan it"), turn("2026-01-08T09:10:00Z", "yes"),
                 turn("2026-01-08T09:40:00Z", "yes")]
        event = lambda ask, ts: dict(prompt_hash=harness.fnv(ask), started_at=ms(ts))
        self.assertIs(harness.turn_of(event("/wayfinder plan it", "2026-01-08T09:00:00Z"), turns), turns[0])
        self.assertIsNone(harness.turn_of(event("/wayfinder plan it", "2026-01-08T08:00:00Z"), turns))
        self.assertIs(harness.turn_of(event("yes", "2026-01-08T09:40:00Z"), turns), turns[2])
        self.assertIs(harness.turn_of(dict(prompt_hash=harness.fnv("yes")), turns), turns[1])

    def test_live_pr_urls(self):
        start = int(datetime(2026, 1, 8, 8, tzinfo=timezone.utc).timestamp() * 1000)
        urls = ["https://github.com/acme/web-app/pull/344", "https://github.com/acme/web-app/pull/345",
                "https://github.com/acme/web-app/pull/12", "https://github.com/acme/web-app/issues/346",
                "https://github.com/acme/web-app/pull/347"]
        lines = []
        for i, url in enumerate(urls):
            lines += [user("2026-01-08T09:0%d:00Z" % i, "<command-name>/review</command-name>\n<command-args>%s</command-args>" % url,
                           "feat-a"), said("2026-01-08T09:0%d:30Z" % i, "Reviewing.", "feat-a", "r%d" % i)]
        with open(os.path.join(harness.PROJECTS, "-w-app", "review.jsonl"), "w") as fh:
            fh.writelines(json.dumps(o) + "\n" for o in lines)
        mem = os.path.join(harness.PROJECTS, "-w-app", "memory")
        for name in ("a", "b"):
            with open(os.path.join(mem, name + ".md"), "w") as fh:
                fh.write("---\nname: %s\ndescription: PR #345 notes\n---\n\nPR #345 changes the export.\n" % name)
        old = datetime(2026, 1, 1, tzinfo=timezone.utc).timestamp()
        os.utime(os.path.join(mem, "a.md"), (old, old))
        cand = lambda name: dict(address="builtin/-w-app/" + name, score=2.0, text_hash="h" + name)
        recall = lambda i, at, injected: dict(event="recall", session="review", at=at, prompt_hash=harness.fnv("/review " + urls[i]),
                                              candidates=[cand("a"), cand("b"), cand("c")], injected=injected)
        before = [dict(event="update", step="binary", result="current", **{"from": "0.3.0"}, to="", reason="", at=start - 3),
                  recall(0, start - 2, ["builtin/-w-app/a"]),
                  dict(event="update", step="plugin", result="updated", **{"from": "0.3.0"}, to="0.4.0", reason="", at=start - 1)]
        after = [dict(event="update", step="binary", result="updated", **{"from": "0.3.0"}, to="0.4.0", reason="", at=start),
                 recall(1, start + 1, ["github.com/acme/web-app/handoffs/feat-a", "builtin/-w-app/a", "builtin/-w-app/b"]),
                 recall(2, start + 2, ["builtin/-w-app/a"]), recall(3, start + 3, ["builtin/-w-app/a"]), recall(4, start + 4, []),
                 dict(recall(1, start + 5, ["builtin/-w-app/a"]), session="gone")]
        log = os.path.join(os.path.dirname(harness.EVAL), "log")
        os.makedirs(log)
        sessions, _ = harness.load()
        by_sid = {s["sid"]: s for s in sessions}
        asked = []

        def label(c, todo, memories, earlier):
            asked.append([x["address"] for x in todo])
            return [dict(case=c["id"], address=x["address"], text_hash=x["text_hash"], judge="test", at="", note={},
                         label="useful" if x["address"].endswith("/a") else "noise") for x in todo if x["address"] in memories]

        def write(events):
            with open(os.path.join(log, "openrecall.jsonl"), "w") as fh:
                fh.writelines(json.dumps(e) + "\n" for e in events)

        write(before)
        self.assertEqual(harness.live_pr_urls(by_sid, label), "- Live PR URL prompts (issue 13): binary 0.4.0 is not in "
                         "the recall log yet, so nothing is counted.", "a plugin step to 0.4.0 is not the binary")
        write(before + after)
        line = harness.live_pr_urls(by_sid, label)
        for part in ("since binary 0.4.0 at 2026-01-08 08:00 UTC: 2 prompts, 1 with an injection, 2 injections, precision 50% (1/2,",
                     "Labels: useful 1, partly 0, noise 1, unlabeled 0. Injected memories whose file changed after the prompt: 1."):
            self.assertIn(part, line)
        self.assertEqual(asked, [["builtin/-w-app/a", "builtin/-w-app/b"]], "the handoff record and the candidate c are no injection")
        self.assertEqual(harness.live_pr_urls(by_sid, label), line)
        self.assertEqual(len(asked), 1, "each injection is labeled once")

    def test_extraction_lines(self):
        home = os.path.dirname(harness.EVAL)
        repo = os.path.join(home, "repos", "github.com", "x", "app")
        os.makedirs(os.path.join(repo, "replaced"))
        os.makedirs(os.path.join(repo, "handoffs"))
        for rel, text in (("a.md", "---\ndescription: A\n---\n\nnew fact a"), ("b.md", "---\ndescription: B\n---\n\nmerged b"),
                          ("c.md", "---\ndescription: C\n---\n\nown c"), ("replaced/b.2026-01-02T000000Z.md", "old b"),
                          ("handoffs/feat.md", "record"),
                          ("claude-md-suggestions.md", "- Run cargo fmt. (S1, 2026-01-01)\n- Use uv. (S2, 2026-01-03)\n")):
            with open(os.path.join(repo, rel), "w") as fh:
                fh.write(text)
        mem = os.path.join(harness.PROJECTS, "-w-app", "memory")
        with open(os.path.join(mem, "note.md"), "w") as fh:
            fh.write("builtin note")
        with open(os.path.join(mem, "far.md"), "w") as fh:
            fh.write("the same fact, stated in a file the search never ranked")
        os.makedirs(os.path.join(home, "extract"))
        with open(os.path.join(home, "extract", "S1.json"), "w") as fh:
            json.dump(dict(main="/w/app"), fh)
        os.makedirs(os.path.join(home, "log"))
        ms = 1767225600000
        with open(os.path.join(home, "log", "openrecall.jsonl"), "w") as fh:
            fh.writelines(json.dumps(e) + "\n" for e in (
                dict(event="extract", off="no extract.toml", at=ms),
                dict(event="extract", session="S1", **{"from": 0}, to=10, proposed=3, new=1, replaced=1, skipped=0,
                     dropped={"secret": 1}, normalized=1, suggestions=1, tokens_in=100, tokens_out=20, cost=0.05, at=ms + 1),
                dict(event="dedupe", session="S1", name="a", action="new", address="github.com/x/app/a", at=ms + 2,
                     candidates=[dict(address="github.com/x/app/c", score=9), dict(address="builtin/-w-app/note", score=5)]),
                dict(event="dedupe", session="S1", name="b", action="replace", address="github.com/x/app/b", at=ms + 3,
                     copy="b.2026-01-02T000000Z.md", candidates=[dict(address="github.com/x/app/b", score=9)]),
                dict(event="dedupe", session="S1", name="z", action="skip", address="github.com/x/app/c", at=ms + 4, candidates=[]),
                dict(event="extract", session="S2", strike="context length", strikes=3, failed=True, at=ms + 5),
                dict(event="extract", session="S3", stop="http 401", at=ms + 6)))
        asked = []

        def check(fact, candidates, old):
            asked.append((fact, candidates, old))
            return dict(repeat=[0.9, 0.1, 0.95][:len(candidates)], supersede=[0.8, 0.75, 0.0][:len(candidates)],
                        kept=0.2 if old is not None else None)

        since = datetime(2026, 1, 2, tzinfo=timezone.utc)
        text = "\n".join(harness.extraction_lines(since, check))
        far = "the same fact, stated in a file the search never ranked"
        self.assertEqual(asked, [("A\n\nnew fact a", ["C\n\nown c", "builtin note", far], None), ("B\n\nmerged b", [], "old b")],
                         "a memory that shares words with the fact is checked though the binary never ranked it")
        for part in ("1 runs over 1 sessions, last success 2026-01-01 00:00 UTC; strikes 1; sessions marked failed 1: S2;",
                     "last stop: http 401 at", "last start with extraction off: no extract.toml at",
                     "proposed 3, new 1, replaced 1, skipped 0, dropped 1 (secret 1); names normalized 1; tokens 100 in, 20 out; cost $0.05.",
                     "2 of 2 written facts checked, each against its logged candidates and up to 10 unranked memories",
                     "Repeats that got through: 2: github.com/x/app/a (github.com/x/app/c); github.com/x/app/a (builtin/-w-app/far).",
                     "Missed replaces of an own fact: 1: github.com/x/app/a (github.com/x/app/c).",
                     "out of date: 1: github.com/x/app/a (builtin/-w-app/note).", "Merges that lost old detail: 1 of 1: github.com/x/app/b.",
                     "Store size: github.com/x/app 3.",
                     "since the last report (2026-01-02): 1: `repos/github.com/x/app/claude-md-suggestions.md`: Use uv. (S2, 2026-01-03)."):
            self.assertIn(part, text)
        harness.extraction_lines(since, check)
        self.assertEqual(len(asked), 2, "each written fact is asked once")
        with open(os.path.join(harness.EVAL, "dedupe.jsonl")) as fh:
            rows = [json.loads(line) for line in fh]
        with open(os.path.join(harness.EVAL, "dedupe.jsonl"), "w") as fh:
            fh.writelines(json.dumps({k: v for k, v in r.items() if k != "unranked"}) + "\n" for r in rows)
        harness.extraction_lines(since, check)
        self.assertEqual(len(asked), 4, "a row checked against the logged candidates alone is asked again")

        fact = os.path.join(repo, "state.md")
        for expires, kept in (("2026-01-10", False), ("2026-01-05", True)):
            with open(fact, "w") as fh:
                fh.write("---\nname: s\nexpires: %s\n---\n\nPR #1 waits.\n" % expires)
            harness.unexpired(fact, datetime(2026, 1, 5, 12, tzinfo=timezone.utc))
            with open(fact) as fh:
                self.assertEqual("expires:" in fh.read(), kept)

    def checkout_with_memories(self):
        """A repo folder with `src/export.py`, and its built-in memory: one export fact and 40 fillers."""
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
        return tmp, mem

    @unittest.skipUnless(os.path.exists(harness.BINARY), "needs cargo build --release")
    def test_replay_cases_through_the_binary(self):
        tmp, mem = self.checkout_with_memories()
        later = dict(id="L1", session="new", at="2030-01-01T00:00:00Z", prompt="why do exports fail on empty rows in ABC-12",
                     repo="github.com/someone/app", branch="feat-a", folder=tmp)
        before = dict(later, id="L2", at="2000-01-01T00:00:00Z")
        gone = dict(later, id="L3", folder="/nonexistent/app")
        short = dict(later, id="L4", prompt="/implement")
        results = harness.replay_cases(harness.BINARY, [later, before, gone, short], harness.mains_of([later, before, gone, short]))
        self.assertEqual(results[0]["candidates"][0]["address"], "builtin/%s/export-empty-rows" % harness.slug(tmp))
        self.assertEqual(results[0]["injected"], ["builtin/%s/export-empty-rows" % harness.slug(tmp)])
        self.assertGreaterEqual(results[0]["index_size"], 41, "the fact and its 40 fillers")
        self.assertIn("bm25", results[0]["candidates"][0])
        self.assertEqual(results[0]["memories"]["builtin/%s/export-empty-rows" % harness.slug(tmp)],
                         os.path.join(mem, "export-empty-rows.md"))
        self.assertIsNone(results[0]["candidates"][0]["rot"])
        self.assertEqual(results[1]["candidates"], [], "a memory born after the prompt is not there")
        self.assertEqual(results[2]["candidates"][0]["address"], results[0]["candidates"][0]["address"],
                         "a gone folder still finds the repo's memory directory through another case")
        self.assertEqual(results[3]["skipped"], "empty-args")
        self.assertFalse(any(r["errors"] for r in results))

    @unittest.skipUnless(os.path.exists(harness.BINARY), "needs cargo build --release")
    def test_picks_through_the_binary(self):
        tmp, mem = self.checkout_with_memories()
        prompt = "why do exports fail on empty rows in ABC-12"
        with open(os.path.join(harness.PROJECTS, "-w-app", "picked.jsonl"), "w") as fh:
            fh.writelines(json.dumps(o) + "\n" for o in (
                user("2030-01-01T00:00:00Z", "plan the export fix for the empty rows", "feat-a"),
                said("2030-01-01T00:01:00Z", "The null check runs late.", "feat-a", "p1"),
                user("2030-01-01T00:10:00Z", prompt, "feat-a"),
                said("2030-01-01T00:11:00Z", "It reads the rows first.", "feat-a", "p2")))
        sent = []

        class Provider(http.server.BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def do_POST(self):
                text = json.loads(self.rfile.read(int(self.headers["Content-Length"])))["messages"][1]["content"]
                sent.append(text)
                name = "export-empty-rows" if text.endswith("again") else "filler-0"
                ids = re.findall(r"^- (m\d+): %s:" % name, text, re.M)
                reply = json.dumps(dict(choices=[dict(finish_reason="stop", message=dict(content=json.dumps(dict(ids=ids))))],
                                        usage=dict(prompt_tokens=900, completion_tokens=12, cost=0.01))).encode()
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(reply)))
                self.send_header("Connection", "close")
                self.end_headers()
                self.wfile.write(reply)

            def log_message(self, *args):
                pass

        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Provider)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        self.addCleanup(server.server_close)
        self.addCleanup(server.shutdown)
        self.addCleanup(setattr, harness, "PICK_WAIT", harness.PICK_WAIT)
        harness.PICK_WAIT = 30
        case = dict(id="P1", session="picked", at="2030-01-01T00:10:00Z", prompt=prompt, repo="github.com/someone/app",
                    branch="feat-a", folder=tmp)
        cases = [case, dict(case, id="P2", prompt=prompt + " again"), dict(case, id="P3", prompt="/implement")]
        toml = 'base_url = "http://127.0.0.1:%d"\npick = false\nmodel = "made-up"\n' % server.server_address[1]
        results = harness.replay_cases(harness.BINARY, cases, harness.mains_of(cases), (toml, "made-up-key"))
        address = "builtin/%s/" % harness.slug(tmp)
        with open(os.path.join(mem, "filler-0.md")) as fh:
            self.assertEqual(results[0]["picked"], [dict(address=address + "filler-0", text_hash=harness.fnv(fh.read()))])
        self.assertEqual((results[0]["pick"]["memory"], results[0]["pick"]["cost"]), (address + "filler-0", 0.01))
        self.assertEqual(results[0]["injected"], [address + "export-empty-rows"])
        self.assertEqual(results[1]["picked"], [], "recall injected the picked memory first")
        self.assertEqual(results[1]["pick"]["skip"], "ledger")
        self.assertIsNone(results[2].get("pick"), "a skipped prompt starts no job")
        self.assertEqual(len(sent), 2)
        self.assertIn("## Session so far, newest turn first\n[user] plan the export fix for the empty rows\n"
                      "[assistant] The null check runs late.\n\n## Prompt\n" + prompt, sent[0])
        self.assertNotIn("It reads the rows first", sent[0], "the transcript stops before the case's prompt")

    def test_a_task_that_moves_to_a_sibling_repo(self):
        header = ("Handoff record for branch feat/x, last written 2026-01-05T10:01:00Z by an earlier session on this task "
                  "(github.com/acme/api/handoffs/feat--x). It reflects what was true then in another repo, "
                  "github.com/acme/api, in the folder /w/api. Its paths are paths of that repo, not of this one; check "
                  "them in that folder before acting on it.\n\n## Goal\nPlan ABC-12\n")
        self.assertEqual(harness.FRAME.match(header).groups(), ("feat/x", "github.com/acme/api/handoffs/feat--x"))
        for sid, hour, cwd, ask, branch in (
                ("api", 11, "/w/api", "Plan ABC-12 for the web repo", "feat/x"),
                ("web", 12, "/w/web", "read ~/Code/api/docs/handoff-abc-12.md and go", "feat/y"),
                ("shapes", 13, "/w/web", "see notes/standup-oct-05.md, utf-16 and sha-256", "feat/z")):
            with open(os.path.join(harness.PROJECTS, "-w-app", sid + ".jsonl"), "w") as fh:
                fh.writelines(json.dumps(o) + "\n" for o in (user("2026-01-08T%d:00:00Z" % hour, ask, branch, cwd),
                                                            said("2026-01-08T%d:01:00Z" % hour, "Done.", branch, sid)))
        sessions, _ = harness.load()
        by_sid = {s["sid"]: s for s in sessions}
        web, api = by_sid["web"], by_sid["api"]
        self.assertEqual(harness.task_key(web, api), "alias", "the ticket only in lower case inside a file name")
        self.assertTrue(harness.push_is_right(web, dict(aliases={"ABC-12"}, branch="feat/x")))
        self.assertIsNone(harness.task_key(by_sid["shapes"], api), "a false shape pairs nothing")
        self.assertEqual(harness.tickets_in("ABC-12 in docs/handoff-def-34.md, not Ghi-56"), {"ABC-12", "DEF-34"})

    def test_a_pick_is_one_more_injection(self):
        mem = {}
        for name, about in (("m", "Exports fail on empty rows"), ("n", "The export job reads the null check first"),
                            ("o", "Release notes go out on Monday")):
            mem[name] = os.path.join(harness.EVAL, name + ".md")
            with open(mem[name], "w") as fh:
                fh.write("---\nname: %s\ndescription: %s\n---\n\n%s.\n" % (name, about, about))
        cand = lambda a, h: dict(address=a, score=3.0, text_hash=h, rot=None)
        result = lambda cands, injected: dict(candidates=cands, injected=injected, memories=dict(mem), index_size=20)
        line = lambda memory: dict(event="pick", session="s", prompt_id="p", memory=memory, skip=None, cost=0.03)
        new, ledger, below = result([cand("m", "hm")], ["m"]), result([cand("m", "hm")], ["m"]), result([cand("o", "ho")], [])
        harness.take_pick(new, line("n"), "n")
        harness.take_pick(ledger, line("m"), "m")
        harness.take_pick(below, line("o"), "o")
        with open(mem["n"]) as fh:
            self.assertEqual(new["picked"], [dict(address="n", text_hash=harness.fnv(fh.read()))],
                             "a memory that is no candidate is hashed from its file")
        self.assertEqual(ledger["picked"], [], "recall injected it first")
        self.assertEqual(ledger["pick"]["skip"], "ledger")
        self.assertEqual(below["picked"], [dict(address="o", text_hash="ho")], "a candidate under the gate keeps its hash")
        cases = [dict(id="C%d" % i, session="s", at="2026-01-05T10:40:00Z", prompt="fix the export") for i in (1, 2, 3)]
        results = [new, ledger, below]
        asked = []

        def label(c, todo, memories, earlier):
            asked.append((c["id"], [x["address"] for x in todo]))
            return [dict(case=c["id"], address=x["address"], text_hash=x["text_hash"], judge="test", at="", note={},
                         label="noise" if x["address"] == "o" else "useful") for x in todo]

        labels = harness.label_candidates(cases, results, {}, label)
        self.assertEqual(asked, [("C1", ["m", "n"]), ("C2", ["m"]), ("C3", ["o"])], "each memory is labeled once")
        m = harness.gate_metrics(cases, results, labels)
        self.assertEqual((m["injections"], m["useful"], m["cases"]), (2, 2, 2), "without the picks")
        m = harness.gate_metrics(cases, results, labels, picks=True)
        self.assertEqual((m["injections"], m["useful"], m["cases"], m["false"]), (4, 3, 3, 1), "the ledger drop counts once")

        for job in (dict(line("o"), skip="echo"), dict(line(None), skip="too_big", cost=0), dict(line(None), cost=0.02),
                    dict(line(None), skip="error", error="http 500", cost=0)):
            results.append(result([], []))
            harness.take_pick(results[-1], job, None)
        results.append(result([], []))
        cases += [dict(cases[0], id="C%d" % i) for i in range(4, 9)]
        text = "\n".join(harness.pick_lines(cases, results, labels))
        for part in ("At the binary's gate with the picks (spec 5.2): injections 4 on 3 cases (picks 2), useful 3 (picks 1), "
                     "precision 75% (3/4,", "Goals: precision 0.67 or more, useful 13 or more. Without the picks: "
                     "injections 2, useful 2, precision 100% (2/2,",
                     "Pick jobs started: 7. Picks with a memory: 4; dropped: echo 1, ledger 1; injected: 2. No memory "
                     "picked: 1. Skips: too_big 1. Errors: 1 (1 × http 500). Cost of the pick lines: $0.14."):
            self.assertIn(part, text)

    def test_report_args(self):
        self.assertEqual(harness.report_args([]), (harness.BINARY, False), "picks are off by default")
        self.assertEqual(harness.report_args(["--picks"]), (harness.BINARY, True))
        self.assertEqual(harness.report_args(["--binary", "x/openrecall"]), (os.path.abspath("x/openrecall"), False))
        self.assertEqual(harness.report_args(["--binary", "x/openrecall", "--picks"]), (os.path.abspath("x/openrecall"), True))
        for wrong in (["--pick"], ["--picks", "--picks"], ["--binary"], ["--binary", "--picks"], ["--picks", "--binary", "x"]):
            self.assertIsNone(harness.report_args(wrong), wrong)

    def test_rules(self):
        edit, commit = ("Edit", {}), ("Bash", dict(command="git -C /w/app commit -m x"))
        turns = [dict(branch_end="feat-b", tools=[edit]), dict(branch_end="feat-a", tools=[("Bash", {})]),
                 dict(branch_end="feat-a", tools=[commit]), dict(branch_end="feat-a", tools=[])]
        s, push = dict(turns=turns, real=turns[3:], tickets=set()), dict(branch="feat-a", aliases=set())
        self.assertEqual([harness.is_late(s, dict(push, turn=i)) for i in (2, 3)], [False, True], "only feat-a's own commit counts")
        self.assertEqual(harness.verdict(s, push), "unjudged")
        self.assertEqual(harness.verdict(dict(s, real=turns[:2]), push), "wrong")
        self.assertEqual(harness.verdict(dict(s, real=turns[1:]), push), "right")
        lo, hi = harness.wilson(67, 100)
        self.assertEqual((round(lo, 2), round(hi, 2)), (0.57, 0.75))
        found = harness.mine("PR #345 at abc1234 in /w/app/src/export.py, line 346", "/w/app")
        self.assertTrue(harness.present("commits", "abc1234def", found))
        self.assertTrue(harness.present("paths", "src/export.py", found))
        self.assertFalse(harness.present("prs", "346", found))
        tails = 'cat "$d"/.cache/a.jsonl ${d}/src/a.py $d/.cache/b.jsonl {root}/src/b.py src/app/[id]/team/route.ts ' \
                '~/x/*/hooks/hooks.json https://example.com/owner/repo.git s3://my-bucket/dir/a.html origin/main:api-docs/a.mdx'
        self.assertEqual(harness.PATH.findall(tails), [], "the tail of a longer token is not a path, as in src/turn.rs")
        self.assertEqual(harness.PATH.findall('cat "/w/app/a.py" (\'src/b.py\') **src/c.py** x="src/d.py"'),
                         ["/w/app/a.py", "src/b.py", "src/c.py", "src/d.py"])
        for url in ("git@github.com:someone/app.git", "https://github.com/someone/app", "ssh://git@github.com:22/someone/app/"):
            self.assertEqual(harness.normalize(url), "github.com/someone/app")
        pool = [("big", i) for i in range(8)] + [("small", i) for i in range(2)]
        self.assertEqual(sorted(r for r, _ in harness.draw(pool, 5, random.Random(1))), ["big"] * 4 + ["small"])


if __name__ == "__main__":
    unittest.main()
