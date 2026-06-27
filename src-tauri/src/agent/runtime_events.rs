use std::{
    collections::HashMap,
    path::Path,
    sync::{Mutex, OnceLock},
};

use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};
use tokio::sync::broadcast;

use crate::{
    error::{AppError, AppResult},
    models::{
        new_id, now_iso, tool_event_kind, AgentGoalState, AgentRunPhaseRecord, AgentRunRecord,
        ChatMessage, PlannerTraceRecord, ToolDefinition, ToolEvent,
    },
    store::AppStore,
};

use super::{
    decision_parser::{planner_decision_error, provider_tool_call_id, summarize_planner_step},
    is_internal_tool, redact_json_value, redact_sensitive_text, resolve_mcp_tool,
    truncate_for_prompt,
};

static AGENT_RUN_BROADCASTERS: OnceLock<Mutex<HashMap<String, broadcast::Sender<AgentRunRecord>>>> =
    OnceLock::new();

fn agent_run_broadcasters() -> &'static Mutex<HashMap<String, broadcast::Sender<AgentRunRecord>>> {
    AGENT_RUN_BROADCASTERS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(crate) fn subscribe_agent_run_record(run_id: &str) -> broadcast::Receiver<AgentRunRecord> {
    let mut broadcasters = agent_run_broadcasters()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    broadcasters
        .entry(run_id.to_string())
        .or_insert_with(|| {
            let (sender, _) = broadcast::channel(256);
            sender
        })
        .subscribe()
}

pub(crate) fn publish_agent_run_record(run: &AgentRunRecord) {
    let sender = {
        let mut broadcasters = agent_run_broadcasters()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        broadcasters
            .entry(run.run_id.clone())
            .or_insert_with(|| {
                let (sender, _) = broadcast::channel(256);
                sender
            })
            .clone()
    };
    let _ = sender.send(run.clone());
}

pub(super) fn record_tool_event_for_run(
    store: &AppStore,
    app: Option<&AppHandle>,
    conversation_id: &str,
    run_id: &str,
    mut event: ToolEvent,
) -> AppResult<()> {
    normalize_tool_event_for_display(&mut event);
    if let Ok(mut run) = store.agent_run(run_id) {
        let run_is_terminal = matches!(run.state.as_str(), "completed" | "failed" | "aborted");
        if run_is_terminal && event.status.as_deref() == Some("running") {
            event = tool_canceled_event_from_running(
                &event,
                terminal_tool_event_summary_for_run_state(&run.state),
            );
        }
        let tool_message = ChatMessage::new(
            conversation_id.to_string(),
            "tool",
            json!({"type": "toolEvent", "event": event.clone()}).to_string(),
            "desktop-agent-tool",
        );
        let tool_message = store.append_message(tool_message)?;
        if !run_is_terminal {
            run.state = "running".into();
            run.completed_at = None;
        }
        run.touch_activity(format!(
            "tool {}: {}",
            event.status.as_deref().unwrap_or("event"),
            event.tool_name
        ));
        push_tool_event_record(&mut run, &event);
        run.phase_events.push(AgentRunPhaseRecord {
            phase: "tool_message_recorded".into(),
            detail: json!({
                "messageId": tool_message.id,
                "serverId": event.server_id,
                "toolName": event.tool_name,
                "status": event.status,
                "ok": event.ok,
            }),
            updated_at: run.updated_at.clone(),
        });
        let saved_run = store.save_agent_run(run)?;
        emit_agent_run_record(app, &saved_run, Some(&tool_message));
    } else {
        if event.status.as_deref() == Some("running") {
            event = tool_canceled_event_from_running(&event, "运行已结束");
        }
        let tool_message = ChatMessage::new(
            conversation_id.to_string(),
            "tool",
            json!({"type": "toolEvent", "event": event.clone()}).to_string(),
            "desktop-agent-tool",
        );
        let _ = store.append_message(tool_message)?;
    }
    Ok(())
}

pub(super) fn record_tool_failed_for_run(
    store: &AppStore,
    app: Option<&AppHandle>,
    conversation_id: &str,
    run_id: &str,
    requested_tool_name: &str,
    mcp_tools: &[ToolDefinition],
    payload: &Value,
    error: &AppError,
) -> AppResult<()> {
    let (server_id, tool_name) = tool_event_target_for_request(requested_tool_name, mcp_tools);
    let (started, event) =
        tool_failed_transition_events(run_id, &server_id, &tool_name, payload, &error.to_string());
    if !tool_error_is_run_inactive(&error.to_string()) {
        record_tool_event_for_run(store, app, conversation_id, run_id, started)?;
    }
    record_tool_event_for_run(store, app, conversation_id, run_id, event)
}

