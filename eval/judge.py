#!/usr/bin/env python3
"""Jev, the eval judge (ticket 09): labels the level-2 candidates of each case, calibrated against spec Appendix A.

    python3 eval/judge.py calibrate    # Jev against the 14 hand labels, written to ~/.openrecall/eval/calibration.jsonl

Jev is reached only through the sensibility plugin's judge.py (OPENRECALL_JUDGE overrides its path), never from the
binary. `harness.py report` calls `label_case` for every candidate that has no label yet; labels are keyed by case,
address and text hash in labels.jsonl, append-only, and reused until the memory's text changes (ticket 16).
"""
import glob
import json
import os
import subprocess
import sys
from datetime import datetime, timezone

import harness

LABELS = ("useful", "partly", "noise")
CRITERIA = {
    "useful": "it states a fact, decision, pointer or gotcha that the task in `prompt` needs and the prompt does not already contain",
    "partly": "it is about the subject of `prompt` but adds little the task needs, or only a part of it applies",
    "noise": "it is about something else, or only repeats what `prompt` already says",
}
JUDGE = [os.environ.get("OPENRECALL_JUDGE", ""),
         os.path.expanduser("~/.claude/plugins/marketplaces/sensibility/skills/judge/scripts/judge.py"),
         os.path.expanduser("~/Code/sensibility/skills/judge/scripts/judge.py")]
MEMORY_CHARS = 1500
CONTEXT_CHARS = 400


class JudgeError(Exception):
    pass


def ask(state, questions):
    """One Jev call; a second failure stops the run, as the build notes ask."""
    script = next((p for p in JUDGE if p and os.path.exists(p)), None)
    if not script:
        raise JudgeError("no judge.py: install the sensibility plugin or set OPENRECALL_JUDGE")
    last = None
    for _ in range(2):
        run = subprocess.run([sys.executable, script, json.dumps(questions), "--state", json.dumps(state)],
                             capture_output=True, text=True)
        if run.returncode == 0:
            return json.loads(run.stdout)["answers"]
        last = (run.stdout or run.stderr).strip()
        if run.returncode == 2:
            break
    raise JudgeError("Jev failed twice: %s" % last)


def question(key):
    return dict(type="choice", criteria=CRITERIA,
                instructions="A coding assistant is about to answer `prompt`; `earlier_prompts` is what the user asked "
                             "before. Would the memory `memories.%s` help it answer or act on `prompt`?" % key)


def label_texts(prompt, earlier, texts):
    """Labels {key: memory text} for one prompt in one call. Returns {key: (label, probabilities)}."""
    if not texts:
        return {}
    state = dict(prompt=prompt, earlier_prompts=[e[:CONTEXT_CHARS] for e in earlier],
                 memories={k: t[:MEMORY_CHARS] for k, t in texts.items()})
    answers = ask(state, {k: question(k) for k in texts})
    out = {}
    for k, a in answers.items():
        probs = a.get("probabilities") or {}
        out[k] = (a.get("choice") if a.get("choice") in LABELS else max(LABELS, key=lambda l: probs.get(l, 0)), probs)
    return out


def label_case(case, candidates, memories, earlier):
    """Jev labels of a case's unlabeled candidates: rows for labels.jsonl."""
    keys = {"m%d" % i: c for i, c in enumerate(candidates, 1) if c["address"] in memories}
    got = label_texts(case["prompt"], earlier, {k: harness.memory_text(memories[c["address"]]) for k, c in keys.items()})
    now = datetime.now(timezone.utc).isoformat(timespec="seconds")
    return [dict(case=case["id"], address=c["address"], text_hash=c["text_hash"], label=got[k][0], judge="jev", at=now,
                 note=got[k][1]) for k, c in keys.items() if k in got]


def dedupe_check(fact, candidates, old=None):
    """Ticket 26's questions for one written fact, one call: per candidate, does it already state the fact (repeat)
    and does the fact make it out of date (supersede); for a replace, does the merged fact keep the old copy's detail."""
    keys = ["c%d" % i for i in range(len(candidates))]
    state = dict(fact=fact[:MEMORY_CHARS], candidates={k: t[:MEMORY_CHARS] for k, t in zip(keys, candidates)})
    questions = {}
    for k in keys:
        questions["repeat_" + k] = dict(type="noul", instructions="Does `candidates.%s` already state what `fact` says, "
                                        "so a reader of it would learn nothing new from `fact`?" % k)
        questions["supersede_" + k] = dict(type="noul", instructions="Is `candidates.%s` about the same thing as `fact`, "
                                           "and does `fact` make it wrong or out of date?" % k)
    if old is not None:
        state["old"] = old[:MEMORY_CHARS]
        questions["kept"] = dict(type="noul", instructions="`fact` replaced `old`. Does `fact` keep every detail of `old` "
                                                           "that is still true?")
    answers = ask(state, questions) if questions else {}
    p = lambda k: answers.get(k, {}).get("noul", 0.0)
    return dict(repeat=[p("repeat_" + k) for k in keys], supersede=[p("supersede_" + k) for k in keys],
                kept=p("kept") if old is not None else None)


def openviking_block(session, at):
    """The digest lines OpenViking injected after the prompt at `at`, from the transcript."""
    for path in glob.glob(os.path.join(harness.PROJECTS, "*", session + ".jsonl")):
        seen = False
        with open(path, encoding="utf-8", errors="replace") as fh:
            for line in fh:
                try:
                    o = json.loads(line)
                except ValueError:
                    continue
                seen = seen or o.get("timestamp") == at
                a = o.get("attachment") or {}
                if seen and a.get("type") == "hook_additional_context":
                    c = a.get("content")
                    c = c if isinstance(c, str) else "\n".join(x if isinstance(x, str) else x.get("text", "") for x in c or [])
                    if "<openviking-context>" in c:
                        return "\n".join(l for l in c.splitlines()
                                         if l.strip() and not l.startswith(("<", "Relevant memory from OpenViking",
                                                                            "OpenViking memory digest")))
    return None


def calibrate():
    cases = [c for c in harness.read(os.path.join(harness.EVAL, "cases.jsonl")) if c["id"].startswith("A")]
    if len(cases) != len(harness.HAND):
        sys.exit("expected %d Appendix A cases, found %d" % (len(harness.HAND), len(cases)))
    rows = []
    for case, hand in zip(cases, harness.HAND):
        block = openviking_block(case["session"], case["at"])
        if block is None:
            rows.append(dict(case=case["id"], hand=hand, jev=None, note="OpenViking block not found"))
            continue
        s = harness.scan(glob.glob(os.path.join(harness.PROJECTS, "*", case["session"] + ".jsonl"))[0])
        earlier = [t["ask"] for t in s["turns"] if t["real"] and t["at"] < case["at"]][-2:]
        got = label_texts(case["prompt"], earlier, {"m1": block})["m1"]
        rows.append(dict(case=case["id"], hand=hand, jev=got[0], note=got[1]))
    now = datetime.now(timezone.utc).isoformat(timespec="seconds")
    with open(os.path.join(harness.EVAL, "calibration.jsonl"), "a") as fh:
        fh.writelines(json.dumps(dict(r, at=now)) + "\n" for r in rows)
    print(harness.agreement(rows))
    return rows


if __name__ == "__main__":
    if sys.argv[1:] == ["calibrate"]:
        calibrate()
    else:
        sys.exit(__doc__)
