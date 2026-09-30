#!/usr/bin/env python3
"""OpenRecall eval harness: the eval set, the baseline (built-in memory alone), and OpenRecall replayed beside it.

    python3 eval/harness.py freeze                   # draw cases.jsonl once, append new handoff pairs to pairs.jsonl
    python3 eval/harness.py report [--binary PATH]   # write runs/<UTC time>/report.md

Reads Claude Code transcripts under ~/.claude/projects and writes only under ~/.openrecall/eval/
(OPENRECALL_HOME replaces ~/.openrecall). The first freeze needs appendix-a.jsonl there: one
{"id", "session", "at"} line per spec Appendix A prompt, `at` being the prompt line's timestamp.
The report replays every session through the openrecall binary (default: target/release/openrecall) in a
scratch world; without a binary it prints the baseline alone.
"""
import glob
import json
import math
import os
import random
import re
import shutil
import subprocess
import sys
import tempfile
import time
from collections import Counter, defaultdict
from datetime import datetime, timedelta, timezone

PROJECTS = os.path.expanduser("~/.claude/projects")
EVAL = os.path.join(os.environ.get("OPENRECALL_HOME") or os.path.expanduser("~/.openrecall"), "eval")
HOME = os.path.expanduser("~")
CASES = 100
WINDOW = timedelta(days=7)
QUIET = timedelta(minutes=30)
CLASSES = ("tickets", "prs", "commits", "paths")
TRUNK = {"", "main", "master", "HEAD"}
TICKET = re.compile(r"\b[A-Z]{2,5}-\d{2,5}\b")
PR = re.compile(r"(?:\bPR\s*#?|pull/|(?<![\w/])#)(\d{2,6})\b")
GH_PR = re.compile(r"\bgh pr \w+ (\d{2,6})\b")
COMMIT = re.compile(r"(?<![\w-])(?=[0-9a-f]*[a-f])(?=[0-9a-f]*\d)[0-9a-f]{7,40}(?![\w-])")
PATH = re.compile(r"(?<![\w/:@$])(?:~/|/)?(?:[\w.@-]+/)+[\w.@-]*[A-Za-z][\w.@-]*\.[A-Za-z]\w{0,7}(?::\d+(?:-\d+)?)?(?![\w/])")
FRAME = re.compile(r"^Handoff record for branch (.*), last written .* on this task \((.*)\)\. It reflects")
IDENT = re.compile(r"\b[A-Z]{2,5}-\d{2,5}\b|#\d{2,6}\b|/pull/\d+|\b[\w.-]+/[\w./-]+|`[^`\s]{3,}`|\b[0-9a-f]{7,40}\b|https?://\S+")
BINARY = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "target", "release", "openrecall")
NOT_A_PROMPT = ("<local-command-stdout>", "<local-command-caveat>", "<system-reminder>", "<bash-input>",
                "This session is being continued from a previous conversation")


def ts_of(s):
    return datetime.fromisoformat(s.replace("Z", "+00:00")).astimezone(timezone.utc)


def prompt_of(o):
    """The prompt a user line carries, as the UserPromptSubmit hook sees it, or None."""
    if o.get("isMeta"):
        return None
    c = o.get("message", {}).get("content")
    if isinstance(c, list):
        if any(isinstance(b, dict) and b.get("type") == "tool_result" for b in c):
            return None
        c = "\n".join(b.get("text", "") for b in c if isinstance(b, dict) and b.get("type") == "text")
    if not isinstance(c, str) or not c.strip():
        return None
    if "<command-name>" in c:
        name = re.search(r"<command-name>(.*?)</command-name>", c, re.S)
        args = re.search(r"<command-args>(.*?)</command-args>", c, re.S)
        return ("%s %s" % (name.group(1).strip() if name else "/?", args.group(1).strip() if args else "")).strip()
    if "<task-notification>" not in c and c.lstrip().startswith(NOT_A_PROMPT):
        return None
    return c


