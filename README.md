# OpenRecall

You stop a Claude Code session halfway through a task. The next session starts blank. It does not know the ticket, the PR, the commands that worked, or what you asked last, so you spend the first few prompts typing it all again.

OpenRecall carries that over. When a new session picks up a task, OpenRecall hands it the state the last session left: the goal, the identifiers (tickets, PRs, commits, paths, commands), the last ask and the last answer. It also finds the few stored memories that match each prompt.

It is a Claude Code plugin plus one small Rust binary. Nothing on the prompt path calls a model; the hook adds a few milliseconds. Everything lives as plain Markdown under `~/.openrecall/`, and nothing leaves your machine unless you turn on [extraction](#extraction-optional).

## What Claude sees

A made-up example. Yesterday a session worked on ticket ABC-12 on the branch `abc-12-empty-rows`. Today you open a new session in the same repo and type `pick up ABC-12, the Windows job still times out`. Before Claude reads your prompt, OpenRecall adds this to its context:

```
Handoff record for branch abc-12-empty-rows, last written 2026-09-30T17:42:08Z by an earlier session on this task (github.com/acme/exporter/handoffs/abc-12-empty-rows). It reflects what was true then; check the working tree before acting on it.

## Goal
Fix ABC-12 so exports stop failing on empty rows
## Identifiers
tickets: ABC-12
prs: #48
paths: src/export.rs, tests/export.rs
commands:
- cargo test export
## Last ask
open the PR and wait for CI
## Last answer
Opened #48. CI passes except the Windows job, which times out in `cargo test export`. Next: find out why Windows is slow.

Recalled memories from earlier sessions (OpenRecall). They reflect what was true when written. Full text: mcp__plugin_openrecall_openrecall__recall with the address.
- gotcha 2026-09-28 github.com/acme/exporter/windows-runner-timeout: The Windows CI runner kills any single test that runs longer than 60 s.
```

Claude starts with the PR number, the failing command and a likely cause, and you did not retype any of it.

## Install

You need macOS on Apple silicon, with Rust installed.

```sh
# Build the openrecall binary from this repo and put it in ~/.local/bin.
# --locked builds with the exact dependency versions in Cargo.lock.
cargo install --locked --git https://github.com/rajnandan1/openrecall --root ~/.local

# Add this repo as a plugin marketplace, so Claude Code can find the plugin.
claude plugin marketplace add rajnandan1/openrecall

# Install the plugin: the hooks and the MCP server, which all run that binary.
claude plugin install openrecall@openrecall
```

The plugin is only config: every hook and the MCP server run `openrecall`, so `~/.local/bin` must be on your `PATH`. A session that cannot find `openrecall` says so when it starts.

To check that it works, open a session in a git repo on any branch except `main` or `master`, send one prompt, and let Claude answer. A handoff record for that branch shows up:

```sh
find ~/.openrecall/repos -path '*/handoffs/*.md'
```

### Update

Update the binary and the plugin together, then restart Claude Code:

```sh
# Rebuild the binary from the latest commit. --force replaces the one you have.
cargo install --locked --force --git https://github.com/rajnandan1/openrecall --root ~/.local

# Fetch the latest plugin list from GitHub, then update the plugin.
claude plugin marketplace update openrecall
claude plugin update openrecall@openrecall
```

If the binary and the plugin versions drift apart, the next session start tells you, with the command to run.

## How it works

### Handoff records

A task is one branch of one repo. `main` and `master` are never tasks.

- At the end of each turn, a background writer folds the turn into its task's handoff record, `~/.openrecall/repos/<host>/<owner>/<repo>/handoffs/<branch>.md`. A `/` in the branch name becomes `--` in the file name. Secrets are redacted before anything is written.
- When a new session proves which task it is on (a prompt names one of the record's tickets, or the branch stays the same for one completed turn), the record is added to that prompt's context, once per context window.
- Claude gets at most about 800 tokens of the record. A longer record is cut and ends with `[...]`.
- The record is plain Markdown. You can add your own section by hand, and it is kept.
- A record nobody wrote for 7 days retires to `handoffs/retired/`.

### Memory search

Each prompt also searches three places:

- OpenRecall's own facts, under `~/.openrecall/repos/<host>/<owner>/<repo>/`
- the repo's Claude Code memory files, `~/.claude/projects/<slug>/memory/` (read-only, `MEMORY.md` left out)
- global memories, under `~/.openrecall/global/`

It injects at most 3 memories, about 400 tokens in all, and only memories that score 4.0 or more. If nothing scores that high, it injects nothing. Each memory arrives as one line: `- <type> <date> <address>: <text>`. The text is the whole memory when it fits in the room the other lines leave in those 400 tokens. Otherwise it is at most 200 characters.

These limits are not settings. They are constants in `src/index.rs`: `MAX_LINES` (3), `MAX_CHARS` (1040 characters, about 400 tokens) and `GATE` (4.0, the score cut-off the eval picked). To change one, edit it in a clone and reinstall from there:

```sh
cargo install --locked --force --path . --root ~/.local
```

A memory is left out when:

- it was already injected in this context window, or the same session wrote it
- it is a state fact past its `expires`
- it is a pointer whose path is gone, or whose backticked code symbol (`snake_case`, `camelCase`, `Type::item`, `call()`) none of its cited source files contains any more

A prompt under 4 words with no identifier, or a slash command with no arguments, is not searched. Task notifications and subagent prompts are skipped.

The index, `~/.openrecall/index.db`, is a cache: it follows the Markdown files on every prompt and can be deleted at any time.

### MCP tools

The hooks only push, and they push very little: one handoff record and at most 3 one-line memories. That keeps prompts fast and Claude's context clean. But sometimes Claude needs the full text of a memory, or a memory the score cut-off held back, or you want to save something right now. A hook cannot help there, because Claude cannot call a hook. Claude can call an MCP tool.

So the plugin also starts an MCP server, `openrecall mcp`. It is the same binary reading the same `~/.openrecall/`, and installing the plugin sets it up. It gives Claude three tools:

- `recall` reads the whole memory at an address, or searches with any other words. A recalled line that does not fit whole is cut to 200 characters and carries its address, so this is how Claude gets the rest. A search here skips the score cut-off and returns 5 memories, or up to 20 if Claude asks for more.
- `remember` stores a fact: type `decision`, `preference`, `pointer`, `state` or `gotcha`; scope `repo` or `global`. A text that holds a secret is refused. Without [extraction](#extraction-optional), this is the only way OpenRecall's own facts get written.
- `forget` deletes one of OpenRecall's own facts by address.

The tools act only when asked. You do not call them yourself: tell Claude something like "remember that the Windows runner kills tests after 60 s" and it calls `remember`.

Claude sees them as `mcp__plugin_openrecall_openrecall__recall`, `__remember` and `__forget`. In Claude Code's "don't ask" permission mode, the tools are denied until you allow them, for example in `~/.claude/settings.json`:

```json
{
  "permissions": {
    "allow": [
      "mcp__plugin_openrecall_openrecall__recall",
      "mcp__plugin_openrecall_openrecall__remember",
      "mcp__plugin_openrecall_openrecall__forget"
    ]
  }
}
```

## Extraction (optional)

Without extraction, OpenRecall's own facts come only from `remember`. With it, a background worker reads each finished session and writes the decisions, preferences, pointers, state and gotchas a later session would still need. It never runs on the prompt path.

Extraction sends your session text to an LLM provider you choose, and it costs money. So it is off until you write two files: `api-key` with your key, and `extract.toml` with where to send it.

1. Create the folder, if the plugin has not already.
2. Write your key to `~/.openrecall/api-key`, alone on one line. Replace `YOUR_API_KEY` with the real key.
3. Make that file readable only by you. The worker refuses a key file that group or others can read.
4. Write `~/.openrecall/extract.toml` with the endpoint and the model.

```sh
# 1. The folder OpenRecall keeps everything in.
mkdir -p ~/.openrecall

# 2. The key alone; the worker trims the trailing newline echo adds.
echo 'YOUR_API_KEY' > ~/.openrecall/api-key

# 3. Owner can read and write, nobody else can do anything.
chmod 600 ~/.openrecall/api-key

# 4. base_url: the provider's OpenAI-style API root. model: the model's full ID.
cat > ~/.openrecall/extract.toml <<'EOF'
base_url = "https://openrouter.ai/api/v1"
model = "anthropic/claude-sonnet-5.5"
EOF
```

The `echo` line leaves the key in your shell history. Delete that entry afterwards, or open the file in an editor and paste the key there instead.

Any endpoint that speaks OpenAI's `POST <base_url>/chat/completions` with a JSON schema works. Write the model as its full ID, never an alias. Do not export the key in a shell profile.

To check it, start a new Claude session and end it. The worker logs each run, or the reason extraction is off, to `~/.openrecall/log/openrecall.jsonl`.

The worker starts at every session start and end, takes each session that ended or has been quiet for 30 minutes, and reads only what it has not read before, with the earlier turns as context. One worker runs at a time.

It sends the session condensed (prompts, Claude's text, one line per tool call, no tool output), with secrets redacted, through `/usr/bin/curl`. The key goes to curl on stdin, never on a command line. The provider sees the session text; with OpenRouter, turn on "deny data collection" in its privacy settings.

To avoid duplicates:

- A second call on the same model compares each fact with the 10 closest memories and decides new, skip or replace. A replace rewrites the fact in place and keeps the old text in `replaced/`.
- Claude Code's memory files and global memories are never replaced.
- A state fact stops being recalled 14 days after it was written or replaced.

Standing rules the model finds go to `~/.openrecall/repos/<host>/<owner>/<repo>/claude-md-suggestions.md` for you to copy into CLAUDE.md. That file is never recalled.

What it costs: there is no spend cap in OpenRecall. With Claude Sonnet 5.5 through OpenRouter, both calls cost about $0.085 a session; set a credit limit on the key if you want one. Every run logs its tokens and cost to `~/.openrecall/log/openrecall.jsonl`.

## Statusline

Each prompt writes one line, such as `recall 1 · 4 ms` (memories injected, handoff record included), to `~/.openrecall/status/<session_id>`. To show it, add two lines to your own statusline script, then print `$or` wherever you want it:

```sh
sid=$(printf '%s' "$input" | jq -r .session_id)
or=$(cat "${OPENRECALL_HOME:-$HOME/.openrecall}/status/$sid" 2>/dev/null)
```

## Turning it off

- For one session: start Claude with `OPENRECALL=0 claude`.
- Headless `claude -p` sessions are always skipped. The MCP tools still answer there, since they act only when asked.

To remove it completely:

```sh
claude plugin uninstall openrecall@openrecall
cargo uninstall --root ~/.local openrecall
rm -rf ~/.openrecall      # every fact, handoff record and setting OpenRecall wrote
```

The last line leaves Claude Code's own memory files alone; OpenRecall never writes there.

## Where things live

Everything sits under `~/.openrecall/`. Set `OPENRECALL_HOME` to put it somewhere else.

| Path | What it holds |
| --- | --- |
| `repos/<host>/<owner>/<repo>/` | OpenRecall's facts for that repo |
| `repos/<host>/<owner>/<repo>/handoffs/` | Handoff records, one per branch; `retired/` holds the idle ones |
| `repos/<host>/<owner>/<repo>/claude-md-suggestions.md` | Rules for you to copy into CLAUDE.md |
| `global/` | Memories every repo may recall |
| `index.db` | The search cache, safe to delete |
| `log/openrecall.jsonl` | One line per event, with tokens and cost for extraction runs |
| `status/<session_id>` | The statusline text |
| `api-key`, `extract.toml` | Extraction settings |

## Working on OpenRecall

```sh
cargo test                       # the binary's unit tests and hook tests
python3 eval/test_harness.py     # the eval harness's own tests
```

`CONTEXT.md` defines the words this README uses: handoff record, pointer, gate, extraction, and the rest.

## License

OpenRecall is under the MIT license, in `LICENSE`. `third_party/gitleaks/` holds gitleaks v8.30.1's default rules, under the MIT license next to them.
