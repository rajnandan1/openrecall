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
- Each prompt also searches the repo's memories: OpenRecall's own facts under `~/.openrecall/repos/<host>/<owner>/<repo>/`, the repo's Claude Code memory files (`~/.claude/projects/<slug>/memory/`, read-only, `MEMORY.md` left out) and `~/.openrecall/global/`. At most 3 memories, about 400 tokens, only above a calibrated score, each as one line: `- <type> <date> <address>: <text>`. A memory is never injected twice in one context window, never to the session that wrote it, and a state fact past its `expires` is dropped, as is a pointer whose path is gone or whose backticked code symbol (`snake_case`, `camelCase`, `Type::item`, `call()`) none of its cited source files contains any more. The index, `~/.openrecall/index.db`, is a cache: it follows the Markdown files on every prompt and can be deleted at any time.
- A prompt under 4 words with no identifier, or a slash command with no arguments, is not searched. Task notifications and subagent prompts are skipped.
- A recalled line ends with an address. The MCP tool `recall` reads the whole memory at an address, or searches with any other words; `remember` stores a fact (type decision, preference, pointer, state or gotcha; scope `repo` or `global`), and `forget` deletes one of OpenRecall's own facts by address. The tools act only when asked: Claude sees them as `mcp__plugin_openrecall_openrecall__recall`, `__remember` and `__forget`. A `remember` whose text holds a secret is refused.
- In Claude Code's "don't ask" permission mode, the MCP tools are denied until `mcp__plugin_openrecall_openrecall__recall`, `__remember` and `__forget` are allowed.

`OPENRECALL=0` turns it off for a session. Headless `claude -p` sessions are always skipped; the MCP tools still answer there, since they act only when asked.

## Extraction

OpenRecall's own facts come from `remember` and from a background worker that reads each finished session and writes the decisions, preferences, pointers, state and gotchas a later session would still need. It never runs on the prompt path. It is off until you give it a provider:

```sh
mkdir -p ~/.openrecall && umask 077
pbpaste > ~/.openrecall/api-key      # the key alone; the worker refuses the file if group or others can read it
printf 'base_url = "https://openrouter.ai/api/v1"\nmodel = "anthropic/claude-sonnet-5.5"\n' > ~/.openrecall/extract.toml
```

Any endpoint that speaks OpenAI's `POST <base_url>/chat/completions` with a JSON schema works. Write the model as its full ID, never an alias. Do not export the key in a shell profile.

- The worker starts at every session start and end, takes each session that ended or has been quiet for 30 minutes, and reads only what it has not read before, with the earlier turns as context. One worker runs at a time.
- It sends the session condensed (prompts, Claude's text, one line per tool call, no tool output), with secrets redacted, through `/usr/bin/curl`. The key goes to curl on stdin, never on a command line. The provider sees the session text; with OpenRouter, turn on "deny data collection" in its privacy settings.
- A second call on the same model compares each fact with the 10 closest memories and decides new, skip or replace. A replace rewrites the fact in place and keeps the old text in `replaced/`. Claude Code's memory files and global memories are never replaced. A state fact stops being recalled 14 days after it was written or replaced.
- Standing rules the model finds go to `~/.openrecall/repos/<host>/<owner>/<repo>/claude-md-suggestions.md` for you to copy into CLAUDE.md. That file is never recalled.
- There is no spend cap in OpenRecall. With Claude Sonnet 5.5 through OpenRouter, both calls cost about $0.085 a session; set a credit limit on the key if you want one. Every run logs its tokens and cost to `~/.openrecall/log/openrecall.jsonl`.

## Statusline

Each prompt writes one line, such as `recall 1 · 4 ms` (memories injected, handoff record included), to `~/.openrecall/status/<session_id>`. To show it, add two lines to your own statusline script:

```sh
sid=$(printf '%s' "$input" | jq -r .session_id)
or=$(cat "${OPENRECALL_HOME:-$HOME/.openrecall}/status/$sid" 2>/dev/null)
```

`third_party/gitleaks/` holds gitleaks v8.30.1's default rules, under the MIT license next to them.
