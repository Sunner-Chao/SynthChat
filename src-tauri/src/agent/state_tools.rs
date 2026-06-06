use std::{
    fs,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::{
    error::{AppError, AppResult},
    models::{new_id, now_iso, AgentCheckpointRecord, AgentDefinition},
    store::AppStore,
};

use super::workspace::{resolve_workspace_path, workspace_root};

pub(super) fn file_state_tool(
    store: &AppStore,
    agent: &AgentDefinition,
    run_id: &str,
    payload: &Value,
) -> AppResult<String> {
    let action = payload
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("check")
        .trim()
        .to_ascii_lowercase();
    match action.as_str() {
        "register" | "record_read" | "read" => {
            let path = file_state_payload_path(payload)?;
            let full_path = resolve_file_state_path(agent, &path)?;
            let state = current_file_state(&full_path)?;
            let actor = payload
                .get("actor")
                .or_else(|| payload.get("taskId"))
                .or_else(|| payload.get("task_id"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or("file_state");
            store.record_file_read_state(
                &full_path.to_string_lossy(),
                &state.sha256,
                state.modified_unix_ms,
                state.bytes,
                false,
                Some(actor),
                Some(run_id),
            )?;
            Ok(serde_json::to_string_pretty(&json!({
                "action": "register",
                "path": full_path.to_string_lossy(),
                "sha256": state.sha256,
                "modifiedUnixMs": state.modified_unix_ms,
                "bytes": state.bytes,
                "actor": actor,
                "runId": run_id,
            }))?)
        }
        "check" | "status" => {
            let path = file_state_payload_path(payload)?;
            let full_path = resolve_file_state_path(agent, &path)?;
            let key = full_path.to_string_lossy().to_string();
            let registered = store.registered_file_state(&key)?;
            let current = current_file_state(&full_path).ok();
            let stale = match (&registered, &current) {
                (Some(registered), Some(current)) => {
                    registered.sha256 != current.sha256
                        || registered.modified_unix_ms != current.modified_unix_ms
                }
                (Some(_), None) => true,
                _ => false,
            };
            Ok(serde_json::to_string_pretty(&json!({
                "action": "check",
                "path": key,
                "registered": registered,
                "current": current,
                "stale": stale,
                "message": if stale {
                    "File changed since the registered state; re-read before writing."
                } else if registered.is_some() {
                    "Registered file state matches current file state."
                } else {
                    "No registered file state for this path."
                }
            }))?)
        }
        "remove" | "forget" => {
            let path = file_state_payload_path(payload)?;
            let full_path = resolve_file_state_path(agent, &path)?;
            let key = full_path.to_string_lossy().to_string();
            store.remove_file_state(&key)?;
            Ok(serde_json::to_string_pretty(&json!({
                "action": "remove",
                "path": key,
                "removed": true
            }))?)
        }
        "writes_since" | "writes-since" => {
            let reader_run_id = payload
                .get("readerRunId")
                .or_else(|| payload.get("reader_run_id"))
                .or_else(|| payload.get("runId"))
                .or_else(|| payload.get("run_id"))
                .and_then(Value::as_str)
                .unwrap_or(run_id);
            let since = payload
                .get("since")
                .or_else(|| payload.get("sinceIso"))
                .or_else(|| payload.get("since_iso"))
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    AppError::BadRequest("file_state writes_since requires payload.since".into())
                })?;
            let writes = store.file_writes_since_for_reader(reader_run_id, since)?;
            Ok(serde_json::to_string_pretty(&json!({
                "action": "writes_since",
                "readerRunId": reader_run_id,
                "since": since,
                "writes": writes,
                "count": writes.len()
            }))?)
        }
        other => Err(AppError::BadRequest(format!(
            "unsupported file_state action '{other}'. Use register, check, remove, or writes_since."
        ))),
    }
}

fn file_state_payload_path(payload: &Value) -> AppResult<String> {
    payload
        .get("path")
        .or_else(|| payload.get("filePath"))
        .or_else(|| payload.get("file_path"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| AppError::BadRequest("file_state requires payload.path".into()))
}

fn resolve_file_state_path(agent: &AgentDefinition, path: &str) -> AppResult<PathBuf> {
    let root = workspace_root(agent)?;
    resolve_workspace_path(&root, path)
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct CurrentFileState {
    sha256: String,
    modified_unix_ms: u128,
    bytes: usize,
    exists: bool,
}

fn current_file_state(path: &Path) -> AppResult<CurrentFileState> {
    let bytes = fs::read(path)?;
    let metadata = fs::metadata(path)?;
    let modified_unix_ms = metadata
        .modified()?
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Ok(CurrentFileState {
        sha256: format!("{:x}", hasher.finalize()),
        modified_unix_ms,
        bytes: bytes.len(),
        exists: true,
    })
}

pub(super) fn todo_tool(
    store: &AppStore,
    run_id: &str,
    conversation_id: &str,
    payload: &Value,
) -> AppResult<String> {
    let Some(todos_value) = payload.get("todos") else {
        let todos = store.agent_todos_for_run(run_id)?;
        return Ok(serde_json::to_string_pretty(&json!({
            "runId": run_id,
            "todos": todos,
            "summary": todo_summary(&todos)
        }))?);
    };
    let incoming = todos_value
        .as_array()
        .ok_or_else(|| AppError::BadRequest("todo payload.todos must be an array".into()))?
        .iter()
        .map(parse_todo_payload_item)
        .collect::<Vec<_>>();
    let todos = if payload
        .get("merge")
        .or_else(|| payload.get("mergeById"))
        .or_else(|| payload.get("merge_by_id"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        merge_todo_items(store, run_id, incoming)?
    } else {
        incoming
            .into_iter()
            .map(|item| {
                (
                    item.id,
                    item.content.unwrap_or_else(|| "(no description)".into()),
                    item.status.unwrap_or_else(|| "pending".into()),
                )
            })
            .collect()
    };
    let saved = store.replace_agent_todos_with_ids(run_id, conversation_id, todos)?;
    Ok(serde_json::to_string_pretty(&json!({
        "runId": run_id,
        "todos": saved,
        "summary": todo_summary(&saved)
    }))?)
}

#[derive(Debug, Clone)]
struct TodoPayloadItem {
    id: Option<String>,
    content: Option<String>,
    status: Option<String>,
}

fn parse_todo_payload_item(item: &Value) -> TodoPayloadItem {
    if let Some(text) = item.as_str() {
        return TodoPayloadItem {
            id: None,
            content: Some(text.to_string()),
            status: Some("pending".to_string()),
        };
    }
    let id = item
        .get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let content = item
        .get("content")
        .or_else(|| item.get("task"))
        .or_else(|| item.get("text"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let status = item
        .get("status")
        .and_then(Value::as_str)
        .map(normalize_todo_status);
    TodoPayloadItem {
        id,
        content,
        status,
    }
}

fn merge_todo_items(
    store: &AppStore,
    run_id: &str,
    incoming: Vec<TodoPayloadItem>,
) -> AppResult<Vec<(Option<String>, String, String)>> {
    let mut merged = store
        .agent_todos_for_run(run_id)?
        .into_iter()
        .map(|item| (Some(item.id), item.content, item.status))
        .collect::<Vec<_>>();
    for update in incoming {
        let Some(id) = update.id.as_deref() else {
            if let Some(content) = update.content {
                merged.push((
                    None,
                    content,
                    update.status.unwrap_or_else(|| "pending".into()),
                ));
            }
            continue;
        };
        if let Some(existing) = merged.iter_mut().find(|item| item.0.as_deref() == Some(id)) {
            if let Some(content) = update.content {
                existing.1 = content;
            }
            if let Some(status) = update.status {
                existing.2 = status;
            }
        } else if let Some(content) = update.content {
            merged.push((
                Some(id.to_string()),
                content,
                update.status.unwrap_or_else(|| "pending".into()),
            ));
        }
    }
    Ok(merged)
}

fn todo_summary(todos: &[crate::models::AgentTodoItem]) -> Value {
    let pending = todos.iter().filter(|item| item.status == "pending").count();
    let in_progress = todos
        .iter()
        .filter(|item| item.status == "in_progress")
        .count();
    let completed = todos
        .iter()
        .filter(|item| item.status == "completed")
        .count();
    let cancelled = todos
        .iter()
        .filter(|item| item.status == "cancelled")
        .count();
    json!({
        "total": todos.len(),
        "pending": pending,
        "in_progress": in_progress,
        "completed": completed,
        "cancelled": cancelled,
    })
}

fn normalize_todo_status(status: &str) -> String {
    match status.trim().to_lowercase().as_str() {
        "done" | "complete" | "completed" => "completed".into(),
        "doing" | "active" | "in-progress" | "in_progress" => "in_progress".into(),
        "cancel" | "cancelled" | "canceled" => "cancelled".into(),
        "blocked" => "blocked".into(),
        _ => "pending".into(),
    }
}

pub(super) fn checkpoint_tool(
    store: &AppStore,
    run_id: &str,
    payload: &Value,
) -> AppResult<String> {
    let summary = payload
        .get("summary")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| AppError::BadRequest("checkpoint requires payload.summary".into()))?
        .to_string();
    let state = payload
        .get("state")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("checkpoint")
        .to_string();
    let mut run = store.agent_run(run_id)?;
    let checkpoint = AgentCheckpointRecord {
        checkpoint_id: new_id("ckpt"),
        run_id: run_id.to_string(),
        iteration: run.checkpoints.len() as u32 + 1,
        created_at: now_iso(),
        state,
        completed_call_ids: payload_string_array(payload, "completedCallIds", "completed_call_ids"),
        event_refs: payload_string_array(payload, "eventRefs", "event_refs"),
        summary,
    };
    run.checkpoints.push(checkpoint.clone());
    run.updated_at = now_iso();
    store.save_agent_run(run)?;
    Ok(serde_json::to_string_pretty(&checkpoint)?)
}

pub(super) fn automatic_mutation_checkpoint(
    store: &AppStore,
    run_id: &str,
    tool_name: &str,
    payload: &Value,
) -> AppResult<Option<AgentCheckpointRecord>> {
    if !store.config()?.chat.tool_mutation_checkpoint_enabled {
        return Ok(None);
    }
    let mut run = store.agent_run(run_id)?;
    let checkpoint = AgentCheckpointRecord {
        checkpoint_id: new_id("ckpt"),
        run_id: run_id.to_string(),
        iteration: run.checkpoints.len() as u32 + 1,
        created_at: now_iso(),
        state: "pre_file_mutation".into(),
        completed_call_ids: vec![],
        event_refs: vec![],
        summary: format!(
            "Automatic checkpoint before {tool_name}: {}",
            summarize_mutation_payload(tool_name, payload)
        ),
    };
    run.checkpoints.push(checkpoint.clone());
    run.updated_at = now_iso();
    store.save_agent_run(run)?;
    Ok(Some(checkpoint))
}

fn summarize_mutation_payload(tool_name: &str, payload: &Value) -> String {
    let summary = match tool_name {
        "write_file" | "delete_file" => payload_path(payload, "path")
            .map(|path| format!("path={path}"))
            .unwrap_or_else(|| "path=<missing>".into()),
        "move_file" => format!(
            "src={} dst={}",
            payload_path(payload, "src").unwrap_or_else(|| "<missing>".into()),
            payload_path(payload, "dst").unwrap_or_else(|| "<missing>".into())
        ),
        "patch" => summarize_patch_payload(payload),
        "skill_manage" => summarize_skill_manage_payload(payload),
        _ => "file mutation requested".into(),
    };
    truncate_summary(&summary, 360)
}

fn summarize_patch_payload(payload: &Value) -> String {
    if let Some(path) = payload_path(payload, "path") {
        return format!("path={path}");
    }
    if let Some(states) = payload
        .get("expectedFileStates")
        .or_else(|| payload.get("expected_file_states"))
        .and_then(Value::as_object)
    {
        let mut paths = states.keys().cloned().collect::<Vec<_>>();
        paths.sort();
        return format!("paths={}", paths.join(", "));
    }
    let Some(patch) = payload.get("patch").and_then(Value::as_str) else {
        return "patch=<missing path>".into();
    };
    let mut paths = Vec::new();
    for line in patch.lines() {
        for prefix in [
            "*** Add File: ",
            "*** Update File: ",
            "*** Delete File: ",
            "*** Move to: ",
        ] {
            if let Some(path) = line.strip_prefix(prefix) {
                let path = path.trim();
                if !path.is_empty() {
                    paths.push(path.to_string());
                }
            }
        }
    }
    paths.sort();
    paths.dedup();
    if paths.is_empty() {
        "patch=<unparsed path>".into()
    } else {
        format!("paths={}", paths.join(", "))
    }
}

fn summarize_skill_manage_payload(payload: &Value) -> String {
    let action = payload
        .get("action")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("unknown");
    let name = payload
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("<missing>");
    let file_path = payload_path(payload, "filePath")
        .or_else(|| payload_path(payload, "file_path"))
        .map(|path| format!(" filePath={path}"))
        .unwrap_or_default();
    format!("action={action} name={name}{file_path}")
}

fn payload_path(payload: &Value, key: &str) -> Option<String> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn truncate_summary(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        return value.to_string();
    }
    let mut truncated = value
        .chars()
        .take(limit.saturating_sub(15))
        .collect::<String>();
    truncated.push_str("... [truncated]");
    truncated
}

pub(super) fn artifact_tool(
    store: &AppStore,
    agent: &AgentDefinition,
    run_id: &str,
    payload: &Value,
) -> AppResult<String> {
    let action = payload
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("create")
        .trim()
        .to_lowercase();
    if matches!(action.as_str(), "publish_file" | "publish" | "file") {
        let path = payload
            .get("path")
            .or_else(|| payload.get("file"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                AppError::BadRequest("artifact publish_file requires payload.path".into())
            })?;
        let root = workspace_root(agent)?;
        let source = resolve_workspace_path(&root, path)?;
        if !source.is_file() {
            return Err(AppError::BadRequest(format!(
                "artifact publish_file requires a file: {}",
                source.display()
            )));
        }
        let bytes = fs::read(&source)?;
        let extension = source
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("bin");
        let name = payload
            .get("name")
            .or_else(|| payload.get("toolName"))
            .or_else(|| payload.get("tool_name"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("artifact_file");
        let artifact_path = store.save_tool_binary_artifact(run_id, name, extension, &bytes)?;
        return Ok(serde_json::to_string_pretty(&json!({
            "runId": run_id,
            "name": name,
            "sourcePath": source.to_string_lossy(),
            "path": artifact_path.to_string_lossy(),
            "mimeType": mime_from_path(&source),
            "sizeBytes": bytes.len(),
        }))?);
    }
    let content = payload
        .get("content")
        .or_else(|| payload.get("text"))
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::BadRequest("artifact requires payload.content".into()))?;
    let name = payload
        .get("name")
        .or_else(|| payload.get("toolName"))
        .or_else(|| payload.get("tool_name"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("artifact");
    let path = store.save_tool_artifact(run_id, name, content)?;
    Ok(serde_json::to_string_pretty(&json!({
        "runId": run_id,
        "name": name,
        "path": path.to_string_lossy(),
        "sizeBytes": content.len()
    }))?)
}

pub(super) fn list_artifacts_tool(store: &AppStore, run_id: &str) -> AppResult<String> {
    Ok(serde_json::to_string_pretty(&json!({
        "runId": run_id,
        "artifacts": store.tool_artifacts_for_run(run_id)?
    }))?)
}

fn mime_from_path(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .trim_start_matches('.')
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "pdf" => "application/pdf",
        "json" => "application/json",
        "md" | "markdown" => "text/markdown",
        "txt" | "log" => "text/plain",
        "csv" => "text/csv",
        "html" | "htm" => "text/html",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "webm" => "video/webm",
        "mp4" => "video/mp4",
        "mov" => "video/quicktime",
        "zip" => "application/zip",
        _ => "application/octet-stream",
    }
}

fn payload_string_array(payload: &Value, camel_key: &str, snake_key: &str) -> Vec<String> {
    payload
        .get(camel_key)
        .or_else(|| payload.get(snake_key))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}