def scan(path, until=None):
    """One transcript: its turns (a prompt, then the assistant messages, tool calls and branch after it)."""
    s = dict(sid=os.path.basename(path)[:-6], path=path, entrypoint=None, first=None, last=None, cwd=None, turns=[],
             instructions=None, recalled=0, end_line=0)
    tools, groups, cur = {}, {}, None
    with open(path, encoding="utf-8", errors="replace") as fh:
        for n, line in enumerate(fh):
            try:
                o = json.loads(line)
            except ValueError:
                continue
            if not o.get("timestamp"):
                continue
            t = ts_of(o["timestamp"])
            if until and t > until:
                continue
            s["end_line"] = n + 1
            s["first"] = min(s["first"] or t, t)
            s["last"] = max(s["last"] or t, t)
            s["entrypoint"] = s["entrypoint"] or o.get("entrypoint")
            s["cwd"] = s["cwd"] or o.get("cwd")
            typ, branch = o.get("type"), o.get("gitBranch")
            if typ == "attachment":
                a = o.get("attachment") or {}
                s["recalled"] += str(a.get("type")).startswith("relevant_memor")
                if a.get("type") == "instructions" and s["instructions"] is None:
                    s["instructions"] = "\n".join(f.get("content", "") for f in a.get("files") or [])
                continue
            if o.get("isSidechain"):
                continue
            if typ == "user":
                text = prompt_of(o)
                if text is not None:
                    cur = dict(line=n, at=o["timestamp"], start=t, end=t, ask=text, real="<task-notification>" not in text,
                               branch_start=branch, branch_end=branch, tools=[], groups=[], results=[])
                    s["turns"].append(cur)
                    groups = {}
                    continue
            if cur is None or typ not in ("user", "assistant") or o.get("isApiErrorMessage"):
                continue
            cur["end"] = max(cur["end"], t)
            cur["branch_end"] = branch or cur["branch_end"]
            content = o.get("message", {}).get("content")
            blocks = [b for b in content if isinstance(b, dict)] if isinstance(content, list) else []
            if typ == "user":
                for b in blocks:
                    if b.get("type") == "tool_result" and b.get("tool_use_id") in tools:
                        name, inp = tools[b["tool_use_id"]]
                        r = o.get("toolUseResult")
                        out = r.get("stdout", "") if isinstance(r, dict) else r if isinstance(r, str) else ""
                        if name == "Bash" and out:
                            cur["results"].append((inp.get("command", ""), out[:4000]))
                continue
            mid = o.get("message", {}).get("id") or o.get("requestId") or o.get("uuid")
            if mid not in groups:
                groups[mid] = dict(ts=t, text=[])
                cur["groups"].append(groups[mid])
            for b in blocks:
                if b.get("type") == "text" and b.get("text"):
                    groups[mid]["text"].append(b["text"])
                elif b.get("type") == "tool_use":
                    tools[b.get("id")] = (b.get("name"), b.get("input") or {})
                    cur["tools"].append(tools[b.get("id")])
    for t in s["turns"]:
        # Ticket 15: a built-in command (/model, /effort, ...) never reaches the hook or the model, so it is no prompt.
        t["builtin"] = t["ask"].startswith("/") and not t["groups"]
        t["real"] = t["real"] and not t["builtin"]
    return s


def is_scratch(cwd):
    return not cwd or "scratchpad" in cwd or "/tmp/" in cwd


def repo_of(folder):
    m = re.match(r"^(.*/orca/workspaces/[^/]+)/", folder + "/")
    return os.path.basename(m.group(1)) if m else os.path.basename(folder)


def load(until=None):
    """Interactive, non-scratch sessions with a real prompt, oldest first, plus every session that has a
    relevant_memories attachment. `until` cuts each transcript at that time."""
    sessions, recalled = [], []
    for path in sorted(glob.glob(os.path.join(PROJECTS, "*", "*.jsonl"))):
        s = scan(path, until)
        if s["recalled"]:
            recalled.append(s["sid"])
        s["real"] = [t for t in s["turns"] if t["real"]]
        if not s["real"] or is_scratch(s["cwd"]) or s["entrypoint"] == "sdk-cli":
            continue
        s["repo"] = repo_of(s["cwd"])
        s["tickets"] = {tk for t in s["real"] for tk in TICKET.findall(t["ask"])}
        s["first3"] = {tk for t in s["real"][:3] for tk in TICKET.findall(t["ask"])}
        b = (s["real"][1]["branch_start"] if len(s["real"]) > 1 else s["real"][0]["branch_end"]) or ""
        s["settled"] = "" if b in TRUNK else b
        sessions.append(s)
    return sorted(sessions, key=lambda s: s["first"]), recalled


def norm_path(p, folder):
    p = re.sub(r":\d+(?:-\d+)?$", "", p.strip().strip("`'\"(),;:"))
    if any(x in p for x in ("://", "node_modules", "/tmp/", "/.claude/", "scratchpad", "..")) or p.startswith("origin/"):
        return None
    if p.startswith("~/"):
        p = HOME + p[1:]
    if folder and p.startswith(folder + "/"):
        p = p[len(folder) + 1:]
    elif p.startswith(HOME + "/"):
        p = "~" + p[len(HOME):]
    return (p[2:] if p.startswith("./") else p) or None


