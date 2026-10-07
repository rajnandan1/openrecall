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

OpenRecall calls no model between your prompt and the start of Claude's answer. The hook that runs on each prompt adds a few milliseconds. OpenRecall keeps your memories and handoff records as plain Markdown files under `~/.openrecall/`. Nothing leaves your machine unless you configure [extraction](#extraction-optional) or [picks](#picks-optional).

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

You need macOS on Apple silicon.

```sh
# Download the latest release of the openrecall binary into ~/.local/bin.
curl -fsSL https://raw.githubusercontent.com/rajnandan1/openrecall/main/install.sh | sh

# Add this repo as a plugin marketplace, so Claude Code can find the plugin.
claude plugin marketplace add rajnandan1/openrecall

# Install the plugin: the hooks and the MCP server. They all run the binary.
claude plugin install openrecall@openrecall
```

The first command is the install command. It downloads the latest release, checks its SHA-256 hash, and puts the binary at `~/.local/bin/openrecall`. It needs no Rust.

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

OpenRecall updates itself in the background, at most once in 24 hours. It replaces the binary first, then it updates the plugin. Sessions that were open before an update get the new MCP server and plugin after a restart of Claude Code.

To turn off the update check for every session, set `OPENRECALL_UPDATE` to `0` in the `env` key of `~/.claude/settings.json`:

```json
{
  "env": {
    "OPENRECALL_UPDATE": "0"
  }
}
```

If you turned on Claude Code's own auto-update for the `openrecall` marketplace, turn it off there too. `OPENRECALL_UPDATE` does not reach it.

A source build never updates itself.

To update by hand, run the install command again. For the plugin, run `claude plugin update openrecall@openrecall`.

When the plugin and the binary have different versions and you must act, the next session tells you when it starts. It also gives you the command to run.

### Build from source

To build the binary from source, you need Rust. This command builds it from this repo into `~/.local/bin`. `--locked` builds with the exact dependency versions in `Cargo.lock`.

```sh
cargo install --locked --git https://github.com/rajnandan1/openrecall --root ~/.local
```

A source build never updates itself. To move from a source build to a release binary, run the install command.

## How it works

### Handoff records

A task is one branch of one repo. `main` and `master` are never tasks.

At the end of each turn, OpenRecall updates the handoff record of the task in the background. A turn is one prompt and Claude's full answer to it. The record is the file `~/.openrecall/repos/<host>/<owner>/<repo>/handoffs/<branch>.md`. In the example above, `<host>/<owner>/<repo>` is `github.com/acme/exporter`. A `/` in the branch name becomes `--` in the file name.

Before OpenRecall writes a record, it replaces each secret with `[REDACTED:<rule-id>]`. It finds secrets with the rules of gitleaks, a secret scanner.

OpenRecall injects a handoff record only when the new session shows which task it is on. To inject means to add text to Claude's context before Claude reads your prompt. The session shows its task in one of four ways:

- A prompt names one of the tickets in the record. OpenRecall injects the record with that prompt. The ticket can be in upper case, such as `ABC-12`. It can also be in lower case inside a path or a file name, such as `docs/handoff-abc-12.md`, but only when a record already has the ticket.
- Before the session finishes a turn on the branch, a prompt names one of the paths, PR numbers or commits in the record of the branch. OpenRecall injects the record with that prompt. A path must include its folder, such as `src/export.py` or `@src/export.py`. A prompt of 2,000 characters or more does not count, because it is usually pasted text.
- The session finishes one turn on the branch of the record, without a switch to another branch. OpenRecall injects the record with the next prompt.
- A prompt names a ticket that no record of this repo has, and a record of another repo with the same owner has it. For example, a session in `github.com/acme/web` gets a record of `github.com/acme/api`. OpenRecall injects that record with the prompt. Its header names the other repo and its folder, because the paths in the record are paths of that repo.

The fourth way has a risk. When one owner has a public repo and a private repo, a record of the private repo can reach a session of the public repo. Claude could then repeat private text in a commit, a PR or an issue of the public repo. OpenRecall cannot see which repo is public.

The rest of the rules for handoff records:

- OpenRecall injects each record at most once per context window.
- Claude gets at most about 800 tokens of the record. If the record is longer, OpenRecall cuts it and ends it with `[...]`.
- The record is plain Markdown. You can add your own section to it by hand. OpenRecall keeps your section when it updates the record.
- If no session updates a record for 7 days, OpenRecall retires the record. It moves the record to `handoffs/retired/`.

### Branches and worktrees

OpenRecall follows the branch that a worktree is on. So you can use one worktree for many tasks, one after the other:

- When you switch to a new branch, OpenRecall starts a new record for it at the end of the next turn. The record of the old branch does not change.
- When you come back to an old branch, OpenRecall injects its record again. The first three ways above apply: a prompt that names one of its tickets, a prompt that names one of its paths, PR numbers or commits, or one finished turn on the branch.
- A session does not get a record that only it wrote, because its context already holds that work. After `/clear` or a compaction, the session gets the record again.
- If the record of a branch retired before you come back, OpenRecall starts a new record for the branch.
- Facts and Claude Code's built-in memory belong to the repo, not to a branch. Every branch and every worktree of the repo can recall them.
- A worktree is on one branch at a time. All sessions that are open in the worktree follow that branch. When one session switches the branch, the other sessions move to the new task too.

