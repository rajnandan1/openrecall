# OpenRecall

A recall layer for Claude Code. When a new session picks up a task, OpenRecall hands it the state the last session left: the goal, the identifiers (tickets, PRs, commits, paths, commands), the last ask and the last answer. Nothing on the prompt path calls a model; the hook adds a few milliseconds.

## Install

macOS on Apple silicon, with Rust installed.

```sh
cargo install --locked --git https://github.com/rajnandan1/openrecall --root ~/.local
claude plugin marketplace add rajnandan1/openrecall
claude plugin install openrecall@openrecall
```

`~/.local/bin` must be on your `PATH`. A session that cannot find `openrecall` says so when it starts.

To update, run the same `cargo install` with `--force`, then `claude plugin update openrecall@openrecall`.

## What it does

- At the end of each turn, a background writer folds the turn into its task's handoff record, `~/.openrecall/repos/<host>/<owner>/<repo>/handoffs/<branch>.md`. Secrets are redacted before anything is written.
- When a new session proves which task it is on (a prompt names one of the record's tickets, or the branch stays the same for one completed turn), the record is added to that prompt's context, once per context window.
- A record nobody wrote for 7 days retires to `handoffs/retired/`.
- Each prompt also searches the repo's memories: OpenRecall's own facts under `~/.openrecall/repos/<host>/<owner>/<repo>/`, the repo's Claude Code memory files (`~/.claude/projects/<slug>/memory/`, read-only, `MEMORY.md` left out) and `~/.openrecall/global/`. At most 3 memories, about 400 tokens, only above a calibrated score, each as one line: `- <type> <date> <address>: <text>`. A memory is never injected twice in one context window, never to the session that wrote it, and a pointer whose path is gone is dropped. The index, `~/.openrecall/index.db`, is a cache: it follows the Markdown files on every prompt and can be deleted at any time.
- A prompt under 4 words with no identifier, or a slash command with no arguments, is not searched. Task notifications and subagent prompts are skipped.

`OPENRECALL=0` turns it off for a session. Headless `claude -p` sessions are always skipped.

## Statusline

Each prompt writes one line, such as `recall 1 · 4 ms` (memories injected, handoff record included), to `~/.openrecall/status/<session_id>`. To show it, add two lines to your own statusline script:

```sh
sid=$(printf '%s' "$input" | jq -r .session_id)
or=$(cat "${OPENRECALL_HOME:-$HOME/.openrecall}/status/$sid" 2>/dev/null)
```

`third_party/gitleaks/` holds gitleaks v8.30.1's default rules, under the MIT license next to them.