def mine(text, *folders):
    """Identifiers by class. Paths are made relative to whichever folder they sit under."""
    ids = {c: set() for c in CLASSES}
    ids["tickets"].update(TICKET.findall(text))
    ids["prs"].update(PR.findall(text) + GH_PR.findall(text))
    ids["commits"].update(COMMIT.findall(text))
    for m in PATH.findall(text):
        ids["paths"].update(p for p in (norm_path(m, f) for f in folders) if p)
    return ids


def present(cls, ident, found):
    """Ticket 16's presence test against what mine() found in a text: whole tokens, a commit on its first 7 characters."""
    if cls == "commits":
        return ident[:7] in {c[:7] for c in found["commits"]}
    return ident in found[cls]


def final_state(E, before):
    groups = sorted((g for t in E["turns"] for g in t["groups"] if g["text"] and g["ts"] < before), key=lambda g: g["ts"])
    ids = {c: set() for c in CLASSES}
    for g in groups[-5:]:
        for c, found in mine("\n".join(g["text"]), E["cwd"]).items():
            ids[c] |= found
    return ids


def before(E, S):
    """E's branches and prompt tickets from the turns it finished before S started: a session running beside S
    shares its worktree's HEAD, so its later turns can land on the branch S switched to."""
    turns = [t for t in E["turns"] if t["end"] < S["first"]]
    return ({t["branch_end"] for t in turns} - TRUNK - {None},
            {tk for t in turns if t["real"] for tk in TICKET.findall(t["ask"])})


def task_key(S, E):
    """Ticket 12: S settled on a branch E worked on in the same repo, or S's first 3 prompts name one of E's aliases."""
    branches, tickets = before(E, S)
    alias = bool(S["first3"] & tickets)
    branch = bool(S["settled"]) and S["repo"] == E["repo"] and S["settled"] in branches
    return "both" if alias and branch else "alias" if alias else "branch" if branch else None


def pairs(sessions, match=task_key, window=WINDOW):
    """Each session with the most recent earlier session that matches and was active within the window before it."""
    out = []
    for S in sessions:
        best = None
        for E in sessions:
            acts = [t["end"] for t in E["turns"] if t["end"] < S["first"]]
            if E["first"] >= S["first"] or not acts or S["first"] - max(acts) > window:
                continue
            rule = match(S, E)
            if rule and (best is None or max(acts) > best[1]):
                best = (E, max(acts), rule)
        if best:
            out.append((S, best[0], best[2]))
    return out


def right_push(S, E, rule):
    """Ticket 13: the new session shares an alias with the record, or does 2 or more real turns on its branch."""
    return rule != "branch" or bool(S["tickets"] & before(E, S)[1]) or sum(t["branch_end"] == S["settled"] for t in S["real"]) >= 2


def still_a_pair(p, by_sid):
    """A frozen pair passes the pair rule as corrected in build step 2, or its transcripts are gone and cannot tell."""
    S, E = by_sid.get(p["new"]), by_sid.get(p["earlier"])
    rule = S and E and task_key(S, E)
    return not (S and E) or bool(rule and right_push(S, E, rule))


def searchable(ask):
    """Ticket 15: a real prompt gets the level-2 search unless its query (slash arguments, else the text) is empty,
    or under 4 words with no identifier."""
    query = ask.partition(" ")[2] if ask.startswith("/") else ask
    return len(query.split()) >= 4 or bool(IDENT.search(query))


def draw(pool, k, rng):
    """k items of (repo, ...) at random, proportional by repo, largest remainder first."""
    by = defaultdict(list)
    for item in pool:
        by[item[0]].append(item)
    k = min(k, len(pool))
    quota = {r: k * len(v) / len(pool) for r, v in by.items()}
    n = {r: int(q) for r, q in quota.items()}
    for r in sorted(quota, key=lambda r: n[r] - quota[r])[:k - sum(n.values())]:
        n[r] += 1
    return [x for r in by for x in rng.sample(by[r], n[r])]


def git_dirs(folder):
    d = folder
    while d and d != os.path.dirname(d):
        g = os.path.join(d, ".git")
        if os.path.isdir(g):
            return d, g
        if os.path.isfile(g):
            with open(g) as fh:
                gd = os.path.normpath(os.path.join(d, fh.read().split("gitdir:", 1)[-1].strip()))
            if os.path.isfile(os.path.join(gd, "commondir")):
                with open(os.path.join(gd, "commondir")) as fh:
                    gd = os.path.normpath(os.path.join(gd, fh.read().strip()))
            return d, gd
        d = os.path.dirname(d)
    return None, None


