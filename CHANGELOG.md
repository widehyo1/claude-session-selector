# Changelog

## Unreleased

- Order the session list by file modified time, newest first, and drop the
  session start date column. The modified time is shown as ISO 8601 UTC to
  the minute (`2026-11-25T10:46Z`); the `i` details still show the date.
- Shorten long paths and branch names from the start (`…ne/project`) so the
  last segments stay visible, and narrow the branch column to 12 cells.
- Add a header row to the session list.

## 0.3.2

- Add a `YYYY-mm-dd HH:mm` time column at the end of each replay timeline row,
  taken from the record's transcript timestamp (UTC). The summary narrows so
  the time stays visible; entries without a timestamp show `-`.
- Add a `file modified` column right after the date in the session list, and
  to the `i` details: when the transcript file was last written, as
  `YYYY-mm-dd HH:mm` UTC. It comes from the index's file fingerprint, so the
  schema is unchanged; it approximates the last activity, not the last prompt.

## 0.3.1

- Add `/` search to replay. It filters the timeline to the entries that
  contain every word of the query and highlights the matches; `n` / `N` scroll
  the detail pane between matches. `Esc` clears the filter and keeps the
  selected entry, so `Esc` now quits replay only when no filter is set.

## 0.3.0

- Store `called_at` and `result_at` for every Bash call in `exec_events`, so
  timing queries no longer need to join the transcripts.
- Upgrade the index to schema v2 on the next refresh, filling existing rows and
  keeping their keys. `--no-refresh` refuses a v1 index until then, and 0.2.x
  cannot open an upgraded index.
- Add an optional Claude Code hook for the run time of each Bash call. Each
  refresh moves its log to `~/.claude/exec-log/exec.sqlite3` after indexing.
  It is not retroactive: calls made before the hook was installed have no run
  time.

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
