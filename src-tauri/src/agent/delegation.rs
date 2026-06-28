use std::sync::{Mutex, OnceLock};

use futures::future::join_all;
use serde_json::{json, Value};

use crate::{
    error::{AppError, AppResult},
    models::{
        new_id, now_iso, AgentDefinition, AgentRunRecord, ChatMessage, SendChatRequest, ToolEvent,
    },
    store::AppStore,
};

use super::{
    acp_events::acp_tool_event_kind,
    acp_server::{acp_json_rpc_error, acp_json_rpc_response},
    acp_session::latest_run_record_for_session,
    append_parent_phase_event, normalize_toolset_name, payload_string_array,
    push_tool_event_record, redact_json_value, redact_sensitive_text,
    run_chat_turn_with_toolset_policy_and_iteration_limit,
};

#[cfg(test)]
use super::acp_prompt::acp_idle_steer_prompt_text;

#[cfg(test)]
use super::acp_queue::acp_queue_update_for_current_state;

#[cfg(test)]
use super::acp_auth::{
    acp_authenticate_result_from_methods, acp_terminal_setup_auth_method,
    ACP_TERMINAL_SETUP_AUTH_METHOD_ID,
};

pub(super) use super::acp_commands::{
    acp_compact_text_for_prompt, acp_context_text_for_prompt, acp_help_text_for_prompt,
    acp_local_command_reply_for_prompt, acp_model_text_for_prompt, acp_reset_text_for_prompt,
    acp_tools_text_for_prompt, acp_version_text_for_prompt,
};
pub(super) use super::acp_events::acp_final_agent_message_notifications;
pub(super) use super::acp_events::acp_tool_event_update_from_value;
pub(super) use super::acp_prompt::acp_prompt_provider_data_from_params;
pub(super) use super::acp_prompt_runtime::acp_emit_new_tool_notifications;
pub(super) use super::acp_session::{
    acp_list_sessions_for_store, acp_take_interrupted_prompt_text,
};
pub(super) use super::acp_subprocess::{
    acp_closed_error, acp_delegate_error_implies_aborted, acp_delegate_was_aborted,
    acp_handle_server_message, acp_observer_aborted, acp_rpc_request, acp_timeout_error,
    append_acp_observer_phase, run_acp_prompt, AcpRunObserver,
};

static DELEGATION_SPAWN_PAUSED: OnceLock<Mutex<bool>> = OnceLock::new();

pub(super) use super::acp_child_events::{
    acp_session_update_kind, acp_session_update_record, acp_session_update_text,
    append_acp_tool_event_record,
};
pub(super) use super::acp_client::{
    acp_prompt_result_error, acp_prompt_result_is_cancelled, acp_prompt_result_stop_reason,
    acp_session_cancel_request, acp_session_start_request,
};
pub(super) use super::acp_client_fs::{
    acp_path_within_cwd, acp_read_text_file_response, acp_sensitive_path_reason,
    acp_write_text_file_response,
};
pub(super) use super::acp_tool_output::acp_string_text;
use super::delegation_acp::execute_acp_delegate_task_request;
pub(super) use super::delegation_artifacts::{
    append_delegation_memory_observation, append_diagnostic_artifact_to_error,
    delegation_file_state_reminder, save_subagent_failure_diagnostic_artifact,
};
pub(super) use super::delegation_request::{
    apply_delegation_iteration_budget, apply_delegation_runtime_config, delegate_task_requests,
    DelegateTaskRequest,
};
use super::delegation_run_state::{latest_run_for_conversation, mark_run_as_subagent};
pub(super) use super::delegation_scope::{acp_mcp_servers_for_agent, delegation_child_toolsets};
use super::delegation_synthchat::execute_synthchat_delegate_task_request;

