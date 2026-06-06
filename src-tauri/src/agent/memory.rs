use serde_json::{json, Value};

use crate::{
    error::{AppError, AppResult},
    models::{MemoryEntry, Persona},
    store::AppStore,
};

use super::{on_memory_write, string_arg};
fn persona_for_conversation(store: &AppStore, conversation_id: &str) -> AppResult<Persona> {
    let conversation = store.conversation(conversation_id)?;
    store
        .persona(conversation.persona_id.as_deref())
        .or_else(|_| store.persona(None))
}

pub(super) fn recall_memory_tool(
    store: &AppStore,
    conversation_id: &str,
    payload: &Value,
) -> AppResult<String> {
    let persona = persona_for_conversation(store, conversation_id)?;
    let mut payload = payload.clone();
    if payload.get("action").is_none() {
        if let Value::Object(map) = &mut payload {
            map.insert("action".into(), Value::String("read".into()));
        }
    }
    let (text, raw, ok) = execute_manage_memory(store, &persona, &payload)?;
    Ok(serde_json::to_string_pretty(&json!({
        "ok": ok,
        "tool": "recall_memory",
        "text": text,
        "result": raw
    }))?)
}

pub(super) fn remember_fact_tool(
    store: &AppStore,
    conversation_id: &str,
    payload: &Value,
) -> AppResult<String> {
    remember_fact_tool_for_run(store, conversation_id, "", payload)
}

pub(super) fn remember_fact_tool_for_run(
    store: &AppStore,
    conversation_id: &str,
    run_id: &str,
    payload: &Value,
) -> AppResult<String> {
    let persona = persona_for_conversation(store, conversation_id)?;
    let summary = string_arg(payload, &["summary", "content", "fact"])
        .ok_or_else(|| AppError::BadRequest("remember_fact requires payload.summary".into()))?;
    if summary.trim().is_empty() {
        return Err(AppError::BadRequest(
            "remember_fact summary cannot be empty".into(),
        ));
    }
    let importance = payload
        .get("importance")
        .and_then(Value::as_u64)
        .unwrap_or(4)
        .clamp(1, 5) as u8;
    let memory = store.save_memory(MemoryEntry {
        id: String::new(),
        persona_id: persona.id.clone(),
        summary: summary.trim().to_string(),
        importance,
        created_at: String::new(),
        updated_at: String::new(),
    })?;
    on_memory_write(store, run_id, &persona, "add", &memory.id, summary.trim())?;
    Ok(serde_json::to_string_pretty(&json!({
        "ok": true,
        "tool": "remember_fact",
        "memory": memory
    }))?)
}

pub(super) fn manage_memory_tool(
    store: &AppStore,
    conversation_id: &str,
    payload: &Value,
) -> AppResult<String> {
    manage_memory_tool_for_run(store, conversation_id, "", payload)
}

pub(super) fn manage_memory_tool_for_run(
    store: &AppStore,
    conversation_id: &str,
    run_id: &str,
    payload: &Value,
) -> AppResult<String> {
    let persona = persona_for_conversation(store, conversation_id)?;
    let (text, raw, ok) = execute_manage_memory_for_run(store, &persona, run_id, payload)?;
    Ok(serde_json::to_string_pretty(&json!({
        "ok": ok,
        "tool": "manage_memory",
        "text": text,
        "result": raw
    }))?)
}

pub(super) fn memory_tool(
    store: &AppStore,
    conversation_id: &str,
    payload: &Value,
) -> AppResult<String> {
    memory_tool_for_run(store, conversation_id, "", payload)
}

pub(super) fn memory_tool_for_run(
    store: &AppStore,
    conversation_id: &str,
    run_id: &str,
    payload: &Value,
) -> AppResult<String> {
    let normalized = normalize_memory_payload(payload);
    let persona = persona_for_conversation(store, conversation_id)?;
    let (text, raw, ok) = execute_manage_memory_for_run(store, &persona, run_id, &normalized)?;
    Ok(serde_json::to_string_pretty(&json!({
        "ok": ok,
        "tool": "memory",
        "action": normalized.get("action").and_then(Value::as_str).unwrap_or("read"),
        "text": text,
        "result": raw
    }))?)
}

