# Claude Session Selector

A local TUI for indexing, finding, and replaying Claude Code sessions.
It reads `~/.claude/projects`, keeps a local SQLite search index, and replays a
selected JSONL transcript without uploading session data.

It is a port of
[codex-session-selector](https://github.com/widehyo1/codex-session-selector)
0.3.4 and keeps its keys, search syntax, and index behavior.

## Quickstart

Install the single executable, then open the selector:

```bash
./install-bundle.sh
select-claude-session
```

The first run builds `~/claude-session-info.sqlite3` from
`~/.claude/projects`. Select a row with `Enter`; the replay closes back into
the selector with its search and selection intact. Press `y` to copy the
command that resumes the selected session.

Verify an installed version:

```bash
select-claude-session --version
```

## Daily use

```bash
# Open the selector and refresh its default local index.
select-claude-session

# Show subagent transcripts and sessions without a typed prompt.
select-claude-session --include-subsessions --include-empty-messages

# Use a different index file or leave it unchanged.
select-claude-session --db /tmp/sessions.sqlite3
select-claude-session --no-refresh

# Replay one transcript directly.
select-claude-session replay ~/.claude/projects/-Users-me-repo/<session-id>.jsonl

# Start replay with Bash command entries visible.
select-claude-session --include-exec
```

Each row shows when the transcript was last modified, the working directory,
git branch, repository, and session title, newest first. Press `/` to search, `Tab` to select `any` or a content scope,
`s` to sort or filter by a header column, and `e` to choose whether the next replay shows Bash commands.

- `y` copies `cd '<cwd>' && claude --resume <session-id>`.
- With `--include-subsessions`, rows are labelled `+` (main) or `-` (sub). On a
  `-` row, `p` selects its parent session and `y` copies the parent's
  resume command.
- `>` marks a session that Claude Code continued in another session.

See the [manual](MANUAL.md) for every key binding and the FTS query syntax.

## How it works

```text
~/.claude/projects/**.jsonl  ->  ~/claude-session-info.sqlite3  ->  selector / replay
```

- Refreshing is incremental: unchanged JSONL files are not reparsed.
- The index stores sessions, user and agent messages, and Bash commands with
  their output and their [call and result times](MANUAL.md#execution-times).
- Subagent transcripts and sessions without a typed first prompt are stored but
  hidden by default; the corresponding selector options reveal them.
- An optional hook records the real run time of each Bash call; refreshing
  moves it to `~/.claude/exec-log/exec.sqlite3`. It is
  [not retroactive](MANUAL.md#exec-timing-hook).
- All index writes are transactional. Replay reads the selected JSONL directly.
- The index and replay operate locally; this tool has no hosted service or
  session upload path.

## Commands

| Command | Purpose |
| --- | --- |
| `select-claude-session` | Refresh and open the session selector. |
| `select-claude-session index` | Refresh or rebuild the local SQLite index. |
| `select-claude-session replay [PATH]` | Replay a JSONL file, or stdin when `PATH` is omitted. |

Useful selector options:

| Option | Effect |
| --- | --- |
| `--db PATH` | Use a specific SQLite index. |
| `--no-refresh` | Open the existing index without updating it. |
| `--include-subsessions` | Show subagent transcripts. |
| `--include-empty-messages` | Show sessions with no typed first prompt. |
| `--include-exec` | Initially show Bash entries in replay. |
| `--print-path` | Print the selected JSONL path rather than replaying it. |

For custom source paths, forced rebuilds, external compatibility commands, and
all command options, see [MANUAL.md](MANUAL.md).

## Compatibility

Checked against Claude Code `2.1.311` and `2.1.322` transcripts.

- The first message is the first `user` record with string content and
  `origin.kind = "human"`; a record that also has `promptSource` wins over one
  without it (such as a slash command). In a subagent transcript it is the
  root prompt (`isSidechain = true`, `parentUuid = null`).
- Records are selected by their root `type`, so `api-request-blob` copies of
  messages are not indexed twice.
- A file under `<session>/subagents/agent-<id>.jsonl` is a subsession of
  `<session>`. Its title, agent type, and parent tool call come from the
  sibling `agent-<id>.meta.json`.
- Session titles are the last `ai-title` record, or the last `agent-name`
  record when there is no `ai-title`.
- The repository is `host/remote` from the transcript's git origin, when
  Claude Code recorded one.

Transcripts written before Claude Code recorded `origin` have no recognized
first message and are visible only with `--include-empty-messages`.

## Install and development

Building from source requires Rust 1.97 or newer. `./install-bundle.sh`
installs to `~/.local/bin`; set `CLAUDE_CLI_BIN_DIR` to choose another
directory, or run `cargo install --path .`.

For contributors, enable the repository hook and run the quality gate:

```bash
scripts/install-git-hooks.sh
scripts/check-before-commit.sh
```

## Project

- [Manual](MANUAL.md)
- [Changelog](CHANGELOG.md)
- [License](LICENSE)
- Repository: <https://github.com/widehyo1/claude-session-selector>
