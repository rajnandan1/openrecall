#!/usr/bin/env python3
"""OpenRecall eval harness: the eval set, the baseline (built-in memory alone), and OpenRecall replayed beside it.

    python3 eval/harness.py freeze                   # draw cases.jsonl once, append new handoff pairs to pairs.jsonl
    python3 eval/harness.py report [--binary PATH]   # write runs/<UTC time>/report.md

Reads Claude Code transcripts under ~/.claude/projects and writes only under ~/.openrecall/eval/
(OPENRECALL_HOME replaces ~/.openrecall). The first freeze needs appendix-a.jsonl there: one
{"id", "session", "at"} line per spec Appendix A prompt, `at` being the prompt line's timestamp.
The report replays every session through the openrecall binary (default: target/release/openrecall) in a
scratch world, then every frozen case for level 2, labeled by Jev through judge.py; without a binary it prints the
baseline alone.
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
PR_URL = re.compile(r"https?://\S*?pull/\d{3,6}\b")
COMMIT = re.compile(r"(?<![\w-])(?=[0-9a-f]*[a-f])(?=[0-9a-f]*\d)[0-9a-f]{7,40}(?![\w-])")
PATH = re.compile(r"(?<![\w/:@$.})\]-])(?<![\w})][\"'])(?!(?<=\*)/)(?:~/|/)?(?:[\w.@-]+/)+[\w.@-]*[A-Za-z][\w.@-]*\.[A-Za-z]\w{0,7}(?::\d+(?:-\d+)?)?(?![\w/])")
FRAME = re.compile(r"^Handoff record for branch (.*), last written .* on this task \((.*)\)\. It reflects")
EDITS = {"Edit", "Write", "NotebookEdit", "MultiEdit"}
GIT_COMMIT = re.compile(r"\bgit(?:\s+-C\s+\S+)?\s+commit\b")
IDENT = re.compile(r"\b[A-Z]{2,5}-\d{2,5}\b|#\d{2,6}\b|/pull/\d+|\b[\w.-]+/[\w./-]+|`[^`\s]{3,}`|\b[0-9a-f]{7,40}\b|https?://\S+")
GOAL = 0.67
# Spec Appendix A, rows 0 to 13, in the order of appendix-a.jsonl (A00 to A13).
HAND = ["useful", "useful", "useful", "partly", "partly", "useful", "noise", "noise", "useful", "partly", "noise",
        "noise", "noise", "noise"]
BINARY = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "target", "release", "openrecall")
MCP_RECALL = "mcp__plugin_openrecall_openrecall__recall"
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


def moved(text, subs):
    for real, stand_in in subs:
        text = text.replace(real, stand_in)
    return text


def copy_turns(s, st, upto, subs):
    """Appends the transcript's lines up to line `upto` to the session's copy, with real paths moved into the world."""
    with open(s["path"], "rb") as src, open(st["copy"], "ab") as dst:
        src.seek(st["pos"])
        while st["done"] < upto:
            line = src.readline()
            if not line:
                break
            dst.write(moved(line.decode("utf-8", "replace"), subs).encode("utf-8"))
            st["done"] += 1
        st["pos"] = src.tell()


def scratch():
    """A scratch directory that is never under /private/tmp/claude-, which the binary treats as a session skip."""
    tmp = tempfile.mkdtemp(prefix="openrecall-replay-")
    if tmp.startswith(("/private/tmp/claude-", "/tmp/claude-")):
        shutil.rmtree(tmp)
        tmp = tempfile.mkdtemp(prefix="openrecall-replay-", dir="/var/tmp")
    return tmp


def replay(binary, sessions, snapshot=()):
    """Every session's hooks through the binary in time order, in a stand-in world (ticket 16): a scratch home, one
    stand-in folder per worktree whose `.git` holds the origin URL, HEAD and refs, and each transcript copied turn by
    turn with its folder and home moved into the world. Returns what each session was pushed (real prompt number, text,
    the record's branch and aliases), recall timings in ms, the binary's log events, and for each (earlier, new) pair
    in `snapshot` the earlier session's record as it stood when the new session started."""
    tmp = scratch()
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
                out, ms = call("recall", hook_event_name="UserPromptSubmit", prompt=moved(t["ask"], w["subs"]))
                timings += [ms] if t["real"] else []
                text = json.loads(out)["hookSpecificOutput"]["additionalContext"] if out.strip() else ""
                text = text.split("\n\nRecalled memories from earlier sessions (OpenRecall).")[0]
                m = FRAME.match(text)
                if m:
                    with open(os.path.join(env["OPENRECALL_HOME"], "repos", m.group(2) + ".md")) as fh:
                        front = dict(l.split(": ", 1) for l in fh.read().split("\n---\n")[0].splitlines() if ": " in l)
                    pushes[sid].append(dict(prompt=st["prompts"], turn=i, text=text, branch=m.group(1),
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
        routes = defaultdict(list)
        for e in events:
            if e.get("event") == "pushed":
                routes[e.get("session")].append(e.get("how"))
        for sid, xs in pushes.items():
            for x, how in zip(xs, routes[sid]):
                x["how"] = how
    finally:
        # Each `handoff` starts a detached extraction worker, which finds no extract.toml here and exits.
        shutil.rmtree(tmp, ignore_errors=True)
    return pushes, timings, events, snaps


def slug(path):
    """Claude Code's project slug: every character that is not ASCII alphanumeric becomes `-` (ticket 11)."""
    return re.sub(r"[^A-Za-z0-9]", "-", path)


def born(path):
    st = os.stat(path)
    return datetime.fromtimestamp(getattr(st, "st_birthtime", st.st_mtime), timezone.utc)


def front(path, key):
    """One flat frontmatter value, `type` also from under `metadata:`."""
    with open(path, encoding="utf-8", errors="replace") as fh:
        m = re.search(r"^\s*%s:\s*(.*)$" % key, fh.read(4000), re.M)
    return m.group(1).strip().strip("\"'") if m else ""


def fact_date(path):
    """A fact's `source` date (ticket 21): the time cut for OpenRecall's own facts."""
    parts = front(path, "source").split()
    try:
        return ts_of(parts[1]) if len(parts) > 1 else None
    except ValueError:
        return None


def memory_text(path):
    """The memory as the judge and the used signal read it: description and body, frontmatter stripped."""
    with open(path, encoding="utf-8", errors="replace") as fh:
        text = fh.read()
    body = text[4:].partition("\n---")[2] if text.startswith("---\n") and "\n---" in text[4:] else text
    return ("%s\n\n%s" % (front(path, "description"), body.strip())).strip()


def rot_of(path, main):
    """How a memory's cited paths stand under the main checkout: None when every cited path exists (or none is
    cited), "any" when at least one is missing, "all" when every one is. The binary drops only pointer facts
    (ticket 09); the report simulates the two stricter rules."""
    text = re.sub(r"https?://\S+", " ", memory_text(path))
    cited = [p for p in (norm_path(raw, main) for raw in PATH.findall(text)) if p]
    missing = [p for p in cited if not os.path.exists(
        os.path.join(HOME, p[2:]) if p.startswith("~/") else p if p.startswith("/") else os.path.join(main, p))]
    return None if not missing else "all" if len(missing) == len(cited) else "any"


def unexpired(path, at):
    """The binary drops a state fact whose `expires` is today or earlier; a replayed prompt judges it at its own time,
    so a fact still live then loses the field in the stand-in world."""
    with open(path) as fh:
        text = fh.read()
    m = re.search(r"^expires: (\d{4}-\d{2}-\d{2})\n", text, re.M)
    if m and m.group(1) > at.date().isoformat():
        with open(path, "w") as fh:
            fh.write(text.replace(m.group(0), "", 1))


def address_path(address, orhome):
    """Ticket 19: the file behind an address."""
    parts = address.split("/")
    if parts[0] == "builtin" and len(parts) == 3:
        return os.path.join(PROJECTS, parts[1], "memory", parts[2] + ".md")
    if parts[0] == "global":
        return os.path.join(orhome, "global", parts[1] + ".md")
    return os.path.join(orhome, "repos", address + ".md")


def when(ms):
    return datetime.fromtimestamp(ms / 1000, timezone.utc).strftime("%Y-%m-%d %H:%M UTC")


def unranked(e, orhome, n=10):
    """Build step 7: the `n` memories that share the most words with a written fact and were not among its logged
    candidates, from its repo's facts and the built-in files of its session's main checkout (the extraction queue
    entry names it). The check then also sees a repeat the binary's own search ranked too low."""
    path = address_path(e["address"], orhome)
    identity = e["address"].rsplit("/", 1)[0]
    pool = {"%s/%s" % (identity, os.path.basename(f)[:-3]): f for f in glob.glob(os.path.join(os.path.dirname(path), "*.md"))
            if os.path.basename(f) != "claude-md-suggestions.md"}
    try:
        with open(os.path.join(orhome, "extract", e["session"] + ".json")) as fh:
            main = json.load(fh).get("main")
    except (OSError, ValueError):
        main = None
    if main:
        pool.update(("builtin/%s/%s" % (slug(main), os.path.basename(f)[:-3]), f)
                    for f in glob.glob(os.path.join(PROJECTS, slug(main), "memory", "*.md")) if os.path.basename(f) != "MEMORY.md")
    words = lambda text: set(re.findall(r"[\w-]{4,}", text.lower()))
    fact = words(memory_text(path))
    logged = {c["address"] for c in e.get("candidates", [])} | {e["address"]}
    shared = sorted(((len(fact & words(memory_text(f))), a) for a, f in pool.items() if a not in logged), reverse=True)
    return [a for k, a in shared[:n] if k]


def extraction_lines(since, dedupe_fn=None, yes=0.7):
    """Tickets 17, 21 and 26 from the live log: extraction health, the facts written, Jev's check of each written fact
    against its logged candidates and the unranked memories most like it (asked once, cached in dedupe.jsonl), store
    size and the CLAUDE.md suggestions added since the last report. `dedupe_fn(fact, candidates, old)` returns
    judge.dedupe_check's answer; None asks nothing."""
    orhome = os.path.dirname(EVAL)
    log = os.path.join(orhome, "log", "openrecall.jsonl")
    events = read(log) if os.path.exists(log) else []
    extract = [e for e in events if e.get("event") == "extract"]
    runs = [e for e in extract if "to" in e]
    strikes = [e for e in extract if "strike" in e]
    stops = [e for e in extract if "stop" in e]
    off = [e for e in extract if "off" in e]
    failed = sorted({e["session"] for e in strikes if e.get("failed")})
    total = Counter()
    dropped = Counter()
    for e in runs:
        total.update({k: e.get(k, 0) for k in ("proposed", "new", "replaced", "skipped", "normalized", "suggestions",
                                              "tokens_in", "tokens_out")})
        dropped.update(e.get("dropped") or {})
    cost = sum(e.get("cost", 0) for e in runs)

    cache_path = os.path.join(EVAL, "dedupe.jsonl")
    # A row from before build step 7 was checked against the logged candidates alone: it is asked again, once.
    cache = {(r["session"], r["name"], r["at"]): r for r in (read(cache_path) if os.path.exists(cache_path) else [])
             if r.get("unranked")}
    written = [e for e in events if e.get("event") == "dedupe" and e.get("action") in ("new", "replace")]
    fresh = []
    for e in written:
        key = (e["session"], e["name"], e["at"])
        path = address_path(e["address"], orhome)
        if key in cache or dedupe_fn is None or not os.path.exists(path):
            continue
        cands = [c["address"] for c in e.get("candidates", []) if c["address"] != e["address"]
                 and os.path.exists(address_path(c["address"], orhome))] + unranked(e, orhome)
        copy = os.path.join(os.path.dirname(path), "replaced", e.get("copy") or "")
        old = memory_text(copy) if e["action"] == "replace" and os.path.isfile(copy) else None
        got = dedupe_fn(memory_text(path), [memory_text(address_path(a, orhome)) for a in cands], old)
        fresh.append(dict(session=e["session"], name=e["name"], at=e["at"], address=e["address"], action=e["action"],
                          candidates=cands, unranked=True, **got))
    if fresh:
        with open(cache_path, "a") as fh:
            fh.writelines(json.dumps(r) + "\n" for r in fresh)
        cache.update({(r["session"], r["name"], r["at"]): r for r in fresh})
    checked = [cache[k] for k in ((e["session"], e["name"], e["at"]) for e in written) if k in cache]
    flag = lambda kind, own: ["%s (%s)" % (r["address"], a) for r in checked if r["action"] == "new"
                              for a, p in zip(r["candidates"], r[kind]) if p >= yes and own(a)]
    repeats = flag("repeat", lambda a: True)
    missed = flag("supersede", lambda a: not a.startswith(("builtin/", "global/")))
    builtin = flag("supersede", lambda a: a.startswith("builtin/"))
    merges = [r for r in checked if r["action"] == "replace" and r.get("kept") is not None]
    lost = [r["address"] for r in merges if r["kept"] < 0.5]

    sizes = Counter()
    repos = os.path.join(orhome, "repos")
    for f in glob.glob(os.path.join(repos, "**", "*.md"), recursive=True):
        rel = os.path.relpath(os.path.dirname(f), repos)
        if os.path.basename(f) != "claude-md-suggestions.md" and not re.search(r"(^|/)(handoffs|replaced)(/|$)", rel):
            sizes[rel] += 1
    added = []
    for f in sorted(glob.glob(os.path.join(repos, "**", "claude-md-suggestions.md"), recursive=True)):
        with open(f) as fh:
            for line in fh:
                m = re.search(r" \([\w-]+, (\d{4}-\d{2}-\d{2})\)$", line.rstrip())
                if m and (since is None or m.group(1) >= since.date().isoformat()):
                    added.append("`%s`: %s" % (os.path.relpath(f, orhome), line.rstrip()[2:]))
    listed = lambda xs: ": " + "; ".join(xs[:10]) + (" …" if len(xs) > 10 else "") if xs else ""
    return [
        "", "## Extraction", "",
        "From the live log, `%s`; nothing here is replayed." % os.path.relpath(log, orhome),
        "",
        "- Health: %d runs over %d sessions, last success %s; strikes %d; sessions marked failed %d%s; last stop: %s; "
        "last start with extraction off: %s." % (
            len(runs), len({e["session"] for e in runs}), when(max(e["at"] for e in runs)) if runs else "never",
            len(strikes), len(failed), listed(failed),
            "%s at %s" % (stops[-1]["stop"], when(stops[-1]["at"])) if stops else "none",
            "%s at %s" % (off[-1]["off"], when(off[-1]["at"])) if off else "none"),
        "- Facts: proposed %d, new %d, replaced %d, skipped %d, dropped %d (%s); names normalized %d; tokens %d in, %d "
        "out; cost $%.2f." % (total["proposed"], total["new"], total["replaced"], total["skipped"], sum(dropped.values()),
                             ", ".join("%s %d" % kv for kv in sorted(dropped.items())) or "none", total["normalized"],
                             total["tokens_in"], total["tokens_out"], cost),
        "- Dedupe check (ticket 26; Jev, yes at %.1f or more): %d of %d written facts checked, each against its logged "
        "candidates and up to 10 unranked memories that share the most words with it. Repeats that got through: %d%s. "
        "Missed replaces of an own fact: %d%s. New facts that make a built-in file out of date: %d%s. "
        "Merges that lost old detail: %d of %d%s. Whether a merge kept the new fact's detail is not checked: the "
        "proposed text is never stored." % (yes, len(checked), len(written), len(repeats), listed(repeats), len(missed),
                                            listed(missed), len(builtin), listed(builtin), len(lost), len(merges),
                                            listed(lost)),
        "- Store size: %s." % (", ".join("%s %d" % kv for kv in sorted(sizes.items())) or "no OpenRecall facts yet"),
        "- CLAUDE.md suggestions added since the last report%s: %d%s." % (
            " (%s)" % since.strftime("%Y-%m-%d") if since else "", len(added), listed(added)),
    ]


def mains_of(cases):
    """Each case's main checkout: from its folder's .git while the folder exists, else the checkout another case of
    the same repo identity has (one clone, one memory directory, ticket 11)."""
    by_repo, out = {}, {}
    for c in cases:
        gd = git_dirs(c["folder"])[1] if os.path.isdir(c["folder"]) else None
        if gd:
            out[c["id"]] = os.path.dirname(gd)
            by_repo.setdefault(c["repo"], out[c["id"]])
    for c in cases:
        if c["id"] not in out and c["repo"]:
            out[c["id"]] = by_repo.get(c["repo"])
    return out


def replay_cases(binary, cases, mains):
    """Ticket 16: each case through `openrecall recall` in its own stand-in world: a folder whose `.git` holds the
    case's origin and branch, the repo's main checkout linked in so the path check sees today's tree, a scratch home
    with the built-in memory files born before the prompt at the slug the binary computes, and OpenRecall's own
    facts with a `source` date before it. Returns one result per case: candidates, injections, drops, timing."""
    tmp = scratch()
    orhome = os.environ.get("OPENRECALL_HOME") or os.path.expanduser("~/.openrecall")
    results = []
    try:
        for i, c in enumerate(cases):
            folder, home = os.path.join(tmp, "w%d" % i), os.path.join(tmp, "h%d" % i)
            os.makedirs(folder)
            os.makedirs(home)
            at, main, memories = ts_of(c["at"]), mains.get(c["id"]), {}
            if c["repo"]:
                os.makedirs(os.path.join(folder, ".git", "refs", "heads"))
                with open(os.path.join(folder, ".git", "config"), "w") as fh:
                    fh.write('[remote "origin"]\n\turl = https://%s\n' % re.sub(r"^local/", "local.invalid/", c["repo"]))
                checkout(folder, c["branch"])
            if main and os.path.isdir(main):
                for entry in os.listdir(main):
                    if entry != ".git":
                        os.symlink(os.path.join(main, entry), os.path.join(folder, entry))
                dst = os.path.join(home, ".claude", "projects", slug(folder), "memory")
                for f in glob.glob(os.path.join(PROJECTS, slug(main), "memory", "*.md")):
                    if os.path.basename(f) != "MEMORY.md" and born(f) < at:
                        os.makedirs(dst, exist_ok=True)
                        shutil.copy2(f, dst)
                        memories["builtin/%s/%s" % (slug(main), os.path.basename(f)[:-3])] = f
            for rel, prefix in ((os.path.join("repos", c["repo"] or ""), (c["repo"] or "") + "/"), ("global", "global/")):
                for f in glob.glob(os.path.join(orhome, rel, "*.md")) if c["repo"] or rel == "global" else []:
                    d = fact_date(f)
                    if d and d < at:
                        os.makedirs(os.path.join(home, ".openrecall", rel), exist_ok=True)
                        copy = shutil.copy2(f, os.path.join(home, ".openrecall", rel))
                        unexpired(copy, at)
                        memories[prefix + os.path.basename(f)[:-3]] = f
            env = dict(HOME=home, OPENRECALL_HOME=os.path.join(home, ".openrecall"), CLAUDE_CODE_ENTRYPOINT="cli",
                       CLAUDE_PROJECT_DIR=folder, PATH="/usr/bin:/bin")
            # The first call builds the index; the case's own call is timed the way a live prompt runs, warm.
            subprocess.run([binary, "recall"], env=env, capture_output=True, input=json.dumps(dict(
                session_id="warm", cwd=folder, prompt="warm the index with a prompt that recalls nothing")).encode())
            began = time.perf_counter()
            subprocess.run([binary, "recall"], env=env, capture_output=True, input=json.dumps(dict(
                session_id=c["session"], cwd=folder, prompt=c["prompt"], hook_event_name="UserPromptSubmit")).encode())
            ms = (time.perf_counter() - began) * 1000
            log = os.path.join(env["OPENRECALL_HOME"], "log", "openrecall.jsonl")
            events = read(log) if os.path.exists(log) else []
            recall = next((e for e in events if e.get("event") == "recall" and e.get("session") == c["session"]), {})
            stand_in, real = "builtin/%s/" % slug(folder), "builtin/%s/" % slug(main or "")
            fix = lambda a: a.replace(stand_in, real) if main else a
            rot = lambda a: rot_of(memories[a], main) if a in memories and main else None
            results.append(dict(
                case=c["id"], ms=round(ms, 1), terms=recall.get("terms", 0), index_size=recall.get("index_size", 0),
                blind=bool(c["repo"]) and not main,
                skipped=next((e["reason"] for e in events if e.get("event") == "skipped" and e.get("session") == c["session"]), None),
                candidates=[dict(x, address=fix(x["address"]), rot=rot(fix(x["address"]))) for x in recall.get("candidates", [])],
                injected=[fix(a) for a in recall.get("injected", [])], dropped=recall.get("dropped", {}),
                errors=[e.get("error", "")[:60] for e in events if e.get("event") == "error"], memories=memories))
    finally:
        shutil.rmtree(tmp)
    return results


def load_labels():
    path = os.path.join(EVAL, "labels.jsonl")
    return {(r["case"], r["address"], r["text_hash"]): r["label"] for r in (read(path) if os.path.exists(path) else [])}


def label_candidates(cases, results, by_sid, label_fn):
    """Ticket 16: every logged candidate gets a label, reused until the memory's text changes. The judge is called
    only for candidates with no label yet, one call per case."""
    labels = load_labels()
    if not label_fn:
        return labels
    with open(os.path.join(EVAL, "labels.jsonl"), "a") as fh:
        for c, r in zip(cases, results):
            todo = [x for x in r["candidates"] if (c["id"], x["address"], x["text_hash"]) not in labels]
            if not todo:
                continue
            s = by_sid.get(c["session"])
            earlier = [t["ask"] for t in s["turns"] if t["real"] and t["at"] < c["at"]][-2:] if s else []
            for row in label_fn(c, todo, r["memories"], earlier):
                labels[(row["case"], row["address"], row["text_hash"])] = row["label"]
                fh.write(json.dumps(row) + "\n")
    return labels


def kind_of(result, address):
    path = result["memories"].get(address)
    kind = front(path, "type") or "memory" if path else "?"
    return "builtin " + kind if address.startswith("builtin/") else kind


def gate_metrics(cases, results, labels, gate=None, rot=None, min_rows=0):
    """Precision, misses and false injections (ticket 16) at the binary's own gate (None), or with the top 3
    candidates at `gate` or above injected in an index of `min_rows` rows or more, the 1,040-character cap left out.
    `rot` "any" or "all" also drops the candidates that rule would drop (rot_of)."""
    m = dict(injections=0, useful=0, unlabeled=0, misses=0, false=0, cases=0, kinds=Counter(), kinds_useful=Counter())
    for c, r in zip(cases, results):
        label = lambda x: labels.get((c["id"], x["address"], x["text_hash"]))
        useful = {x["address"] for x in r["candidates"] if label(x) == "useful"}
        kept = [x for x in r["candidates"] if not (rot and x.get("rot") in {"any": ("any", "all"), "all": ("all",)}[rot])]
        if gate is None:
            inj = [x for x in kept if x["address"] in r["injected"]]
        elif r.get("index_size", 0) >= min_rows:
            inj = [x for x in kept if x["score"] >= gate][:3]
        else:
            inj = []
        m["cases"] += bool(inj)
        m["injections"] += len(inj)
        m["useful"] += sum(label(x) == "useful" for x in inj)
        m["unlabeled"] += sum(label(x) is None for x in inj)
        m["misses"] += len(useful - {x["address"] for x in inj})
        m["false"] += len(inj) if inj and not useful else 0
        for x in inj:
            m["kinds"][kind_of(r, x["address"])] += 1
            m["kinds_useful"][kind_of(r, x["address"])] += label(x) == "useful"
    return m


def sweep(cases, results, labels, min_rows=0):
    """The gate threshold, calibrated offline over the logged scores (ticket 16) with the gate closed below `min_rows`
    rows: the most useful injections at a precision of 0.67 or more over at least 5 injections; fewer false
    injections, then the higher threshold, break ties. Without such a threshold, the most precise one."""
    grid = sorted({round(x["score"], 2) for r in results for x in r["candidates"]} | {0.0})
    rows = [(t, gate_metrics(cases, results, labels, t, min_rows=min_rows)) for t in grid]
    enough = [(t, m) for t, m in rows if m["injections"] >= 5]
    ok = [(t, m) for t, m in enough if m["useful"] / m["injections"] >= GOAL]
    if ok:
        pick = max(ok, key=lambda tm: (tm[1]["useful"], -tm[1]["false"], tm[0]))
    else:
        pick = max(enough or rows, key=lambda tm: (tm[1]["useful"] / max(tm[1]["injections"], 1), tm[0]))
    return rows, pick, bool(ok)


def line_idents(path):
    """The identifiers the injected line carries (tickets 19 and 21): the body's when it fits in 200 characters,
    else the description's plus the body's, in order, while the line stays under 200 characters."""
    with open(path, encoding="utf-8", errors="replace") as fh:
        text = fh.read()
    body = text[4:].partition("\n---")[2] if text.startswith("---\n") and "\n---" in text[4:] else text
    body = " ".join(body.split())
    if len(body) <= 200:
        return IDENT.findall(body)
    desc = front(path, "description")
    ids, room = IDENT.findall(desc), 200 - min(len(desc), 200) - 3
    for i in IDENT.findall(body):
        if i not in desc and i not in ids and len(i) + 2 <= room:
            ids.append(i)
            room -= len(i) + 2
    return ids


def used_signal(case, s, path):
    """Ticket 19 item 5 on a replayed case: the next assistant turn repeats an identifier from the injected line that
    the prompt did not contain. None when the transcript is gone."""
    t = next((t for t in s["turns"] if t["at"] == case["at"]), None) if s else None
    if not t:
        return None
    after = "\n".join(x for g in t["groups"] for x in g["text"]) + "\n" + "\n".join(json.dumps(i) for _, i in t["tools"])
    ids = set(line_idents(path)) - set(IDENT.findall(case["prompt"]))
    return any(i in after for i in ids)


def fnv(text):
    """FNV-1a 64, the hash the binary logs a prompt under."""
    h = 0xCBF29CE484222325
    for b in text.encode("utf-8"):
        h = ((h ^ b) * 0x100000001B3) & 0xFFFFFFFFFFFFFFFF
    return "%016x" % h


def live_expansions(by_sid):
    """Ticket 19: over the live recall log, the prompts that got an injection, and how many of them saw the model
    call `recall` on an injected address in that turn. The frame's tool sentence goes if the first 100 show none."""
    log = os.path.join(os.path.dirname(EVAL), "log", "openrecall.jsonl")
    prompts = expanded = 0
    for e in read(log) if os.path.exists(log) else []:
        if e.get("event") != "recall" or not e.get("injected"):
            continue
        s = by_sid.get(e.get("session"))
        t = next((t for t in s["turns"] if fnv(t["ask"]) == e.get("prompt_hash")), None) if s else None
        if not t:
            continue
        prompts += 1
        expanded += any(n == MCP_RECALL and (i or {}).get("query") in e["injected"] for n, i in t["tools"])
    return prompts, expanded


def live_pr_urls(by_sid, label_fn=None, version="0.4.0"):
    """Issue 13's live line: the real prompts with a PR URL in the recall log since the first `update` event of the
    binary step from or to `version`. An injection is an injected address that is also a candidate, so a pushed
    handoff record is not one. `label_fn` labels each injection once, in labels.jsonl."""
    orhome = os.path.dirname(EVAL)
    log = os.path.join(orhome, "log", "openrecall.jsonl")
    events = read(log) if os.path.exists(log) else []
    start = min((e["at"] for e in events if e.get("event") == "update" and e.get("step") == "binary"
                 and version in (e.get("from"), e.get("to"))), default=None)
    if start is None:
        return "- Live PR URL prompts (issue 13): binary %s is not in the recall log yet, so nothing is counted." % version
    cases, results = [], []
    for e in events:
        s = by_sid.get(e.get("session")) if e.get("event") == "recall" and e.get("at", 0) >= start else None
        t = next((t for t in s["real"] if fnv(t["ask"]) == e.get("prompt_hash")), None) if s else None
        if not t or not PR_URL.search(t["ask"]):
            continue
        injected = [x for x in e.get("candidates", []) if x["address"] in e.get("injected", [])]
        paths = {x["address"]: address_path(x["address"], orhome) for x in injected}
        cases.append(dict(id="live:%s:%s" % (e["session"], e["prompt_hash"]), session=e["session"], at=t["at"],
                          prompt=t["ask"]))
        results.append(dict(candidates=injected, memories={a: p for a, p in paths.items() if os.path.exists(p)}))
    labels = label_candidates(cases, results, by_sid, label_fn)
    injections = [(c, x) for c, r in zip(cases, results) for x in r["candidates"]]
    got = Counter(labels.get((c["id"], x["address"], x["text_hash"])) for c, x in injections)
    changed = 0
    for c, x in injections:
        path = address_path(x["address"], orhome)
        changed += not os.path.exists(path) or os.path.getmtime(path) > ts_of(c["at"]).timestamp()
    return ("- Live PR URL prompts (issue 13), in the recall log since binary %s at %s: %d prompts, %d with an injection, "
            "%d injections, precision %s. Labels: useful %d, partly %d, noise %d, unlabeled %d. Injected memories whose "
            "file changed after the prompt: %d." % (
                version, when(start), len(cases), sum(bool(r["candidates"]) for r in results), len(injections),
                rate(got["useful"], len(injections)), got["useful"], got["partly"], got["noise"], got[None], changed))


def agreement(rows):
    """Jev against the hand labels of spec Appendix A (ticket 09): rows of {case, hand, jev}."""
    judged = [r for r in rows if r.get("jev")]
    exact = sum(r["hand"] == r["jev"] for r in judged)
    useful = sum((r["hand"] == "useful") == (r["jev"] == "useful") for r in judged)
    return ("Jev against the %d hand labels of spec Appendix A: exact %s, useful-or-not %s%s. Per case: %s."
            % (len(rows), rate(exact, len(judged)), rate(useful, len(judged)),
               "" if len(judged) == len(rows) else ", %d not judged" % (len(rows) - len(judged)),
               ", ".join("%s hand %s jev %s" % (r["case"], r["hand"], r.get("jev")) for r in rows)))


def binary_gate():
    """The gate the binary was built with, read from src/index.rs beside this file: (threshold, the index size below
    which it stays closed)."""
    src = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "src", "index.rs")
    try:
        with open(src) as fh:
            text = fh.read()
        return (float(re.search(r"pub const GATE: f64 = ([\d.]+);", text).group(1)),
                int(re.search(r"pub const MIN_ROWS: i64 = (\d+);", text).group(1)))
    except (OSError, AttributeError):
        return None


def level2_lines(cases, results, labels, by_sid, gate, label_fn=None):
    """The report's level-2 section. `label_fn` labels the live injections on PR URL prompts (live_pr_urls)."""
    searched = [(c, r) for c, r in zip(cases, results) if not r["skipped"]]
    skips = Counter(r["skipped"] for r in results if r["skipped"])
    dropped = Counter()
    for r in results:
        dropped.update(r["dropped"])
    cands = [(c, x) for c, r in zip(cases, results) for x in r["candidates"]]
    got = Counter(labels.get((c["id"], x["address"], x["text_hash"])) for c, x in cands)
    at_gate = gate_metrics(cases, results, labels)
    min_rows = gate[1] if gate else 0
    rows, (pick, best), met = sweep(cases, results, labels, min_rows)
    calibration = os.path.join(EVAL, "calibration.jsonl")
    confirm, used = [], Counter()
    for c, r in zip(cases, results):
        for x in r["candidates"]:
            label = labels.get((c["id"], x["address"], x["text_hash"]))
            path = r["memories"].get(x["address"])
            fired = used_signal(c, by_sid.get(c["session"]), path) if path else None
            used[fired] += 1
            if x["address"] in r["injected"] or label == "partly" or (label == "noise" and fired):
                confirm.append("%s %s (%sjev %s, used signal %s)" % (c["id"], x["address"],
                                                                  "injected, " if x["address"] in r["injected"] else "", label, fired))
    live, expanded = live_expansions(by_sid)
    timings = sorted(r["ms"] for _, r in searched)
    step = max(1, len(rows) // 12)
    shown = sorted({rows[i][0] for i in range(0, len(rows), step)} | {pick, rows[-1][0]})
    lines = [
        "", "## Level 2: recalled memories", "",
        "Each of the %d frozen cases replayed through `openrecall recall` in its own stand-in world (ticket 16): a "
        "folder with the case's origin and branch, the repo's main checkout linked in for the path check, and the "
        "built-in memory files born before the prompt, with today's text. Searched: %d. Skipped by the binary: %s. "
        "No repo, so global memories only: %d. Main checkout unknown: %d."
        % (len(cases), len(searched), ", ".join("%s %d" % kv for kv in sorted(skips.items())) or "none",
           sum(1 for c in cases if not c["repo"]), sum(r["blind"] for r in results)),
        "",
        "- Candidates: %d over %d cases, the top 5 after the drops (dropped before ranking: %s). Labels: useful %d, "
        "partly %d, noise %d, unlabeled %d." % (len(cands), sum(1 for _, r in searched if r["candidates"]),
                                                  ", ".join("%s %d" % kv for kv in sorted(dropped.items())) or "none",
                                                  got["useful"], got["partly"], got["noise"], got[None]),
        "- Jev calibration: %s" % (agreement(read(calibration)[-len(HAND):]) if os.path.exists(calibration)
                                   else "not run (`python3 eval/judge.py calibrate`)."),
        "- Used signal over the injected line's identifiers (ticket 19): fired %d, silent %d, no transcript %d. To confirm by hand "
        "(injected, or Jev says partly, or noise while the signal fired): %d%s" % (used[True], used[False], used[None], len(confirm),
                                                                       ": " + "; ".join(confirm[:15]) + (" …" if len(confirm) > 15 else "") if confirm else "."),
        "- Live, from the recall log: %d prompts got an injection, and in %d of them the model then called `recall` on "
        "an injected address (ticket 19: the frame's tool sentence goes if the first 100 show none)." % (live, expanded),
        live_pr_urls(by_sid, label_fn),
        "",
        "At the binary's gate (%s), injections %d on %d cases: precision %s, misses %d, false injections %s, "
        "unlabeled %d. Goals: precision 0.67 or more, false injections 0."
        % ("%.2f and above, in an index of %d rows or more" % gate if gate else "?", at_gate["injections"], at_gate["cases"],
           rate(at_gate["useful"], at_gate["injections"]), at_gate["misses"], rate(at_gate["false"], at_gate["injections"]),
           at_gate["unlabeled"]),
        "",
        "| Memory type | Injections | Useful | Precision |", "|---|---|---|---|",
    ]
    lines += ["| %s | %d | %d | %s |" % (k, n, at_gate["kinds_useful"][k], rate(at_gate["kinds_useful"][k], n))
              for k, n in sorted(at_gate["kinds"].items())]
    lines += [
        "",
        "Threshold sweep over the logged scores, the top 3 candidates at the threshold or above injected in an index of "
        "%d rows or more (the 1,040-character cap left out). Pick: %.2f, %s." % (
            min_rows, pick, "precision %s with %d misses and %d false injections"
            % (rate(best["useful"], best["injections"]), best["misses"], best["false"])
            + ("" if met else "; no threshold reaches 0.67 over 5 or more injections")),
        "",
        "| Threshold | Injections | Cases | Precision | Misses | False injections |", "|---|---|---|---|---|---|",
    ]
    lines += ["| %.2f%s | %d | %d | %s | %d | %d |" % (t, " (pick)" if t == pick else "", m["injections"], m["cases"],
                                                        rate(m["useful"], m["injections"]), m["misses"], m["false"])
              for t, m in rows if t in shown]
    variants = ["%s: injections %d, precision %s, misses %d, false injections %d" % (
        name, v["injections"], rate(v["useful"], v["injections"]), v["misses"], v["false"])
        for name, v in (("any cited path missing", gate_metrics(cases, results, labels, pick, "any", min_rows)),
                        ("every cited path missing", gate_metrics(cases, results, labels, pick, "all", min_rows)))]
    lines += [
        "",
        "Rot (ticket 09): the binary drops a pointer fact whose path or symbol is gone; dropped in this replay: %d. Candidates "
        "citing a path the main checkout lacks: %d of %d (every cited path missing: %d). Dropping them too, at the "
        "pick: %s." % (dropped["rot"], sum(1 for _, x in cands if x.get("rot")), len(cands),
                       sum(1 for _, x in cands if x.get("rot") == "all"), "; ".join(variants)),
        "",
        "Replay latency of `openrecall recall` over the %d searched cases, process start included: p50 %.1f ms, "
        "p95 %.1f ms. Binary errors: %s." % (len(timings), pct(timings, 0.5), pct(timings, 0.95),
                                             ", ".join("%d × %s" % kv for kv in Counter(e for r in results for e in r["errors"]).items()) or "none"),
    ]
    return lines, pick


def push_is_right(s, push):
    """Ticket 13: the session shares an alias with the pushed record, or does 2 or more real turns on its branch."""
    return bool(push["aliases"] & s["tickets"]) or sum(t["branch_end"] == push["branch"] for t in s["real"]) >= 2


def verdict(s, push):
    """Ticket 13's test can never pass a push to a session with one real prompt and no shared alias: it is unjudged."""
    return "right" if push_is_right(s, push) else "unjudged" if len(s["real"]) < 2 else "wrong"


def is_late(s, push):
    """A turn before the push ended on its branch and edited a file or ran `git commit` in Bash."""
    return any(t["branch_end"] == push["branch"] and (n in EDITS or n == "Bash" and GIT_COMMIT.search(inp.get("command", "")))
               for t in s["turns"][:push["turn"]] for n, inp in t["tools"])


def live_routes():
    """The live recall log's `pushed` events by route, and the time of its first event."""
    log = os.path.join(os.path.dirname(EVAL), "log", "openrecall.jsonl")
    events = read(log) if os.path.exists(log) else []
    return Counter(e.get("how") for e in events if e.get("event") == "pushed"), min((e["at"] for e in events if "at" in e), default=None)


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
    return "%.0f%% (%d/%d, %.0f–%.0f%%)" % (100.0 * k / n, k, n, max(0.0, 100 * lo), 100 * hi) if n else "n/a"


def report(now=None, binary=BINARY, label_fn=None, gate=None, dedupe_fn=None):
    """`label_fn(case, candidates, memories, earlier_prompts)` returns label rows (judge.label_case); None labels
    nothing new. `gate` is binary_gate()'s (threshold, minimum index size), printed beside the binary's results; the
    sweep keeps its minimum. `dedupe_fn` checks written facts (judge.dedupe_check); None checks nothing new."""
    now = now or datetime.now(timezone.utc)
    earlier = sorted(d for d in glob.glob(os.path.join(EVAL, "runs", "*")) if os.path.isdir(d))
    since = datetime.strptime(os.path.basename(earlier[-1]), "%Y-%m-%dT%H%M%SZ").replace(tzinfo=timezone.utc) if earlier else None
    cases = read(os.path.join(EVAL, "cases.jsonl"))
    rows = read(os.path.join(EVAL, "pairs.jsonl"))
    sessions, recalled = load()
    by_sid = {s["sid"]: s for s in sessions}
    ran = os.path.exists(binary)
    pushes, timings, events, snaps = replay(binary, sessions, [(p["earlier"], p["new"]) for p in rows]) if ran else ({}, [], [], {})
    results = replay_cases(binary, cases, mains_of(cases)) if ran else []
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
    verdicts = defaultdict(Counter)
    for s, x in level1:
        verdicts[x.get("how")][verdict(s, x)] += 1
    late = sum(is_late(s, x) for s, x in level1)
    live, since_ms = live_routes()
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
        "OpenRecall %s, beside the baseline: Claude Code's built-in memory alone."
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
            "- By route: %s." % (", ".join("%s %d" % kv for kv in sorted(how.items())) or "none"),
            "- Right by route: %s." % ("; ".join("%s %d right, %d wrong, %d unjudged" % (k, v["right"], v["wrong"], v["unjudged"])
                                                 for k, v in sorted(verdicts.items())) or "none"),
            "- Late pushes, after a turn on the push's branch edited a file or ran `git commit`: %s. Edits by a subagent "
            "or by other Bash commands are not seen, so this is a lower bound." % rate(late, len(level1)),
            "- Live pushes by route, in the recall log%s: %s." % (
                " since " + when(since_ms) if since_ms else "", ", ".join("%s %d" % kv for kv in sorted(live.items())) or "none"),
            "- At real prompt 1: %d, 2: %d, 3: %d, later: %d." % tuple(
                sum(1 for _, x in level1 if (x["prompt"] == k if k < 4 else x["prompt"] >= 4)) for k in (1, 2, 3, 4)),
            "- Binary errors in the replay: %s." % (", ".join("%d × %s" % (n, e) for e, n in errors.items()) or "none"),
            "",
            "## Replay latency",
            "",
            "`openrecall recall` over %d real prompts, process start included: p50 %.1f ms, p95 %.1f ms. Replay timings "
            "only; live latency comes from the recall log." % (len(timings), pct(timings, 0.5), pct(timings, 0.95)),
        ]
        labels = label_candidates(cases, results, by_sid, label_fn)
        lines += level2_lines(cases, results, labels, by_sid, gate, label_fn)[0]
    lines += extraction_lines(since, dedupe_fn)
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
    if ran:
        with open(os.path.join(out, "results.jsonl"), "w") as fh:
            fh.writelines(json.dumps(r) + "\n" for r in results)
    print("\n".join(lines) + "\n\nwritten to %s/report.md" % out)


if __name__ == "__main__":
    cmd, rest = (sys.argv[1] if len(sys.argv) > 1 else ""), sys.argv[2:]
    if cmd == "freeze" and not rest:
        freeze()
    elif cmd == "report" and rest[:1] in ([], ["--binary"]) and len(rest) in (0, 2):
        import judge
        report(binary=os.path.abspath(rest[1]) if rest else BINARY, label_fn=judge.label_case, gate=binary_gate(),
               dedupe_fn=judge.dedupe_check)
    else:
        sys.exit(__doc__)