fn normalize_memory_payload(payload: &Value) -> Value {
    let mut normalized = payload.as_object().cloned().unwrap_or_default();
    let action = normalized
        .get("action")
        .or_else(|| normalized.get("operation"))
        .or_else(|| normalized.get("mode"))
        .and_then(Value::as_str)
        .map(normalize_memory_action)
        .unwrap_or_else(|| infer_memory_action(payload));
    normalized.insert("action".into(), json!(action));
    if !normalized.contains_key("summary") {
        if let Some(value) = payload
            .get("fact")
            .or_else(|| payload.get("memory"))
            .or_else(|| payload.get("content"))
            .and_then(Value::as_str)
        {
            normalized.insert("summary".into(), json!(value));
        }
    }
    if !normalized.contains_key("query") {
        if let Some(value) = payload
            .get("q")
            .or_else(|| payload.get("search"))
            .and_then(Value::as_str)
        {
            normalized.insert("query".into(), json!(value));
        }
    }
    normalized.into()
}

fn normalize_memory_action(action: &str) -> String {
    match action
        .trim()
        .to_ascii_lowercase()
        .replace('-', "_")
        .as_str()
    {
        "search" | "recall" | "find" | "list" | "get" => "read".into(),
        "remember" | "save" | "create" | "insert" => "add".into(),
        "update" | "edit" => "replace".into(),
        "delete" | "forget" => "remove".into(),
        "add" | "read" | "replace" | "remove" => action.trim().to_ascii_lowercase(),
        _ => "read".into(),
    }
}

fn infer_memory_action(payload: &Value) -> String {
    if payload.get("id").is_some()
        && (payload
            .get("remove")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            || payload
                .get("delete")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            || payload
                .get("forget")
                .and_then(Value::as_bool)
                .unwrap_or(false))
    {
        return "remove".into();
    }
    if payload.get("id").is_some()
        && (payload.get("summary").is_some()
            || payload.get("fact").is_some()
            || payload.get("memory").is_some()
            || payload.get("content").is_some())
    {
        return "replace".into();
    }
    if payload.get("summary").is_some()
        || payload.get("fact").is_some()
        || payload.get("memory").is_some()
        || payload.get("content").is_some()
    {
        return "add".into();
    }
    "read".into()
}

pub(super) fn execute_manage_memory(
    store: &AppStore,
    persona: &Persona,
    payload: &Value,
) -> AppResult<(String, Value, bool)> {
    execute_manage_memory_for_run(store, persona, "", payload)
}

