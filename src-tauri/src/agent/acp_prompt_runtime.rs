use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use serde_json::{json, Value};

use crate::{
    error::AppResult,
    models::{ChatMessage, SendChatRequest},
    store::AppStore,
};

use super::{
    acp_events::{acp_agent_message_notification, acp_tool_event_notifications},
    acp_session::{acp_session_runtime_config_for_store, latest_run_record_for_session},
    run_chat_turn_with_toolset_policy_and_iteration_limit, AcpNotificationSink,
};

pub(super) async fn acp_run_chat_turn_with_live_tool_notifications(
    store: &AppStore,
    request: SendChatRequest,
    session_id: &str,
    previous_active_tool_events: Option<&(String, usize)>,
    sink: &AcpNotificationSink,
) -> AppResult<(Vec<ChatMessage>, Option<(String, usize)>, Option<String>)> {
    let mut observed_tool_events = previous_active_tool_events.cloned();
    let streamed_agent_message = Arc::new(Mutex::new(String::new()));
    let callback_sink = Arc::clone(sink);
    let callback_session_id = session_id.to_string();
    let callback_streamed_text = Arc::clone(&streamed_agent_message);
    let stream_delta_callback: crate::llm::LlmDeltaCallback = Arc::new(move |kind, delta| {
        if kind != crate::llm::LlmStreamDeltaKind::Answer {
            return Ok(());
        }
        if delta.trim().is_empty() {
            return Ok(());
        }
        if let Ok(mut text) = callback_streamed_text.lock() {
            text.push_str(delta);
        }
        callback_sink(acp_agent_message_notification(&callback_session_id, delta))
    });
    let (provider_override, model_override) =
        acp_session_runtime_model_overrides_for_store(store, session_id)?;
    let future = super::acp_session_env::run_with_session_env(
        session_id,
        run_chat_turn_with_toolset_policy_and_iteration_limit(
            store,
            request,
            super::ToolExecutionContext::Interactive,
            None,
            None,
            None,
            provider_override,
            model_override,
            None,
            None,
            None,
            None,
            None,
            Some(stream_delta_callback),
            None,
        ),
    );
    tokio::pin!(future);
    let mut interval = tokio::time::interval(Duration::from_millis(250));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            result = &mut future => {
                let messages = result?;
                observed_tool_events = acp_emit_new_tool_notifications(
                    store,
                    session_id,
                    observed_tool_events.as_ref(),
                    Some(sink),
                )?;
                return Ok((
                    messages,
                    observed_tool_events,
                    streamed_agent_message
                        .lock()
                        .ok()
                        .map(|text| text.clone())
                        .filter(|text| !text.trim().is_empty()),
                ));
            }
            _ = interval.tick() => {
                observed_tool_events = acp_emit_new_tool_notifications(
                    store,
                    session_id,
                    observed_tool_events.as_ref(),
                    Some(sink),
                )?;
            }
        }
    }
}

pub(super) async fn acp_run_chat_turn(
    store: &AppStore,
    request: SendChatRequest,
    session_id: &str,
) -> AppResult<Vec<ChatMessage>> {
    let (provider_override, model_override) =
        acp_session_runtime_model_overrides_for_store(store, session_id)?;
    super::acp_session_env::run_with_session_env(
        session_id,
        run_chat_turn_with_toolset_policy_and_iteration_limit(
            store,
            request,
            super::ToolExecutionContext::Interactive,
            None,
            None,
            None,
            provider_override,
            model_override,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        ),
    )
    .await
}

pub(super) fn acp_emit_new_tool_notifications(
    store: &AppStore,
    session_id: &str,
    previous_tool_events: Option<&(String, usize)>,
    sink: Option<&AcpNotificationSink>,
) -> AppResult<Option<(String, usize)>> {
    let runs = store.agent_runs()?;
    let Some(run) = latest_run_record_for_session(&runs, session_id) else {
        return Ok(previous_tool_events.cloned());
    };
    let start = previous_tool_events
        .filter(|(run_id, _)| run_id == &run.run_id)
        .map(|(_, count)| *count)
        .unwrap_or(0)
        .min(run.tool_events.len());
    if start < run.tool_events.len() {
        if let Some(sink) = sink {
            for notification in acp_tool_event_notifications(session_id, &run.tool_events[start..])
            {
                sink(notification)?;
            }
        }
    }
    Ok(Some((run.run_id.clone(), run.tool_events.len())))
}

pub(super) fn acp_prompt_result_for_latest_run(
    store: &AppStore,
    session_id: &str,
) -> AppResult<Value> {
    let runs = store.agent_runs()?;
    let Some(run) = latest_run_record_for_session(&runs, session_id) else {
        return Ok(json!({"stopReason": "end_turn"}));
    };
    match run.state.as_str() {
        "aborted" => {
            let mut result = json!({"stopReason": "cancelled"});
            if let Some(error) = run
                .error
                .as_deref()
                .filter(|value| !value.trim().is_empty())
            {
                result["error"] = Value::String(error.to_string());
            }
            Ok(result)
        }
        "failed" => {
            let mut result = json!({"stopReason": "refusal"});
            if let Some(error) = run
                .error
                .as_deref()
                .filter(|value| !value.trim().is_empty())
            {
                result["error"] = Value::String(error.to_string());
            }
            Ok(result)
        }
        _ => Ok(json!({"stopReason": "end_turn"})),
    }
}

pub(super) fn acp_prompt_tool_notifications(
    store: &AppStore,
    session_id: &str,
    previous_active_tool_events: Option<&(String, usize)>,
) -> AppResult<Vec<Value>> {
    let runs = store.agent_runs()?;
    let Some(run) = latest_run_record_for_session(&runs, session_id) else {
        return Ok(Vec::new());
    };
    let events = if previous_active_tool_events
        .as_ref()
        .is_some_and(|(run_id, _)| run_id == &run.run_id)
    {
        let start = previous_active_tool_events
            .as_ref()
            .map(|(_, count)| *count)
            .unwrap_or(0)
            .min(run.tool_events.len());
        &run.tool_events[start..]
    } else {
        &run.tool_events
    };
    Ok(acp_tool_event_notifications(session_id, events))
}

fn acp_session_runtime_model_overrides_for_store(
    store: &AppStore,
    session_id: &str,
) -> AppResult<(Option<String>, Option<String>)> {
    let runtime = acp_session_runtime_config_for_store(store, session_id)?;
    Ok(match runtime {
        Some(runtime) => (runtime.provider, runtime.model),
        None => (None, None),
    })
}