pub(super) async fn delegate_task_tool(
    store: &AppStore,
    agent: &AgentDefinition,
    _conversation_id: &str,
    parent_run_id: &str,
    payload: &Value,
) -> AppResult<String> {
    let is_batch = payload.get("tasks").is_some();
    let mut requests = delegate_task_requests(payload)?;
    let chat_config = store.config()?.chat;
    apply_delegation_iteration_budget(&mut requests, agent.max_tool_iterations);
    let max_concurrent_children = chat_config.delegation_max_concurrent_children.max(1);
    if is_batch && requests.len() as u32 > max_concurrent_children {
        return Err(AppError::BadRequest(format!(
            "delegate_task requested {} concurrent children, but delegationMaxConcurrentChildren is {}",
            requests.len(),
            max_concurrent_children
        )));
    }
    apply_delegation_runtime_config(&mut requests, chat_config.delegation_orchestrator_enabled);
    if delegation_spawn_paused() {
        return Err(AppError::BadRequest(
            "delegate_task spawning is paused. Use /subagents resume before retrying.".into(),
        ));
    }
    let parent = ensure_parent_run_accepts_delegation(store, parent_run_id)?;
    let parent_depth = parent.subagent_depth.unwrap_or(0);
    if parent_depth >= agent.max_subagent_depth {
        return Err(AppError::BadRequest(format!(
            "delegate_task depth limit reached: {}",
            agent.max_subagent_depth
        )));
    }
    let child_count = store
        .agent_runs()?
        .into_iter()
        .filter(|run| run.parent_run_id.as_deref() == Some(parent_run_id))
        .count() as u32;
    if child_count + requests.len() as u32 > agent.max_subagents {
        return Err(AppError::BadRequest(format!(
            "delegate_task subagent limit reached: {} existing + {} requested exceeds {}",
            child_count,
            requests.len(),
            agent.max_subagents
        )));
    }

    let delegation_started_at = now_iso();
    let results = if is_batch {
        let futures = requests.iter().enumerate().map(|(offset, request)| {
            let child_index = child_count + offset as u32 + 1;
            execute_delegate_task_request(
                store,
                agent,
                parent_run_id,
                parent_depth,
                child_index,
                request,
                &chat_config.delegation_subagent_provider_id,
                &chat_config.delegation_subagent_model,
                chat_config.delegation_subagent_auto_approve,
                chat_config.delegation_inherit_mcp_toolsets,
            )
        });
        let resolved = join_all(futures).await;
        let mut results = Vec::with_capacity(resolved.len());
        for result in resolved {
            results.push(result?);
        }
        results
    } else {
        vec![
            execute_delegate_task_request(
                store,
                agent,
                parent_run_id,
                parent_depth,
                child_count + 1,
                requests.first().ok_or_else(|| {
                    AppError::BadRequest("delegate_task produced no request".into())
                })?,
                &chat_config.delegation_subagent_provider_id,
                &chat_config.delegation_subagent_model,
                chat_config.delegation_subagent_auto_approve,
                chat_config.delegation_inherit_mcp_toolsets,
            )
            .await?,
        ]
    };
    if !is_batch {
        let result = results
            .into_iter()
            .next()
            .ok_or_else(|| AppError::BadRequest("delegate_task produced no result".into()))?;
        if result.get("status").and_then(Value::as_str) == Some("failed") {
            return Err(AppError::BadRequest(
                result
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("delegate_task failed")
                    .to_string(),
            ));
        }
        let mut response = json!({
            "childRunId": result["childRunId"],
            "role": result["role"],
            "task": result["task"],
            "maxIterations": result["maxIterations"],
            "result": result["result"]
        });
        if let Some(reminder) =
            delegation_file_state_reminder(store, parent_run_id, &delegation_started_at)?
        {
            response["fileStateReminder"] = reminder;
        }
        return Ok(serde_json::to_string_pretty(&response)?);
    }
    let ok = results
        .iter()
        .all(|result| result.get("status").and_then(Value::as_str) == Some("completed"));
    let mut response = json!({
        "ok": ok,
        "count": results.len(),
        "results": results
    });
    if let Some(reminder) =
        delegation_file_state_reminder(store, parent_run_id, &delegation_started_at)?
    {
        response["fileStateReminder"] = reminder;
    }
    Ok(serde_json::to_string_pretty(&response)?)
}

