# Manual

This manual contains the detailed operational contract for
`select-claude-session`. Start with the [README](README.md) for installation
and the everyday workflow.

## Selector

The selector refreshes `~/claude-session-info.sqlite3` from
`~/.claude/projects` unless `--no-refresh` is given. `--record-command` and
`--replay-command` are compatibility overrides: each is an executable path,
not a shell command.

```text
--db PATH
--record-command COMMAND
--replay-command COMMAND
--no-refresh
--quick-check
--include-subsessions
--include-empty-messages
--include-exec
--print-path
-h, --help
-V, --version
```

### Controls

| Key | Action |
| --- | --- |
| `j` / `k`, Up / Down | Move the selected session or scroll the focused message. |
| `d` / `u`, PageDown / PageUp | Move or scroll by half a pane. |
| `g` / `G`, Home / End | Go to the first or last session/message position. |
| `Tab` | Switch focus; while searching, cycle the search target. |
| `h` / `l`, Left / Right | Horizontally scroll session metadata. |
| `0` | Reset metadata horizontal scroll. |
| `/` | Start a search. |
| `i` | Show full metadata for the selected session; use `j` / `k`, `d` / `u`, or `g` / `G` to scroll and `i`, `Esc`, or `q` to close. |
| `e` | Toggle initial Bash visibility for the next replay. |
| `Enter` | Replay the selected session. |
| `y` | Copy `cd '<cwd>' && claude --resume <session-id>`. |
| `p` | Select the parent session of a subsession. |
| `?` | Open help. |
| `q`, Ctrl-C, Esc | Quit, or leave search/help when active. |

Each row reads `date | cwd | branch | repository | title`; a missing value is
shown as `-`. The date, working directory, branch, and repository have fixed
column widths. Values that exceed their column are shortened with `…`; the
title remains unshortened and can be viewed with horizontal scrolling. Press `i`
to see the full values of every field. Badge positions use one character each, with blank spaces for absent badges to keep the columns aligned. The bottom legend explains their colors and meanings.

The default view hides subsessions and rows without a non-empty first user
message. `--include-subsessions` and `--include-empty-messages` change only
the visible set; the canonical index always stores every valid session.
When `--include-subsessions` is active, every list row is prefixed with
`+` (main) or `-` (sub). A `>` badge marks a session whose transcript records
that it was continued in another session; it is shown in every view.

### Resume command

`y` copies a command for a POSIX shell. The working directory is the first
`cwd` recorded in the session and is single-quoted; a `'` inside it is written
as `'\''`. When the session has no recorded `cwd`, the command is only
`claude --resume <session-id>`.

A subsession has no resumable id of its own. On a `-` row, `y` copies the
command of its parent main session. If the parent is not among the sessions
loaded in the current view, the status line shows `parent session is not
indexed in this view`.

### Parent navigation

`p` on a `-` row selects the parent `+` row in the current list. The
search query is kept, so a parent that the current search filters out cannot
be selected; the status line then shows `parent session is not in the current
list`. On a main session the status line shows `selected session has no
parent`. While searching, `p` is search text.

## Search

Metadata search is case-insensitive all-term substring matching across the
first message, title, subagent type, cwd, repository, branch, timestamp, and
date. Content search uses persistent FTS5/BM25 indexes over normalized user
messages, agent messages, Bash commands, and Bash outputs.

Use `Tab` during a search to choose `metadata`, `content:all`, `content:user`,
`content:agent`, or `content:exec`. Bare terms use AND and token-prefix
matching: `fix read` finds “Fix README parser”; `"readme parser"` is an exact
phrase; whitespace-delimited `|` separates OR groups. The FTS tokenizer is
Unicode `unicode61`, so Korean words without spaces are single tokens.

The right pane always displays the selected session's first user message, not
a matched content excerpt. While searching, `e` is search text rather than an
execution-visibility command.

When Claude Code continues a session, the new transcript starts with a copy of
the previous one. Both transcripts are indexed as they are, so content search
can return both sessions for the copied part.

## Index

```bash
select-claude-session index
select-claude-session index --output /tmp/sessions.sqlite3
select-claude-session index --sessions-root /path/to/.claude/projects
select-claude-session index --rebuild
```

```text
-o, --output PATH
--sessions-root PATH
--rebuild
--quick-check
--include-subsessions
--include-empty-messages
--include-exec
-h, --help
-V, --version
```

The three `index --include-*` options remain accepted for script compatibility
but do not affect canonical storage. The index stores subsessions, empty first
messages, and Bash events regardless of those options.

The indexer reads `<root>/<project>/*.jsonl` as main sessions and
`<root>/<project>/<session>/subagents/agent-*.jsonl` as subsessions. Other
files and directories, such as `tool-results` and `memory`, are not read. A
transcript without any `user` or `assistant` record is stored as skipped.

