use std::borrow::Cow;

use anyhow::{Result, bail};
use serde::{Deserialize, de::IgnoredAny};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecordKind {
    User,
    Assistant,
    AiTitle,
    AgentName,
    ContinuedIn,
    Other,
}

#[derive(Deserialize)]
struct RecordHead<'a> {
    #[serde(rename = "type", borrow, default)]
    kind: Option<TypeField<'a>>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum TypeField<'a> {
    Name(#[serde(borrow)] Cow<'a, str>),
    Other(IgnoredAny),
}

// Reads only the root `type`; serde skips every other field without building values.
pub(crate) fn record_kind(line: &str) -> serde_json::Result<RecordKind> {
    let head: RecordHead = serde_json::from_str(line)?;
    let name = match &head.kind {
        Some(TypeField::Name(name)) => Some(name.as_ref()),
        Some(TypeField::Other(_)) | None => None,
    };
    Ok(match name {
        Some("user") => RecordKind::User,
        Some("assistant") => RecordKind::Assistant,
        Some("ai-title") => RecordKind::AiTitle,
        Some("agent-name") => RecordKind::AgentName,
        Some("continued-in") => RecordKind::ContinuedIn,
        _ => RecordKind::Other,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MessageRole {
    User,
    Agent,
}

impl MessageRole {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Agent => "agent",
        }
    }
    pub(crate) fn from_str(value: &str) -> Result<Self> {
        match value {
            "user" => Ok(Self::User),
            "agent" => Ok(Self::Agent),
            _ => bail!("invalid canonical message role {value}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NormalizedMessage {
    pub role: MessageRole,
    pub content: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum FirstUserMessageConfidence {
    Unsourced,
    Confirmed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FirstUserMessageCandidate {
    pub confidence: FirstUserMessageConfidence,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BashCall {
    pub call_id: String,
    pub command: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ToolOutput {
    pub call_id: String,
    pub output: String,
}

const HANDBACK_TOOL: &str = "SubagentHandback";
const SYNTHETIC_MODEL: &str = "<synthetic>";

fn root_type(value: &Value) -> Option<&str> {
    value.get("type")?.as_str()
}

fn str_field<'a>(value: &'a Value, field: &str) -> Option<&'a str> {
    value.get(field)?.as_str()
}

// Only string content is a prompt; array content holds tool results or injected text.
fn user_text(value: &Value) -> Option<&str> {
    if root_type(value)? != "user" {
        return None;
    }
    value.pointer("/message/content")?.as_str()
}

fn is_human(value: &Value) -> bool {
    value.pointer("/origin/kind").and_then(Value::as_str) == Some("human")
}

fn is_sidechain(value: &Value) -> bool {
    value.get("isSidechain").and_then(Value::as_bool) == Some(true)
}

fn is_meta(value: &Value) -> bool {
    value.get("isMeta").and_then(Value::as_bool) == Some(true)
}

fn blocks(value: &Value) -> impl Iterator<Item = &Value> {
    value
        .pointer("/message/content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
}

pub(crate) fn first_user_message_candidate(value: &Value) -> Option<FirstUserMessageCandidate> {
    let content = user_text(value)?.to_owned();
    let confidence = if is_human(value) {
        match value.get("promptSource") {
            Some(Value::String(_)) => FirstUserMessageConfidence::Confirmed,
            _ => FirstUserMessageConfidence::Unsourced,
        }
    } else if is_sidechain(value) && matches!(value.get("parentUuid"), Some(Value::Null)) {
        // A subagent transcript starts with the delegated prompt.
        FirstUserMessageConfidence::Confirmed
    } else {
        return None;
    };
    Some(FirstUserMessageCandidate {
        confidence,
        content,
    })
}

pub(crate) fn normalize_message_record(value: &Value) -> Option<NormalizedMessage> {
    match root_type(value)? {
        "user" => {
            let content = user_text(value)?;
            let accepted = is_human(value) || (is_sidechain(value) && !is_meta(value));
            (accepted && !content.trim().is_empty()).then(|| NormalizedMessage {
                role: MessageRole::User,
                content: content.to_owned(),
            })
        }
        "assistant" => {
            if value.pointer("/message/model").and_then(Value::as_str) == Some(SYNTHETIC_MODEL) {
                return None;
            }
            let mut parts = Vec::new();
            for block in blocks(value) {
                match str_field(block, "type") {
                    Some("text") => parts.extend(str_field(block, "text")),
                    Some("tool_use") if str_field(block, "name") == Some(HANDBACK_TOOL) => {
                        parts.extend(block.pointer("/input/message").and_then(Value::as_str));
                    }
                    _ => {}
                }
            }
            parts.retain(|part| !part.trim().is_empty());
            (!parts.is_empty()).then(|| NormalizedMessage {
                role: MessageRole::Agent,
                content: parts.join("\n"),
            })
        }
        _ => None,
    }
}

pub(crate) fn bash_calls(value: &Value) -> Vec<BashCall> {
    if root_type(value) != Some("assistant") {
        return Vec::new();
    }
    blocks(value)
        .filter(|block| {
            str_field(block, "type") == Some("tool_use") && str_field(block, "name") == Some("Bash")
        })
        .filter_map(|block| {
            Some(BashCall {
                call_id: str_field(block, "id")?.to_owned(),
                command: block.pointer("/input/command")?.as_str()?.to_owned(),
            })
        })
        .collect()
}

pub(crate) fn tool_outputs(value: &Value) -> Vec<ToolOutput> {
    if root_type(value) != Some("user") {
        return Vec::new();
    }
    blocks(value)
        .filter(|block| str_field(block, "type") == Some("tool_result"))
        .filter_map(|block| {
            Some(ToolOutput {
                call_id: str_field(block, "tool_use_id")?.to_owned(),
                output: value_to_text(block.get("content").unwrap_or(&Value::Null)),
            })
        })
        .collect()
}

pub(crate) fn repository_ref(value: &Value) -> Option<String> {
    let origin = value.pointer("/serverClassifierContext/context/git_state/visibility/origin")?;
    let host = str_field(origin, "host")?.trim();
    let remote = str_field(origin, "remote")?.trim();
    (!host.is_empty() && !remote.is_empty()).then(|| format!("{host}/{remote}"))
}

pub(crate) fn value_to_text(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        Value::Array(values) => values
            .iter()
            .map(value_to_text)
            .filter(|value| !value.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Object(values) => values
            .get("text")
            .and_then(Value::as_str)
            .or_else(|| values.get("output").and_then(Value::as_str))
            .map(str::to_owned)
            .unwrap_or_else(|| value.to_string()),
        Value::Number(_) | Value::Bool(_) => value.to_string(),
        Value::Null => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const MAIN: &str = include_str!("../tests/fixtures/main-session.jsonl");
    const SUB: &str = include_str!("../tests/fixtures/subagent/agent-a1.jsonl");

    // Fixture line numbers in the plan are 1-based.
    fn line(fixture: &str, number: usize) -> &str {
        fixture.lines().nth(number - 1).unwrap()
    }

    fn value(fixture: &str, number: usize) -> Value {
        serde_json::from_str(line(fixture, number)).unwrap()
    }

    fn confirmed(content: &str) -> Option<FirstUserMessageCandidate> {
        Some(FirstUserMessageCandidate {
            confidence: FirstUserMessageConfidence::Confirmed,
            content: content.to_owned(),
        })
    }

    #[test]
    fn record_kind_reads_only_the_root_type() {
        let kinds = [1, 2, 3, 7, 13, 15]
            .map(|number| record_kind(line(MAIN, number)).unwrap())
            .to_vec();
        assert_eq!(
            kinds,
            [
                RecordKind::Other,
                RecordKind::Other,
                RecordKind::User,
                RecordKind::Assistant,
                RecordKind::AiTitle,
                RecordKind::ContinuedIn,
            ]
        );
        assert_eq!(
            record_kind(r#"{"type":"agent-name","agentName":"x"}"#).unwrap(),
            RecordKind::AgentName
        );
        assert_eq!(record_kind(r#"{"type":1}"#).unwrap(), RecordKind::Other);
        assert_eq!(record_kind("{}").unwrap(), RecordKind::Other);
    }

    #[test]
    fn record_kind_rejects_invalid_json() {
        assert!(record_kind(r#"{"type":"#).is_err());
        assert!(record_kind("not-json").is_err());
    }

    #[test]
    fn typed_human_prompt_is_confirmed() {
        assert_eq!(
            first_user_message_candidate(&value(MAIN, 5)),
            confirmed("fix the parser")
        );
    }

    #[test]
    fn human_prompt_without_source_is_unsourced() {
        assert_eq!(
            first_user_message_candidate(&value(MAIN, 4)),
            Some(FirstUserMessageCandidate {
                confidence: FirstUserMessageConfidence::Unsourced,
                content: "<command-message>init</command-message>".to_owned(),
            })
        );
    }

    #[test]
    fn meta_and_blob_records_are_not_first_message_candidates() {
        assert_eq!(first_user_message_candidate(&value(MAIN, 2)), None);
        assert_eq!(first_user_message_candidate(&value(MAIN, 3)), None);
    }

    #[test]
    fn tool_result_user_record_is_not_a_candidate() {
        assert_eq!(first_user_message_candidate(&value(MAIN, 9)), None);
    }

    #[test]
    fn sidechain_root_prompt_is_confirmed() {
        assert_eq!(
            first_user_message_candidate(&value(SUB, 1)),
            confirmed("count the files")
        );
    }

    #[test]
    fn sidechain_meta_message_is_not_a_candidate() {
        assert_eq!(first_user_message_candidate(&value(SUB, 2)), None);
        assert_eq!(normalize_message_record(&value(SUB, 2)), None);
    }

    #[test]
    fn assistant_text_is_an_agent_message() {
        assert_eq!(
            normalize_message_record(&value(MAIN, 7)),
            Some(NormalizedMessage {
                role: MessageRole::Agent,
                content: "checking status".to_owned(),
            })
        );
    }

    #[test]
    fn thinking_and_synthetic_records_are_not_messages() {
        assert_eq!(normalize_message_record(&value(MAIN, 6)), None);
        assert_eq!(normalize_message_record(&value(MAIN, 12)), None);
    }

    #[test]
    fn handback_message_is_an_agent_message() {
        assert_eq!(
            normalize_message_record(&value(SUB, 3)),
            Some(NormalizedMessage {
                role: MessageRole::Agent,
                content: "there are 3 files".to_owned(),
            })
        );
    }

    #[test]
    fn text_and_handback_in_one_record_are_joined() {
        let record = json!({"type": "assistant", "message": {"model": "m", "content": [
            {"type": "text", "text": "done"},
            {"type": "tool_use", "id": "t", "name": "SubagentHandback", "input": {"message": "report"}}
        ]}});
        assert_eq!(
            normalize_message_record(&record).unwrap().content,
            "done\nreport"
        );
    }

    #[test]
    fn blob_message_is_not_normalized() {
        assert_eq!(normalize_message_record(&value(MAIN, 2)), None);
        assert!(bash_calls(&value(MAIN, 2)).is_empty());
    }

    #[test]
    fn human_user_messages_are_normalized() {
        assert_eq!(
            normalize_message_record(&value(MAIN, 5)),
            Some(NormalizedMessage {
                role: MessageRole::User,
                content: "fix the parser".to_owned(),
            })
        );
        assert_eq!(normalize_message_record(&value(MAIN, 3)), None);
        assert_eq!(normalize_message_record(&value(MAIN, 9)), None);
    }

    #[test]
    fn bash_calls_ignore_other_tools() {
        assert_eq!(
            bash_calls(&value(MAIN, 8)),
            [BashCall {
                call_id: "toolu_1".to_owned(),
                command: "git status".to_owned(),
            }]
        );
        assert!(bash_calls(&value(MAIN, 10)).is_empty());
    }

    #[test]
    fn tool_outputs_flatten_string_and_block_content() {
        assert_eq!(
            tool_outputs(&value(MAIN, 9)),
            [ToolOutput {
                call_id: "toolu_1".to_owned(),
                output: "clean".to_owned(),
            }]
        );
        assert_eq!(
            tool_outputs(&value(MAIN, 11)),
            [ToolOutput {
                call_id: "toolu_2".to_owned(),
                output: "file body".to_owned(),
            }]
        );
    }

    #[test]
    fn repository_ref_joins_host_and_remote() {
        assert_eq!(
            repository_ref(&value(MAIN, 9)).as_deref(),
            Some("git.example/team/repo")
        );
    }

    #[test]
    fn repository_ref_requires_both_parts() {
        let origin = |origin: Value| {
            json!({"serverClassifierContext": {"context": {"git_state": {"visibility": {
                "origin": origin
            }}}}})
        };
        assert_eq!(repository_ref(&origin(Value::Null)), None);
        assert_eq!(
            repository_ref(&origin(json!({"host": "git.example", "remote": ""}))),
            None
        );
        assert_eq!(repository_ref(&json!({"type": "user"})), None);
    }
}