pub(super) fn ensure_parent_run_accepts_delegation(
    store: &AppStore,
    parent_run_id: &str,
) -> AppResult<AgentRunRecord> {
    let parent = store.agent_run(parent_run_id)?;
    if matches!(parent.state.as_str(), "completed" | "failed" | "aborted") {
        return Err(AppError::BadRequest(format!(
            "parent agent run {parent_run_id} is already terminal: {}",
            parent.state
        )));
    }
    Ok(parent)
}

pub(super) fn delegation_spawn_paused() -> bool {
    DELEGATION_SPAWN_PAUSED
        .get_or_init(|| Mutex::new(false))
        .lock()
        .map(|guard| *guard)
        .unwrap_or(false)
}

pub(super) fn set_delegation_spawn_paused(paused: bool) -> bool {
    let lock = DELEGATION_SPAWN_PAUSED.get_or_init(|| Mutex::new(false));
    match lock.lock() {
        Ok(mut guard) => {
            let previous = *guard;
            *guard = paused;
            previous
        }
        Err(_) => false,
    }
}

async fn execute_delegate_task_request(
    store: &AppStore,
    agent: &AgentDefinition,
    parent_run_id: &str,
    parent_depth: u32,
    child_index: u32,
    request: &DelegateTaskRequest,
    provider_id_override: &str,
    model_override: &str,
    subagent_auto_approve: bool,
    inherit_mcp_toolsets: bool,
) -> AppResult<Value> {
    if !request.acp_command.is_empty() {
        return execute_acp_delegate_task_request(
            store,
            agent,
            parent_run_id,
            parent_depth,
            child_index,
            request,
            subagent_auto_approve,
            inherit_mcp_toolsets,
        )
        .await;
    }
    execute_synthchat_delegate_task_request(
        store,
        agent,
        parent_run_id,
        parent_depth,
        child_index,
        request,
        provider_id_override,
        model_override,
        subagent_auto_approve,
        inherit_mcp_toolsets,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::super::acp_history::acp_session_history_updates;
    use super::*;
    use std::fs;

    #[test]
    fn acp_live_tool_event_maps_cancelled_to_completed_with_prefix() {
        let update = acp_tool_event_update_from_value(&json!({
            "toolName": "todo",
            "callId": "tc-todo",
            "status": "cancelled",
            "text": "Drop stale task",
            "raw": {
                "payload": {
                    "todos": [{"id": "old", "content": "Drop stale task", "status": "cancelled"}]
                }
            }
        }))
        .unwrap();

        assert_eq!(update["sessionUpdate"], "tool_call_update");
        assert_eq!(update["status"], "completed");
        assert_eq!(update["rawOutput"], "[cancelled] Drop stale task");
        assert_eq!(
            update["content"][0]["content"]["text"],
            "[cancelled] Drop stale task"
        );
    }

    #[test]
    fn acp_history_replay_maps_cancelled_tool_result_to_completed() {
        let messages = vec![
            ChatMessage {
                id: "assistant".into(),
                conversation_id: "conv".into(),
                role: "assistant".into(),
                content: json!({
                    "tool_calls": [{
                        "id": "tc-todo",
                        "type": "function",
                        "function": {
                            "name": "todo",
                            "arguments": {"todos": [{"id": "old", "content": "Drop stale task"}]}
                        }
                    }]
                })
                .to_string(),
                created_at: "2026-06-05T00:00:00Z".into(),
                source: "test".into(),
                account_id: None,
                provider_data: None,
            },
            ChatMessage {
                id: "tool".into(),
                conversation_id: "conv".into(),
                role: "tool".into(),
                content: json!({
                    "type": "toolEvent",
                    "event": {
                        "toolName": "todo",
                        "callId": "tc-todo",
                        "status": "cancelled",
                        "text": "Drop stale task"
                    }
                })
                .to_string(),
                created_at: "2026-06-05T00:00:01Z".into(),
                source: "test".into(),
                account_id: None,
                provider_data: None,
            },
        ];

        let updates = acp_session_history_updates(&messages);
        let completion = updates
            .iter()
            .find(|update| update["sessionUpdate"] == "tool_call_update")
            .unwrap();
        assert_eq!(completion["status"], "completed");
        assert_eq!(completion["rawOutput"], "[cancelled] Drop stale task");
    }

    #[test]
    fn acp_idle_steer_prompt_text_rewrites_guidance_only() {
        assert_eq!(
            acp_idle_steer_prompt_text("/steer prefer smaller steps"),
            Some("prefer smaller steps".into())
        );
        assert_eq!(
            acp_idle_steer_prompt_text("／steer use the queue state"),
            Some("use the queue state".into())
        );
        assert!(acp_idle_steer_prompt_text("/steering wheel").is_none());
        assert!(acp_idle_steer_prompt_text("/steer").is_none());
    }

    #[test]
    fn acp_queue_update_reflects_running_and_completed_states() {
        let dir = std::env::temp_dir().join(format!("synthchat-acp-queue-{}", new_id("test")));
        fs::create_dir_all(&dir).unwrap();
        let store = AppStore::new(dir.join("state.json")).unwrap();
        let persona = store.persona(None).unwrap();
        let conversation = store
            .create_conversation(Some("ACP Queue".into()), Some(persona.id.clone()))
            .unwrap();
        let message = ChatMessage::new(
            conversation.id.clone(),
            "user",
            "queued follow-up".into(),
            "test",
        );
        let item = store
            .enqueue_agent_request(conversation.id.clone(), persona.id.clone(), &message)
            .unwrap();
        let claimed = store
            .claim_next_agent_request(&conversation.id)
            .unwrap()
            .unwrap();

        let running =
            acp_queue_update_for_current_state(&store, &conversation.id, &claimed).unwrap();
        assert_eq!(running["params"]["update"]["sessionUpdate"], "queue_update");
        assert_eq!(running["params"]["update"]["queueId"], item.id);
        assert_eq!(running["params"]["update"]["status"], "running");
        assert_eq!(running["params"]["update"]["pendingCount"], 0);

        let completed = store
            .complete_agent_queue_item(&item.id, "completed", None)
            .unwrap()
            .unwrap();
        let terminal =
            acp_queue_update_for_current_state(&store, &conversation.id, &completed).unwrap();
        assert_eq!(terminal["params"]["update"]["status"], "completed");
        assert_eq!(
            terminal["params"]["update"]["content"]["text"],
            "queued follow-up"
        );

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn acp_terminal_setup_auth_method_matches_acp_terminal_shape() {
        let method = acp_terminal_setup_auth_method();
        assert_eq!(method["id"], ACP_TERMINAL_SETUP_AUTH_METHOD_ID);
        assert_eq!(method["type"], "terminal");
        assert_eq!(method["args"], json!(["--setup"]));
        assert!(method["name"]
            .as_str()
            .unwrap()
            .contains("Configure SynthChat provider"));
    }

    #[test]
    fn acp_setup_authenticate_requires_provider_method_after_setup() {
        let setup = acp_terminal_setup_auth_method();
        assert_eq!(
            acp_authenticate_result_from_methods(ACP_TERMINAL_SETUP_AUTH_METHOD_ID, &[setup]),
            Value::Null
        );

        let setup = acp_terminal_setup_auth_method();
        let provider = json!({"id": "openrouter", "name": "openrouter runtime credentials"});
        assert_eq!(
            acp_authenticate_result_from_methods(
                ACP_TERMINAL_SETUP_AUTH_METHOD_ID,
                &[provider, setup]
            ),
            json!({})
        );
    }
}
