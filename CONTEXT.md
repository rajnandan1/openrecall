# OpenRecall

A recall layer for Claude Code: it hands a session the right past context at the right moment, without slowing the prompt.

## Language

### Memories

**Memory**:
A stored fact about a repo or a task that a later session may need.
_Avoid_: note, entity, event, chat summary

**Fact**:
A memory of one of five types: decision, preference, pointer, state, gotcha. A built-in memory is not a fact; it keeps Claude Code's own type.
_Avoid_: item, insight, learning

**Pointer**:
A fact that says where something lives: a repo-relative path plus a symbol name, never a line number.
_Avoid_: reference, citation, location

**Rot**:
The state of a pointer whose cited path no longer exists, or whose code symbol none of its cited source files still contains. A rotted pointer is never injected.
_Avoid_: stale, broken link, drift

**State**:
A fact about the current status or next step of a ticket or PR. It expires.
_Avoid_: progress, status note

**Gotcha**:
A fact about surprising behavior someone discovered.
_Avoid_: lesson, tip, warning

**Identifier**:
An exact token that names a thing: ticket ID, PR number, file path, symbol, commit hash. The strongest recall key.
_Avoid_: keyword, entity, tag

**Address**:
A memory's stable ID that Claude can quote to expand or forget the memory. It names the memory's file, so renaming the file changes it and editing the text does not. Built-in memories and handoff records have one too.
_Avoid_: URI, link, handle, name

**Built-in memory**:
Claude Code's own per-project memory files and their MEMORY.md index. OpenRecall indexes the topic files read-only; it never writes there and never copies them.
_Avoid_: native memory, Claude memory

**Global memory**:
A memory in no repo's scope. Every session may recall it.
_Avoid_: shared memory, user memory (that is level 3)

**Home**:
The one directory on a machine that holds all of OpenRecall's state: memories, handoff records, the index and the log.
_Avoid_: data dir, store, cache

**Index**:
The searchable cache built from the memory files. The Markdown is the source of truth; the index can be deleted and rebuilt at any time.
_Avoid_: database, store

### Levels

**Level**:
A memory's rank for recall: live (0), handoff (1), project (2), user (3). A higher level is recalled first and has its own token budget.
_Avoid_: tier, layer, priority

**Handoff**:
The moment a new session takes over a task from an earlier session, usually in the same worktree.
_Avoid_: resume, continuation, context switch

**Handoff record**:
The level-1 memory that holds a task's current state: its goal, its identifiers, the last ask and the last answer (which carries now and next). Anyone may add a section by hand; it is kept.
_Avoid_: session summary, working memory, snapshot

**Task**:
What a handoff record belongs to: one branch of one repo identity, also known by its aliases. A trunk branch is never a task.
_Avoid_: work item, job, worktree

**Alias**:
A ticket ID that names a task, learned from the user's prompts.
_Avoid_: tag, label, ticket key

**Folder**:
The worktree root, the directory that holds `.git`. Never the hook's working directory, which wanders.
_Avoid_: cwd, directory, path

**Settled**:
The state of a session's branch after one completed turn on it, counted from the session's start or from its last branch change. Only a settled branch can match a handoff record by folder and branch.
_Avoid_: stable, confirmed

**Task hop**:
A session that switches to a new task's branch after it starts, so the branch it started on belongs to the previous task. A rename keeps the task; a hop changes it.
_Avoid_: branch switch, context switch

**Retired**:
The state of a handoff record whose task went idle. A retired record is never injected and never revived; a returning task gets a new record.
_Avoid_: expired, archived, stale

**Scope**:
The set of memories a session may recall: those of its repo identity, when it has one, plus global ones.
_Avoid_: namespace, project, folder

**Repo identity**:
The key that names a repository across all its worktrees. It comes from the repo's origin remote, or from its main checkout when there is no remote.
_Avoid_: project slug, folder path, workspace

### Recall

**Recall**:
Finding the memories that match a prompt or a session start.
_Avoid_: search, lookup, retrieval (retrieval is only the index query step inside recall)

**Prompt path**:
The work that runs between the user pressing enter and Claude starting to answer. It has a hard time budget and no LLM calls.
_Avoid_: hot path, critical path

**Skip rule**:
A rule that stops OpenRecall for a session or a prompt. It is either a session skip or a prompt skip.
_Avoid_: blacklist, exclusion, filter

**Session skip**:
A skip rule that turns off every hook and every write for a whole session, such as a headless session. Nothing is recorded, not even the skip.
_Avoid_: disabled session, opt-out

**Prompt skip**:
A skip rule that drops recall for one prompt, such as a task notification or a prompt too short to search on. The skip is logged; a short prompt is still a turn.
_Avoid_: ignored prompt, filtered prompt

**Task notification**:
A user turn that Claude Code sends itself when background work finishes. It is not a prompt: it never triggers recall and never counts as a turn.
_Avoid_: system prompt, background message

