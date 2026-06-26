use serde_json::{json, Value};

use crate::{error::AppResult, mcp, models::SendChatRequest, store::AppStore};

use super::{
    acp_auth::{acp_auth_methods_for_store, acp_server_authenticate},
    acp_commands::acp_local_command_reply_for_prompt,
    acp_events::{
        acp_agent_message_notification, acp_final_agent_message_notifications,
        acp_user_message_notification,
    },
    acp_prompt::{
        acp_prompt_is_local_queue_command, acp_prompt_provider_data_from_params,
        acp_prompt_text_from_params, acp_prompt_usage_delta,
    },
    acp_prompt_runtime::{
        acp_prompt_result_for_latest_run, acp_prompt_tool_notifications, acp_run_chat_turn,
        acp_run_chat_turn_with_live_tool_notifications,
    },
    acp_queue::{
        acp_prompt_queue_notifications, acp_queue_ids_for_session, acp_queue_preflight_for_prompt,
        acp_queue_update_for_current_state, AcpPromptQueuePreflight,
    },
    acp_session::{
        acp_list_sessions_for_store, acp_server_cancel_session, acp_server_fork_session,
        acp_server_load_session, acp_server_new_session, acp_server_resume_session,
        acp_server_set_session_config_option, acp_server_set_session_mode,
        acp_server_set_session_model, acp_session_id_from_params, acp_session_info_notification,
        acp_session_mcp_server_id_prefix, acp_session_status_notifications_for_params,
        acp_session_status_notifications_for_result, acp_usage_notification,
    },
    now_iso, AcpNotificationSink,
};

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct AcpSessionInfo {
    pub session_id: String,
    pub cwd: String,
    pub title: String,
    pub updated_at: String,
    pub model: String,
    pub history_len: usize,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct AcpListSessionsResponse {
    pub sessions: Vec<AcpSessionInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct AcpServerHandleResult {
    pub response: Value,
    pub notifications: Vec<Value>,
}

pub(super) fn acp_json_rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message
        }
    })
}

pub(super) fn acp_json_rpc_response(id: Value, result: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result
    })
}

pub(super) fn acp_server_initialize_result(store: &AppStore) -> AppResult<Value> {
    let auth_methods = acp_auth_methods_for_store(store)?;
    Ok(json!({
        "protocolVersion": 1,
        "agentInfo": {
            "name": "synthchat-agent",
            "version": env!("CARGO_PKG_VERSION")
        },
        "agentCapabilities": {
            "loadSession": true,
            "promptCapabilities": {
                "image": true
            },
            "sessionCapabilities": {
                "fork": {},
                "list": {},
                "resume": {}
            }
        },
        "authMethods": auth_methods
    }))
}

