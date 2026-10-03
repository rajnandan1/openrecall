<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/logo/openrecall-horizontal-on-dark.svg">
    <img src="assets/logo/openrecall-horizontal.svg" alt="OpenRecall" width="360">
  </picture>
</p>

# OpenRecall

OpenRecall gives a new Claude Code session the state that the last session on the same task left. A task is one branch of one repo.

Without OpenRecall, a new session starts blank. It does not know the ticket, the pull request (PR), the commands that worked, or what you asked last. So you type it all again in your first few prompts.

OpenRecall keeps a handoff record for each task. A handoff record holds the goal, the identifiers, the last ask and the last answer. Identifiers are exact names, such as tickets, PRs, commits, paths and commands. When a new session continues a task, OpenRecall gives it the handoff record. On each prompt, OpenRecall also finds the few stored memories that match the prompt.

OpenRecall is a Claude Code plugin and one small Rust binary, `openrecall`. The plugin holds hooks and an MCP server. A hook is a command that Claude Code runs at a fixed moment, for example when you send a prompt. An MCP server gives Claude tools that it can call. MCP stands for Model Context Protocol. Every hook and the MCP server run the `openrecall` binary.

OpenRecall calls no model between your prompt and the start of Claude's answer. The hook that runs on each prompt adds a few milliseconds. OpenRecall keeps your memories and handoff records as plain Markdown files under `~/.openrecall/`. Nothing leaves your machine unless you configure [extraction](#extraction-optional).

## What Claude sees

This example is made up. Yesterday a session worked on ticket ABC-12, on the branch `abc-12-empty-rows`. Today you open a new session in the same repo. You type `pick up ABC-12, the Windows job still times out`. Before Claude reads your prompt, OpenRecall adds this text to its context:

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

The first part is the handoff record for the branch. The last line is one recalled memory. Claude now starts with the PR number, the failing command and a likely cause. You did not type any of it again.

## Install

You need macOS on Apple silicon, with Rust installed.

```sh
# Build the openrecall binary from this repo into ~/.local/bin.
# --locked builds with the exact dependency versions in Cargo.lock.
cargo install --locked --git https://github.com/rajnandan1/openrecall --root ~/.local

# Add this repo as a plugin marketplace, so Claude Code can find the plugin.
claude plugin marketplace add rajnandan1/openrecall

# Install the plugin: the hooks and the MCP server. They all run the binary.
claude plugin install openrecall@openrecall
```

The plugin holds only configuration. Every hook and the MCP server run `openrecall`, so `~/.local/bin` must be on your `PATH`. If a session cannot find `openrecall`, the session tells you when it starts.

To check that OpenRecall works:

1. Open a Claude Code session in a git repo, on any branch except `main` or `master`.
2. Send one prompt.
3. Wait for Claude to finish its answer.
4. Run the command below. It lists the handoff records. One of them must be for your branch.

```sh
find ~/.openrecall/repos -path '*/handoffs/*.md'
```

### Update

Update the binary and the plugin together. Then restart Claude Code.

```sh
# Rebuild the binary from the latest commit. --force replaces the binary you have.
cargo install --locked --force --git https://github.com/rajnandan1/openrecall --root ~/.local

# Get the latest plugin list from GitHub. Then update the plugin.
claude plugin marketplace update openrecall
claude plugin update openrecall@openrecall
```

If the version of the binary does not match the version of the plugin, the next session tells you when it starts. It also gives you the command to run.

## How it works

### Handoff records

A task is one branch of one repo. `main` and `master` are never tasks.

At the end of each turn, OpenRecall updates the handoff record of the task in the background. A turn is one prompt and Claude's full answer to it. The record is the file `~/.openrecall/repos/<host>/<owner>/<repo>/handoffs/<branch>.md`. In the example above, `<host>/<owner>/<repo>` is `github.com/acme/exporter`. A `/` in the branch name becomes `--` in the file name.

Before OpenRecall writes a record, it replaces each secret with `[REDACTED:<rule-id>]`. It finds secrets with the rules of gitleaks, a secret scanner.

