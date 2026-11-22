use std::{
    collections::HashMap,
    fs,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use rusqlite::{Connection, params};
use serde::Deserialize;
use serde_json::Value;

use session_event::RecordKind;

mod application;
mod cli;
mod exec_log;
mod indexer;
mod replay;
mod selector;
mod session_event;
mod terminal;
mod ui;
mod ui_state;

#[cfg(test)]
mod test_support;

pub fn run_from_args<I>(args: I) -> Result<()>
where
    I: IntoIterator<Item = String>,
{
    match cli::parse_args(args.into_iter())? {
        cli::CliAction::Run(command) => application::run(command),
        cli::CliAction::PrintHelp(topic) => {
            println!("{}", cli::help_text(topic));
            Ok(())
        }
        cli::CliAction::PrintVersion => {
            println!("select-claude-session {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionRow {
    pub path: PathBuf,
    pub id: Option<String>,
    pub timestamp: Option<String>,
    pub cwd: Option<String>,
    pub repository_url: Option<String>,
    pub branch: Option<String>,
    pub first_message: String,
    pub is_subsession: bool,
    pub title: Option<String>,
    pub parent_session_id: Option<String>,
    pub parent_tool_use_id: Option<String>,
    pub agent_type: Option<String>,
    pub continued_in_session_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecEvent {
    pub session_path: PathBuf,
    pub session_id: Option<String>,
    pub event_index: usize,
    pub call_id: Option<String>,
    pub kind: String,
    pub name: Option<String>,
    pub command: String,
    pub output: String,
    /// When the `tool_use` record was written; not when the process started.
    pub called_at: Option<String>,
    /// When the `tool_result` that supplied `output` was written.
    pub result_at: Option<String>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct CollectOptions {
    pub include_subsessions: bool,
    pub include_empty_messages: bool,
    pub include_exec: bool,
}

#[derive(Debug, Clone)]
pub struct CollectedSessionData {
    pub rows: Vec<SessionRow>,
    pub exec_events: Vec<ExecEvent>,
    pub total_files: usize,
    pub skipped: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct ParsedSessionFile {
    pub(crate) row: SessionRow,
    pub(crate) exec_events: Vec<ExecEvent>,
    pub(crate) message_events: Vec<MessageEvent>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MessageEvent {
    pub event_index: usize,
    pub role: session_event::MessageRole,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SourceKind {
    Main {
        session_id: String,
    },
    Subagent {
        parent_session_id: String,
        agent_id: String,
    },
}

// Classifies by path shape only; the file is not opened.
pub(crate) fn classify_source(path: &Path) -> Option<SourceKind> {
    if path.extension()?.to_str()? != "jsonl" {
        return None;
    }
    let stem = path.file_stem()?.to_str()?;
    let parent = path.parent()?;
    if parent.file_name().and_then(|name| name.to_str()) == Some("subagents") {
        // <root>/<project>/<parent_session_id>/subagents/agent-<id>.jsonl
        let agent_id = stem.strip_prefix("agent-")?;
        let parent_session_id = parent.parent()?.file_name()?.to_str()?;
        return Some(SourceKind::Subagent {
            parent_session_id: parent_session_id.to_owned(),
            agent_id: agent_id.to_owned(),
        });
    }
    Some(SourceKind::Main {
        session_id: stem.to_owned(),
    })
}

pub fn is_subsession_path(path: &Path) -> bool {
    matches!(classify_source(path), Some(SourceKind::Subagent { .. }))
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SubagentMeta {
    agent_type: Option<String>,
    description: Option<String>,
    tool_use_id: Option<String>,
}

// `agent-<id>.jsonl` is described by the sibling `agent-<id>.meta.json`.
// A missing file is normal; a broken one is reported and ignored.
fn read_subagent_meta(jsonl_path: &Path) -> SubagentMeta {
    let meta_path = jsonl_path.with_extension("meta.json");
    let contents = match fs::read_to_string(&meta_path) {
        Ok(contents) => contents,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return SubagentMeta::default(),
        Err(err) => {
            eprintln!("warning: {}: {err}", meta_path.display());
            return SubagentMeta::default();
        }
    };
    match serde_json::from_str::<SubagentMeta>(&contents) {
        Ok(meta) => SubagentMeta {
            agent_type: nonblank(meta.agent_type),
            description: nonblank(meta.description),
            tool_use_id: nonblank(meta.tool_use_id),
        },
        Err(err) => {
            eprintln!("warning: {}: invalid json: {err}", meta_path.display());
            SubagentMeta::default()
        }
    }
}

fn nonblank(value: Option<String>) -> Option<String> {
    value.filter(|text| !text.trim().is_empty())
}

pub fn parse_session_file(path: &Path) -> Result<Option<SessionRow>> {
    Ok(parse_session_file_data(path, false)?.map(|data| data.row))
}

#[derive(Debug, Default)]
struct SessionMeta {
    timestamp: Option<String>,
    cwd: Option<String>,
    branch: Option<String>,
    repository_url: Option<String>,
    ai_title: Option<String>,
    agent_name: Option<String>,
    continued_in: Option<String>,
}

impl SessionMeta {
    fn observe(&mut self, value: &Value) {
        set_first(&mut self.timestamp, value, "timestamp");
        set_first(&mut self.cwd, value, "cwd");
        set_first(&mut self.branch, value, "gitBranch");
        if self.repository_url.is_none() {
            self.repository_url = session_event::repository_ref(value);
        }
    }
}

pub(crate) fn parse_session_file_data(
    path: &Path,
    include_exec: bool,
) -> Result<Option<ParsedSessionFile>> {
    let file =
        fs::File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let reader = BufReader::new(file);
    let mut seen_conversation = false;
    let mut meta = SessionMeta::default();
    let mut best_first_user_message = None;
    let mut exec_events: Vec<ExecEvent> = Vec::new();
    let mut message_events = Vec::new();
    let mut exec_by_call_id: HashMap<String, usize> = HashMap::new();
    let mut dropped_bash_calls = 0_usize;

    for (line_number, line) in reader.lines().enumerate() {
        let line =
            line.with_context(|| format!("failed to read {}:{}", path.display(), line_number + 1))?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let kind = match session_event::record_kind(line) {
            Ok(RecordKind::Other) => continue,
            Ok(kind) => kind,
            Err(err) => {
                warn_invalid_json(path, line_number, &err);
                continue;
            }
        };
        let value: Value = match serde_json::from_str(line) {
            Ok(value) => value,
            Err(err) => {
                warn_invalid_json(path, line_number, &err);
                continue;
            }
        };
        match kind {
            RecordKind::AiTitle => {
                set_last(&mut meta.ai_title, &value, "aiTitle");
                continue;
            }
            RecordKind::AgentName => {
                set_last(&mut meta.agent_name, &value, "agentName");
                continue;
            }
            RecordKind::ContinuedIn => {
                set_last(&mut meta.continued_in, &value, "continuedInSessionId");
                continue;
            }
            RecordKind::User | RecordKind::Assistant | RecordKind::Other => {}
        }

        seen_conversation = true;
        meta.observe(&value);

        if let Some(candidate) = session_event::first_user_message_candidate(&value)
            && !candidate.content.trim().is_empty()
        {
            best_first_user_message =
                prefer_first_user_message(best_first_user_message, line_number, candidate);
        }

        if include_exec {
            dropped_bash_calls += collect_exec_events(
                path,
                line_number,
                &value,
                &mut exec_events,
                &mut exec_by_call_id,
            );
            if let Some(message) = session_event::normalize_message_record(&value) {
                message_events.push(MessageEvent {
                    event_index: line_number,
                    role: message.role,
                    content: message.content,
                });
            }
        }
    }

    if dropped_bash_calls > 0 {
        eprintln!(
            "warning: {}: indexed only the first Bash call of each record; skipped {dropped_bash_calls}",
            path.display()
        );
    }
    if !seen_conversation {
        return Ok(None);
    }

    let (id, parent_session_id) = match classify_source(path) {
        Some(SourceKind::Subagent {
            agent_id,
            parent_session_id,
        }) => (agent_id, Some(parent_session_id)),
        Some(SourceKind::Main { session_id }) => (session_id, None),
        None => (
            path.file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            None,
        ),
    };
    let is_subsession = parent_session_id.is_some();
    let subagent = if is_subsession {
        read_subagent_meta(path)
    } else {
        SubagentMeta::default()
    };
    let row = SessionRow {
        path: path.to_path_buf(),
        id: Some(id),
        timestamp: meta.timestamp,
        cwd: meta.cwd,
        repository_url: meta.repository_url,
        branch: meta.branch,
        first_message: best_first_user_message
            .map(|(_, candidate)| candidate.content)
            .unwrap_or_default(),
        is_subsession,
        title: if is_subsession {
            subagent.description
        } else {
            meta.ai_title.or(meta.agent_name)
        },
        parent_session_id,
        parent_tool_use_id: subagent.tool_use_id,
        agent_type: subagent.agent_type,
        continued_in_session_id: if is_subsession {
            None
        } else {
            meta.continued_in
        },
    };
    for event in &mut exec_events {
        event.session_id = row.id.clone();
    }
    Ok(Some(ParsedSessionFile {
        row,
        exec_events,
        message_events,
    }))
}

fn warn_invalid_json(path: &Path, line_number: usize, err: &serde_json::Error) {
    eprintln!(
        "warning: {}:{}: invalid json: {err}",
        path.display(),
        line_number + 1
    );
}

// Fills the target once, from the first non-blank string value.
fn set_first(target: &mut Option<String>, value: &Value, field: &str) {
    if target.is_none() {
        *target = nonblank_string_field(value, field);
    }
}

// Keeps the last non-blank string value; title records are rewritten every turn.
fn set_last(target: &mut Option<String>, value: &Value, field: &str) {
    if let Some(text) = nonblank_string_field(value, field) {
        *target = Some(text);
    }
}

fn nonblank_string_field(value: &Value, field: &str) -> Option<String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
        .map(ToOwned::to_owned)
}

// Returns how many Bash calls in this record were not indexed.
fn collect_exec_events(
    path: &Path,
    line_number: usize,
    value: &Value,
    exec_events: &mut Vec<ExecEvent>,
    exec_by_call_id: &mut HashMap<String, usize>,
) -> usize {
    // exec_events is unique per (session, line), so only the first call of a record is kept.
    let mut calls = session_event::bash_calls(value).into_iter();
    if let Some(call) = calls.next() {
        exec_by_call_id.insert(call.call_id.clone(), exec_events.len());
        exec_events.push(ExecEvent {
            session_path: path.to_path_buf(),
            session_id: None,
            event_index: line_number,
            call_id: Some(call.call_id),
            kind: "tool_use".to_string(),
            name: Some("Bash".to_string()),
            command: call.command,
            output: String::new(),
            called_at: nonblank_string_field(value, "timestamp"),
            result_at: None,
        });
    }
    for output in session_event::tool_outputs(value) {
        if let Some(&index) = exec_by_call_id.get(&output.call_id) {
            // A resumed session can replace an "interrupted" result; keep time and output paired.
            exec_events[index].output = output.output;
            exec_events[index].result_at = nonblank_string_field(value, "timestamp");
        }
    }
    calls.count()
}

fn prefer_first_user_message(
    current: Option<(usize, session_event::FirstUserMessageCandidate)>,
    line: usize,
    next: session_event::FirstUserMessageCandidate,
) -> Option<(usize, session_event::FirstUserMessageCandidate)> {
    match current {
        Some(current) if current.1.confidence >= next.confidence => Some(current),
        _ => Some((line, next)),
    }
}

pub fn should_include_row(row: &SessionRow, options: CollectOptions) -> bool {
    if !options.include_empty_messages && row.first_message.trim().is_empty() {
        return false;
    }
    if !options.include_subsessions && row.is_subsession {
        return false;
    }
    true
}

pub fn session_date(row: &SessionRow) -> String {
    row.timestamp
        .as_deref()
        .map(|timestamp| timestamp.chars().take(10).collect())
        .unwrap_or_default()
}

pub fn searchable_text(row: &SessionRow) -> String {
    [
        row.first_message.as_str(),
        row.title.as_deref().unwrap_or_default(),
        row.agent_type.as_deref().unwrap_or_default(),
        row.cwd.as_deref().unwrap_or_default(),
        row.repository_url.as_deref().unwrap_or_default(),
        row.branch.as_deref().unwrap_or_default(),
        row.timestamp.as_deref().unwrap_or_default(),
        &session_date(row),
    ]
    .join("\n")
    .to_lowercase()
}

pub fn filter_sessions(rows: &[SessionRow], query: &str) -> Vec<SessionRow> {
    let terms: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();

    if terms.is_empty() {
        return rows.to_vec();
    }

    rows.iter()
        .filter(|row| {
            let haystack = searchable_text(row);
            terms.iter().all(|term| haystack.contains(term))
        })
        .cloned()
        .collect()
}

pub fn collect_rows(
    sessions_root: &Path,
    options: CollectOptions,
) -> Result<(Vec<SessionRow>, usize, usize)> {
    let data = collect_session_data(sessions_root, options)?;

    Ok((data.rows, data.total_files, data.skipped))
}

pub fn collect_session_data(
    sessions_root: &Path,
    options: CollectOptions,
) -> Result<CollectedSessionData> {
    let paths = session_jsonl_paths(sessions_root)?;

    let mut rows = Vec::new();
    let mut exec_events = Vec::new();
    let mut skipped = 0;
    let total = paths.len();

    for path in paths {
        match parse_session_file_data(&path, options.include_exec)? {
            Some(data) if should_include_row(&data.row, options) => {
                rows.push(data.row);
                if options.include_exec {
                    exec_events.extend(data.exec_events);
                }
            }
            _ => skipped += 1,
        }
    }

    Ok(CollectedSessionData {
        rows,
        exec_events,
        total_files: total,
        skipped,
    })
}

// Main sessions are <root>/<project>/*.jsonl; subagents are
// <root>/<project>/<session>/subagents/agent-*.jsonl. Nothing else is visited.
pub fn session_jsonl_paths(sessions_root: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();

    if !sessions_root.exists() {
        return Ok(out);
    }

    for project in read_dirs(sessions_root)? {
        for entry in fs::read_dir(&project)
            .with_context(|| format!("failed to read {}", project.display()))?
        {
            let path = entry?.path();
            // Pushed by name only, so an unreadable entry fails the scan instead of vanishing.
            if path.extension().and_then(|ext| ext.to_str()) == Some("jsonl") {
                out.push(path);
                continue;
            }
            let subagents = path.join("subagents");
            if !subagents.is_dir() {
                continue;
            }
            for sub in fs::read_dir(&subagents)
                .with_context(|| format!("failed to read {}", subagents.display()))?
            {
                let sub = sub?.path();
                if is_subsession_path(&sub) {
                    out.push(sub);
                }
            }
        }
    }

    out.sort();
    Ok(out)
}

pub fn recreate_database(db_path: &Path, rows: &[SessionRow]) -> Result<()> {
    recreate_database_with_exec(db_path, rows, None)
}

pub fn recreate_database_with_exec(
    db_path: &Path,
    rows: &[SessionRow],
    exec_events: Option<&[ExecEvent]>,
) -> Result<()> {
    if let Some(parent) = db_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    let mut conn = Connection::open(db_path)
        .with_context(|| format!("failed to open {}", db_path.display()))?;
    conn.execute("DROP TABLE IF EXISTS sessions", [])?;
    conn.execute(
        r#"
        CREATE TABLE sessions (
            path TEXT,
            id TEXT,
            timestamp TEXT,
            cwd TEXT,
            repository_url TEXT,
            branch TEXT,
            first_message TEXT
        )
        "#,
        [],
    )?;

    let tx = conn.transaction()?;
    {
        let mut stmt = tx.prepare(
            r#"
            INSERT INTO sessions (
                path, id, timestamp, cwd, repository_url, branch, first_message
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            "#,
        )?;

        for row in rows {
            stmt.execute(params![
                row.path.to_string_lossy().as_ref(),
                row.id.as_deref(),
                row.timestamp.as_deref(),
                row.cwd.as_deref(),
                row.repository_url.as_deref(),
                row.branch.as_deref(),
                row.first_message.as_str(),
            ])?;
        }
    }
    tx.commit()?;

    conn.execute("CREATE INDEX sessions_path_idx ON sessions(path)", [])?;
    conn.execute(
        "CREATE INDEX sessions_timestamp_idx ON sessions(timestamp)",
        [],
    )?;
    conn.execute("CREATE INDEX sessions_cwd_idx ON sessions(cwd)", [])?;

    if let Some(exec_events) = exec_events {
        conn.execute("DROP TABLE IF EXISTS exec_events", [])?;
        conn.execute(
            r#"
            CREATE TABLE exec_events (
                session_path TEXT NOT NULL,
                session_id TEXT,
                event_index INTEGER NOT NULL,
                call_id TEXT,
                kind TEXT NOT NULL,
                name TEXT,
                command TEXT,
                output TEXT
            )
            "#,
            [],
        )?;

        let tx = conn.transaction()?;
        {
            let mut stmt = tx.prepare(
                r#"
                INSERT INTO exec_events (
                    session_path, session_id, event_index, call_id, kind, name, command, output
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                "#,
            )?;

            for event in exec_events {
                stmt.execute(params![
                    event.session_path.to_string_lossy().as_ref(),
                    event.session_id.as_deref(),
                    event.event_index as i64,
                    event.call_id.as_deref(),
                    event.kind.as_str(),
                    event.name.as_deref(),
                    event.command.as_str(),
                    event.output.as_str(),
                ])?;
            }
        }
        tx.commit()?;

        conn.execute(
            "CREATE INDEX exec_events_session_path_idx ON exec_events(session_path)",
            [],
        )?;
        conn.execute(
            "CREATE INDEX exec_events_session_id_idx ON exec_events(session_id)",
            [],
        )?;
    } else {
        conn.execute("DROP TABLE IF EXISTS exec_events", [])?;
    }
    Ok(())
}

pub fn load_sessions(db_path: &Path) -> Result<Vec<SessionRow>> {
    indexer::store::load_sessions_with_view(db_path, indexer::store::SessionView::default())
}

fn read_dirs(path: &Path) -> Result<Vec<PathBuf>> {
    let mut dirs = Vec::new();
    for entry in fs::read_dir(path).with_context(|| format!("failed to read {}", path.display()))? {
        let path = entry?.path();
        if path.is_dir() {
            dirs.push(path);
        }
    }
    dirs.sort();
    Ok(dirs)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::test_support::SessionFixture;

    const MAIN: &str = include_str!("../tests/fixtures/main-session.jsonl");
    const SUB: &str = include_str!("../tests/fixtures/subagent/agent-a1.jsonl");
    const SUB_META: &str = include_str!("../tests/fixtures/subagent/agent-a1.meta.json");

    struct Layout {
        fixture: SessionFixture,
        main: PathBuf,
        sub: PathBuf,
    }

    // <root>/-tmp-fixture/main-1.jsonl and main-1/subagents/agent-a1.jsonl
    fn layout() -> Layout {
        let fixture = SessionFixture::new();
        let project = fixture.sessions_root().join("-tmp-fixture");
        let subagents = project.join("main-1").join("subagents");
        fs::create_dir_all(&subagents).unwrap();
        let main = project.join("main-1.jsonl");
        let sub = subagents.join("agent-a1.jsonl");
        fs::write(&main, MAIN).unwrap();
        fs::write(&sub, SUB).unwrap();
        fs::write(subagents.join("agent-a1.meta.json"), SUB_META).unwrap();
        Layout { fixture, main, sub }
    }

    fn row(path: &str, first_message: &str) -> SessionRow {
        SessionRow {
            path: PathBuf::from(path),
            first_message: first_message.to_string(),
            ..SessionRow::default()
        }
    }

    #[test]
    fn collect_options_default_disables_all_optional_data() {
        let options = CollectOptions::default();

        assert!(!options.include_subsessions);
        assert!(!options.include_empty_messages);
        assert!(!options.include_exec);
    }

    #[test]
    fn classify_source_distinguishes_main_and_subagent() {
        let layout = layout();
        assert_eq!(
            classify_source(&layout.main),
            Some(SourceKind::Main {
                session_id: "main-1".to_string()
            })
        );
        assert_eq!(
            classify_source(&layout.sub),
            Some(SourceKind::Subagent {
                parent_session_id: "main-1".to_string(),
                agent_id: "a1".to_string(),
            })
        );
        assert_eq!(
            classify_source(&layout.sub.with_file_name("agent-a1.meta.json")),
            None
        );
        assert_eq!(
            classify_source(&layout.sub.with_file_name("notes.jsonl")),
            None
        );
    }

    #[test]
    fn session_jsonl_paths_lists_main_and_subagent_files_only() {
        let layout = layout();
        let root = layout.fixture.sessions_root();
        let project = root.join("-tmp-fixture");
        let tool_results = project.join("main-1").join("tool-results");
        fs::create_dir_all(&tool_results).unwrap();
        fs::write(tool_results.join("x.jsonl"), "{}\n").unwrap();
        fs::create_dir_all(project.join("memory")).unwrap();
        fs::write(project.join("memory").join("MEMORY.md"), "").unwrap();
        fs::write(root.join("stray.jsonl"), "{}\n").unwrap();

        assert_eq!(
            session_jsonl_paths(&root).unwrap(),
            vec![layout.sub.clone(), layout.main.clone()]
        );
        assert!(
            session_jsonl_paths(&root.join("missing"))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn main_session_row_uses_first_values_and_last_title() {
        let layout = layout();
        let row = parse_session_file(&layout.main).unwrap().unwrap();

        assert_eq!(row.path, layout.main);
        assert_eq!(row.id.as_deref(), Some("main-1"));
        assert_eq!(row.timestamp.as_deref(), Some("2026-11-01T00:00:00.000Z"));
        assert_eq!(row.cwd.as_deref(), Some("/tmp/fixture"));
        assert_eq!(row.branch.as_deref(), Some("main"));
        assert_eq!(row.repository_url.as_deref(), Some("git.example/team/repo"));
        assert_eq!(row.first_message, "fix the parser");
        assert!(!row.is_subsession);
        assert_eq!(row.title.as_deref(), Some("Parser fix"));
        assert_eq!(row.continued_in_session_id.as_deref(), Some("main-2"));
        assert_eq!(row.parent_session_id, None);
        assert_eq!(row.parent_tool_use_id, None);
        assert_eq!(row.agent_type, None);
    }

    #[test]
    fn agent_name_is_the_title_fallback() {
        let layout = layout();
        let without_ai_title: String = MAIN
            .lines()
            .filter(|line| !line.contains("\"ai-title\""))
            .chain(["{\"type\":\"agent-name\",\"agentName\":\"Named session\"}"])
            .map(|line| format!("{line}\n"))
            .collect();
        fs::write(&layout.main, without_ai_title).unwrap();

        let row = parse_session_file(&layout.main).unwrap().unwrap();
        assert_eq!(row.title.as_deref(), Some("Named session"));
    }

    #[test]
    fn subagent_row_reads_path_and_meta() {
        let layout = layout();
        let row = parse_session_file(&layout.sub).unwrap().unwrap();

        assert_eq!(row.id.as_deref(), Some("a1"));
        assert!(row.is_subsession);
        assert_eq!(row.first_message, "count the files");
        assert_eq!(row.cwd.as_deref(), Some("/tmp/fixture"));
        assert_eq!(row.parent_session_id.as_deref(), Some("main-1"));
        assert_eq!(row.parent_tool_use_id.as_deref(), Some("toolu_parent"));
        assert_eq!(row.agent_type.as_deref(), Some("general-purpose"));
        assert_eq!(row.title.as_deref(), Some("Count files"));
        assert_eq!(row.continued_in_session_id, None);
    }

    #[test]
    fn missing_or_broken_meta_leaves_columns_empty() {
        let layout = layout();
        let meta = layout.sub.with_file_name("agent-a1.meta.json");
        for contents in [None, Some("{")] {
            match contents {
                None => fs::remove_file(&meta).unwrap(),
                Some(contents) => fs::write(&meta, contents).unwrap(),
            }
            let row = parse_session_file(&layout.sub).unwrap().unwrap();
            assert!(row.is_subsession);
            assert_eq!(row.parent_session_id.as_deref(), Some("main-1"));
            assert_eq!(row.title, None);
            assert_eq!(row.parent_tool_use_id, None);
            assert_eq!(row.agent_type, None);
        }
    }

    #[test]
    fn unsourced_prompt_is_used_when_no_typed_prompt_exists() {
        let layout = layout();
        let only_command: String = MAIN
            .lines()
            .filter(|line| !line.contains("fix the parser"))
            .map(|line| format!("{line}\n"))
            .collect();
        fs::write(&layout.main, only_command).unwrap();

        assert_eq!(
            parse_session_file(&layout.main)
                .unwrap()
                .unwrap()
                .first_message,
            "<command-message>init</command-message>"
        );
    }

    #[test]
    fn exec_events_pair_bash_calls_with_results() {
        let layout = layout();
        let parsed = parse_session_file_data(&layout.main, true)
            .unwrap()
            .unwrap();

        assert_eq!(
            parsed.exec_events,
            vec![ExecEvent {
                session_path: layout.main.clone(),
                session_id: Some("main-1".to_string()),
                event_index: 7,
                call_id: Some("toolu_1".to_string()),
                kind: "tool_use".to_string(),
                name: Some("Bash".to_string()),
                command: "git status".to_string(),
                output: "clean".to_string(),
                called_at: Some("2026-11-01T00:00:05.000Z".to_string()),
                result_at: Some("2026-11-01T00:00:06.000Z".to_string()),
            }]
        );
    }

    #[test]
    fn message_events_exclude_meta_blob_and_tool_results() {
        let layout = layout();
        let parsed = parse_session_file_data(&layout.main, true)
            .unwrap()
            .unwrap();

        assert_eq!(
            parsed
                .message_events
                .iter()
                .map(|event| (event.event_index, event.role))
                .collect::<Vec<_>>(),
            vec![
                (3, session_event::MessageRole::User),
                (4, session_event::MessageRole::User),
                (6, session_event::MessageRole::Agent),
            ]
        );

        let sub = parse_session_file_data(&layout.sub, true).unwrap().unwrap();
        assert_eq!(
            sub.message_events
                .iter()
                .map(|event| (event.role, event.content.as_str()))
                .collect::<Vec<_>>(),
            vec![
                (session_event::MessageRole::User, "count the files"),
                (session_event::MessageRole::Agent, "there are 3 files"),
            ]
        );
    }

    #[test]
    fn exec_times_follow_the_stored_output_and_allow_missing_values() {
        let layout = layout();
        let mut call = test_support::bash_call("repeated", "one");
        call["timestamp"] = "called".into();
        let mut interrupted = test_support::tool_result("repeated", "interrupted".into());
        interrupted["timestamp"] = "interrupted".into();
        let mut finished = test_support::tool_result("repeated", "done".into());
        finished["timestamp"] = "finished".into();
        let mut pending = test_support::bash_call("pending", "two");
        pending["timestamp"] = "pending".into();
        let records = [
            call,
            interrupted,
            finished,
            pending,
            test_support::bash_call("untimed", "three"),
            test_support::tool_result("untimed", "done".into()),
        ];
        fs::write(
            &layout.main,
            records.map(|record| record.to_string()).join("\n"),
        )
        .unwrap();

        let events = parse_session_file_data(&layout.main, true)
            .unwrap()
            .unwrap()
            .exec_events;
        let times = events
            .iter()
            .map(|event| (event.called_at.as_deref(), event.result_at.as_deref()))
            .collect::<Vec<_>>();
        assert_eq!(
            times,
            [
                (Some("called"), Some("finished")),
                (Some("pending"), None),
                (None, None),
            ]
        );
    }

    #[test]
    fn only_the_first_bash_call_of_a_record_is_indexed() {
        let layout = layout();
        let record = serde_json::json!({"type": "assistant", "message": {"content": [
            {"type": "tool_use", "id": "t1", "name": "Bash", "input": {"command": "one"}},
            {"type": "tool_use", "id": "t2", "name": "Bash", "input": {"command": "two"}}
        ]}});
        fs::write(&layout.main, format!("{record}\n")).unwrap();

        let parsed = parse_session_file_data(&layout.main, true)
            .unwrap()
            .unwrap();
        assert_eq!(parsed.exec_events.len(), 1);
        assert_eq!(parsed.exec_events[0].command, "one");
    }

    #[test]
    fn file_without_conversation_is_skipped() {
        let layout = layout();
        fs::write(
            &layout.main,
            "{\"type\":\"mode\",\"mode\":\"normal\"}\n{\"type\":\"ai-title\",\"aiTitle\":\"t\"}\n",
        )
        .unwrap();

        assert_eq!(parse_session_file(&layout.main).unwrap(), None);
    }

    #[test]
    fn invalid_json_line_is_skipped_before_valid_session_records() {
        let layout = layout();
        fs::write(&layout.main, format!("not-json\n{{\"type\":\n{MAIN}")).unwrap();

        let row = parse_session_file(&layout.main).unwrap().unwrap();
        assert_eq!(row.id.as_deref(), Some("main-1"));
        assert_eq!(row.first_message, "fix the parser");
    }

    #[test]
    fn should_include_hides_empty_and_subsession_by_default() {
        let mut row = row("/tmp/a.jsonl", "ordinary looking task text");
        row.is_subsession = true;
        assert!(!should_include_row(&row, CollectOptions::default()));

        row.is_subsession = false;
        row.first_message = "   ".to_string();
        assert!(!should_include_row(&row, CollectOptions::default()));

        row.first_message = "human request".to_string();
        assert!(should_include_row(&row, CollectOptions::default()));
    }

    #[test]
    fn filter_sessions_searches_message_metadata_and_date() {
        let mut a = row("/tmp/a.jsonl", "fix docker compose");
        a.timestamp = Some("2026-05-27T01:00:00Z".to_string());
        a.cwd = Some("/repo/alpha".to_string());
        a.repository_url = Some("git.example/alpha".to_string());
        a.branch = Some("main".to_string());
        let mut b = row("/tmp/b.jsonl", "add selector");
        b.timestamp = Some("2026-05-28T01:00:00Z".to_string());
        b.cwd = Some("/repo/beta".to_string());
        b.repository_url = Some("git.example/beta".to_string());
        b.branch = Some("feature/search".to_string());
        let rows = vec![a, b];

        assert_eq!(
            filter_sessions(&rows, "docker")[0].path,
            PathBuf::from("/tmp/a.jsonl")
        );
        assert_eq!(
            filter_sessions(&rows, "example/alpha")[0].path,
            PathBuf::from("/tmp/a.jsonl")
        );
        assert_eq!(
            filter_sessions(&rows, "feature")[0].path,
            PathBuf::from("/tmp/b.jsonl")
        );
        assert_eq!(
            filter_sessions(&rows, "2026-05-28")[0].path,
            PathBuf::from("/tmp/b.jsonl")
        );
    }

    #[test]
    fn searchable_text_includes_title_and_agent_type() {
        let mut titled = row("/tmp/a.jsonl", "first");
        titled.title = Some("Parser fix".to_string());
        let mut typed = row("/tmp/b.jsonl", "second");
        typed.agent_type = Some("general-purpose".to_string());
        let rows = vec![titled, typed];

        assert_eq!(
            filter_sessions(&rows, "parser fix")
                .iter()
                .map(|row| row.path.clone())
                .collect::<Vec<_>>(),
            vec![PathBuf::from("/tmp/a.jsonl")]
        );
        assert_eq!(
            filter_sessions(&rows, "general-purpose")
                .iter()
                .map(|row| row.path.clone())
                .collect::<Vec<_>>(),
            vec![PathBuf::from("/tmp/b.jsonl")]
        );
    }

    #[test]
    fn collect_session_data_omits_exec_by_default_and_collects_when_enabled() {
        let fixture = SessionFixture::new();
        fixture.write_session_with_exec();
        let root = fixture.sessions_root();

        let default_data = collect_session_data(&root, CollectOptions::default()).unwrap();
        assert_eq!(default_data.rows.len(), 1);
        assert_eq!(default_data.exec_events.len(), 0);

        let with_exec = collect_session_data(
            &root,
            CollectOptions {
                include_exec: true,
                ..CollectOptions::default()
            },
        )
        .unwrap();

        assert_eq!(with_exec.rows.len(), 1);
        let events = with_exec
            .exec_events
            .iter()
            .map(|event| {
                (
                    event.call_id.as_deref().unwrap(),
                    event.command.as_str(),
                    event.output.as_str(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            events,
            vec![
                ("call-0", "sed -n '1,80p' README.md", "# Title"),
                ("call-1", "pwd", "Script completed\n/repo/demo"),
                ("call-2", "git status --short", " M README.md"),
            ]
        );
        assert_eq!(
            with_exec.rows[0].repository_url.as_deref(),
            Some("git.example/demo")
        );
    }

    #[test]
    fn recreate_database_creates_exec_table_only_when_requested() {
        let fixture = SessionFixture::new();
        let db_path = fixture.path("sessions.sqlite3");
        let mut row = row("/tmp/session.jsonl", "real user request");
        row.id = Some("session-id".to_string());
        let exec = ExecEvent {
            session_path: row.path.clone(),
            session_id: row.id.clone(),
            event_index: 2,
            call_id: Some("call-1".to_string()),
            kind: "tool_use".to_string(),
            name: Some("Bash".to_string()),
            command: "pwd".to_string(),
            output: "/repo/demo".to_string(),
            called_at: None,
            result_at: None,
        };

        recreate_database(&db_path, std::slice::from_ref(&row)).unwrap();
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        assert!(conn.prepare("SELECT count(*) FROM exec_events").is_err());
        drop(conn);

        recreate_database_with_exec(&db_path, &[row], Some(&[exec])).unwrap();
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        let count: i64 = conn
            .query_row("SELECT count(*) FROM exec_events", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }
}