A rename keeps the record. If the old branch does not exist when OpenRecall sees the switch, OpenRecall treats the switch as a rename, such as `git branch -m`. It then moves the old record to the new branch name. OpenRecall sees a switch at the end of the next turn. So delete an old branch only after one turn finishes on the new branch.

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

Recall reads a PR URL in the prompt as its PR number, such as 345 for `https://github.com/acme/web-app/pull/345`, when the number has 3 to 6 digits.

Recall gives each memory it finds a score. The score tells how well the memory matches the prompt. Then a rule, the gate, decides which memories OpenRecall injects:

- The gate allows at most 3 memories, about 400 tokens in all.
- The gate allows a memory with a score of 1.50 or more. This number is the threshold.
- The gate also allows a memory with a lower score when its file name, name or description shares a ticket ID, a PR number or a path with the prompt. This is the identifier rule.
- The gate allows nothing while the index holds fewer than 10 memories. This count covers all repos together.

If no memory passes the gate, OpenRecall injects nothing. OpenRecall also does not inject a memory whose line only points at a path that the prompt already names. Such a memory is an echo.

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

The hooks inject very little: one handoff record, and at most 3 one-line memories per prompt. With [picks](#picks-optional) on, a pick adds at most one more memory per prompt. This keeps prompts fast and Claude's context small. But sometimes Claude needs more:

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

## Picks (optional)

A pick is the one memory, or none, that an LLM chooses for your prompt. The LLM also reads the session so far, so a pick can find a memory that the words of the prompt do not match. Picks send your session text to the provider of extraction. The provider sees that text. Each pick also costs money. Picks are off until you turn them on.

The prompt hook is the hook that Claude Code runs when you send a prompt. On each prompt that recall does not skip, the prompt hook starts a background job. The job sends one request to the provider. The request holds three parts:

- Every memory that recall searches for this repo, one line each, with its name and its description.
- The short form of the session, newest turn first. It is the same short form that extraction sends: your prompts, Claude's text, and one line per tool call, with no tool output. OpenRecall replaces each secret in it with `[REDACTED:<rule-id>]` first.
- Your prompt.

The request holds at most 32,000 characters. OpenRecall cuts the oldest turns first. If the memory list and the prompt alone are too long, OpenRecall sends nothing.

The prompt hook does not wait for the job. Claude starts its answer while the job runs. So the call to the provider never delays the start of Claude's answer.

The plugin also has a tool hook. Claude Code runs it after each tool call that Claude makes. A pick reaches Claude with the result of the turn's first tool call that finds it ready, never inside a subagent, and a turn with no tool call drops it.

A picked memory skips the gate. OpenRecall still drops a pick in each case where it does not inject a memory, as listed in [Recalled memories](#recalled-memories). It also drops a pick that is an echo of the prompt.

Each pick makes at most one call to the model. With Claude Sonnet 5.5 through OpenRouter, a call costs about $0.02. The largest call that we measured cost $0.034. A session makes at most 10 calls. A day of 57 to 72 prompts costs about $1.50 to $1.90. This cost comes on top of the cost of extraction.

Picks need Claude Code 2.1.196 or later. On an older version, the prompt hook starts no job, so nothing is sent and nothing is spent.

Picks use the provider, the model and the key of extraction. To turn picks on:

1. Configure [extraction](#extraction-optional).
2. Add the line `pick = true` to `~/.openrecall/extract.toml`.

```toml
base_url = "https://openrouter.ai/api/v1"
model = "anthropic/claude-sonnet-5.5"
pick = true
```

To turn picks off, remove the line or change it to `pick = false`. Without a configured provider, picks are off too.

OpenRecall logs each pick to `~/.openrecall/log/openrecall.jsonl`:

- A `pick` line holds the tokens and the cost of the call, and the picked memory. If OpenRecall made no call or dropped the pick, the line also gives the reason.
- A `deliver` line shows that a pick reached Claude.
- A `pick_drop` line shows that a pick did not reach Claude, and why.

## Statusline

On each prompt, OpenRecall writes one line, such as `recall 3 · 4 ms`, to `~/.openrecall/status/<session_id>`. The line has two numbers:

- The first number is the total of memories and handoff records that OpenRecall injected in this session so far.
- The second number is the time that OpenRecall took on the last prompt, in milliseconds.

OpenRecall injects nothing for a task notification or for a prompt inside a subagent. For these prompts, the line shows `skipped` in place of the time, such as `recall 3 · skipped`. The total stays the same.

The total starts at 0 in each new session. `/clear` starts a new session with a new session ID, so the total starts at 0 again. A compaction or a resume keeps the session ID, so the total continues. In a new session, the line is empty until the first prompt.

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
rm ~/.local/bin/openrecall
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
| `log/openrecall.jsonl` | It holds one line per event. Extraction runs and picks also log their tokens and cost here. |
| `status/<session_id>` | It holds the statusline text for that session. |
| `picks/<session_id>/` | It holds the pick results of that session. OpenRecall deletes it when the session ends. |
| `api-key`, `extract.toml` | They hold the extraction and pick settings. |

## Working on OpenRecall

```sh
cargo test                       # Runs the unit tests and the hook tests of the binary.
python3 eval/test_harness.py     # Runs the tests of the eval harness itself.
```

`GLOSSARY.md` defines the words that this README uses, such as handoff record, pointer, gate and extraction.

## License

OpenRecall uses the MIT license. The text is in `LICENSE`. `third_party/gitleaks/` holds the default rules of gitleaks v8.30.1. These rules also use the MIT license. Its text is in the same folder.
