use anyhow::{Result, bail};
use serde_json::Value;

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
    pub phase: Option<String>,
    pub content: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum FirstUserMessageConfidence {
    Fallback,
    Legacy,
    Confirmed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FirstUserMessageCandidate {
    pub confidence: FirstUserMessageConfidence,
    pub content: String,
}

pub(crate) fn first_user_message_candidate(value: &Value) -> Option<FirstUserMessageCandidate> {
    let kind = value.get("type").and_then(Value::as_str)?;
    let payload = value.get("payload")?;
    match (kind, payload.get("type").and_then(Value::as_str)) {
        ("response_item", Some("message"))
            if payload.get("role").and_then(Value::as_str) == Some("user") =>
        {
            let content = input_text_content(payload.get("content")?);
            let metadata =
                payload.pointer("/internal_chat_message_metadata_passthrough/content_item_kinds");
            let confidence = match metadata {
                Some(Value::Array(kinds))
                    if kinds.iter().any(|kind| kind.as_str() == Some("user.text")) =>
                {
                    FirstUserMessageConfidence::Confirmed
                }
                Some(_) => return None,
                None => FirstUserMessageConfidence::Fallback,
            };
            Some(FirstUserMessageCandidate {
                confidence,
                content,
            })
        }
        ("event_msg", Some("item_completed"))
            if payload.pointer("/item/type").and_then(Value::as_str) == Some("UserMessage") =>
        {
            Some(FirstUserMessageCandidate {
                confidence: FirstUserMessageConfidence::Confirmed,
                content: text_content(payload.pointer("/item/content")?),
            })
        }
        ("event_msg", Some("user_message")) => Some(FirstUserMessageCandidate {
            confidence: FirstUserMessageConfidence::Legacy,
            content: payload.get("message")?.as_str()?.to_owned(),
        }),
        _ => None,
    }
}

fn input_text_content(value: &Value) -> String {
    content_by_type(value, "input_text")
}

fn text_content(value: &Value) -> String {
    content_by_type(value, "text")
}

fn content_by_type(value: &Value, expected_type: &str) -> String {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some(expected_type))
        .filter_map(|item| item.get("text").and_then(Value::as_str))
        .filter(|text| !text.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

pub(crate) fn normalize_message_record(value: &Value) -> Option<NormalizedMessage> {
    let (kind, payload) = match value.get("type").and_then(Value::as_str) {
        Some("event_msg") | Some("response_item") => (
            value.get("type").and_then(Value::as_str)?,
            value.get("payload")?,
        ),
        _ => ("bare", value),
    };
    match kind {
        "event_msg" | "bare" => match payload.get("type").and_then(Value::as_str) {
            Some("user_message") => payload
                .get("message")
                .and_then(Value::as_str)
                .zip(valid_phase(payload))
                .map(|(content, phase)| NormalizedMessage {
                    role: MessageRole::User,
                    phase,
                    content: content.to_owned(),
                }),
            Some("agent_message") => payload
                .get("message")
                .and_then(Value::as_str)
                .zip(valid_phase(payload))
                .map(|(content, phase)| NormalizedMessage {
                    role: MessageRole::Agent,
                    phase,
                    content: content.to_owned(),
                }),
            _ => None,
        },
        "response_item" if payload.get("type").and_then(Value::as_str) == Some("message") => {
            let role = match payload.get("role").and_then(Value::as_str) {
                Some("user") => MessageRole::User,
                Some("assistant") => MessageRole::Agent,
                _ => return None,
            };
            Some(NormalizedMessage {
                role,
                phase: valid_phase(payload)?,
                content: value_to_text(payload.get("content").unwrap_or(&Value::Null)),
            })
        }
        _ => None,
    }
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

fn valid_phase(value: &Value) -> Option<Option<String>> {
    match value.get("phase") {
        None | Some(Value::Null) => Some(None),
        Some(Value::String(phase)) => Some(Some(phase.clone())),
        Some(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn response_item_user_text_is_confirmed() {
        let value = json!({"type": "response_item", "payload": {
            "type": "message", "role": "user",
            "internal_chat_message_metadata_passthrough": {"content_item_kinds": ["user.text"]},
            "content": [{"type": "input_text", "text": "first"}, {"type": "input_text", "text": "second"}]
        }});
        assert_eq!(
            first_user_message_candidate(&value),
            Some(FirstUserMessageCandidate {
                confidence: FirstUserMessageConfidence::Confirmed,
                content: "first\nsecond".to_owned(),
            })
        );
    }

    #[test]
    fn instructions_are_not_first_user_message_candidates() {
        let value = json!({"type": "response_item", "payload": {
            "type": "message", "role": "user",
            "internal_chat_message_metadata_passthrough": {"content_item_kinds": ["agents_md.instructions"]},
            "content": [{"type": "input_text", "text": "do not select"}]
        }});
        assert_eq!(first_user_message_candidate(&value), None);
    }

    #[test]
    fn item_completed_user_message_is_confirmed() {
        let value = json!({"type": "event_msg", "payload": {
            "type": "item_completed", "item": {"type": "UserMessage", "content": [
                {"type": "text", "text": "actual request"}
            ]}
        }});
        assert_eq!(
            first_user_message_candidate(&value),
            Some(FirstUserMessageCandidate {
                confidence: FirstUserMessageConfidence::Confirmed,
                content: "actual request".to_owned(),
            })
        );
    }
}