def normalize(url):
    u = re.sub(r"^[\w+.-]+://", "", url)
    u = re.sub(r"^[^@/]+@", "", u)
    u = re.sub(r"^([^/:]+):(\d+/)?", r"\1/", u)
    return re.sub(r"(\.git)?/*$", "", u).lower()


def identity(folder):
    """Ticket 10: the folder holding .git, and the repo identity (None outside a repo)."""
    root, gd = git_dirs(folder)
    if not gd:
        return root, None
    try:
        with open(os.path.join(gd, "config")) as fh:
            m = re.search(r'\[remote "origin"\]([^\[]*)', fh.read())
    except OSError:
        return root, None
    url = m and re.search(r"^\s*url\s*=\s*(\S+)", m.group(1), re.M)
    return root, normalize(url.group(1)) if url else "local" + os.path.dirname(gd)


def checkout(folder, branch):
    """A stand-in `.git` on `branch`: HEAD, plus a ref so the writer's rename check sees the branch exists."""
    git = os.path.join(folder, ".git")
    task = bool(branch) and branch != "HEAD"
    with open(os.path.join(git, "HEAD"), "w") as fh:
        fh.write("ref: refs/heads/%s\n" % branch if task else "0" * 40 + "\n")
    if task:
        try:
            os.makedirs(os.path.dirname(os.path.join(git, "refs", "heads", branch)), exist_ok=True)
            open(os.path.join(git, "refs", "heads", branch), "a").close()
        except OSError:
            pass


def copy_turns(s, st, upto, subs):
    """Appends the transcript's lines up to line `upto` to the session's copy, with real paths moved into the world."""
    with open(s["path"], "rb") as src, open(st["copy"], "ab") as dst:
        src.seek(st["pos"])
        while st["done"] < upto:
            line = src.readline()
            if not line:
                break
            text = line.decode("utf-8", "replace")
            for real, stand_in in subs:
                text = text.replace(real, stand_in)
            dst.write(text.encode("utf-8"))
            st["done"] += 1
        st["pos"] = src.tell()


