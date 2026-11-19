# Changelog

## 0.2.1

- Align session metadata columns and place the title last. Long fixed-width
  fields are shortened in the list; press `i` to inspect their full values.

## 0.2.0

Breaking changes:

- One `select-claude-session` binary replaces the three 0.1.0 binaries:
  `record-claude-session-info` is now `select-claude-session index`, and
  `claude-replay-tui` is now `select-claude-session replay`.
- `y` copies `cd '<cwd>' && claude --resume <session-id>` instead of
  `claude --resume <session-id>`.
- A 0.1.0 index is rebuilt on the next refresh. `--no-refresh` refuses to open
  it until then.

Added, from the codex-session-selector 0.3.4 base:

- Incremental indexing with stable keys, and FTS5/BM25 content search over user
  messages, agent messages, Bash commands, and Bash output.
- Replay of user prompts, agent replies, and Bash commands, with `e` to toggle
  Bash entries.
- `--include-subsessions`, `--include-empty-messages`, `--include-exec`, and
  `--quick-check`.

Added for Claude Code:

- First messages come from prompts a person typed (`origin.kind = "human"`),
  or from the delegated prompt of a subagent.
- Subagent transcripts under `<session>/subagents/` are indexed as subsessions
  with their description, agent type, and parent session.
- Session titles from `ai-title` or `agent-name` are shown and searchable.
- `[CONT]` marks sessions that were continued in another session.
- `p` selects the parent session of a subsession, and `y` on a subsession
  copies its parent's resume command.