**Real prompt**:
A prompt the user typed that reaches the model. A task notification is not one, and neither is a built-in command such as `/model` or `/effort`, which never fires a hook.
_Avoid_: user message, turn (a turn also follows a task notification)

**Headless session**:
A session started by `claude -p` or the SDK, with no person typing. OpenRecall does nothing in it.
_Avoid_: batch session, non-interactive run, sdk session

**Gate**:
The rule that decides whether a recalled memory is confident enough to inject. Silence is the default.
_Avoid_: threshold (that is the number inside the gate), filter

**Injection**:
Handing recalled memories to Claude as extra context.
_Avoid_: digest, push (only when contrasting with pull)

**Session ledger**:
The per-context-window record of which memories were already injected, and which handoff records the window created, so none is injected twice. It is cleared with the context, on clear and compact.
_Avoid_: seen-set, cache

### Writing

**Capture**:
Writing the handoff record at the end of each turn, without an LLM: identifiers from the turn's tool calls and assistant text, the last ask, the last answer. Until a session is pushed a record that others wrote, it adds only identifiers and aliases, behind the record's own.
_Avoid_: extraction, snapshot

**Extraction**:
Turning a session's new turns into facts, off the prompt path, once the session has ended or gone quiet.
_Avoid_: capture, summarization, ingestion

**Dedupe**:
The step in extraction that compares each proposed fact with the existing memories most like it, and writes the fact new, skips it as a repeat, or replaces an existing fact with a merged text. Built-in memories, global memories and handoff records are never replaced.
_Avoid_: merge, consolidation, cleanup

**Provider**:
The LLM service that extraction sends a session to, chosen by the user. OpenRecall has no default provider; with none set, extraction is off.
_Avoid_: vendor, backend, model (one provider serves many models)

**Quiet**:
A session whose transcript has not changed for 30 minutes. A quiet session can still resume; its later turns are extracted when it goes quiet again.
_Avoid_: idle, finished, stale

**Cursor**:
How far extraction has read a session's transcript. The next run sends the whole session again, with the part before the cursor marked as context, and asks only for facts from the turns past it.
_Avoid_: offset, checkpoint, watermark

**Strike**:
A failed extraction run that is the session's own fault: too long for the model, a reply still invalid after one retry, a reply cut off or filtered. The cursor stays; after 3 strikes the session is marked failed and never taken again. Any other failure stops the whole run instead, with every cursor where it was.
_Avoid_: error, retry

**Suggestion**:
A line that extraction proposes for a repo's CLAUDE.md, kept in a per-repo file for the user to copy by hand. It is never a fact and never recalled.
_Avoid_: rule (a rule lives in CLAUDE.md), tip, recommendation

**Secret scan**:
The check every text OpenRecall stores or sends goes through, using gitleaks' rules. A hit drops a fact; in a handoff record or in extraction's input it is replaced by `[REDACTED:<rule-id>]`.
_Avoid_: sanitizer, secret filter, DLP

### Measuring

**Eval set**:
The cases and handoff pairs, with their labels, used to measure precision, misses, false injections, carry-over and latency.
_Avoid_: test set, benchmark, fixtures

**Case**:
One real prompt in the eval set, with the repo, branch and session it came from.
_Avoid_: test case, example, sample

**Handoff pair**:
An earlier and a later session of the same task. Carry-over is measured over handoff pairs.
_Avoid_: session pair, handoff (that is the event, not the pair)

**Candidate**:
A memory the search scored for a prompt, whether or not the gate let it through.
_Avoid_: hit, result, match

**Label**:
The verdict on one candidate for one case: useful, partly, or noise. Only useful counts toward precision.
_Avoid_: rating, grade, verdict

**Precision**:
The share of injections labeled useful in the eval set.
_Avoid_: accuracy, hit rate

**Miss**:
A candidate labeled useful that the gate did not inject.
_Avoid_: false negative, gap

**Used signal**:
The mechanical check that an injected memory was used: the next assistant turn quotes its address, expands it, or repeats one of its identifiers that the prompt lacked. It only sends a label to a person when it disagrees with the judge.
_Avoid_: citation check, usage metric

**False injection**:
An injection on a case where no candidate is labeled useful.
_Avoid_: false positive, noise (that is a label)

**Judge**:
Whoever produces a label: a person, or Jev through the eval harness.
_Avoid_: labeler, grader, oracle

**Baseline**:
What Claude Code's built-in memory alone achieves on the same measure. Every number is reported next to it.
_Avoid_: control, zero point

**Replay**:
Running a past case or handoff pair through OpenRecall after the fact, seeing only the memories that existed at the time.
_Avoid_: simulation, backtest, rerun

**Final state**:
The identifiers in an earlier session's last five assistant messages before a handoff. Carry-over is measured against it.
_Avoid_: end state, last state

**Carry-over**:
The share of an earlier session's final-state identifiers that reach a new session on the same task by its third real prompt, without the user retyping them.
_Avoid_: recall rate, hit rate, continuity score