def replay(binary, sessions, snapshot=()):
    """Every session's hooks through the binary in time order, in a stand-in world (ticket 16): a scratch home, one
    stand-in folder per worktree whose `.git` holds the origin URL, HEAD and refs, and each transcript copied turn by
    turn with its folder and home moved into the world. Returns what each session was pushed (real prompt number, text,
    the record's branch and aliases), recall timings in ms, the binary's log events, and for each (earlier, new) pair
    in `snapshot` the earlier session's record as it stood when the new session started."""
    tmp = tempfile.mkdtemp(prefix="openrecall-replay-")
    if tmp.startswith(("/private/tmp/claude-", "/tmp/claude-")):
        shutil.rmtree(tmp)
        tmp = tempfile.mkdtemp(prefix="openrecall-replay-", dir="/var/tmp")
    home = os.path.join(tmp, "home")
    env = dict(HOME=home, OPENRECALL_HOME=os.path.join(home, ".openrecall"), CLAUDE_CODE_ENTRYPOINT="cli",
               PATH="/usr/bin:/bin")
    known, worlds, state = {}, {}, {}
    for s in sessions:
        known[s["repo"]] = known.get(s["repo"]) or identity(s["cwd"])[1]
    for s in sessions:
        real_root = git_dirs(s["cwd"])[0]
        root = real_root or s["cwd"]
        if root not in worlds:
            ident = identity(root)[1] or known[s["repo"]] or "github.com/replay/" + s["repo"]
            url = "https://" + re.sub(r"^local/", "local.invalid/", ident)
            folder = os.path.join(tmp, "w%d" % len(worlds))
            os.makedirs(folder)
            if real_root or not os.path.exists(s["cwd"]):
                os.makedirs(os.path.join(folder, ".git", "refs", "heads"))
                with open(os.path.join(folder, ".git", "config"), "w") as fh:
                    fh.write('[remote "origin"]\n\turl = %s\n' % url)
            worlds[root] = dict(folder=folder, first=s["sid"], subs=[(root + "/", folder + "/"), ('"%s"' % root, '"%s"' % folder),
                                                                   (HOME + "/", home + "/")],
                                records=os.path.join(env["OPENRECALL_HOME"], "repos", normalize(url), "handoffs"))
        state[s["sid"]] = dict(w=worlds[root], pos=0, done=0, branch=None, renamed=False, prompts=0,
                               copy=os.path.join(tmp, s["sid"] + ".jsonl"))
    events = []
    for s in sessions:
        events.append((s["first"], 0, len(events), s, None))
        done = s["first"]
        for i, t in enumerate(s["turns"]):
            # A queued prompt can be stamped before the turn ahead of it ends; its hook fires after that end.
            start = max(t["start"], done)
            done = max(t["end"], start)
            events += [(start, 1, len(events), s, i), (done, 2, len(events) + 1, s, i)]
    events.sort(key=lambda e: e[:3])
    after = {new: old for old, new in snapshot}
    pushes, timings, snaps = defaultdict(list), [], {}
    try:
        for _, kind, _, s, i in events:
            sid, st = s["sid"], state[s["sid"]]
            w = st["w"]
            if os.path.isdir(os.path.join(w["folder"], ".git")):
                checkout(w["folder"], s["turns"][i]["branch_start" if kind == 1 else "branch_end"] if i is not None
                         else s["turns"][0]["branch_start"] if s["turns"] else None)

            def call(*args, **payload):
                began = time.perf_counter()
                out = subprocess.run((binary,) + args, input=json.dumps(dict(payload, session_id=sid, cwd=w["folder"])).encode(),
                                     env=dict(env, CLAUDE_PROJECT_DIR=w["folder"]), capture_output=True).stdout
                return out.decode("utf-8", "replace"), (time.perf_counter() - began) * 1000

            if kind == 0:
                earlier = state.get(after.get(sid))
                if earlier and earlier["branch"]:
                    record = os.path.join(earlier["w"]["records"], earlier["branch"].replace("/", "--") + ".md")
                    if os.path.exists(record):
                        with open(record) as fh:
                            snaps[sid] = fh.read()
                st["branch"] = s["turns"][0]["branch_start"] if s["turns"] else None
                call("handoff", hook_event_name="SessionStart", source="startup")
                continue
            t = s["turns"][i]
            if kind == 1:
                st["prompts"] += t["real"]
                if t["builtin"]:
                    continue
                out, ms = call("recall", hook_event_name="UserPromptSubmit", prompt=t["ask"])
                timings += [ms] if t["real"] else []
                text = json.loads(out)["hookSpecificOutput"]["additionalContext"] if out.strip() else ""
                m = FRAME.match(text)
                if m:
                    with open(os.path.join(env["OPENRECALL_HOME"], "repos", m.group(2) + ".md")) as fh:
                        front = dict(l.split(": ", 1) for l in fh.read().split("\n---\n")[0].splitlines() if ": " in l)
                    pushes[sid].append(dict(prompt=st["prompts"], text=text, branch=m.group(1),
                                            aliases=set(front.get("aliases", "").split(", ")) - {""}))
                continue
            if not t["groups"]:
                continue
            if st["branch"] and t["branch_end"] and st["branch"] != t["branch_end"] and w["first"] == sid and not st["renamed"]:
                # Orca renames a fresh worktree's placeholder branch once, so its old ref is gone (ticket 12).
                try:
                    os.remove(os.path.join(w["folder"], ".git", "refs", "heads", st["branch"]))
                except OSError:
                    pass
                st["renamed"] = True
            copy_turns(s, st, s["turns"][i + 1]["line"] if i + 1 < len(s["turns"]) else s["end_line"], w["subs"])
            texts = [x for g in t["groups"] for x in g["text"]]
            call("capture", "--job", hook_event_name="Stop", stop_hook_active=False, transcript_path=st["copy"],
                 transcript_len=os.path.getsize(st["copy"]), last_assistant_message=texts[-1] if texts else "")
            st["branch"] = t["branch_end"] or st["branch"]
        log = os.path.join(env["OPENRECALL_HOME"], "log", "openrecall.jsonl")
        events = read(log) if os.path.exists(log) else []
    finally:
        shutil.rmtree(tmp)
    return pushes, timings, events, snaps


def push_is_right(s, push):
    """Ticket 13: the session shares an alias with the pushed record, or does 2 or more real turns on its branch."""
    return bool(push["aliases"] & s["tickets"]) or sum(t["branch_end"] == push["branch"] for t in s["real"]) >= 2


def found_in(text, roots, folders):
    """mine() over pushed text, with each repo-relative path also read from every worktree root it may belong to."""
    found = mine(text, *folders)
    for p in list(found["paths"]):
        if not p.startswith(("/", "~")):
            found["paths"].update(q for q in (norm_path(r + "/" + p, f) for r in roots for f in folders) if q)
    return found


def pct(values, q):
    v = sorted(values)
    return v[min(len(v) - 1, int(q * len(v)))] if v else 0.0