pub(super) fn acp_server_handle_json_rpc(
    store: &AppStore,
    request: &Value,
) -> AppResult<AcpServerHandleResult> {
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let Some(method) = request.get("method").and_then(Value::as_str) else {
        return Ok(AcpServerHandleResult {
            response: acp_json_rpc_error(id, -32600, "ACP request missing method"),
            notifications: Vec::new(),
        });
    };
    let params = request.get("params").cloned().unwrap_or_else(|| json!({}));
    let (result, notifications) = match method {
        "initialize" => (acp_server_initialize_result(store)?, Vec::new()),
        "authenticate" | "session/authenticate" => {
            (acp_server_authenticate(store, &params)?, Vec::new())
        }
        "session/new" | "new_session" => {
            let result = acp_server_new_session(store, &params)?;
            let notifications = acp_session_status_notifications_for_result(store, &result)?;
            (result, notifications)
        }
        "list_sessions" | "session/list" | "session/list_sessions" => {
            let cursor = params
                .get("cursor")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty());
            let cwd = params
                .get("cwd")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty());
            (
                serde_json::to_value(acp_list_sessions_for_store(store, cwd, cursor)?)?,
                Vec::new(),
            )
        }
        "session/load" | "load_session" => acp_server_load_session(store, &params)?,
        "session/resume" | "resume_session" => acp_server_resume_session(store, &params)?,
        "session/fork" | "fork_session" => {
            let result = acp_server_fork_session(store, &params)?;
            let notifications = acp_session_status_notifications_for_result(store, &result)?;
            (result, notifications)
        }
        "session/set_model" | "set_session_model" => {
            let result = acp_server_set_session_model(store, &params)?;
            let notifications = acp_session_status_notifications_for_params(store, &params)?;
            (result, notifications)
        }
        "session/set_mode" | "set_session_mode" => {
            let result = acp_server_set_session_mode(store, &params)?;
            let notifications = acp_session_status_notifications_for_params(store, &params)?;
            (result, notifications)
        }
        "session/set_config_option" | "set_config_option" => {
            let result = acp_server_set_session_config_option(store, &params)?;
            let notifications = acp_session_status_notifications_for_params(store, &params)?;
            (result, notifications)
        }
        "session/cancel" | "cancel" => acp_server_cancel_session(store, &params)?,
        _ => {
            return Ok(AcpServerHandleResult {
                response: acp_json_rpc_error(
                    id,
                    -32601,
                    &format!("ACP server method '{method}' is not supported by SynthChat yet."),
                ),
                notifications: Vec::new(),
            });
        }
    };
    Ok(AcpServerHandleResult {
        response: acp_json_rpc_response(id, result),
        notifications,
    })
}

pub(super) async fn acp_server_handle_json_rpc_async(
    store: &AppStore,
    request: &Value,
) -> AppResult<AcpServerHandleResult> {
    acp_server_handle_json_rpc_async_with_sink(store, request, None).await
}

pub(super) async fn acp_server_handle_json_rpc_async_with_sink(
    store: &AppStore,
    request: &Value,
    notification_sink: Option<AcpNotificationSink>,
) -> AppResult<AcpServerHandleResult> {
    let method = request.get("method").and_then(Value::as_str).unwrap_or("");
    if matches!(method, "prompt" | "session/prompt") {
        return acp_server_prompt(store, request, notification_sink).await;
    }
    let handled = acp_server_handle_json_rpc(store, request)?;
    if matches!(
        method,
        "session/new"
            | "new_session"
            | "session/load"
            | "load_session"
            | "session/resume"
            | "resume_session"
            | "session/fork"
            | "fork_session"
    ) {
        acp_refresh_session_mcp_tools_for_result(store, &handled.response).await?;
    }
    Ok(handled)
}