OpenRecall injects a handoff record only when the new session shows which task it is on. To inject means to add text to Claude's context before Claude reads your prompt. The session shows its task in one of two ways:

- A prompt names one of the tickets in the record. OpenRecall injects the record with that prompt.
- The session finishes one turn on the branch of the record, with no branch change. OpenRecall injects the record with the next prompt.

The rest of the rules for handoff records:

- OpenRecall injects each record at most once per context window.
- Claude gets at most about 800 tokens of the record. If the record is longer, OpenRecall cuts it and ends it with `[...]`.
- The record is plain Markdown. You can add your own section to it by hand. OpenRecall keeps your section when it updates the record.
- If no session updates a record for 7 days, OpenRecall retires the record. It moves the record to `handoffs/retired/`.

### Recalled memories

On each prompt, OpenRecall also looks for stored memories that match the prompt. This step is recall. Recall looks in three places:

- The facts that OpenRecall wrote for the repo, under `~/.openrecall/repos/<host>/<owner>/<repo>/`.
- Claude Code's built-in memory for the repo, in `~/.claude/projects/<slug>/memory/`. Here `<slug>` is the folder name that Claude Code gives the project. OpenRecall only reads these files, and it skips `MEMORY.md`.
- Global memories, under `~/.openrecall/global/`. A global memory belongs to no repo, so every repo may recall it.

A fact is a memory that OpenRecall writes itself. Each fact has one of five types:

- `decision` records what someone decided, and why.
- `preference` records how you want the work done.
- `pointer` records where something lives in the repo, as a path and a symbol name.
- `state` records the status or the next step of a ticket, PR or branch. A state fact expires 14 days after OpenRecall writes or replaces it.
- `gotcha` records surprising behavior that someone found.

Recall searches all three places through the index, `~/.openrecall/index.db`. The index is a cache of the Markdown files. OpenRecall updates the index from the files on every prompt. You can delete the index at any time.

Recall gives each memory it finds a score. The score tells how well the memory matches the prompt. Then a rule, the gate, decides which memories OpenRecall injects:

- The gate allows at most 3 memories, about 400 tokens in all.
- The gate allows only a memory with a score of 1.50 or more. This number is the threshold. If no memory reaches the threshold, OpenRecall injects nothing.
- The gate allows nothing while the index holds fewer than 10 memories. This count covers all repos together.

The score does not grow when the index grows. So one threshold works at every index size.

Each injected memory is one line: `- <type> <date> <address>: <text>`. The address is a stable ID for the memory, such as `github.com/acme/exporter/windows-runner-timeout`. Claude can use the address to read the full memory or to forget it. The text is the whole memory if it fits in the space that the other lines leave in the 400 tokens. If not, OpenRecall cuts the text to at most 200 characters.

These limits are not settings. They are constants in `src/index.rs`:

- `MAX_LINES` is 3. It is the most memories that OpenRecall injects on one prompt.
- `MAX_CHARS` is 1040 characters, about 400 tokens.
- `GATE` is 1.50, the threshold. The eval picked this number. The eval is the set of Python scripts in `eval/` that measure how well recall works.
- `MIN_ROWS` is 10. Below this index size, OpenRecall injects nothing.

To change a limit, edit its constant in a clone of this repo. Then reinstall from the clone:

```sh
cargo install --locked --force --path . --root ~/.local
```

OpenRecall does not inject a memory in these cases:

- OpenRecall already injected the memory in this context window.
- The same session wrote the memory.
- The memory is a state fact, and its `expires` date is in the past.
- The memory is a pointer, and the path that it cites no longer exists.
- The memory is a pointer, and none of the source files that it cites still contains its code symbol. A code symbol is a name in backticks, such as `snake_case`, `camelCase`, `Type::item` or `call()`.

OpenRecall does no recall for these prompts:

- A prompt of fewer than 4 words that holds no identifier.
- A slash command with no arguments.
- A task notification. Claude Code sends this message to itself when background work finishes.
- A prompt inside a subagent.

### MCP tools

