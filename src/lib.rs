use std::{
    fs,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use rusqlite::{Connection, params};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRow {
    pub path: PathBuf,
    pub id: Option<String>,
    pub timestamp: Option<String>,
    pub cwd: Option<String>,
    pub repository_url: Option<String>,
    pub branch: Option<String>,
    pub first_message: String,
    pub is_subsession: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct CollectOptions {
    pub include_subsessions: bool,
    pub include_empty_messages: bool,
}

impl Default for CollectOptions {
    fn default() -> Self {
        Self {
            include_subsessions: false,
            include_empty_messages: false,
        }
    }
}

pub fn is_subsession_path(path: &Path) -> bool {
    path.components()
        .any(|component| component.as_os_str() == "subagents")
}

pub fn parse_session_file(path: &Path) -> Result<Option<SessionRow>> {
    let file =
        fs::File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let reader = BufReader::new(file);
    let mut meta: Option<SessionRow> = None;
    let mut first_message: Option<String> = None;

    for (line_number, line) in reader.lines().enumerate() {
        let line =
            line.with_context(|| format!("failed to read {}:{}", path.display(), line_number + 1))?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let value: Value = match serde_json::from_str(line) {
            Ok(value) => value,
            Err(err) => {
                eprintln!(
                    "warning: {}:{}: invalid json: {err}",
                    path.display(),
                    line_number + 1
                );
                continue;
            }
        };

        if meta.is_none() && value.get("message").is_some() {
            meta = Some(SessionRow {
                path: path.to_path_buf(),
                id: string_field(&value, "sessionId"),
                timestamp: string_field(&value, "timestamp"),
                cwd: string_field(&value, "cwd"),
                repository_url: None,
                branch: string_field(&value, "gitBranch"),
                first_message: String::new(),
                is_subsession: is_subsession_path(path),
            });
        }

        if first_message.is_none() {
            first_message = extract_claude_user_text(&value);
        }

        if meta.is_some() && first_message.is_some() {
            break;
        }
    }

    let Some(mut row) = meta else { return Ok(None) };
    row.first_message = first_message.unwrap_or_default();
    Ok(Some(row))
}

pub fn extract_claude_user_text(value: &Value) -> Option<String> {
    let message = value.get("message")?;

    if message.get("role").and_then(Value::as_str) != Some("user") {
        return None;
    }

    let text = message.get("content")?.as_str()?;

    if text.starts_with("<task-notification>") || text.trim().is_empty() {
        return None;
    }

    Some(text.to_string())
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
    let mut paths = session_jsonl_paths(sessions_root)?;
    paths.sort();

    let mut rows = Vec::new();
    let mut skipped = 0;
    let total = paths.len();

    for path in paths {
        match parse_session_file(&path)? {
            Some(row) if should_include_row(&row, options) => rows.push(row),
            _ => skipped += 1,
        }
    }

    Ok((rows, total, skipped))
}

pub fn session_jsonl_paths(sessions_root: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();

    if !sessions_root.exists() {
        return Ok(out);
    }

    collect_jsonl_paths_recursive(sessions_root, &mut out)?;

    Ok(out)
}

fn collect_jsonl_paths_recursive(path: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(path).with_context(|| format!("failed to read {}", path.display()))? {
        let path = entry?.path();
        if path.is_dir() {
            collect_jsonl_paths_recursive(&path, out)?;
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("jsonl") {
            out.push(path);
        }
    }

    Ok(())
}

pub fn recreate_database(db_path: &Path, rows: &[SessionRow]) -> Result<()> {
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
    Ok(())
}

pub fn load_sessions(db_path: &Path) -> Result<Vec<SessionRow>> {
    let conn = Connection::open(db_path)
        .with_context(|| format!("failed to open {}", db_path.display()))?;
    let mut stmt = conn.prepare(
        r#"
        SELECT path, id, timestamp, cwd, repository_url, branch, first_message
        FROM sessions
        ORDER BY timestamp DESC
        "#,
    )?;

    let rows = stmt
        .query_map([], |row| {
            Ok(SessionRow {
                path: PathBuf::from(row.get::<_, String>(0)?),
                id: row.get(1)?,
                timestamp: row.get(2)?,
                cwd: row.get(3)?,
                repository_url: row.get(4)?,
                branch: row.get(5)?,
                first_message: row.get::<_, Option<String>>(6)?.unwrap_or_default(),
                is_subsession: false,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    Ok(rows)
}

fn string_field(value: &Value, field: &str) -> Option<String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    use serde_json::json;

    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("claude-session-selector-{nonce}-{name}"))
    }

    fn write_jsonl(path: &Path, message: &str) {
        let snapshot = json!({
            "type": "file-history-snapshot",
            "snapshot": {}
        });
        let meta_user = json!({
            "type": "user",
            "sessionId": "session-id",
            "timestamp": "2026-05-28T00:00:00Z",
            "cwd": "/repo/demo",
            "gitBranch": "main",
            "message": {
                "role": "user",
                "content": ["meta content should be ignored"]
            }
        });
        let user = json!({
            "type": "user",
            "sessionId": "session-id",
            "timestamp": "2026-05-28T00:00:01Z",
            "cwd": "/repo/demo",
            "gitBranch": "main",
            "message": {
                "role": "user",
                "content": message
            }
        });
        fs::write(path, format!("{snapshot}\n{meta_user}\n{user}\n")).unwrap();
    }

    #[test]
    fn detects_subsessions_from_directory_structure() {
        assert!(is_subsession_path(Path::new(
            "/home/user/.claude/projects/-repo/session-id/subagents/agent-a.jsonl"
        )));
        assert!(!is_subsession_path(Path::new(
            "/home/user/.claude/projects/-repo/session-id.jsonl"
        )));
    }

    #[test]
    fn parse_session_file_extracts_metadata_and_first_message() {
        let path = temp_path("human.jsonl");
        write_jsonl(&path, "real user request");

        let row = parse_session_file(&path).unwrap().unwrap();

        assert_eq!(row.path, path);
        assert_eq!(row.id.as_deref(), Some("session-id"));
        assert_eq!(row.timestamp.as_deref(), Some("2026-05-28T00:00:00Z"));
        assert_eq!(row.cwd.as_deref(), Some("/repo/demo"));
        assert_eq!(row.repository_url, None);
        assert_eq!(row.branch.as_deref(), Some("main"));
        assert_eq!(row.first_message, "real user request");
        assert!(!row.is_subsession);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn session_jsonl_paths_recurses_project_and_subagent_layout() {
        let root = temp_path("projects");
        let main_dir = root.join("-repo");
        let subagent_dir = main_dir.join("session-id").join("subagents");
        fs::create_dir_all(&subagent_dir).unwrap();

        let main = main_dir.join("session-id.jsonl");
        let subagent = subagent_dir.join("agent-a.jsonl");
        fs::write(&main, "{}\n").unwrap();
        fs::write(&subagent, "{}\n").unwrap();

        let mut paths = session_jsonl_paths(&root).unwrap();
        paths.sort();

        let mut expected = vec![main, subagent];
        expected.sort();

        assert_eq!(paths, expected);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn should_include_hides_empty_and_subsession_by_default() {
        let mut row = SessionRow {
            path: PathBuf::from("/tmp/a.jsonl"),
            id: None,
            timestamp: None,
            cwd: None,
            repository_url: None,
            branch: None,
            first_message: "ordinary looking task text".to_string(),
            is_subsession: true,
        };
        assert!(!should_include_row(&row, CollectOptions::default()));

        row.is_subsession = false;
        row.first_message = "   ".to_string();
        assert!(!should_include_row(&row, CollectOptions::default()));

        row.first_message = "human request".to_string();
        assert!(should_include_row(&row, CollectOptions::default()));
    }

    #[test]
    fn filter_sessions_searches_message_metadata_and_date() {
        let rows = vec![
            SessionRow {
                path: PathBuf::from("/tmp/a.jsonl"),
                id: None,
                timestamp: Some("2026-05-27T01:00:00Z".to_string()),
                cwd: Some("/repo/alpha".to_string()),
                repository_url: Some("https://git.example/alpha.git".to_string()),
                branch: Some("main".to_string()),
                first_message: "fix docker compose".to_string(),
                is_subsession: false,
            },
            SessionRow {
                path: PathBuf::from("/tmp/b.jsonl"),
                id: None,
                timestamp: Some("2026-05-28T01:00:00Z".to_string()),
                cwd: Some("/repo/beta".to_string()),
                repository_url: Some("https://git.example/beta.git".to_string()),
                branch: Some("feature/search".to_string()),
                first_message: "add selector".to_string(),
                is_subsession: false,
            },
        ];

        assert_eq!(
            filter_sessions(&rows, "docker")[0].path,
            PathBuf::from("/tmp/a.jsonl")
        );
        assert_eq!(
            filter_sessions(&rows, "alpha.git")[0].path,
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
}