pub(super) fn execute_manage_memory_for_run(
    store: &AppStore,
    persona: &Persona,
    run_id: &str,
    payload: &Value,
) -> AppResult<(String, Value, bool)> {
    let action = payload
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("read")
        .trim()
        .to_lowercase();
    let limit = payload
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(8)
        .clamp(1, 20) as usize;
    let memories = store.memories(Some(&persona.id))?;
    match action.as_str() {
        "read" | "list" | "search" => {
            let query = payload
                .get("query")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string();
            let mut ranked = memories
                .into_iter()
                .filter(|memory| crate::store::scan_memory_content(&memory.summary).is_none())
                .filter_map(|memory| {
                    let score = if query.is_empty() {
                        memory.importance as u32
                    } else {
                        memory_relevance_score(&memory.summary, &query)
                    };
                    (score > 0).then_some((score, memory))
                })
                .collect::<Vec<_>>();
            ranked.sort_by(|(left_score, left), (right_score, right)| {
                right_score
                    .cmp(left_score)
                    .then_with(|| right.importance.cmp(&left.importance))
                    .then_with(|| right.updated_at.cmp(&left.updated_at))
            });
            let mut memories = ranked
                .into_iter()
                .map(|(_, memory)| memory)
                .collect::<Vec<_>>();
            memories.truncate(limit);
            let text = if memories.is_empty() {
                if query.is_empty() {
                    "No long-term memory is stored for this persona.".into()
                } else {
                    format!("No long-term memory matched `{query}`.")
                }
            } else {
                memories
                    .iter()
                    .map(|memory| {
                        format!("- {} [{}] {}", memory.id, memory.importance, memory.summary)
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            Ok((
                text,
                json!({"action": "read", "query": query, "memories": memories}),
                true,
            ))
        }
        "add" | "remember" => {
            let summary = string_arg(payload, &["summary", "content", "fact"])
                .ok_or_else(|| AppError::BadRequest("manage_memory add requires summary".into()))?;
            if summary.trim().is_empty() {
                return Err(AppError::BadRequest(
                    "manage_memory add summary cannot be empty".into(),
                ));
            }
            let importance = payload
                .get("importance")
                .and_then(Value::as_u64)
                .unwrap_or(4)
                .clamp(1, 5) as u8;
            let memory = store.save_memory(MemoryEntry {
                id: String::new(),
                persona_id: persona.id.clone(),
                summary: summary.trim().to_string(),
                importance,
                created_at: String::new(),
                updated_at: String::new(),
            })?;
            on_memory_write(store, run_id, persona, "add", &memory.id, summary.trim())?;
            Ok((
                format!(
                    "Stored long-term memory: {} [{}] {}",
                    memory.id, memory.importance, memory.summary
                ),
                json!({"action": "add", "memoryId": memory.id, "memory": memory}),
                true,
            ))
        }
        "replace" | "update" => {
            let id = string_arg(payload, &["id", "memoryId", "memory_id"])
                .ok_or_else(|| AppError::BadRequest("manage_memory replace requires id".into()))?;
            let summary =
                string_arg(payload, &["summary", "content", "fact"]).ok_or_else(|| {
                    AppError::BadRequest("manage_memory replace requires summary".into())
                })?;
            let existing = memories
                .iter()
                .find(|memory| memory.id == id)
                .ok_or_else(|| AppError::BadRequest(format!("memory not found: {id}")))?;
            let importance = payload
                .get("importance")
                .and_then(Value::as_u64)
                .unwrap_or(existing.importance as u64)
                .clamp(1, 5) as u8;
            let memory = store.save_memory(MemoryEntry {
                id: existing.id.clone(),
                persona_id: persona.id.clone(),
                summary: summary.trim().to_string(),
                importance,
                created_at: existing.created_at.clone(),
                updated_at: String::new(),
            })?;
            on_memory_write(
                store,
                run_id,
                persona,
                "replace",
                &memory.id,
                summary.trim(),
            )?;
            Ok((
                format!(
                    "Replaced long-term memory: {} [{}] {}",
                    memory.id, memory.importance, memory.summary
                ),
                json!({"action": "replace", "memoryId": memory.id, "memory": memory}),
                true,
            ))
        }
        "remove" | "delete" | "forget" => {
            let id = string_arg(payload, &["id", "memoryId", "memory_id"])
                .ok_or_else(|| AppError::BadRequest("manage_memory remove requires id".into()))?;
            let existing = memories
                .iter()
                .find(|memory| memory.id == id)
                .ok_or_else(|| AppError::BadRequest(format!("memory not found: {id}")))?;
            store.delete_memory(&existing.id)?;
            on_memory_write(
                store,
                run_id,
                persona,
                "remove",
                &existing.id,
                &existing.summary,
            )?;
            Ok((
                format!(
                    "Removed long-term memory: {} {}",
                    existing.id, existing.summary
                ),
                json!({"action": "remove", "memoryId": existing.id, "memory": existing}),
                true,
            ))
        }
        other => Err(AppError::BadRequest(format!(
            "unsupported manage_memory action '{other}'. Use read, add, replace, or remove."
        ))),
    }
}

fn memory_relevance_score(text: &str, query: &str) -> u32 {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return 1;
    }
    let text = text.to_lowercase();
    if text.contains(&query) {
        return 100 + query.len() as u32;
    }
    query
        .split_whitespace()
        .filter(|term| !term.is_empty() && text.contains(*term))
        .map(|term| 10 + term.len() as u32)
        .sum()
}
