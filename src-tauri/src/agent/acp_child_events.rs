use serde_json::{json, Value};

use crate::{
    error::AppResult,
    models::{new_id, AgentRunRecord, ToolEvent},
    store::AppStore,
};

use super::{
    acp_events::acp_tool_event_kind, acp_tool_output::acp_string_text, push_tool_event_record,
    redact_json_value,
};
pub(super) fn acp_session_update_kind(update: &Value) -> String {
    update
        .get("sessionUpdate")
        .or_else(|| update.get("session_update"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

pub(super) fn acp_session_update_text(update: &Value) -> String {
    let Some(content) = update.get("content") else {
        return String::new();
    };
    if let Some(text) = acp_content_text(content) {
        return text;
    }
    if let Some(items) = content.as_array() {
        return items
            .iter()
            .filter_map(acp_content_text)
            .collect::<Vec<_>>()
            .join("");
    }
    String::new()
}

fn acp_content_text(value: &Value) -> Option<String> {
    if let Some(text) = value.as_str() {
        return Some(text.to_string());
    }
    if let Some(text) = value.get("text").and_then(Value::as_str) {
        return Some(text.to_string());
    }
    value
        .get("content")
        .and_then(|content| {
            content
                .get("text")
                .and_then(Value::as_str)
                .or_else(|| content.as_str())
        })
        .map(str::to_string)
}

pub(super) fn acp_session_update_record(message: &Value) -> Option<Value> {
    let update = message.get("params")?.get("update")?;
    let kind = acp_session_update_kind(update);
    let kind = kind.trim();
    if kind.is_empty() {
        return None;
    }

    let record = match kind {
        "agent_message_chunk" | "agent_thought_chunk" => {
            return None;
        }
        "tool_call" | "tool_call_update" => json!({
            "sessionUpdate": kind,
            "toolCallId": acp_string_field(update, &["toolCallId", "tool_call_id"]),
            "title": acp_string_field(update, &["title"]),
            "kind": acp_string_field(update, &["kind"]),
            "status": acp_string_field(update, &["status"]),
            "rawInput": acp_limited_value(update.get("rawInput").or_else(|| update.get("raw_input"))),
            "rawOutput": acp_limited_value(update.get("rawOutput").or_else(|| update.get("raw_output"))),
            "content": acp_limited_value(update.get("content")),
            "locations": acp_limited_value(update.get("locations"))
        }),
        "plan" => {
            let entries = update
                .get("entries")
                .and_then(Value::as_array)
                .map(|items| {
                    Value::Array(
                        items
                            .iter()
                            .take(50)
                            .map(|item| {
                                json!({
                                    "content": acp_string_field(item, &["content"]),
                                    "status": acp_string_field(item, &["status"]),
                                    "priority": acp_string_field(item, &["priority"])
                                })
                            })
                            .collect(),
                    )
                })
                .unwrap_or_else(|| Value::Array(Vec::new()));
            json!({
                "sessionUpdate": kind,
                "entries": entries
            })
        }
        "available_commands_update" => json!({
            "sessionUpdate": kind,
            "availableCommandCount": update
                .get("availableCommands")
                .or_else(|| update.get("available_commands"))
                .and_then(Value::as_array)
                .map(|items| items.len())
                .unwrap_or(0)
        }),
        _ => json!({
            "sessionUpdate": kind,
            "update": acp_limited_value(Some(update))
        }),
    };
    Some(redact_json_value(record))
}

pub(super) fn append_acp_tool_event_record(
    store: &AppStore,
    child_run_id: &str,
    record: &Value,
) -> AppResult<()> {
    let session_update = acp_string_text(record, &["sessionUpdate", "session_update"]);
    if !matches!(session_update.as_str(), "tool_call" | "tool_call_update") {
        return Ok(());
    }
    let mut run = match store.agent_run(child_run_id) {
        Ok(run) => run,
        Err(_) => return Ok(()),
    };
    let mut status = if session_update == "tool_call" {
        "running".to_string()
    } else {
        let status = acp_string_text(record, &["status"]);
        if status.is_empty() {
            "completed".to_string()
        } else {
            status
        }
    };
    let cancelled = matches!(status.as_str(), "cancelled" | "canceled");
    if cancelled {
        status = "completed".into();
    }
    let title = acp_string_text(record, &["title"]);
    let tool_name = acp_acp_tool_event_name(record, &title);
    let call_id = acp_resolved_tool_call_id(
        &run,
        &tool_name,
        &acp_string_text(record, &["toolCallId", "tool_call_id"]),
        session_update == "tool_call",
    );
    let raw_input = record
        .get("rawInput")
        .or_else(|| record.get("raw_input"))
        .filter(|value| !value.is_null())
        .cloned()
        .or_else(|| acp_running_tool_raw_input(&run, &call_id))
        .unwrap_or_else(|| json!({}));
    let raw_output = record
        .get("rawOutput")
        .or_else(|| record.get("raw_output"))
        .cloned()
        .unwrap_or(Value::Null);
    let mut text = raw_output
        .as_str()
        .map(str::to_string)
        .or_else(|| (!raw_output.is_null()).then(|| raw_output.to_string()));
    if cancelled {
        if let Some(value) = text.as_mut() {
            if !value.trim_start().starts_with("[cancelled]") {
                *value = format!("[cancelled] {value}");
            }
        }
    }
    let ok = !matches!(status.as_str(), "failed" | "error");
    let kind = {
        let value = acp_string_text(record, &["kind"]);
        if value.is_empty() {
            acp_tool_event_kind("acp", &tool_name)
        } else {
            value
        }
    };
    let event = ToolEvent {
        status: Some(status.clone()),
        reference_id: None,
        call_id: Some(call_id),
        run_id: Some(child_run_id.to_string()),
        checkpoint_id: None,
        event_type: "acp_tool".into(),
        server_id: "acp".into(),
        tool_name: tool_name.clone(),
        ok,
        timed_out: false,
        elapsed_ms: 0,
        kind,
        title: if title.is_empty() {
            tool_name.clone()
        } else {
            title
        },
        summary: text
            .clone()
            .or_else(|| record.get("content").map(|content| content.to_string()))
            .unwrap_or_else(|| {
                if status == "running" {
                    "ACP tool call started".into()
                } else {
                    "ACP tool call completed".into()
                }
            }),
        path: raw_input
            .get("path")
            .and_then(Value::as_str)
            .map(str::to_string),
        exists: None,
        mime_type: text.as_ref().map(|_| "text/plain".to_string()),
        text: if ok { text.clone() } else { None },
        error: if ok { None } else { text.clone() },
        raw: Some(redact_json_value(json!({
            "payload": raw_input,
            "content": record.get("content").cloned().unwrap_or(Value::Null)
        }))),
    };
    push_tool_event_record(&mut run, &event);
    run.touch_activity(format!("ACP child tool event: {tool_name} {status}"));
    store.save_agent_run(run)?;
    Ok(())
}

fn acp_resolved_tool_call_id(
    run: &AgentRunRecord,
    tool_name: &str,
    raw_call_id: &str,
    is_start: bool,
) -> String {
    let raw_call_id = raw_call_id.trim();
    if !raw_call_id.is_empty() {
        return raw_call_id.to_string();
    }
    if !is_start {
        if let Some(call_id) = acp_fifo_running_tool_call_id(run, tool_name) {
            return call_id;
        }
    }
    new_id("acp-call")
}

fn acp_fifo_running_tool_call_id(run: &AgentRunRecord, tool_name: &str) -> Option<String> {
    run.tool_events
        .iter()
        .find(|event| {
            event.get("serverId").and_then(Value::as_str) == Some("acp")
                && event.get("toolName").and_then(Value::as_str) == Some(tool_name)
                && event.get("status").and_then(Value::as_str) == Some("running")
        })
        .and_then(|event| event.get("callId").and_then(Value::as_str))
        .filter(|call_id| !call_id.trim().is_empty())
        .map(str::to_string)
}

fn acp_running_tool_raw_input(run: &AgentRunRecord, call_id: &str) -> Option<Value> {
    let call_id = call_id.trim();
    if call_id.is_empty() {
        return None;
    }
    run.tool_events
        .iter()
        .find(|event| {
            event
                .get("callId")
                .or_else(|| event.get("call_id"))
                .and_then(Value::as_str)
                == Some(call_id)
                && event.get("status").and_then(Value::as_str) == Some("running")
        })
        .and_then(|event| event.get("raw"))
        .and_then(|raw| raw.get("payload"))
        .cloned()
}

fn acp_acp_tool_event_name(record: &Value, title: &str) -> String {
    let direct = acp_string_text(record, &["toolName", "tool_name", "name", "tool"]);
    if !direct.is_empty() {
        return direct;
    }
    let title = title.trim();
    if title.is_empty() {
        return "acp_tool".into();
    }
    let name = title
        .split(':')
        .next()
        .unwrap_or(title)
        .trim()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_matches('_')
        .to_string();
    if name.is_empty() {
        "acp_tool".into()
    } else {
        name
    }
}

fn acp_string_field(value: &Value, keys: &[&str]) -> Value {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_str))
        .map(|text| Value::String(acp_truncate_text(text, 1000)))
        .unwrap_or(Value::Null)
}

fn acp_limited_value(value: Option<&Value>) -> Value {
    match value {
        Some(Value::String(text)) => Value::String(acp_truncate_text(text, 2000)),
        Some(Value::Array(items)) => {
            Value::Array(items.iter().take(20).map(acp_limited_value_ref).collect())
        }
        Some(Value::Object(map)) => Value::Object(
            map.iter()
                .take(30)
                .map(|(key, value)| (key.clone(), acp_limited_value_ref(value)))
                .collect(),
        ),
        Some(value) => value.clone(),
        None => Value::Null,
    }
}

fn acp_limited_value_ref(value: &Value) -> Value {
    acp_limited_value(Some(value))
}

fn acp_truncate_text(value: &str, max_chars: usize) -> String {
    let mut output = String::new();
    for (index, ch) in value.chars().enumerate() {
        if index >= max_chars {
            output.push_str("...");
            return output;
        }
        output.push(ch);
    }
    output
}