async fn acp_refresh_session_mcp_tools_for_result(
    store: &AppStore,
    response: &Value,
) -> AppResult<()> {
    let session_id = response
        .get("result")
        .and_then(|result| result.get("sessionId"))
        .and_then(Value::as_str)
        .unwrap_or("");
    if session_id.is_empty() {
        return Ok(());
    }
    let prefix = acp_session_mcp_server_id_prefix(session_id);
    let server_ids = store
        .static_list("mcpServers")?
        .into_iter()
        .filter_map(|server| {
            let id = server.get("id").and_then(Value::as_str)?.to_string();
            if id.starts_with(&prefix) {
                Some(id)
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    for server_id in server_ids {
        let _ = mcp::list_tools(store, server_id, Some(5)).await;
    }
    Ok(())
}
async fn acp_server_prompt(
    store: &AppStore,
    request: &Value,
    notification_sink: Option<AcpNotificationSink>,
) -> AppResult<AcpServerHandleResult> {
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let params = request.get("params").cloned().unwrap_or_else(|| json!({}));
    let session_id = acp_session_id_from_params(&params);
    if session_id.is_empty() || store.conversation(&session_id).is_err() {
        return Ok(AcpServerHandleResult {
            response: acp_json_rpc_response(id, json!({"stopReason": "refusal"})),
            notifications: Vec::new(),
        });
    }
    let active_before = store.active_agent_run_for_conversation(&session_id)?;
    let mut prompt_text = acp_prompt_text_from_params(&params);
    match acp_queue_preflight_for_prompt(store, &session_id, prompt_text, active_before.as_ref())? {
        AcpPromptQueuePreflight::Continue {
            prompt_text: rewritten,
        } => {
            prompt_text = rewritten;
        }
        AcpPromptQueuePreflight::EndTurn {
            notifications: preflight_notifications,
            include_usage,
        } => {
            let mut notifications = Vec::new();
            acp_emit_or_collect_many(
                notification_sink.as_ref(),
                &mut notifications,
                preflight_notifications,
            )?;
            if include_usage {
                acp_emit_or_collect_many(
                    notification_sink.as_ref(),
                    &mut notifications,
                    acp_usage_notification(store, &session_id)?,
                )?;
            }
            return Ok(AcpServerHandleResult {
                response: acp_json_rpc_response(id, json!({"stopReason": "end_turn"})),
                notifications,
            });
        }
    }
    if prompt_text.trim().is_empty() {
        return Ok(AcpServerHandleResult {
            response: acp_json_rpc_response(id, json!({"stopReason": "end_turn"})),
            notifications: Vec::new(),
        });
    }
    if let Some(command_reply) =
        acp_local_command_reply_for_prompt(store, &session_id, &prompt_text).await?
    {
        let mut notifications = Vec::new();
        acp_emit_or_collect(
            notification_sink.as_ref(),
            &mut notifications,
            acp_agent_message_notification(&session_id, &command_reply.text),
        )?;
        if command_reply.include_usage {
            acp_emit_or_collect_many(
                notification_sink.as_ref(),
                &mut notifications,
                acp_usage_notification(store, &session_id)?,
            )?;
        }
        if command_reply.include_session_info {
            acp_emit_or_collect_many(
                notification_sink.as_ref(),
                &mut notifications,
                acp_session_info_notification(store, &session_id)?,
            )?;
        }
        return Ok(AcpServerHandleResult {
            response: acp_json_rpc_response(id, json!({"stopReason": "end_turn"})),
            notifications,
        });
    }
    let should_drain_after_prompt = !acp_prompt_is_local_queue_command(&prompt_text);
    let queue_before = acp_queue_ids_for_session(store, &session_id)?;
    let usage_before = store.token_usage()?;
    let tool_events_before = active_before
        .as_ref()
        .map(|run| (run.run_id.clone(), run.tool_events.len()));
    let chat_request = SendChatRequest {
        conversation_id: Some(session_id.clone()),
        persona_id: None,
        agent_id: None,
        content: prompt_text,
        provider_data: acp_prompt_provider_data_from_params(&params),
        queue_item_id: None,
    };
    let (messages, final_tool_events_before, streamed_agent_message) =
        if let Some(sink) = notification_sink.as_ref() {
            acp_run_chat_turn_with_live_tool_notifications(
                store,
                chat_request,
                &session_id,
                tool_events_before.as_ref(),
                sink,
            )
            .await?
        } else {
            (
                acp_run_chat_turn(store, chat_request, &session_id).await?,
                tool_events_before,
                None,
            )
        };
    let mut notifications =
        acp_prompt_queue_notifications(store, &session_id, active_before.as_ref(), &queue_before)?;
    if notification_sink.is_none() {
        notifications.extend(acp_prompt_tool_notifications(
            store,
            &session_id,
            final_tool_events_before.as_ref(),
        )?);
    }
    notifications.extend(acp_usage_notification(store, &session_id)?);
    notifications.extend(acp_session_info_notification(store, &session_id)?);
    let agent_message_notifications = acp_final_agent_message_notifications(
        &session_id,
        messages,
        streamed_agent_message.as_deref(),
    );
    if let Some(sink) = notification_sink.as_ref() {
        for notification in agent_message_notifications {
            sink(notification)?;
        }
    } else {
        notifications.extend(agent_message_notifications);
    }
    let mut result = acp_prompt_result_for_latest_run(store, &session_id)?;
    if let Some(usage) = acp_prompt_usage_delta(&usage_before, &store.token_usage()?) {
        result["usage"] = usage;
    }
    if store
        .active_agent_run_for_conversation(&session_id)?
        .is_none()
        && should_drain_after_prompt
    {
        acp_drain_session_queue_after_prompt(
            store,
            &session_id,
            notification_sink.as_ref(),
            &mut notifications,
        )
        .await?;
    }
    Ok(AcpServerHandleResult {
        response: acp_json_rpc_response(id, result),
        notifications,
    })
}

async fn acp_drain_session_queue_after_prompt(
    store: &AppStore,
    session_id: &str,
    notification_sink: Option<&AcpNotificationSink>,
    notifications: &mut Vec<Value>,
) -> AppResult<()> {
    while let Some(item) = store.claim_next_agent_request(session_id)? {
        acp_emit_or_collect(
            notification_sink,
            notifications,
            acp_queue_update_for_current_state(store, session_id, &item)?,
        )?;
        acp_emit_or_collect(
            notification_sink,
            notifications,
            acp_user_message_notification(session_id, &item.content),
        )?;
        let tool_events_before = store
            .active_agent_run_for_conversation(session_id)?
            .as_ref()
            .map(|run| (run.run_id.clone(), run.tool_events.len()));
        let request = SendChatRequest {
            conversation_id: Some(item.conversation_id.clone()),
            persona_id: Some(item.persona_id.clone()),
            agent_id: None,
            content: item.content.clone(),
            provider_data: item.request_provider_data(),
            queue_item_id: Some(item.id.clone()),
        };
        let run_result = if let Some(sink) = notification_sink {
            acp_run_chat_turn_with_live_tool_notifications(
                store,
                request,
                session_id,
                tool_events_before.as_ref(),
                sink,
            )
            .await
            .map(|(messages, _, streamed)| (messages, streamed))
        } else {
            acp_run_chat_turn(store, request, session_id)
                .await
                .map(|messages| (messages, None))
        };
        let (status, error, messages, streamed_agent_message) = match run_result {
            Ok((messages, streamed)) => {
                crate::wechat_settings::finalize_queued_wechat_turn(
                    store,
                    &messages,
                    item.provider_data.as_ref(),
                    item.started_at.as_deref(),
                )
                .await?;
                ("completed", None, messages, streamed)
            }
            Err(error) => {
                let error_text = error.to_string();
                ("failed", Some(error_text), Vec::new(), None)
            }
        };
        let completed = store
            .complete_agent_queue_item(&item.id, status, error.clone())?
            .unwrap_or_else(|| {
                let mut fallback = item.clone();
                fallback.status = status.into();
                fallback.error = error.clone();
                fallback.updated_at = now_iso();
                fallback.completed_at = Some(now_iso());
                fallback
            });
        acp_emit_or_collect(
            notification_sink,
            notifications,
            acp_queue_update_for_current_state(store, session_id, &completed)?,
        )?;
        if notification_sink.is_none() {
            notifications.extend(acp_prompt_tool_notifications(store, session_id, None)?);
        }
        for notification in acp_final_agent_message_notifications(
            session_id,
            messages,
            streamed_agent_message.as_deref(),
        ) {
            acp_emit_or_collect(notification_sink, notifications, notification)?;
        }
        acp_emit_or_collect_many(
            notification_sink,
            notifications,
            acp_usage_notification(store, session_id)?,
        )?;
        acp_emit_or_collect_many(
            notification_sink,
            notifications,
            acp_session_info_notification(store, session_id)?,
        )?;
        if status == "failed" {
            break;
        }
    }
    Ok(())
}
fn acp_emit_or_collect(
    notification_sink: Option<&AcpNotificationSink>,
    notifications: &mut Vec<Value>,
    notification: Value,
) -> AppResult<()> {
    if let Some(sink) = notification_sink {
        sink(notification)
    } else {
        notifications.push(notification);
        Ok(())
    }
}

fn acp_emit_or_collect_many(
    notification_sink: Option<&AcpNotificationSink>,
    notifications: &mut Vec<Value>,
    items: Vec<Value>,
) -> AppResult<()> {
    for notification in items {
        acp_emit_or_collect(notification_sink, notifications, notification)?;
    }
    Ok(())
}
