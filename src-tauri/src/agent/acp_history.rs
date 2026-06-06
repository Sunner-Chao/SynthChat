use std::collections::HashMap;

use serde_json::{json, Value};

use crate::{
    error::AppResult,
    models::{ChatMessage, ToolEvent},
    store::AppStore,
};

use super::{
    acp_events::{acp_parse_tool_json_output, acp_tool_event_update_from_value},
    delegation::acp_string_text,
};

pub(super) fn acp_session_history_updates_for_store(
    store: &AppStore,
    session_id: &str,
) -> AppResult<Vec<Value>> {
    store.conversation(session_id)?;
    let messages = store.messages(session_id, None)?;
    Ok(acp_session_history_updates(&messages))
}

pub(super) fn acp_session_history_updates(messages: &[ChatMessage]) -> Vec<Value> {
    let mut updates = Vec::new();
    let mut active_tool_calls = HashMap::<String, (String, Value)>::new();
    for message in messages {
        match message.role.as_str() {
            "user" => {
                if let Some(update) =
                    acp_history_message_update("user_message_chunk", &message.content)
                {
                    updates.push(update);
                }
            }
            "assistant" => {
                if let Some(thought) = acp_history_reasoning_text(message) {
                    if let Some(update) =
                        acp_history_message_update("agent_thought_chunk", &thought)
                    {
                        updates.push(update);
                    }
                }
                let assistant_text = acp_history_assistant_message_text(message);
                if let Some(update) =
                    acp_history_message_update("agent_message_chunk", &assistant_text)
                {
                    updates.push(update);
                }
                for tool_call in acp_history_tool_calls(message) {
                    let call_id = acp_history_tool_call_id(&tool_call);
                    if call_id.is_empty() {
                        continue;
                    }
                    let (tool_name, args) = acp_history_tool_call_name_args(&tool_call);
                    active_tool_calls.insert(call_id.clone(), (tool_name.clone(), args.clone()));
                    if let Some(update) = acp_tool_event_update_from_value(&json!({
                        "toolName": tool_name,
                        "callId": call_id,
                        "status": "running",
                        "raw": {"payload": args}
                    })) {
                        updates.push(update);
                    }
                }
            }
            "tool" => {
                for update in acp_history_tool_result_updates(message, &mut active_tool_calls) {
                    updates.push(update);
                }
            }
            _ => {}
        }
    }
    updates
}

fn acp_history_message_update(kind: &str, text: &str) -> Option<Value> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    Some(json!({
        "sessionUpdate": kind,
        "content": {
            "type": "text",
            "text": text
        }
    }))
}

fn acp_history_reasoning_text(message: &ChatMessage) -> Option<String> {
    if let Some(reasoning) = message
        .provider_data
        .as_ref()
        .and_then(acp_history_reasoning_text_from_value)
    {
        return Some(reasoning);
    }
    parse_json_object(&message.content)
        .and_then(|value| acp_history_reasoning_text_from_value(&value))
}

fn acp_history_reasoning_text_from_value(value: &Value) -> Option<String> {
    for pointer in [
        "/reasoning_content",
        "/reasoning",
        "/responses/reasoning_content",
        "/responses/reasoning",
        "/responses/reasoningItems/0/text",
    ] {
        if let Some(text) = value.pointer(pointer).and_then(Value::as_str) {
            let text = text.trim();
            if !text.is_empty() {
                return Some(text.to_string());
            }
        }
    }
    None
}