Refresh fingerprints each source by path, size, and modification timestamp.
New and changed files are parsed; deleted sources and their derived rows are
removed in the same transaction. A file changing during both parse attempts is
deferred. If content changed while both file size and timestamp were restored,
run `index --rebuild`.

A subagent's `agent-<id>.meta.json` is read whenever its transcript is parsed
but is not fingerprinted. A change to the metadata file alone is picked up the
next time the transcript changes, or by `index --rebuild`. A missing metadata
file leaves the title, agent type, and parent tool call empty; an unreadable
one is reported on stderr and treated the same way.

Unchanged refreshes skip SQLite `PRAGMA quick_check` by default. Refreshes
with new, changed, or deleted sources, index rebuilds, and FTS repairs still
run it. Other consistency checks remain enabled on every refresh. Use
`select-claude-session --quick-check` or `select-claude-session index
--quick-check` to run it even when sources are unchanged. The selector option
requires the internal refresh and cannot be combined with `--no-refresh` or
`--record-command`.

### Execution times

`exec_events` stores two timestamps as written in the transcript (ISO 8601,
UTC):

| Column | Source |
| --- | --- |
| `called_at` | The record containing the Bash `tool_use`. |
| `result_at` | The record containing the `tool_result` stored in `output`. |

Claude Code does not record when the process itself started or exited. The
interval between the two columns therefore also contains permission waits,
hooks, queueing behind other calls, and any time the machine slept. For the
execution itself, use the [exec timing hook](#exec-timing-hook).

`result_at` does not always mean that the command finished:

- For a command started in the background, it is when the launch was reported.
- For a command moved to the background while running, for example at its
  timeout, it is when it was moved.
- A denied or blocked call also has one, although it never ran.

When a call has several results, the last one supplies both `output` and
`result_at`. A missing result or timestamp is `NULL`. A continued session
contains copies of earlier calls, so deduplicate by `call_id` before adding up
times across sessions.

```sql
SELECT call_id, called_at, result_at,
       (julianday(result_at) - julianday(called_at)) * 86400.0 AS seconds
FROM exec_events
ORDER BY seconds DESC;
```

## Exec timing hook

An optional Claude Code hook records how long each Bash call really ran, as
raw input for judging session efficiency. One row is one Bash tool call that
started and returned; child processes are not counted separately.

```text
PostToolUse, PostToolUseFailure
  -> ~/.claude/exec-log/exec.jsonl      appended by the hook
  -> ~/.claude/exec-log/exec.sqlite3    moved on each refresh, after indexing
```

### Install

The hook needs `jq`. Run from the repository:

```bash
mkdir -p -m 700 ~/.claude/exec-log
cp hooks/exec-log.jq ~/.claude/exec-log/exec.jq
```

Append this entry to both `hooks.PostToolUse` and `hooks.PostToolUseFailure`
in `~/.claude/settings.json`, keeping the entries already there:

```json
{
  "matcher": "Bash",
  "hooks": [
    {
      "type": "command",
      "timeout": 5,
      "command": "jq -c -f \"$HOME/.claude/exec-log/exec.jq\" >> \"$HOME/.claude/exec-log/exec.jsonl\" || true"
    }
  ]
}
```

`select-claude-session` and `select-claude-session index` then store the log
whenever they refresh. `--no-refresh` leaves it untouched, and so does
`--record-command` unless that command does the same. The log directory is
always `exec-log` next to the sessions root, independent of `--db` and
`--output`.

### Stored data

`exec_timings` holds no command or output text.

| Column | Meaning |
| --- | --- |
| `tool_use_id` | Primary key; equals `exec_events.call_id`. |
| `session_id` | The session that ran the call; for a subagent, its main session. |
| `agent_id` | The subagent that ran the call, or `NULL`. |
| `started_at_ms` | `ended_at_ms - duration_ms`, Unix milliseconds. |
| `ended_at_ms` | When the hook ran, Unix milliseconds. |
| `duration_ms` | Claude Code's execution time, without permission-prompt and hook time. |
| `outcome` | `ok`, `error`, `interrupted`, or `backgrounded`. |
| `background_task_id` | Set when the call was backgrounded. |

Join it to the index to split a call into waiting and running:

```sql
-- sqlite3 -readonly ~/claude-session-info.sqlite3
ATTACH 'file:/Users/me/.claude/exec-log/exec.sqlite3?mode=ro' AS timing;
WITH calls AS (
  SELECT call_id, min(called_at) AS called_at, max(result_at) AS result_at
  FROM exec_events
  GROUP BY call_id
)
SELECT t.session_id, t.tool_use_id, t.outcome,
       t.started_at_ms / 1000.0 - unixepoch(c.called_at, 'subsec') AS wait_seconds,
       t.duration_ms / 1000.0 AS run_seconds,
       unixepoch(c.result_at, 'subsec') - t.ended_at_ms / 1000.0 AS report_seconds
FROM timing.exec_timings AS t
JOIN calls AS c ON c.call_id = t.tool_use_id;
```

### Limitations

- **Not retroactive.** Only calls made after the hook was installed have a
  row. Earlier sessions have just `called_at` and `result_at`, and their
  execution time cannot be recovered.
- The timings cannot be rebuilt either. They are kept outside the index so that
  `index --rebuild` or deleting the index does not lose them; deleting
  `exec.sqlite3` does.
- For `backgrounded` rows the end and duration describe the launch or the move
  to the background. When the command finished is not recorded.
- A call that was denied, or still running when Claude Code was killed, has no
  row. A missing row alone therefore does not say why.
- Start and end were within about 25 ms of the real process in a test on Claude
  Code 2.1.323. The first call of a session also included about 1 s of shell
  setup.
- Calls can overlap, so summed `duration_ms` can exceed the elapsed time.

A log file is deleted on the first refresh at least 60 seconds after its lines
were stored. A file with unreadable lines is kept as `rejected.*.jsonl`.

## Replay

```bash
select-claude-session replay SESSION.jsonl
select-claude-session replay --include-exec SESSION.jsonl
jq -c . SESSION.jsonl | select-claude-session replay -
```

An omitted input path or `-` reads stdin, as JSONL or as one JSON array of
records. Replay shows:

- user prompts typed by a person, and the delegated prompt of a subagent;
- agent `text` blocks and the report a subagent hands back
  (`SubagentHandback`);
- Bash commands with their output, which `e` shows or hides in memory.

Thinking blocks, other tools, and records injected by Claude Code are not
shown.

| Key | Action |
| --- | --- |
| `j` / `k`, Up / Down | Move timeline selection or scroll detail. |
| `d` / `u`, PageDown / PageUp | Move or scroll by half a pane. |
| `g` / `G`, Home / End | Go to the first or last timeline/detail position. |
| `1` / `2` | Toggle timeline/detail fullscreen. |
| `f` | Toggle the focused pane fullscreen. |
| `e` | Toggle Bash command entries. |
| `y` | Copy detail text when the detail pane is focused. |
| `/` | Filter the timeline while typing. |
| `n` / `N` | Scroll the focused detail pane to the next or previous match. |
| `?`, `q`, Ctrl-C, Esc | Help and exit controls. |

`/` keeps the entries whose summary or detail contains every word of the
query, ignoring case, and highlights the matches. It filters within the
current Bash visibility, and entry numbers stay the same as without the
filter. `Enter` keeps the filter. `Esc` clears it and stays on the selected
entry; outside search input, `Esc` leaves fullscreen first and quits only when
no filter is set.

## Migration and SQLite contract

Version 0.2.0 replaces the three 0.1.0 binaries with one:

```text
record-claude-session-info ARGS  -> select-claude-session index ARGS
claude-replay-tui ARGS           -> select-claude-session replay ARGS
select-claude-session ARGS       -> select-claude-session ARGS
```

The default database path is unchanged. A 0.1.0 database is detected and
rebuilt from the transcripts on the next refresh; `--no-refresh` refuses to
open it until then.

Version 0.3.0 upgrades schema v1 to v2 on the next refresh. It adds the
execution time columns and reparses every transcript once to fill them,
keeping the existing keys; that refresh reports the files as changed. As on
any refresh, sessions whose transcript was deleted are removed. `--no-refresh`
refuses a v1 index until it is upgraded, and 0.2.x refuses to open a v2 index.

The crate uses `rusqlite 0.40.1` with bundled SQLite. Schema v2 contains the
STRICT tables `index_metadata`, `source_files`, `sessions`, `exec_events`, and
`message_events`, plus a contentless-delete FTS5 index. Besides the codex
selector's columns, `sessions` stores `title`, `parent_session_id`,
`parent_tool_use_id`, `agent_type`, and `continued_in_session_id`. CHECK
constraints allow the parent columns only on subsessions and the continuation
link only on main sessions.

Source, session, event, and message keys remain stable across ordinary
incremental updates; rebuilds, renames, and root changes may assign new keys.
All writes validate foreign keys and SQLite integrity inside one immediate
transaction. A dirty or inconsistent FTS index is rebuilt without replacing
canonical session keys. `--no-refresh` requires schema v2; an unknown schema
requires `index --rebuild`, and a newer schema is never overwritten.