pub(super) fn tool_failed_transition_events(
    run_id: &str,
    server_id: &str,
    tool_name: &str,
    payload: &Value,
    error: &str,
) -> (ToolEvent, ToolEvent) {
    (
        tool_started_event(run_id, server_id, tool_name, payload),
        tool_failed_event(run_id, server_id, tool_name, payload, error),
    )
}

fn tool_event_target_for_request(
    requested_tool_name: &str,
    mcp_tools: &[ToolDefinition],
) -> (String, String) {
    if is_internal_tool(requested_tool_name) {
        ("__internal".into(), requested_tool_name.to_string())
    } else if let Some(definition) = resolve_mcp_tool(mcp_tools, requested_tool_name) {
        (definition.server_id, definition.tool_name)
    } else {
        ("<missing>".into(), requested_tool_name.to_string())
    }
}

pub(super) fn emit_agent_run_record(
    app: Option<&AppHandle>,
    run: &AgentRunRecord,
    message: Option<&ChatMessage>,
) {
    publish_agent_run_record(run);
    let Some(app) = app else {
        return;
    };
    let phase = run.phase_events.last();
    let tool_event = run.tool_events.last().cloned();
    let payload = json!({
        "runId": &run.run_id,
        "conversationId": &run.conversation_id,
        "personaId": &run.persona_id,
        "agentId": &run.agent_id,
        "parentRunId": &run.parent_run_id,
        "subagentIndex": run.subagent_index,
        "subagentDepth": run.subagent_depth,
        "subagentCanDelegate": run.subagent_can_delegate,
        "subagentRole": &run.subagent_role,
        "subagentTask": &run.subagent_task,
        "subagentToolsets": &run.subagent_toolsets,
        "subagentMaxIterations": run.subagent_max_iterations,
        "queueItemId": &run.queue_item_id,
        "state": &run.state,
        "message": message,
        "toolEvent": tool_event,
        "phase": phase.map(|item| item.phase.clone()),
        "detail": phase.map(|item| item.detail.clone()),
        "error": &run.error,
        "updatedAt": &run.updated_at,
        "lastActivityAt": &run.last_activity_at,
        "lastActivityDesc": &run.last_activity_desc,
    });
    let _ = app.emit("synthchat-agent-run-event", payload);
}

pub(crate) fn emit_pet_assistant_event(
    app: Option<&AppHandle>,
    event_type: &str,
    source: &str,
    persona_id: Option<&str>,
    conversation_id: &str,
    message: &ChatMessage,
) {
    let Some(app) = app else {
        return;
    };
    let _ = app.emit(
        "synthchat-pet-event",
        json!({
            "type": event_type,
            "source": source,
            "personaId": persona_id,
            "conversationId": conversation_id,
            "message": message,
        }),
    );
}

pub(crate) fn emit_agent_queue_event(
    app: Option<&AppHandle>,
    event_type: &str,
    item: Option<&crate::models::AgentQueuedRequest>,
    conversation_id: Option<&str>,
) {
    let Some(app) = app else {
        return;
    };
    let payload = json!({
        "type": event_type,
        "conversationId": item
            .map(|item| item.conversation_id.as_str())
            .or(conversation_id),
        "item": item,
    });
    let _ = app.emit("synthchat-agent-queue-event", payload);
}

pub(crate) fn emit_agent_goal_event(
    app: Option<&AppHandle>,
    event_type: &str,
    conversation_id: &str,
    goal: Option<&AgentGoalState>,
    reason: Option<&str>,
) {
    let Some(app) = app else {
        return;
    };
    let payload = json!({
        "type": event_type,
        "conversationId": conversation_id,
        "goal": goal,
        "reason": reason,
    });
    let _ = app.emit("synthchat-agent-goal-event", payload);
}

pub(super) fn record_tool_started_for_run(
    store: &AppStore,
    app: Option<&AppHandle>,
    run_id: &str,
    server_id: &str,
    tool_name: &str,
    payload: &Value,
    iteration: u32,
) -> AppResult<()> {
    if let Ok(mut run) = store.agent_run(run_id) {
        let event = tool_started_event(run_id, server_id, tool_name, payload);
        let tool_message = ChatMessage::new(
            run.conversation_id.clone(),
            "tool",
            json!({"type": "toolEvent", "event": event.clone()}).to_string(),
            "desktop-agent-tool",
        );
        let tool_message = store.append_message(tool_message)?;
        run.state = "running".into();
        run.completed_at = None;
        run.touch_activity(format!("tool started: {server_id}.{tool_name}"));
        push_tool_event_record(&mut run, &event);
        run.phase_events.push(AgentRunPhaseRecord {
            phase: "tool_started".into(),
            detail: json!({
                "iteration": iteration,
                "serverId": server_id,
                "toolName": tool_name,
                "callId": event.call_id,
                "payloadPreview": truncate_for_prompt(
                    &redact_json_value(payload.clone()).to_string(),
                    1000,
                ),
            }),
            updated_at: run.updated_at.clone(),
        });
        let saved_run = store.save_agent_run(run)?;
        emit_agent_run_record(app, &saved_run, Some(&tool_message));
    }
    Ok(())
}

