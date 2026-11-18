use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde_json::json;

pub(crate) struct SessionFixture {
    root: PathBuf,
}

impl SessionFixture {
    pub(crate) fn new() -> Self {
        static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

        let unique = format!(
            "claude-session-selector-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        );
        let root = std::env::temp_dir().join(unique);
        std::fs::create_dir_all(&root).unwrap();
        Self { root }
    }

    pub(crate) fn path(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    pub(crate) fn sessions_root(&self) -> PathBuf {
        self.root.join("sessions")
    }

    pub(crate) fn project_dir(&self) -> PathBuf {
        let project = self.sessions_root().join("-repo-demo");
        std::fs::create_dir_all(&project).unwrap();
        project
    }

    pub(crate) fn write_session_with_exec(&self) -> PathBuf {
        let path = self.project_dir().join("session.jsonl");
        write_jsonl_with_exec(&path);
        path
    }

    // A subsession named `sub.jsonl` is written as
    // `<project>/parent-session/subagents/agent-sub.jsonl`.
    pub(crate) fn write_named_session(
        &self,
        name: &str,
        message: &str,
        subsession: bool,
    ) -> PathBuf {
        let (path, record) = if subsession {
            let dir = self.project_dir().join("parent-session").join("subagents");
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join(format!("agent-{name}"));
            let record = json!({
                "type": "user",
                "isSidechain": true,
                "parentUuid": null,
                "message": {"role": "user", "content": message},
                "timestamp": "2026-07-26T01:00:00Z",
                "cwd": "/repo/demo path",
                "gitBranch": "main"
            });
            (path, record)
        } else {
            let path = self.project_dir().join(name);
            (
                path,
                human_prompt(message, "2026-07-26T01:00:00Z", "/repo/demo path"),
            )
        };
        std::fs::write(&path, format!("{record}\n")).unwrap();
        path
    }

    // A transcript without user or assistant records is skipped by the indexer.
    pub(crate) fn write_no_meta(&self, name: &str) -> PathBuf {
        let path = self.project_dir().join(name);
        std::fs::write(
            &path,
            format!("{}\n", json!({"type": "ai-title", "aiTitle": "orphan"})),
        )
        .unwrap();
        path
    }
}

impl Drop for SessionFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

pub(crate) fn human_prompt(message: &str, timestamp: &str, cwd: &str) -> serde_json::Value {
    json!({
        "type": "user",
        "isSidechain": false,
        "origin": {"kind": "human"},
        "promptSource": "typed",
        "message": {"role": "user", "content": message},
        "timestamp": timestamp,
        "cwd": cwd,
        "gitBranch": "main"
    })
}

pub(crate) fn bash_call(call_id: &str, command: &str) -> serde_json::Value {
    json!({
        "type": "assistant",
        "isSidechain": false,
        "message": {"role": "assistant", "model": "claude-test", "content": [
            {"type": "tool_use", "id": call_id, "name": "Bash", "input": {"command": command}}
        ]}
    })
}

pub(crate) fn tool_result(call_id: &str, content: serde_json::Value) -> serde_json::Value {
    json!({
        "type": "user",
        "isSidechain": false,
        "message": {"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": call_id, "content": content}
        ]}
    })
}

pub(crate) fn write_jsonl_with_exec(path: &Path) {
    let mut origin_result = tool_result("call-0", json!("# Title"));
    origin_result["serverClassifierContext"] = json!({"context": {"git_state": {"visibility": {
        "origin": {"host": "git.example", "remote": "demo", "visibility": "unknown"}
    }}}});
    let records = [
        human_prompt("real user request", "2026-05-28T00:00:00Z", "/repo/demo"),
        bash_call("call-0", "sed -n '1,80p' README.md"),
        origin_result,
        bash_call("call-1", "pwd"),
        tool_result(
            "call-1",
            json!([
                {"type": "text", "text": "Script completed"},
                {"type": "text", "text": "/repo/demo"}
            ]),
        ),
        bash_call("call-2", "git status --short"),
        tool_result("call-2", json!(" M README.md")),
        tool_result("non-exec-call", json!("not an exec result")),
    ];
    let contents = records
        .into_iter()
        .map(|record| record.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(path, format!("{contents}\n")).unwrap();
}

pub(crate) fn table_exists(connection: &Connection, table: &str) -> bool {
    connection
        .query_row(
            "SELECT EXISTS(
                SELECT 1
                FROM sqlite_master
                WHERE type = 'table' AND name = ?1
            )",
            rusqlite::params![table],
            |row| row.get::<_, bool>(0),
        )
        .unwrap()
}

pub(crate) fn table_count(connection: &Connection, table: &str) -> i64 {
    let sql = match table {
        "sessions" => "SELECT count(*) FROM sessions",
        "exec_events" => "SELECT count(*) FROM exec_events",
        _ => panic!("unsupported fixture table: {table}"),
    };
    connection.query_row(sql, [], |row| row.get(0)).unwrap()
}
