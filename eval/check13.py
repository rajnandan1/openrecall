#!/usr/bin/env python3
"""Check that the harness reproduces ticket 13's handoff set: its pairs, final state, and what the prototype record carried.

    python3 eval/check13.py --until <ISO time> --expect <carried>/<final-state identifiers>

--until cuts every transcript at that time, so the corpus looks as it did when the ticket's numbers were taken.
--expect is ticket 13's exact-match count; the harness's presence test (a commit matches on 7 characters) prints
beside it. Exits 1 on a mismatch.
"""
import argparse
import re
import sys
from datetime import timedelta

import harness as h

BUDGET_TOKENS = 800
CMD_VERBS = {"git", "gh", "pnpm", "npm", "yarn", "bun", "cargo", "make", "pytest", "python", "python3", "uv", "go",
             "docker", "kubectl", "helm", "orca", "ruff", "mypy", "tsc", "node", "deno", "just", "railway", "linear"}


def tokens(text):
    return len(text) // 4


def cut_paragraph(text, cap):
    if len(text) <= cap:
        return text
    head = text[:cap]
    at = max(head.rfind("\n\n"), head.rfind("\n"))
    return (head[:at] if at > cap // 2 else head).rstrip() + "\n[...]"


def dedupe_commits(d):
    keep = {}
    for c in sorted(d, key=len):
        if not any(c.startswith(k) for k in keep):
            keep[c] = d[c]
    return keep


class Record:
    def __init__(self, repo, branch, folder, sid):
        self.repo, self.branch, self.folder = repo, branch, folder
        self.writers = [sid]
        self.aliases = set()
        self.goal = None
        self.ask = self.answer = ""
        self.ids = {c: {} for c in h.CLASSES}
        self.commands = {}
        self.turns = 0
        self.updated = None

    def write(self, turn, n):
        self.turns += 1
        self.updated = turn["end"]
        self.branch = turn["branch_end"] or self.branch
        if turn["real"]:
            if self.goal is None and len(turn["ask"]) >= 40:
                self.goal = turn["ask"]
            self.ask = turn["ask"]
            self.aliases |= set(h.TICKET.findall(turn["ask"]))
            for tk in self.aliases:
                self.ids["tickets"][tk] = n
        for name, inp in turn["tools"]:
            fp = inp.get("file_path") or inp.get("notebook_path")
            p = fp and h.norm_path(fp, self.folder)
            if p:
                self.ids["paths"][p] = n
            if name == "Bash":
                cmd = inp.get("command", "")
                for m in h.mine(cmd, self.folder)["paths"]:
                    self.ids["paths"][m] = n
                for m in h.GH_PR.findall(cmd) + re.findall(r"pull/(\d+)", cmd):
                    self.ids["prs"][m] = n
                for m in h.COMMIT.findall(cmd):
                    self.ids["commits"][m] = n
                first = cmd.strip().split("\n")[0]
                if first and first.split(" ")[0].split("/")[-1] in CMD_VERBS:
                    self.commands[first[:120]] = n
        for cmd, out in turn["results"]:
            if "gh pr" in cmd or "git push" in cmd:
                for m in re.findall(r"pull/(\d+)", out):
                    self.ids["prs"][m] = n
            if cmd.startswith("git commit") or "git rev-parse" in cmd or cmd.startswith("git log"):
                for m in h.COMMIT.findall(out)[:3]:
                    self.ids["commits"][m] = n
        texts = [x for g in turn["groups"] for x in g["text"]]
        if texts:
            self.answer = texts[-1] if len(texts[-1]) > 40 else "\n".join(texts[-2:])
        for cls, found in h.mine("\n".join(texts), self.folder).items():
            for i in found:
                self.ids[cls][i] = n

    def render(self, caps):
        def newest(d, cap):
            return [k for k, _ in sorted(d.items(), key=lambda kv: -kv[1])][:cap]
        lines = ["---", "task: %s @ %s" % (self.repo, self.branch), "folder: %s" % self.folder,
                 "aliases: %s" % ", ".join(sorted(self.aliases)),
                 "updated: %s  turns: %d  writers: %s" % (self.updated.isoformat(timespec="minutes") if self.updated else "?",
                                                           self.turns, ", ".join(w[:8] for w in self.writers)), "---",
                 "## Goal", cut_paragraph(self.goal or "", caps["goal"]).strip(),
                 "## Last ask (turn %d)" % self.turns, cut_paragraph(self.ask, caps["ask"]).strip(),
                 "## Last answer", cut_paragraph(self.answer, caps["answer"]).strip(),
                 "## Identifiers",
                 "tickets: " + (", ".join(sorted(self.ids["tickets"])) or "-"),
                 "prs: " + (", ".join("#" + p for p in newest(self.ids["prs"], caps["prs"])) or "-"),
                 "commits: " + (", ".join(newest(dedupe_commits(self.ids["commits"]), caps["commits"])) or "-"),
                 "paths (newest first): " + (", ".join(newest(self.ids["paths"], caps["paths"])) or "-"),
                 "commands (newest first):"] + ["- " + c for c in newest(self.commands, caps["commands"])]
        return "\n".join(lines) + "\n"

    def render_budget(self):
        caps = dict(goal=300, ask=400, answer=1600, prs=10, commits=6, paths=20, commands=8)
        text = self.render(caps)
        for k, v in [("commands", 4), ("paths", 12), ("answer", 1000), ("commands", 2), ("paths", 8), ("answer", 600),
                     ("goal", 150), ("commands", 0), ("paths", 6), ("ask", 200), ("answer", 400)]:
            if tokens(text) <= BUDGET_TOKENS:
                break
            caps[k] = v
            text = self.render(caps)
        return text


def build_record(E, before):
    r = Record(h.repo_of(E["cwd"]), E["turns"][0]["branch_start"] or "", E["cwd"], E["sid"])
    for n, t in enumerate(E["turns"], 1):
        if t["end"] >= before:
            break
        r.write(t, n)
    return r


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--until", required=True, help="ISO time to cut every transcript at")
    ap.add_argument("--expect", required=True, help="carried/identifiers, for example 150/160")
    a = ap.parse_args()
    sessions, _ = h.load(h.ts_of(a.until))
    for s in sessions:
        # Ticket 13 counted built-in commands (/model, /effort) as prompts; its set is kept as it was measured.
        for t in s["turns"]:
            t["real"] = "<task-notification>" not in t["ask"]
        s["real"] = [t for t in s["turns"] if t["real"]]
        s["first3"] = {tk for t in s["real"][:3] for tk in h.TICKET.findall(t["ask"])}
    found = h.pairs(sessions, match=lambda S, E: "alias" if S["first3"] & E["tickets"] else None,
                    window=timedelta(hours=12))
    measured = exact = matched = total = 0
    for S, E, _ in found:
        fs = h.final_state(E, S["first"])
        if not any(fs.values()):
            continue
        measured += 1
        got = h.mine(build_record(E, S["first"]).render_budget(), E["cwd"])
        total += sum(map(len, fs.values()))
        exact += sum(i in got[c] for c, ids in fs.items() for i in ids)
        matched += sum(h.present(c, i, got) for c, ids in fs.items() for i in ids)
    result = "%d/%d" % (exact, total)
    print("ticket 13's set at %s: %d pairs, %d with final-state identifiers; the prototype record carries %s by exact "
          "match (expected %s: %s), %d/%d by the harness's presence test"
          % (a.until, len(found), measured, result, a.expect, "pass" if result == a.expect else "FAIL", matched, total))
    sys.exit(result != a.expect)


if __name__ == "__main__":
    main()