pub(super) fn push_tool_event_record(run: &mut AgentRunRecord, event: &ToolEvent) {
    let value = serde_json::to_value(event).unwrap_or_else(|_| json!({}));
    let status = event.status.as_deref().unwrap_or("");
    let same_provider_call = |item: &Value| match (
        tool_event_provider_call_id(item),
        tool_event_provider_call_id(&value),
    ) {
        (Some(left_id), Some(right_id)) => left_id == right_id,
        _ => false,
    };
    let same_tool_run = |item: &Value| {
        item.get("serverId").and_then(Value::as_str) == Some(event.server_id.as_str())
            && item.get("toolName").and_then(Value::as_str) == Some(event.tool_name.as_str())
            && item.get("title").and_then(Value::as_str) == Some(event.title.as_str())
            && tool_event_payload_matches(item, &value)
    };
    if status != "running" {
        if let Some(index) = run.tool_events.iter().rposition(|item| {
            item.get("status").and_then(Value::as_str) == Some("running")
                && same_provider_call(item)
        }) {
            run.tool_events[index] = value;
            return;
        }
        if let Some(index) = run.tool_events.iter().rposition(|item| {
            item.get("status").and_then(Value::as_str) == Some("running") && same_tool_run(item)
        }) {
            run.tool_events[index] = value;
            return;
        }
    } else {
        if let Some(index) = run.tool_events.iter().rposition(|item| {
            item.get("status").and_then(Value::as_str) == Some("running")
                && same_provider_call(item)
        }) {
            run.tool_events[index] = value;
            return;
        }
        if let Some(index) = run.tool_events.iter().rposition(|item| {
            item.get("status").and_then(Value::as_str) == Some("running") && same_tool_run(item)
        }) {
            run.tool_events[index] = value;
            return;
        }
    }
    run.tool_events.push(value);
}

fn tool_event_payload_matches(left: &Value, right: &Value) -> bool {
    match (
        tool_event_provider_call_id(left),
        tool_event_provider_call_id(right),
    ) {
        (Some(left_id), Some(right_id)) => return left_id == right_id,
        (Some(_), None) | (None, Some(_)) => return false,
        (None, None) => {}
    }
    match (
        left.get("raw").and_then(|raw| raw.get("payload")),
        right.get("raw").and_then(|raw| raw.get("payload")),
    ) {
        (Some(left_payload), Some(right_payload)) => left_payload == right_payload,
        _ => true,
    }
}

fn tool_event_provider_call_id(event: &Value) -> Option<&str> {
    if let Some(call_id) = event
        .get("callId")
        .or_else(|| event.get("call_id"))
        .and_then(Value::as_str)
        .filter(|call_id| !call_id.trim().is_empty())
    {
        return Some(call_id);
    }
    event
        .get("raw")
        .and_then(|raw| raw.get("payload"))
        .and_then(|payload| payload.get(super::decision_parser::PROVIDER_TOOL_CALL_META_KEY))
        .and_then(|metadata| {
            metadata
                .get("id")
                .or_else(|| metadata.get("call_id"))
                .or_else(|| metadata.get("tool_call_id"))
                .or_else(|| metadata.get("toolCallId"))
        })
        .and_then(Value::as_str)
}

pub(super) fn tool_started_event(
    run_id: &str,
    server_id: &str,
    tool_name: &str,
    payload: &Value,
) -> ToolEvent {
    let path = payload_path_for_tool_event(payload);
    ToolEvent {
        status: Some("running".into()),
        reference_id: None,
        call_id: Some(provider_tool_call_id(payload).unwrap_or_else(|| new_id("call"))),
        run_id: Some(run_id.to_string()),
        checkpoint_id: None,
        event_type: if server_id == "__internal" {
            "internal_tool".into()
        } else {
            "mcp_tool".into()
        },
        server_id: server_id.to_string(),
        tool_name: tool_name.to_string(),
        ok: true,
        timed_out: false,
        elapsed_ms: 0,
        kind: tool_event_kind(server_id, tool_name, None),
        title: format!(
            "{} · {tool_name}",
            if server_id == "__internal" {
                "internal"
            } else {
                server_id
            }
        ),
        summary: "工具调用开始".into(),
        path: path.clone(),
        exists: path.as_deref().map(tool_event_path_exists),
        mime_type: None,
        text: None,
        error: None,
        raw: Some(redact_json_value(json!({"payload": payload}))),
    }
}

