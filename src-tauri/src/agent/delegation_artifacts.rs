use std::path::PathBuf;

use serde_json::{json, Value};

use crate::{
    error::AppResult,
    models::{now_iso, AgentRunRecord},
    store::AppStore,
};

use super::{
    append_parent_phase_event, delegation_request::DelegateTaskRequest, redact_sensitive_text,
};

pub(super) fn delegation_file_state_reminder(
    store: &AppStore,
    parent_run_id: &str,
    since_iso: &str,
) -> AppResult<Option<Value>> {
    let writes = store.file_writes_since_for_reader(parent_run_id, since_iso)?;
    if writes.is_empty() {
        return Ok(None);
    }
    let files = writes
        .into_iter()
        .map(|record| {
            json!({
                "path": record.path,
                "lastWriter": record.last_writer,
                "lastWriterRunId": record.last_writer_run_id,
                "lastWriteAt": record.last_write_at,
                "sha256": record.sha256,
                "modifiedUnixMs": record.modified_unix_ms
            })
        })
        .collect::<Vec<_>>();
    Ok(Some(json!({
        "message": "Subagent modified files previously read by the parent run. Re-read them before making follow-up edits.",
        "count": files.len(),
        "files": files
    })))
}

pub(super) fn append_delegation_memory_observation(
    store: &AppStore,
    parent_run_id: &str,
    child_run_id: &str,
    child_conversation_id: &str,
    request: &DelegateTaskRequest,
    result: &str,
    transport: &str,
) -> AppResult<()> {
    append_parent_phase_event(
        store,
        parent_run_id,
        "memory_delegation_observed",
        json!({
            "task": request.task,
            "result": result,
            "childRunId": child_run_id,
            "childConversationId": child_conversation_id,
            "childSessionId": child_conversation_id,
            "role": request.role,
            "toolsets": request.toolsets,
            "maxIterations": request.max_iterations,
            "transport": transport,
            "note": "Hermes-compatible parent-side on_delegation observation. Stored as an agent run event, not persona long-term memory."
        }),
    )
}

pub(super) fn append_diagnostic_artifact_to_error(error: String, path: &Option<PathBuf>) -> String {
    match path {
        Some(path) => format!("{error}\nDiagnostic artifact: {}", path.to_string_lossy()),
        None => error,
    }
}

pub(super) fn save_subagent_failure_diagnostic_artifact(
    store: &AppStore,
    parent_run_id: &str,
    child_conversation_id: &str,
    child_run: Option<&AgentRunRecord>,
    request: &DelegateTaskRequest,
    error: &str,
    transport: &str,
) -> AppResult<Option<PathBuf>> {
    let recent_messages = store
        .messages(child_conversation_id, Some(8))?
        .into_iter()
        .map(|message| {
            json!({
                "id": message.id,
                "role": message.role,
                "source": message.source,
                "createdAt": message.created_at,
                "content": truncate_text(&redact_sensitive_text(&message.content), 2000)
            })
        })
        .collect::<Vec<_>>();
    let diagnostic = json!({
        "kind": "subagentFailureDiagnostic",
        "createdAt": now_iso(),
        "parentRunId": parent_run_id,
        "childConversationId": child_conversation_id,
        "transport": transport,
        "request": {
            "role": request.role,
            "task": request.task,
            "toolsets": request.toolsets,
            "canDelegate": request.can_delegate,
            "maxIterations": request.max_iterations,
            "acpCommand": request.acp_command,
            "acpArgs": request.acp_args,
            "acpSessionId": request.acp_session_id,
            "acpSessionMode": request.acp_session_mode
        },
        "error": truncate_text(&redact_sensitive_text(error), 4000),
        "childRun": child_run.map(|run| {
            json!({
                "runId": run.run_id,
                "state": run.state,
                "startedAt": run.started_at,
                "updatedAt": run.updated_at,
                "lastActivityAt": run.last_activity_at,
                "lastActivityDesc": run.last_activity_desc,
                "completedAt": run.completed_at,
                "error": run.error.as_ref().map(|value| truncate_text(&redact_sensitive_text(value), 2000)),
                "toolEventCount": run.tool_events.len(),
                "phaseEventCount": run.phase_events.len(),
                "checkpointCount": run.checkpoints.len(),
                "recentToolEvents": run.tool_events.iter().rev().take(12).cloned().collect::<Vec<_>>(),
                "recentPhaseEvents": run.phase_events.iter().rev().take(12).cloned().collect::<Vec<_>>(),
                "recentCheckpoints": run.checkpoints.iter().rev().take(5).cloned().collect::<Vec<_>>()
            })
        }),
        "recentMessages": recent_messages
    });
    let content = serde_json::to_string_pretty(&diagnostic)?;
    Ok(Some(store.save_tool_artifact(
        parent_run_id,
        "subagent_failure_diagnostic",
        &content,
    )?))
}

fn truncate_text(value: &str, max_chars: usize) -> String {
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