def case(cid, s, t, known):
    root, repo = identity(s["cwd"])
    return dict(id=cid, session=s["sid"], at=t["at"], prompt=t["ask"], repo=repo or known.get(repo_of(s["cwd"])),
                branch=t["branch_start"], folder=root or s["cwd"])


def seed_case(seed, known):
    paths = glob.glob(os.path.join(PROJECTS, "*", seed["session"] + ".jsonl"))
    s = scan(paths[0]) if paths else dict(turns=[])
    turns = [t for t in s["turns"] if t["at"] == seed["at"]]
    if not turns:
        sys.exit("appendix-a.jsonl: %s: no prompt at %s in session %s" % (seed["id"], seed["at"], seed["session"]))
    return case(seed["id"], s, turns[0], known)


def read(path):
    with open(path) as fh:
        return [json.loads(line) for line in fh if line.strip()]


def freeze(now=None):
    now = now or datetime.now(timezone.utc)
    os.makedirs(EVAL, exist_ok=True)
    sessions, _ = load()
    cases_path, pairs_path = os.path.join(EVAL, "cases.jsonl"), os.path.join(EVAL, "pairs.jsonl")
    if os.path.exists(cases_path):
        print("cases.jsonl is frozen: %d cases" % len(read(cases_path)))
    else:
        known = {}
        for s in sessions:
            repo = identity(s["cwd"])[1]
            if repo:
                known.setdefault(s["repo"], repo)
        cases = [seed_case(x, known) for x in read(os.path.join(EVAL, "appendix-a.jsonl"))]
        taken = {(c["session"], c["at"]) for c in cases}
        pool = [(s["repo"], s, t) for s in sessions for t in s["real"]
                if t["groups"] and searchable(t["ask"]) and (s["sid"], t["at"]) not in taken]
        drawn = sorted(draw(pool, CASES - len(cases), random.Random()), key=lambda x: x[2]["start"])
        cases += [case("D%03d" % i, s, t, known) for i, (_, s, t) in enumerate(drawn, 1)]
        with open(cases_path, "w") as fh:
            fh.writelines(json.dumps(c) + "\n" for c in cases)
        print("cases.jsonl: %d cases, %d from appendix-a.jsonl and %d drawn from %d eligible prompts"
              % (len(cases), len(taken), len(drawn), len(pool)))
    old = read(pairs_path) if os.path.exists(pairs_path) else []
    by_sid = {s["sid"]: s for s in sessions}
    done = {p["new"] for p in old if still_a_pair(p, by_sid)}
    found = pairs(sessions)
    right = [p for p in found if right_push(*p)]
    fresh = [(S, E, rule) for S, E, rule in right if S["sid"] not in done and now - S["last"] >= QUIET]
    with open(pairs_path, "a") as fh:
        for i, (S, E, rule) in enumerate(fresh, len(old) + 1):
            fs = final_state(E, S["first"])
            fh.write(json.dumps(dict(id="P%03d" % i, earlier=E["sid"], new=S["sid"], rule=rule,
                                     final_state={c: sorted(v) for c, v in fs.items()})) + "\n")
    rules = Counter(rule for _, _, rule in found)
    active = sum(1 for S, _, _ in right if S["sid"] not in done and now - S["last"] < QUIET)
    print("pairs.jsonl: %d task-key pairs within 7 days (branch %d, alias %d, both %d), %d right pushes; "
          "%d added, %d wait for their new session to go quiet, %d in all"
          % (len(found), rules["branch"], rules["alias"], rules["both"], len(right), len(fresh), active,
             len(old) + len(fresh)))


def wilson(k, n, z=1.96):
    if not n:
        return 0.0, 0.0
    p, d = k / n, 1 + z * z / n
    mid = (p + z * z / (2 * n)) / d
    half = z * math.sqrt(p * (1 - p) / n + z * z / (4 * n * n)) / d
    return mid - half, mid + half


def rate(k, n):
    lo, hi = wilson(k, n)
    return "%.0f%% (%d/%d, %.0f–%.0f%%)" % (100.0 * k / n, k, n, 100 * lo, 100 * hi) if n else "n/a"