fn acp_history_assistant_message_text(message: &ChatMessage) -> String {
    let Some(value) = parse_json_object(&message.content) else {
        return message.content.clone();
    };
    let looks_like_provider_turn = value.get("tool_calls").is_some()
        || value.get("toolCalls").is_some()
        || value.get("reasoning_content").is_some()
        || value.get("reasoning").is_some()
        || value.get("responses").is_some();
    if !looks_like_provider_turn {
        return message.content.clone();
    }
    value
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn acp_history_tool_calls(message: &ChatMessage) -> Vec<Value> {
    let mut calls = Vec::new();
    if let Some(provider) = message.provider_data.as_ref() {
        for key in ["tool_calls", "toolCalls"] {
            if let Some(items) = provider.get(key).and_then(Value::as_array) {
                calls.extend(items.iter().filter(|item| item.is_object()).cloned());
            }
        }
        for pointer in ["/responses/tool_calls", "/responses/toolCalls"] {
            if let Some(items) = provider.pointer(pointer).and_then(Value::as_array) {
                calls.extend(items.iter().filter(|item| item.is_object()).cloned());
            }
        }
    }
    if calls.is_empty() {
        if let Some(value) = parse_json_object(&message.content) {
            for key in ["tool_calls", "toolCalls"] {
                if let Some(items) = value.get(key).and_then(Value::as_array) {
                    calls.extend(items.iter().filter(|item| item.is_object()).cloned());
                }
            }
        }
    }
    calls
}

fn acp_history_tool_call_id(tool_call: &Value) -> String {
    acp_string_text(
        tool_call,
        &["id", "callId", "call_id", "toolCallId", "tool_call_id"],
    )
}

fn acp_history_tool_call_name_args(tool_call: &Value) -> (String, Value) {
    let function = tool_call.get("function").filter(|value| value.is_object());
    let name = function
        .and_then(|value| value.get("name"))
        .or_else(|| tool_call.get("name"))
        .or_else(|| tool_call.get("toolName"))
        .or_else(|| tool_call.get("tool_name"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("unknown_tool")
        .to_string();
    let raw_args = function
        .and_then(|value| value.get("arguments"))
        .or_else(|| tool_call.get("arguments"))
        .or_else(|| tool_call.get("args"))
        .or_else(|| tool_call.get("payload"));
    (name, normalize_acp_history_args(raw_args))
}

fn normalize_acp_history_args(raw_args: Option<&Value>) -> Value {
    match raw_args {
        Some(Value::String(text)) => serde_json::from_str::<Value>(text)
            .unwrap_or_else(|_| json!({"raw": text}))
            .as_object()
            .map(|_| serde_json::from_str(text).unwrap_or_else(|_| json!({"raw": text})))
            .unwrap_or_else(|| json!({})),
        Some(value) if value.is_object() => value.clone(),
        _ => json!({}),
    }
}

fn acp_history_tool_result_updates(
    message: &ChatMessage,
    active_tool_calls: &mut HashMap<String, (String, Value)>,
) -> Vec<Value> {
    let mut updates = Vec::new();
    let parsed = parse_json_object(&message.content);
    let event = parsed
        .as_ref()
        .and_then(|value| value.get("event"))
        .and_then(|value| serde_json::from_value::<ToolEvent>(value.clone()).ok());
    let call_id = event
        .as_ref()
        .and_then(|event| event.call_id.clone())
        .or_else(|| {
            parsed.as_ref().map(|value| {
                acp_string_text(value, &["toolCallId", "tool_call_id", "callId", "call_id"])
            })
        })
        .unwrap_or_default();
    if call_id.is_empty() {
        return updates;
    }
    let (known_tool_name, known_args) = active_tool_calls.remove(&call_id).unwrap_or_default();
    let tool_name = event
        .as_ref()
        .map(|event| event.tool_name.clone())
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            parsed
                .as_ref()
                .map(|value| acp_string_text(value, &["toolName", "tool_name", "name"]))
                .filter(|value| !value.is_empty())
        })
        .unwrap_or(known_tool_name);
    if tool_name.is_empty() {
        return updates;
    }
    let mut result_text = event
        .as_ref()
        .and_then(|event| event.text.clone().or_else(|| event.error.clone()))
        .or_else(|| {
            parsed
                .as_ref()
                .and_then(|value| value.get("content"))
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
        .unwrap_or_else(|| message.content.clone());
    let raw_status = event
        .as_ref()
        .and_then(|event| event.status.clone())
        .unwrap_or_else(|| "completed".into());
    if raw_status.trim() == "cancelled" && !result_text.trim_start().starts_with("[cancelled]") {
        result_text = format!("[cancelled] {result_text}");
    }
    if let Some(update) = acp_tool_event_update_from_value(&json!({
        "toolName": tool_name,
        "callId": call_id,
        "status": raw_status,
        "text": result_text.clone(),
        "raw": {"payload": known_args.clone()}
    })) {
        updates.push(update);
    }
    if tool_name == "todo" || tool_name == "update_todo" {
        if let Some(plan) = acp_history_plan_update_from_todo_result(&result_text) {
            updates.push(plan);
        }
    }
    updates
}

pub(super) fn acp_history_plan_update_from_todo_result(text: &str) -> Option<Value> {
    let value = parse_json_object(text)?;
    let todos = value
        .get("todos")
        .or_else(|| value.get("result").and_then(|result| result.get("todos")))?
        .as_array()?;
    let entries = todos
        .iter()
        .filter_map(|todo| {
            let mut content = todo
                .get("content")
                .or_else(|| todo.get("task"))
                .or_else(|| todo.get("text"))
                .and_then(Value::as_str)?
                .trim()
                .to_string();
            if content.is_empty() {
                return None;
            }
            let raw_status = todo
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("pending")
                .trim();
            let cancelled = matches!(raw_status, "cancelled" | "canceled");
            if cancelled && !content.starts_with("[cancelled]") {
                content = format!("[cancelled] {content}");
            }
            let status = if cancelled { "completed" } else { raw_status };
            Some(json!({
                "content": content,
                "status": status,
                "priority": todo
                    .get("priority")
                    .and_then(Value::as_str)
                    .unwrap_or("medium")
            }))
        })
        .collect::<Vec<_>>();
    Some(json!({
        "sessionUpdate": "plan",
        "entries": entries
    }))
}

fn parse_json_object(text: &str) -> Option<Value> {
    acp_parse_tool_json_output(text)
        .map(|(value, _)| value)
        .filter(|value| value.is_object())
}
