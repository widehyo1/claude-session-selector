//! Moves the exec timing lines appended by the Claude Code hook into SQLite.
//!
//! Unlike the index, these rows cannot be rebuilt from transcripts, so they have
//! their own database and a line is deleted only after it was committed.

use std::{
    fs,
    io::ErrorKind,
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use rusqlite::{Transaction, params};
use serde::Deserialize;

use crate::indexer::{self, store};

const SCHEMA_VERSION: i64 = 1;
const RECORD_VERSION: i64 = 1;

// A hook that opened the log before the rename may still append to the renamed
// file, so that file is read again until it is well past the 5 s hook timeout.
const SETTLE_TIME: Duration = Duration::from_secs(60);

const TABLE_DDL: &str = "
CREATE TABLE exec_timings (
    tool_use_id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    agent_id TEXT,
    started_at_ms INTEGER,
    ended_at_ms INTEGER NOT NULL,
    duration_ms INTEGER,
    outcome TEXT NOT NULL,
    background_task_id TEXT
) STRICT;
CREATE INDEX exec_timings_session_id_idx ON exec_timings(session_id);
";

#[derive(Debug, Deserialize)]
struct Record {
    v: i64,
    tool_use_id: String,
    session_id: String,
    agent_id: Option<String>,
    ended_at_ms: i64,
    duration_ms: Option<i64>,
    outcome: String,
    background_task_id: Option<String>,
}

/// The log lives next to the sessions root: `~/.claude/exec-log` by default.
/// Without that directory the hook is not installed and nothing is done.
pub(crate) fn refresh(sessions_root: &Path) -> Result<()> {
    let root = indexer::absolute_lexical_path(sessions_root)?;
    let Some(dir) = root.parent().map(|parent| parent.join("exec-log")) else {
        return Ok(());
    };
    if !dir.is_dir() {
        return Ok(());
    }
    ingest(&dir, SystemTime::now().duration_since(UNIX_EPOCH)?)
        .with_context(|| format!("failed to store exec timings in {}", dir.display()))
}

fn ingest(dir: &Path, now: Duration) -> Result<()> {
    let pending_name = format!("pending.{}.{}.jsonl", now.as_millis(), std::process::id());
    ignore_missing(fs::rename(dir.join("exec.jsonl"), dir.join(pending_name)))?;

    let mut pending = Vec::new();
    for entry in fs::read_dir(dir)? {
        let name = entry?.file_name().to_string_lossy().into_owned();
        if name.starts_with("pending.") && name.ends_with(".jsonl") {
            pending.push(name);
        }
    }
    if pending.is_empty() {
        return Ok(());
    }
    pending.sort();

    let mut conn = store::open_configured_connection(&dir.join("exec.sqlite3"))?;
    let tx = store::begin_immediate(&mut conn)?;
    prepare_schema(&tx)?;
    let mut settled = Vec::new();
    for name in pending {
        let contents = match fs::read(dir.join(&name)) {
            Ok(contents) => contents,
            // Another refresh committed and removed it.
            Err(error) if error.kind() == ErrorKind::NotFound => continue,
            Err(error) => return Err(error).with_context(|| format!("failed to read {name}")),
        };
        let unreadable = insert_records(&tx, &contents)?;
        if is_settled(&name, now) {
            settled.push((name, unreadable));
        }
    }
    tx.commit()?;

    for (name, unreadable) in settled {
        let path = dir.join(&name);
        if unreadable == 0 {
            ignore_missing(fs::remove_file(path))?;
        } else {
            let rejected = dir.join(name.replacen("pending.", "rejected.", 1));
            eprintln!(
                "warning: {}: kept {unreadable} unreadable exec timing lines",
                rejected.display()
            );
            ignore_missing(fs::rename(path, rejected))?;
        }
    }
    Ok(())
}

fn prepare_schema(tx: &Transaction<'_>) -> Result<()> {
    match tx.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))? {
        0 => {
            tx.execute_batch(TABLE_DDL)?;
            tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            Ok(())
        }
        SCHEMA_VERSION => Ok(()),
        version => bail!("unsupported exec timing schema version {version}"),
    }
}

// Returns how many lines could not be read. Lines already stored are ignored.
fn insert_records(tx: &Transaction<'_>, contents: &[u8]) -> Result<usize> {
    let mut insert = tx.prepare(
        "INSERT INTO exec_timings(
             tool_use_id, session_id, agent_id, started_at_ms,
             ended_at_ms, duration_ms, outcome, background_task_id
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(tool_use_id) DO NOTHING",
    )?;
    let mut unreadable = 0;
    for line in contents.split(|byte| *byte == b'\n') {
        if line.trim_ascii().is_empty() {
            continue;
        }
        let record = match serde_json::from_slice::<Record>(line) {
            Ok(record) if record.v == RECORD_VERSION => record,
            Ok(_) | Err(_) => {
                unreadable += 1;
                continue;
            }
        };
        let started_at_ms = record
            .duration_ms
            .and_then(|duration| record.ended_at_ms.checked_sub(duration));
        insert.execute(params![
            record.tool_use_id,
            record.session_id,
            record.agent_id,
            started_at_ms,
            record.ended_at_ms,
            record.duration_ms,
            record.outcome,
            record.background_task_id,
        ])?;
    }
    Ok(unreadable)
}