def report(now=None, binary=BINARY):
    now = now or datetime.now(timezone.utc)
    cases = read(os.path.join(EVAL, "cases.jsonl"))
    rows = read(os.path.join(EVAL, "pairs.jsonl"))
    sessions, recalled = load()
    by_sid = {s["sid"]: s for s in sessions}
    ran = os.path.exists(binary)
    pushes, timings, events, snaps = replay(binary, sessions, [(p["earlier"], p["new"]) for p in rows]) if ran else ({}, [], [], {})
    hits = defaultdict(lambda: [0] * 6)
    measured, skipped, pushed_at = Counter(), Counter(), Counter()
    unknown = 0
    for p in rows:
        S, E = by_sid.get(p["new"]), by_sid.get(p["earlier"])
        n = sum(map(len, p["final_state"].values()))
        if not still_a_pair(p, by_sid):
            skipped["that fail the pair rule as corrected in build step 2"] += 1
        elif not n:
            skipped["with no final-state identifiers"] += 1
        elif S is None or S["instructions"] is None:
            skipped["whose new session's transcript is gone" if S is None else
                    "whose new session has no start `instructions` attachment"] += 1
            unknown += n
        else:
            measured[p["rule"]] += 1
            folders = [S["cwd"]] + ([E["cwd"]] if E else [])
            roots = {git_dirs(f)[0] or f for f in folders}
            loaded = mine(S["instructions"], *folders)
            typed = mine("\n".join(t["ask"] for t in S["real"][:3]), *folders)
            got = [x for x in pushes.get(S["sid"], []) if x["prompt"] <= 3]
            pushed_at[got[0]["prompt"] if got else "never"] += 1
            carried = found_in("\n".join(x["text"] for x in got), roots, folders)
            left = found_in(snaps.get(S["sid"], ""), roots, folders)
            for cls, ids in p["final_state"].items():
                for i in ids:
                    c, b = present(cls, i, carried), present(cls, i, loaded)
                    for key in (cls, "all", p["rule"]):
                        for k, v in enumerate((b, present(cls, i, typed), 1, c, c or b, present(cls, i, left))):
                            hits[key][k] += v
    across = sum(1 for p in rows if p["rule"] == "alias" and p["new"] in by_sid and p["earlier"] in by_sid
                 and by_sid[p["new"]]["repo"] != by_sid[p["earlier"]]["repo"])
    level1 = [(by_sid[sid], x) for sid, xs in pushes.items() for x in xs]
    right = sum(push_is_right(s, x) for s, x in level1)
    how = Counter(e.get("how") for e in events if e.get("event") == "pushed")
    errors = Counter(e.get("error", "")[:60] for e in events if e.get("event") == "error")
    drawn = [c for c in cases if c["id"].startswith("D")]
    named = sum(1 for c in drawn if any(mine(c["prompt"], c["folder"]).values()))
    slash = sum(1 for c in drawn if c["prompt"].startswith("/"))
    kept = Counter(p["rule"] for p in rows)
    locks = glob.glob(os.path.join(PROJECTS, "*", "memory", ".consolidate-lock"))
    indexes = []
    for path in glob.glob(os.path.join(PROJECTS, "*", "memory", "MEMORY.md")):
        with open(path, encoding="utf-8", errors="replace") as fh:
            indexes.append((os.path.getsize(path), sum(1 for _ in fh)))
    index_bytes, index_lines = max(indexes, default=(0, 0))
    labels = dict(tickets="tickets", prs="PR numbers", commits="commits", paths="paths", all="all")
    col = lambda k, i: rate(hits[k][i], hits[k][2]) if ran or i < 2 else "not replayed"
    lines = [
        "# Eval report, %s" % now.strftime("%Y-%m-%d %H:%M UTC"),
        "",
        "OpenRecall %s, beside the baseline: Claude Code's built-in memory alone. Level 2 arrives with build step 3."
        % ("replayed from `%s`" % os.path.relpath(binary) if ran else "not replayed (no binary at `%s`; run `cargo build --release`)" % binary),
        "",
        "## Eval set",
        "",
        "- `cases.jsonl`: %d cases, %d from spec Appendix A and %d drawn. Of the drawn: %s name an identifier, "
        "%s are slash commands, from %d repos." % (len(cases), len(cases) - len(drawn), len(drawn), rate(named, len(drawn)),
                                                   rate(slash, len(drawn)), len({c["repo"] for c in drawn} - {None})),
        "- `pairs.jsonl`: %d handoff pairs, right pushes only: branch %d, alias %d, both %d."
        % (len(rows), kept["branch"], kept["alias"], kept["both"]),
        "",
        "## Carry-over",
        "",
        "Final-state identifiers that reach the new session by its third real prompt. Carried: in the handoff record "
        "OpenRecall pushed. Built-in: already in the start `instructions` attachment (the `MEMORY.md` and CLAUDE.md "
        "files Claude Code loaded), the baseline. Retyped: in the user's own first 3 real prompts. Goal: carried 80%% "
        "or more. Measured over %d pairs. Not measured: %s. Each rate shows its count and a 95%% Wilson interval."
        % (sum(measured.values()), ", ".join("%d %s" % (n, why) for why, n in skipped.items()) or "none"),
        "",
        "Counting the pairs without a start attachment as 0%%, the baseline's lower bound is %s."
        % rate(hits["all"][0], hits["all"][2] + unknown),
        "",
        "| Class | Carried by the record | Built-in | Either one | Retyped |",
        "|---|---|---|---|---|",
    ]
    lines += ["| %s | %s | %s | %s | %s |" % (labels[k], col(k, 3), col(k, 0), col(k, 4), col(k, 1))
              for k in CLASSES + ("all",)]
    lines += ["", "| Pair rule | Pairs | Carried by the record | Built-in | Either one | Retyped |", "|---|---|---|---|---|---|"]
    lines += ["| %s | %d | %s | %s | %s | %s |" % (k, measured[k], col(k, 3), col(k, 0), col(k, 4), col(k, 1))
              for k in ("branch", "alias", "both")]
    if ran:
        m = sum(measured.values())
        lines += [
            "",
            "- The record the earlier session left, read when the new session started (the writer alone, before the "
            "push rule): %s." % col("all", 5),
            "- Pairs whose new session got a push by its third real prompt: %s; first push at prompt 1: %d, 2: %d, "
            "3: %d." % (rate(m - pushed_at["never"], m), pushed_at[1], pushed_at[2], pushed_at[3]),
            "- Alias pairs whose two sessions are in different repos: %d. Records are scoped by repo (ticket 10), so "
            "OpenRecall never pushes these." % across,
            "",
            "## Level 1: the handoff push",
            "",
            "Every session replayed: %d pushes in %d sessions of %d. Right, by ticket 13's test (the session shares an "
            "alias with the record, or does 2 or more real turns on its branch): %s. Prototype baseline: 106 of 111."
            % (len(level1), len(pushes), len(sessions), rate(right, len(level1))),
            "",
            "- By rule: %s." % (", ".join("%s %d" % kv for kv in sorted(how.items())) or "none"),
            "- At real prompt 1: %d, 2: %d, 3: %d, later: %d." % tuple(
                sum(1 for _, x in level1 if (x["prompt"] == k if k < 4 else x["prompt"] >= 4)) for k in (1, 2, 3, 4)),
            "- Binary errors in the replay: %s." % (", ".join("%d × %s" % (n, e) for e, n in errors.items()) or "none"),
            "",
            "## Replay latency",
            "",
            "`openrecall recall` over %d real prompts, process start included: p50 %.1f ms, p95 %.1f ms. Replay timings "
            "only; live latency comes from the recall log." % (len(timings), pct(timings, 0.5), pct(timings, 0.95)),
        ]
    lines += [
        "",
        "## Flags",
        "",
        "- Transcripts with a `relevant_memories` attachment: %d%s. Any means Claude Code's per-turn memory recall ran; "
        "it changes this baseline and reopens ticket 11." % (len(recalled), ": " + ", ".join(recalled) if recalled else
                                                             ", so built-in memory injects nothing per prompt and the "
                                                             "level-2 baseline is 0 injections at 0 ms"),
        "- Memory directories with a `.consolidate-lock`: %d%s. Any means auto-dream rewrote memory files; it reopens "
        "ticket 11." % (len(locks), ": " + ", ".join(os.path.dirname(x) for x in locks) if locks else ""),
        "- Largest of %d `MEMORY.md` files: %d bytes, %d lines. Claude Code loads 25,000 bytes or 200 lines at session "
        "start; past either cap the rest is left out, and this baseline drops." % (len(indexes), index_bytes, index_lines),
    ]
    out = os.path.join(EVAL, "runs", now.strftime("%Y-%m-%dT%H%M%SZ"))
    os.makedirs(out, exist_ok=True)
    with open(os.path.join(out, "report.md"), "w") as fh:
        fh.write("\n".join(lines) + "\n")
    print("\n".join(lines) + "\n\nwritten to %s/report.md" % out)


if __name__ == "__main__":
    cmd, rest = (sys.argv[1] if len(sys.argv) > 1 else ""), sys.argv[2:]
    if cmd == "freeze" and not rest:
        freeze()
    elif cmd == "report" and rest[:1] in ([], ["--binary"]) and len(rest) in (0, 2):
        report(binary=os.path.abspath(rest[1]) if rest else BINARY)
    else:
        sys.exit(__doc__)