pub(super) fn tool_failed_event(
    run_id: &str,
    server_id: &str,
    tool_name: &str,
    payload: &Value,
    error: &str,
) -> ToolEvent {
    let path = payload_path_for_tool_event(payload);
    let error_json = serde_json::from_str::<Value>(error).ok();
    let error = error_json
        .as_ref()
        .and_then(|value| value.get("error"))
        .and_then(Value::as_str)
        .map(redact_sensitive_text)
        .unwrap_or_else(|| redact_sensitive_text(error));
    let needs_reauth = error_json
        .as_ref()
        .and_then(|value| {
            value
                .get("needsReauth")
                .or_else(|| value.get("needs_reauth"))
                .and_then(Value::as_bool)
        })
        .unwrap_or(false);
    let raw = if let Some(error_json) = error_json {
        json!({
            "payload": payload,
            "error": error,
            "errorJson": error_json,
            "needsReauth": needs_reauth
        })
    } else {
        json!({"payload": payload, "error": error})
    };
    let canceled = tool_error_is_run_inactive(&error);
    let display_error = if canceled {
        "运行已结束，工具已取消".to_string()
    } else {
        error.clone()
    };
    ToolEvent {
        status: Some(if canceled { "canceled" } else { "failed" }.into()),
        reference_id: None,
        call_id: Some(provider_tool_call_id(payload).unwrap_or_else(|| new_id("call"))),
        run_id: Some(run_id.to_string()),
        checkpoint_id: None,
        event_type: if server_id == "__internal" {
            "internal_tool".into()
        } else {
            "mcp_tool".into()
        },
        server_id: server_id.to_string(),
        tool_name: tool_name.to_string(),
        ok: false,
        timed_out: false,
        elapsed_ms: 0,
        kind: tool_event_kind(server_id, tool_name, None),
        title: format!(
            "{} · {tool_name}",
            if server_id == "__internal" {
                "internal"
            } else {
                server_id
            }
        ),
        summary: display_error.clone(),
        path: path.clone(),
        exists: path.as_deref().map(tool_event_path_exists),
        mime_type: Some("text/plain".into()),
        text: None,
        error: Some(display_error),
        raw: Some(redact_json_value(raw)),
    }
}

fn payload_path_for_tool_event(payload: &Value) -> Option<String> {
    payload
        .get("path")
        .or_else(|| payload.get("dst"))
        .or_else(|| payload.get("target"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn tool_event_path_exists(path: &str) -> bool {
    Path::new(path).exists()
}

fn normalize_tool_event_for_display(event: &mut ToolEvent) {
    if event.status.as_deref() == Some("failed")
        && event
            .error
            .as_deref()
            .is_some_and(tool_error_is_run_inactive)
    {
        event.status = Some("canceled".into());
        event.summary = "运行已结束，工具已取消".into();
        event.error = Some(event.summary.clone());
        event.ok = false;
    }
}

fn tool_error_is_run_inactive(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    lower.contains("agent run is no longer active")
        || lower.contains("agent run was stopped")
        || lower.contains("is already terminal")
        || lower.contains("agent run ended")
        || lower.contains("run ended")
}

fn terminal_tool_event_summary_for_run_state(state: &str) -> &'static str {
    match state {
        "completed" => "运行已完成",
        "aborted" => "运行已取消",
        _ => "运行已结束",
    }
}

fn tool_canceled_event_from_running(event: &ToolEvent, summary: &str) -> ToolEvent {
    let mut event = event.clone();
    event.status = Some("canceled".into());
    event.ok = false;
    event.summary = summary.into();
    event.error = Some(summary.into());
    event
}

pub(super) fn append_planner_trace(
    store: &AppStore,
    run_id: &str,
    conversation_id: &str,
    persona_id: &str,
    agent_id: &str,
    iteration: u32,
    input: &str,
    output: &str,
    decision: &Value,
) -> AppResult<PlannerTraceRecord> {
    store.append_planner_trace(PlannerTraceRecord {
        id: new_id("plan"),
        run_id: run_id.to_string(),
        conversation_id: conversation_id.to_string(),
        persona_id: persona_id.to_string(),
        agent_id: agent_id.to_string(),
        iteration,
        created_at: now_iso(),
        input: redact_sensitive_text(input),
        output: redact_sensitive_text(output),
        parsed_step: summarize_planner_step(decision),
        error: planner_decision_error(decision),
    })
}
