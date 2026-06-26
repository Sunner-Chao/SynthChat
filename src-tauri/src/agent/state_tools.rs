use std::{
    fs,
    io::{Cursor, Write},
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
            .unwrap_or_else(|| {
                source
                    .file_name()
                    .and_then(|value| value.to_str())
                    .unwrap_or("artifact_file")
            });
        let file_name = ensure_extension(name, extension);
        let artifact_path = store.save_tool_named_binary_artifact(run_id, &file_name, &bytes)?;
        let mime_type = mime_from_path(&source);
        let path_text = artifact_path.to_string_lossy().to_string();
        let media_tag = format!(r#"MEDIA:"{}""#, path_text);
        return Ok(serde_json::to_string_pretty(&json!({
            "runId": run_id,
            "name": name,
            "sourcePath": source.to_string_lossy(),
            "path": path_text,
            "mimeType": mime_type,
            "sizeBytes": bytes.len(),
            "mediaTag": media_tag,
            "wechatMarker": media_tag,
            "wechatSendHint": "To send this file to the linked WeChat mobile user, include mediaTag as its own line in the final assistant reply. The bridge hides this internal MEDIA directive from visible text and uploads the file."
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

pub(super) fn document_tool(store: &AppStore, run_id: &str, payload: &Value) -> AppResult<String> {
    let format = payload
        .get("format")
        .or_else(|| payload.get("type"))
        .and_then(Value::as_str)
        .map(normalize_document_format)
        .unwrap_or_else(|| "docx".into());
    if !matches!(
        format.as_str(),
        "docx" | "xlsx" | "pptx" | "html" | "md" | "txt" | "csv"
    ) {
        return Err(AppError::BadRequest(format!(
            "document format is not supported: {format}. Use docx, xlsx, pptx, html, md, txt, or csv."
        )));
    }
    let title = payload
        .get("title")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("Document");
    let content = payload
        .get("content")
        .or_else(|| payload.get("text"))
        .or_else(|| payload.get("body"))
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("");
    if content.is_empty() {
        return Err(AppError::BadRequest(
            "document requires payload.content".into(),
        ));
    }
    let name = payload
        .get("name")
        .or_else(|| payload.get("fileName"))
        .or_else(|| payload.get("file_name"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(title);
    let file_name = ensure_extension(&safe_document_name(name), &format);
    let bytes = match format.as_str() {
        "docx" => build_docx_document(title, content)?,
        "xlsx" => build_xlsx_document(title, content)?,
        "pptx" => build_pptx_document(title, content)?,
        "html" => build_html_document(title, content).into_bytes(),
        "md" => format!("# {title}\n\n{content}\n").into_bytes(),
        "txt" => format!("{title}\n\n{content}\n").into_bytes(),
        "csv" => normalize_csv_content(content).into_bytes(),
        _ => unreachable!(),
    };
    let artifact_path = store.save_tool_named_binary_artifact(run_id, &file_name, &bytes)?;
    let mime_type = mime_from_path(&artifact_path);
    let path_text = artifact_path.to_string_lossy().to_string();
    let media_tag = format!(r#"MEDIA:"{}""#, path_text);
    Ok(serde_json::to_string_pretty(&json!({
        "runId": run_id,
        "title": title,
        "format": format,
        "path": path_text,
        "mimeType": mime_type,
        "sizeBytes": bytes.len(),
        "mediaTag": media_tag,
        "wechatMarker": media_tag,
        "wechatSendHint": "To send this document to the linked WeChat mobile user, include mediaTag as its own line in the final assistant reply. The bridge hides this internal MEDIA directive from visible text and uploads the file."
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
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "webm" => "video/webm",
        "mp4" => "video/mp4",
        "mov" => "video/quicktime",
        "zip" => "application/zip",
        _ => "application/octet-stream",
    }
}

fn normalize_document_format(value: &str) -> String {
    let lowered = value.trim().trim_start_matches('.').to_ascii_lowercase();
    match lowered.as_str() {
        "markdown" => "md".into(),
        "text" => "txt".into(),
        "htm" => "html".into(),
        "xls" => "xlsx".into(),
        "ppt" => "pptx".into(),
        other => other.into(),
    }
}

fn safe_document_name(value: &str) -> String {
    let mut output = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | ' ') || !ch.is_control() {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim()
        .trim_matches('.')
        .to_string();
    if output.is_empty() {
        output = "document".into();
    }
    output.chars().take(80).collect()
}

fn ensure_extension(name: &str, extension: &str) -> String {
    let clean_ext = extension.trim_start_matches('.').to_ascii_lowercase();
    if clean_ext.is_empty() {
        return name.to_string();
    }
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(&format!(".{clean_ext}")) {
        name.to_string()
    } else {
        format!("{name}.{clean_ext}")
    }
}

fn build_docx_document(title: &str, content: &str) -> AppResult<Vec<u8>> {
    let writer = Cursor::new(Vec::new());
    let mut zip = zip::ZipWriter::new(writer);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    zip_start_file(&mut zip, "[Content_Types].xml", options)?;
    zip.write_all(br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/docProps/core.xml" ContentType="application/vnd.openxmlformats-package.core-properties+xml"/></Types>"#)?;
    zip_start_file(&mut zip, "_rels/.rels", options)?;
    zip.write_all(br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties" Target="docProps/core.xml"/></Relationships>"#)?;
    zip_start_file(&mut zip, "docProps/core.xml", options)?;
    zip.write_all(format!(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>{}</dc:title></cp:coreProperties>"#, xml_escape(title)).as_bytes())?;
    zip_start_file(&mut zip, "word/document.xml", options)?;
    zip.write_all(docx_document_xml(title, content).as_bytes())?;
    let writer = zip
        .finish()
        .map_err(|error| AppError::BadRequest(format!("failed to finish docx: {error}")))?;
    Ok(writer.into_inner())
}

fn docx_document_xml(title: &str, content: &str) -> String {
    let mut body = String::new();
    body.push_str(&docx_paragraph(title, true));
    for paragraph in content.split("\n\n") {
        let paragraph = paragraph.trim();
        if paragraph.is_empty() {
            continue;
        }
        body.push_str(&docx_paragraph(paragraph, false));
    }
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{}<w:sectPr><w:pgSz w:w="11906" w:h="16838"/><w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"/></w:sectPr></w:body></w:document>"#,
        body
    )
}

fn docx_paragraph(text: &str, heading: bool) -> String {
    let mut runs = String::new();
    for (index, line) in text.lines().enumerate() {
        if index > 0 {
            runs.push_str("<w:br/>");
        }
        runs.push_str(&format!("<w:t>{}</w:t>", xml_escape(line)));
    }
    let style = if heading {
        r#"<w:pPr><w:spacing w:after="240"/><w:rPr><w:b/><w:sz w:val="32"/></w:rPr></w:pPr>"#
    } else {
        r#"<w:pPr><w:spacing w:after="160"/></w:pPr>"#
    };
    format!(r#"<w:p>{style}<w:r>{runs}</w:r></w:p>"#)
}

fn build_xlsx_document(title: &str, content: &str) -> AppResult<Vec<u8>> {
    let rows = spreadsheet_rows(content);
    let writer = Cursor::new(Vec::new());
    let mut zip = zip::ZipWriter::new(writer);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    zip_start_file(&mut zip, "[Content_Types].xml", options)?;
    zip.write_all(br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/></Types>"#)?;
    zip_start_file(&mut zip, "_rels/.rels", options)?;
    zip.write_all(br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#)?;
    zip_start_file(&mut zip, "xl/_rels/workbook.xml.rels", options)?;
    zip.write_all(br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#)?;
    zip_start_file(&mut zip, "xl/workbook.xml", options)?;
    zip.write_all(format!(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="{}" sheetId="1" r:id="rId1"/></sheets></workbook>"#, xml_escape(&safe_sheet_name(title))).as_bytes())?;
    zip_start_file(&mut zip, "xl/worksheets/sheet1.xml", options)?;
    zip.write_all(xlsx_sheet_xml(&rows).as_bytes())?;
    let writer = zip
        .finish()
        .map_err(|error| AppError::BadRequest(format!("failed to finish xlsx: {error}")))?;
    Ok(writer.into_inner())
}

#[derive(Debug, Clone)]
struct PptxSlide {
    title: String,
    lines: Vec<String>,
    title_slide: bool,
}

fn build_pptx_document(title: &str, content: &str) -> AppResult<Vec<u8>> {
    let slides = pptx_slides(title, content);
    let writer = Cursor::new(Vec::new());
    let mut zip = zip::ZipWriter::new(writer);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);

    zip_start_file(&mut zip, "[Content_Types].xml", options)?;
    zip.write_all(pptx_content_types_xml(slides.len()).as_bytes())?;
    zip_start_file(&mut zip, "_rels/.rels", options)?;
    zip.write_all(br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="ppt/presentation.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties" Target="docProps/core.xml"/><Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/extended-properties" Target="docProps/app.xml"/></Relationships>"#)?;
    zip_start_file(&mut zip, "docProps/core.xml", options)?;
    zip.write_all(format!(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>{}</dc:title></cp:coreProperties>"#, xml_escape(title)).as_bytes())?;
    zip_start_file(&mut zip, "docProps/app.xml", options)?;
    zip.write_all(pptx_app_xml(slides.len()).as_bytes())?;
    zip_start_file(&mut zip, "ppt/presentation.xml", options)?;
    zip.write_all(pptx_presentation_xml(slides.len()).as_bytes())?;
    zip_start_file(&mut zip, "ppt/_rels/presentation.xml.rels", options)?;
    zip.write_all(pptx_presentation_rels_xml(slides.len()).as_bytes())?;
    zip_start_file(&mut zip, "ppt/slideMasters/slideMaster1.xml", options)?;
    zip.write_all(pptx_slide_master_xml().as_bytes())?;
    zip_start_file(
        &mut zip,
        "ppt/slideMasters/_rels/slideMaster1.xml.rels",
        options,
    )?;
    zip.write_all(br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideLayout" Target="../slideLayouts/slideLayout1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/theme" Target="../theme/theme1.xml"/></Relationships>"#)?;
    zip_start_file(&mut zip, "ppt/slideLayouts/slideLayout1.xml", options)?;
    zip.write_all(pptx_slide_layout_xml().as_bytes())?;
    zip_start_file(
        &mut zip,
        "ppt/slideLayouts/_rels/slideLayout1.xml.rels",
        options,
    )?;
    zip.write_all(br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideMaster" Target="../slideMasters/slideMaster1.xml"/></Relationships>"#)?;
    zip_start_file(&mut zip, "ppt/theme/theme1.xml", options)?;
    zip.write_all(pptx_theme_xml().as_bytes())?;
    for (index, slide) in slides.iter().enumerate() {
        let slide_number = index + 1;
        zip_start_file(
            &mut zip,
            &format!("ppt/slides/slide{slide_number}.xml"),
            options,
        )?;
        zip.write_all(pptx_slide_xml(slide).as_bytes())?;
        zip_start_file(
            &mut zip,
            &format!("ppt/slides/_rels/slide{slide_number}.xml.rels"),
            options,
        )?;
        zip.write_all(br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideLayout" Target="../slideLayouts/slideLayout1.xml"/></Relationships>"#)?;
    }
    let writer = zip
        .finish()
        .map_err(|error| AppError::BadRequest(format!("failed to finish pptx: {error}")))?;
    Ok(writer.into_inner())
}

fn zip_start_file<W: Write + std::io::Seek>(
    zip: &mut zip::ZipWriter<W>,
    name: &str,
    options: zip::write::SimpleFileOptions,
) -> AppResult<()> {
    zip.start_file(name, options).map_err(|error| {
        AppError::BadRequest(format!(
            "failed to write document zip entry {name}: {error}"
        ))
    })
}

fn pptx_slides(title: &str, content: &str) -> Vec<PptxSlide> {
    let mut slides = Vec::new();
    slides.push(PptxSlide {
        title: title.to_string(),
        lines: Vec::new(),
        title_slide: true,
    });
    for section in content.split("\n\n") {
        let section = section.trim();
        if section.is_empty() {
            continue;
        }
        let mut lines = section
            .lines()
            .map(|line| {
                line.trim()
                    .trim_start_matches(|ch: char| matches!(ch, '-' | '*' | ' ' | '\t'))
                    .to_string()
            })
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>();
        if lines.is_empty() {
            continue;
        }
        let section_title = lines.remove(0);
        if lines.is_empty() {
            lines = wrap_pptx_lines(&section_title, 68);
        }
        for chunk in lines.chunks(6) {
            slides.push(PptxSlide {
                title: section_title.clone(),
                lines: chunk.to_vec(),
                title_slide: false,
            });
        }
    }
    if slides.len() == 1 {
        slides.push(PptxSlide {
            title: title.to_string(),
            lines: content
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .take(6)
                .map(str::to_string)
                .collect(),
            title_slide: false,
        });
    }
    slides.into_iter().take(40).collect()
}

fn wrap_pptx_lines(text: &str, max_chars: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        if !current.is_empty() && current.len() + word.len() + 1 > max_chars {
            lines.push(current);
            current = String::new();
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    if !current.is_empty() {
        lines.push(current);
    }
    if lines.is_empty() {
        vec![text.to_string()]
    } else {
        lines
    }
}

fn pptx_content_types_xml(slide_count: usize) -> String {
    let mut overrides = String::new();
    for index in 1..=slide_count {
        overrides.push_str(&format!(r#"<Override PartName="/ppt/slides/slide{index}.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slide+xml"/>"#));
    }
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/ppt/presentation.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml"/><Override PartName="/ppt/slideMasters/slideMaster1.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slideMaster+xml"/><Override PartName="/ppt/slideLayouts/slideLayout1.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slideLayout+xml"/><Override PartName="/ppt/theme/theme1.xml" ContentType="application/vnd.openxmlformats-officedocument.theme+xml"/><Override PartName="/docProps/core.xml" ContentType="application/vnd.openxmlformats-package.core-properties+xml"/><Override PartName="/docProps/app.xml" ContentType="application/vnd.openxmlformats-officedocument.extended-properties+xml"/>{overrides}</Types>"#
    )
}

fn pptx_app_xml(slide_count: usize) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/extended-properties" xmlns:vt="http://schemas.openxmlformats.org/officeDocument/2006/docPropsVTypes"><Application>SynthChat</Application><PresentationFormat>On-screen Show (16:9)</PresentationFormat><Slides>{slide_count}</Slides></Properties>"#
    )
}

fn pptx_presentation_xml(slide_count: usize) -> String {
    let mut slide_ids = String::new();
    for index in 1..=slide_count {
        let id = 255 + index;
        let rid = index + 1;
        slide_ids.push_str(&format!(r#"<p:sldId id="{id}" r:id="rId{rid}"/>"#));
    }
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><p:presentation xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"><p:sldMasterIdLst><p:sldMasterId id="2147483648" r:id="rId1"/></p:sldMasterIdLst><p:sldIdLst>{slide_ids}</p:sldIdLst><p:sldSz cx="12192000" cy="6858000" type="screen16x9"/><p:notesSz cx="6858000" cy="9144000"/></p:presentation>"#
    )
}

fn pptx_presentation_rels_xml(slide_count: usize) -> String {
    let mut rels = String::from(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideMaster" Target="slideMasters/slideMaster1.xml"/>"#,
    );
    for index in 1..=slide_count {
        let rid = index + 1;
        rels.push_str(&format!(r#"<Relationship Id="rId{rid}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide{index}.xml"/>"#));
    }
    rels.push_str("</Relationships>");
    rels
}

fn pptx_slide_xml(slide: &PptxSlide) -> String {
    let body = if slide.title_slide {
        format!(
            "{}{}",
            pptx_text_box(
                2,
                &slide.title,
                &[],
                760_000,
                1_890_000,
                10_640_000,
                1_300_000,
                44,
                true
            ),
            pptx_text_box(
                3,
                "Generated document deck",
                &[],
                1_250_000,
                3_420_000,
                9_600_000,
                650_000,
                18,
                false
            )
        )
    } else {
        pptx_text_box(
            2,
            &slide.title,
            &slide.lines,
            700_000,
            520_000,
            10_800_000,
            5_700_000,
            30,
            true,
        )
    };
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><p:sld xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"><p:cSld><p:bg><p:bgPr><a:solidFill><a:srgbClr val="FFFFFF"/></a:solidFill><a:effectLst/></p:bgPr></p:bg><p:spTree><p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="0" cy="0"/><a:chOff x="0" y="0"/><a:chExt cx="0" cy="0"/></a:xfrm></p:grpSpPr>{body}</p:spTree></p:cSld><p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:sld>"#
    )
}

fn pptx_text_box(
    shape_id: u32,
    title: &str,
    lines: &[String],
    x: i64,
    y: i64,
    cx: i64,
    cy: i64,
    title_size: i32,
    centered: bool,
) -> String {
    let align = if centered { r#" algn="ctr""# } else { "" };
    let mut paragraphs = format!(
        r#"<a:p><a:pPr{align}/><a:r><a:rPr lang="zh-CN" sz="{}" b="1"><a:solidFill><a:srgbClr val="1F2937"/></a:solidFill></a:rPr><a:t>{}</a:t></a:r><a:endParaRPr lang="zh-CN" sz="{}"/></a:p>"#,
        title_size * 100,
        xml_escape(title),
        title_size * 100
    );
    for line in lines {
        paragraphs.push_str(&format!(
            r#"<a:p><a:pPr marL="342900" indent="-171450"><a:buChar char="&#8226;"/></a:pPr><a:r><a:rPr lang="zh-CN" sz="2000"><a:solidFill><a:srgbClr val="334155"/></a:solidFill></a:rPr><a:t>{}</a:t></a:r><a:endParaRPr lang="zh-CN" sz="2000"/></a:p>"#,
            xml_escape(line)
        ));
    }
    format!(
        r#"<p:sp><p:nvSpPr><p:cNvPr id="{shape_id}" name="Text"/><p:cNvSpPr txBox="1"/><p:nvPr/></p:nvSpPr><p:spPr><a:xfrm><a:off x="{x}" y="{y}"/><a:ext cx="{cx}" cy="{cy}"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom><a:noFill/></p:spPr><p:txBody><a:bodyPr wrap="square" rtlCol="0"><a:spAutoFit/></a:bodyPr><a:lstStyle/>{paragraphs}</p:txBody></p:sp>"#
    )
}

fn pptx_slide_master_xml() -> String {
    r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><p:sldMaster xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"><p:cSld><p:spTree><p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="0" cy="0"/><a:chOff x="0" y="0"/><a:chExt cx="0" cy="0"/></a:xfrm></p:grpSpPr></p:spTree></p:cSld><p:clrMap bg1="lt1" tx1="dk1" bg2="lt2" tx2="dk2" accent1="accent1" accent2="accent2" accent3="accent3" accent4="accent4" accent5="accent5" accent6="accent6" hlink="hlink" folHlink="folHlink"/><p:sldLayoutIdLst><p:sldLayoutId id="2147483649" r:id="rId1"/></p:sldLayoutIdLst><p:txStyles><p:titleStyle/><p:bodyStyle/><p:otherStyle/></p:txStyles></p:sldMaster>"#.to_string()
}

fn pptx_slide_layout_xml() -> String {
    r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><p:sldLayout xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" type="blank" preserve="1"><p:cSld name="Blank"><p:spTree><p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="0" cy="0"/><a:chOff x="0" y="0"/><a:chExt cx="0" cy="0"/></a:xfrm></p:grpSpPr></p:spTree></p:cSld><p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:sldLayout>"#.to_string()
}

fn pptx_theme_xml() -> String {
    r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><a:theme xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" name="SynthChat"><a:themeElements><a:clrScheme name="SynthChat"><a:dk1><a:srgbClr val="111827"/></a:dk1><a:lt1><a:srgbClr val="FFFFFF"/></a:lt1><a:dk2><a:srgbClr val="334155"/></a:dk2><a:lt2><a:srgbClr val="F8FAFC"/></a:lt2><a:accent1><a:srgbClr val="2563EB"/></a:accent1><a:accent2><a:srgbClr val="10B981"/></a:accent2><a:accent3><a:srgbClr val="F59E0B"/></a:accent3><a:accent4><a:srgbClr val="EF4444"/></a:accent4><a:accent5><a:srgbClr val="8B5CF6"/></a:accent5><a:accent6><a:srgbClr val="06B6D4"/></a:accent6><a:hlink><a:srgbClr val="2563EB"/></a:hlink><a:folHlink><a:srgbClr val="7C3AED"/></a:folHlink></a:clrScheme><a:fontScheme name="SynthChat"><a:majorFont><a:latin typeface="Aptos Display"/><a:ea typeface="Microsoft YaHei"/></a:majorFont><a:minorFont><a:latin typeface="Aptos"/><a:ea typeface="Microsoft YaHei"/></a:minorFont></a:fontScheme><a:fmtScheme name="SynthChat"><a:fillStyleLst><a:solidFill><a:schemeClr val="phClr"/></a:solidFill></a:fillStyleLst><a:lnStyleLst><a:ln w="6350"><a:solidFill><a:schemeClr val="phClr"/></a:solidFill></a:ln></a:lnStyleLst><a:effectStyleLst><a:effectStyle><a:effectLst/></a:effectStyle></a:effectStyleLst><a:bgFillStyleLst><a:solidFill><a:schemeClr val="phClr"/></a:solidFill></a:bgFillStyleLst></a:fmtScheme></a:themeElements></a:theme>"#.to_string()
}

fn spreadsheet_rows(content: &str) -> Vec<Vec<String>> {
    content
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            if line.contains('\t') {
                line.split('\t')
                    .map(|cell| cell.trim().to_string())
                    .collect()
            } else {
                line.split(',')
                    .map(|cell| cell.trim().to_string())
                    .collect()
            }
        })
        .collect()
}

fn xlsx_sheet_xml(rows: &[Vec<String>]) -> String {
    let mut sheet = String::from(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData>"#,
    );
    for (row_index, row) in rows.iter().enumerate() {
        let number = row_index + 1;
        sheet.push_str(&format!(r#"<row r="{number}">"#));
        for (col_index, cell) in row.iter().enumerate() {
            sheet.push_str(&format!(
                r#"<c r="{}{}" t="inlineStr"><is><t>{}</t></is></c>"#,
                spreadsheet_column_name(col_index),
                number,
                xml_escape(cell)
            ));
        }
        sheet.push_str("</row>");
    }
    sheet.push_str("</sheetData></worksheet>");
    sheet
}

fn spreadsheet_column_name(mut index: usize) -> String {
    let mut chars = Vec::new();
    loop {
        let rem = index % 26;
        chars.push((b'A' + rem as u8) as char);
        if index < 26 {
            break;
        }
        index = index / 26 - 1;
    }
    chars.iter().rev().collect()
}

fn safe_sheet_name(title: &str) -> String {
    let name = title
        .chars()
        .filter(|ch| !matches!(ch, ':' | '\\' | '/' | '?' | '*' | '[' | ']'))
        .take(31)
        .collect::<String>();
    if name.trim().is_empty() {
        "Sheet1".into()
    } else {
        name
    }
}

fn build_html_document(title: &str, content: &str) -> String {
    let body = content
        .split("\n\n")
        .map(|paragraph| format!("<p>{}</p>", xml_escape(paragraph.trim())))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>{}</title></head><body><h1>{}</h1>{}</body></html>",
        xml_escape(title),
        xml_escape(title),
        body
    )
}

fn normalize_csv_content(content: &str) -> String {
    let mut output = content.trim().to_string();
    if !output.ends_with('\n') {
        output.push('\n');
    }
    output
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
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