The hooks inject very little: one handoff record, and at most 3 one-line memories per prompt. This keeps prompts fast and Claude's context small. But sometimes Claude needs more:

- Claude needs the full text of a memory.
- Claude needs a memory that the gate blocked.
- You want to save a fact now.

A hook cannot help here, because Claude cannot call a hook. Claude can call an MCP tool.

The plugin also starts an MCP server, `openrecall mcp`. The server is the same binary, and it reads the same `~/.openrecall/`. The plugin install configures the server, so you do nothing extra. The server gives Claude three tools:

- `recall` takes an address or some words. With an address, it returns the whole memory. An injected line that OpenRecall cut to 200 characters still carries its address. So Claude calls `recall` to read the rest. With other words, `recall` finds the memories that match and skips the gate. It returns 5 memories, or up to 20 if Claude asks for more.
- `remember` stores a new fact. Claude gives the fact a type and a scope. The type is `decision`, `preference`, `pointer`, `state` or `gotcha`. The scope is `repo` or `global`. `remember` refuses a text that holds a secret. Without [extraction](#extraction-optional), `remember` is the only way that OpenRecall gets new facts.
- `forget` deletes one fact that OpenRecall wrote. Claude gives the address of the fact.

The tools run only when Claude calls them. You do not call them yourself. Instead, you tell Claude, for example, "remember that the Windows runner kills tests after 60 s". Claude then calls `remember`.

Claude sees the tools as `mcp__plugin_openrecall_openrecall__recall`, `__remember` and `__forget`. Claude Code has a "don't ask" permission mode. In that mode, Claude Code denies these tools until you allow them. For example, you can allow them in `~/.claude/settings.json`:

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

Extraction sends your session text to a large language model (LLM) provider that you choose. The provider sees that text. Each session also costs money. Extraction is off until you configure it.

With extraction on, a background worker reads each finished session. The worker writes the facts that a later session would still need. Without extraction, new facts come only from `remember`. The worker never runs between your prompt and the start of Claude's answer.

OpenRecall has no spend cap. Each session takes two calls to the model. With Claude Sonnet 5.5 through OpenRouter, the two calls cost about $0.085 a session in total. To limit the spend, set a credit limit on your key at the provider. Every run logs its tokens and cost to `~/.openrecall/log/openrecall.jsonl`.

To configure extraction, you write two files:

- `~/.openrecall/api-key` holds the API key for your provider.
- `~/.openrecall/extract.toml` holds the endpoint of your provider and the model to use.

Warning: the `echo` command in step 2 puts your key in your shell history. After you run it, delete that entry from your history. Instead of `echo`, you can also open the file in an editor and paste the key there.

1. Create the folder `~/.openrecall`, if the plugin did not create it yet.
2. Write your key to `~/.openrecall/api-key`, alone on one line. In the command, replace `YOUR_API_KEY` with your real key.
3. Make that file readable only by you. The worker refuses a key file that your group or other users can read.
4. Write `~/.openrecall/extract.toml` with the endpoint and the model.

```sh
# 1. Create the folder where OpenRecall keeps everything.
mkdir -p ~/.openrecall

# 2. Write the key alone. The worker removes the newline that echo adds at the end.
echo 'YOUR_API_KEY' > ~/.openrecall/api-key

# 3. Only you can read and write the file. Nobody else has any access.
chmod 600 ~/.openrecall/api-key

# 4. base_url is the root of the provider's OpenAI-style API. model is the full ID of the model.
cat > ~/.openrecall/extract.toml <<'EOF'
base_url = "https://openrouter.ai/api/v1"
model = "anthropic/claude-sonnet-5.5"
EOF
```

The endpoint must accept OpenAI's `POST <base_url>/chat/completions` with a JSON schema. Any endpoint that does this works. Write the model as its full ID, never as an alias.

Do not export the key in a shell profile. The worker reads the key only from `~/.openrecall/api-key`. An exported key only lets every program that you start read it.

To check extraction:

1. Start a new Claude Code session.
2. End the session.
3. Read `~/.openrecall/log/openrecall.jsonl`. The worker logs each run there. If extraction is off, the worker logs the reason.

The worker starts each time a session starts or ends. It takes each session that ended, or that was quiet for 30 minutes. Quiet means that the transcript of the session did not change. The worker reads only the turns that it did not read before. It sends the earlier turns too, but only as context. Only one worker runs at a time.

The worker sends a short form of the session through `/usr/bin/curl`. The short form holds your prompts, Claude's text, and one line per tool call. It holds no tool output. OpenRecall replaces each secret in it with `[REDACTED:<rule-id>]` first. The worker gives the key to curl on stdin, never on a command line. If you use OpenRouter, turn on "deny data collection" in its privacy settings.

The second call to the model stops duplicate facts. This step is dedupe. Dedupe compares each new fact with the 10 most similar memories. Then the model picks one of three actions for the fact:

- `new` writes the fact as a new memory.
- `skip` drops the fact as a repeat.
- `replace` rewrites an old fact in its own file, with a merged text. OpenRecall keeps the old text in `replaced/`.

Dedupe never replaces a built-in memory or a global memory.

Sometimes the model finds a standing rule: a rule for every session in the repo, such as a coding standard. Extraction writes such rules to `~/.openrecall/repos/<host>/<owner>/<repo>/claude-md-suggestions.md`. You copy the rules that you want into CLAUDE.md by hand. OpenRecall never recalls that file.

## Statusline

On each prompt, OpenRecall writes one line, such as `recall 1 · 4 ms`, to `~/.openrecall/status/<session_id>`. The first number counts what OpenRecall injected on that prompt: the memories and the handoff record. The second number is the time that OpenRecall took, in milliseconds.

To show the line, add these two lines to your own statusline script:

```sh
sid=$(printf '%s' "$input" | jq -r .session_id)
or=$(cat "${OPENRECALL_HOME:-$HOME/.openrecall}/status/$sid" 2>/dev/null)
```

Then print `$or` where you want the text to appear.

## Disable or remove OpenRecall

To disable OpenRecall for one session, start Claude with `OPENRECALL=0 claude`.

OpenRecall always skips headless sessions. A headless session is one that `claude -p` starts, with no person who types. The MCP tools still work in a headless session, because they run only when Claude calls them.

To remove OpenRecall completely, run the three commands below. The last command deletes every fact, handoff record and setting that OpenRecall wrote. You cannot undo it.

```sh
claude plugin uninstall openrecall@openrecall
cargo uninstall --root ~/.local openrecall
rm -rf ~/.openrecall      # every fact, handoff record and setting OpenRecall wrote
```

The `rm` command does not delete Claude Code's built-in memory. OpenRecall never writes there.

## Where things live

OpenRecall keeps all its files under `~/.openrecall/`. To use a different folder, set `OPENRECALL_HOME`.

| Path | What it holds |
| --- | --- |
| `repos/<host>/<owner>/<repo>/` | It holds OpenRecall's facts for that repo. |
| `repos/<host>/<owner>/<repo>/handoffs/` | It holds one handoff record per branch. Its `retired/` folder holds the retired records. |
| `repos/<host>/<owner>/<repo>/claude-md-suggestions.md` | It holds rules that you can copy into CLAUDE.md. |
| `global/` | It holds the global memories, which every repo may recall. |
| `index.db` | It is the index. You can delete it at any time. |
| `log/openrecall.jsonl` | It holds one line per event. Extraction runs also log their tokens and cost here. |
| `status/<session_id>` | It holds the statusline text for that session. |
| `api-key`, `extract.toml` | They hold the extraction settings. |

## Working on OpenRecall

```sh
cargo test                       # Runs the unit tests and the hook tests of the binary.
python3 eval/test_harness.py     # Runs the tests of the eval harness itself.
```

`CONTEXT.md` defines the words that this README uses, such as handoff record, pointer, gate and extraction.

## License

OpenRecall uses the MIT license. The text is in `LICENSE`. `third_party/gitleaks/` holds the default rules of gitleaks v8.30.1. These rules also use the MIT license. Its text is in the same folder.