fn is_settled(name: &str, now: Duration) -> bool {
    let renamed_at = name
        .split('.')
        .nth(1)
        .and_then(|millis| millis.parse().ok())
        .map(Duration::from_millis)
        .unwrap_or_default();
    now.saturating_sub(renamed_at) >= SETTLE_TIME
}

fn ignore_missing(result: std::io::Result<()>) -> Result<()> {
    match result {
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        result => Ok(result?),
    }
}

#[cfg(test)]
mod tests {
    use std::{io::Write, path::PathBuf};

    use rusqlite::Connection;
    use serde_json::json;

    use super::*;
    use crate::test_support::SessionFixture;

    const START: Duration = Duration::from_secs(1_000_000);

    fn log_dir(fixture: &SessionFixture) -> PathBuf {
        let dir = fixture.path("exec-log");
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn line(id: &str, duration_ms: Option<i64>) -> String {
        let record = json!({
            "v": 1, "session_id": "session", "agent_id": null, "tool_use_id": id,
            "ended_at_ms": 5000, "duration_ms": duration_ms, "outcome": "ok",
            "background_task_id": null
        });
        format!("{record}\n")
    }

    fn rows(dir: &Path) -> Vec<(String, Option<i64>)> {
        let conn = Connection::open(dir.join("exec.sqlite3")).unwrap();
        let mut stmt = conn
            .prepare("SELECT tool_use_id, started_at_ms FROM exec_timings ORDER BY tool_use_id")
            .unwrap();
        stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    fn files(dir: &Path, prefix: &str) -> usize {
        fs::read_dir(dir)
            .unwrap()
            .filter(|entry| {
                let name = entry.as_ref().unwrap().file_name();
                name.to_string_lossy().starts_with(prefix)
            })
            .count()
    }

    #[test]
    fn lines_are_stored_at_once_and_deleted_after_settling() {
        let fixture = SessionFixture::new();
        let dir = log_dir(&fixture);
        fs::write(dir.join("exec.jsonl"), line("a", Some(1200))).unwrap();
        // A hook that opened the log before the rename and writes after it.
        let mut late = fs::OpenOptions::new()
            .append(true)
            .open(dir.join("exec.jsonl"))
            .unwrap();

        ingest(&dir, START).unwrap();
        assert_eq!(rows(&dir), [("a".to_owned(), Some(3800))]);
        assert_eq!(files(&dir, "pending."), 1);

        late.write_all(line("b", None).as_bytes()).unwrap();
        ingest(&dir, START + SETTLE_TIME).unwrap();
        assert_eq!(
            rows(&dir),
            [("a".to_owned(), Some(3800)), ("b".to_owned(), None)]
        );
        assert_eq!(files(&dir, "pending."), 0);
    }

    #[test]
    fn repeated_lines_are_stored_once() {
        let fixture = SessionFixture::new();
        let dir = log_dir(&fixture);
        for step in 0..3 {
            fs::write(dir.join("exec.jsonl"), line("a", Some(1000))).unwrap();
            ingest(&dir, START + SETTLE_TIME * step).unwrap();
        }
        assert_eq!(rows(&dir), [("a".to_owned(), Some(4000))]);
    }

    #[test]
    fn unreadable_lines_are_kept_without_blocking_the_others() {
        let fixture = SessionFixture::new();
        let dir = log_dir(&fixture);
        let newer = line("newer", None).replace("\"v\":1", "\"v\":2");
        let log = format!("{}{newer}{{\"v\":1,\"tool", line("a", Some(0)));
        fs::write(dir.join("exec.jsonl"), log).unwrap();

        ingest(&dir, START).unwrap();
        assert_eq!(rows(&dir), [("a".to_owned(), Some(5000))]);
        assert_eq!(files(&dir, "rejected."), 0);

        ingest(&dir, START + SETTLE_TIME).unwrap();
        assert_eq!(files(&dir, "pending."), 0);
        assert_eq!(files(&dir, "rejected."), 1);
    }

    #[test]
    fn a_failed_commit_keeps_the_lines() {
        let fixture = SessionFixture::new();
        let dir = log_dir(&fixture);
        Connection::open(dir.join("exec.sqlite3"))
            .unwrap()
            .execute_batch("PRAGMA user_version = 2;")
            .unwrap();
        fs::write(dir.join("exec.jsonl"), line("a", Some(1))).unwrap();

        assert!(ingest(&dir, START).is_err());
        assert!(ingest(&dir, START + SETTLE_TIME).is_err());
        assert_eq!(files(&dir, "pending."), 1);
    }

    #[test]
    fn refresh_does_nothing_without_the_log_directory() {
        let fixture = SessionFixture::new();
        refresh(&fixture.sessions_root()).unwrap();
        assert!(!fixture.path("exec-log").exists());
    }
}
