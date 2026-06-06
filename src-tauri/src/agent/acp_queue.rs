use std::collections::HashSet;

use serde_json::{json, Value};

use crate::{
    error::AppResult,
    models::{AgentQueuedRequest, AgentRunRecord},
    store::AppStore,
};

use super::{
    acp_events::acp_agent_message_notification,
    acp_prompt::{acp_idle_steer_prompt_text, acp_queue_prompt_text},
    acp_session::acp_take_interrupted_prompt_text,
};

pub(super) enum AcpPromptQueuePreflight {
    Continue {
        prompt_text: String,
    },
    EndTurn {
        notifications: Vec<Value>,
        include_usage: bool,
    },
}

pub(super) fn acp_queue_preflight_for_prompt(
    store: &AppStore,
    session_id: &str,
    prompt_text: String,
    active_before: Option<&AgentRunRecord>,
) -> AppResult<AcpPromptQueuePreflight> {
    if active_before.is_none() {
        if let Some(steer_text) = acp_idle_steer_prompt_text(&prompt_text) {
            if let Some(interrupted_prompt) = acp_take_interrupted_prompt_text(store, session_id)? {
                return Ok(AcpPromptQueuePreflight::Continue {
                    prompt_text: format!(
                        "{interrupted_prompt}\n\nUser correction/guidance after interrupt: {steer_text}"
                    ),
                });
            }
            let queued = acp_enqueue_prompt_for_session(store, session_id, &steer_text)?;
            let depth = acp_pending_queue_depth(store, session_id)?;
            return Ok(AcpPromptQueuePreflight::EndTurn {
                notifications: vec![
                    acp_queue_update_for_current_state(store, session_id, &queued)?,
                    acp_agent_message_notification(
                        session_id,
                        &format!("No active turn - queued for the next turn. ({depth} queued)"),
                    ),
                ],
                include_usage: false,
            });
        }
    }

    if let Some(steer_text) = acp_idle_steer_prompt_text(&prompt_text) {
        if let Some(active) = active_before {
            store.append_agent_run_steer(&active.run_id, steer_text.clone())?;
            let preview = acp_preview_text(&steer_text, 80);
            return Ok(AcpPromptQueuePreflight::EndTurn {
                notifications: vec![acp_agent_message_notification(
                    session_id,
                    &format!("Steer queued for the active turn: {preview}"),
                )],
                include_usage: true,
            });
        }
    }

    if let Some(queue_text) = acp_queue_prompt_text(&prompt_text) {
        if queue_text.trim().is_empty() {
            return Ok(AcpPromptQueuePreflight::EndTurn {
                notifications: vec![acp_agent_message_notification(
                    session_id,
                    "Usage: /queue <prompt>",
                )],
                include_usage: false,
            });
        }
        let queued = acp_enqueue_prompt_for_session(store, session_id, &queue_text)?;
        let depth = acp_pending_queue_depth(store, session_id)?;
        return Ok(AcpPromptQueuePreflight::EndTurn {
            notifications: vec![
                acp_queue_update_for_current_state(store, session_id, &queued)?,
                acp_agent_message_notification(
                    session_id,
                    &format!("Queued for the next turn. ({depth} queued)"),
                ),
            ],
            include_usage: false,
        });
    }

    Ok(AcpPromptQueuePreflight::Continue { prompt_text })
}

pub(super) fn acp_queue_ids_for_session(
    store: &AppStore,
    session_id: &str,
) -> AppResult<HashSet<String>> {
    Ok(store
        .agent_queue()?
        .into_iter()
        .filter(|item| item.conversation_id == session_id)
        .map(|item| item.id)
        .collect())
}

pub(super) fn acp_prompt_queue_notifications(
    store: &AppStore,
    session_id: &str,
    active_before: Option<&AgentRunRecord>,
    queue_before: &HashSet<String>,
) -> AppResult<Vec<Value>> {
    let queue = store.agent_queue()?;
    let pending_for_session = queue
        .iter()
        .filter(|item| item.conversation_id == session_id && item.status == "pending")
        .collect::<Vec<_>>();
    let mut notifications = Vec::new();
    for item in queue
        .iter()
        .filter(|item| item.conversation_id == session_id && !queue_before.contains(&item.id))
    {
        let position = pending_for_session
            .iter()
            .position(|candidate| candidate.id == item.id)
            .map(|idx| idx + 1);
        notifications.push(acp_queue_update_notification(
            session_id,
            item,
            position,
            pending_for_session.len(),
            active_before.map(|run| run.run_id.as_str()),
        ));
    }
    Ok(notifications)
}

pub(super) fn acp_queue_update_for_current_state(
    store: &AppStore,
    session_id: &str,
    item: &AgentQueuedRequest,
) -> AppResult<Value> {
    let queue = store.agent_queue()?;
    let pending_for_session = queue
        .iter()
        .filter(|candidate| {
            candidate.conversation_id == session_id && candidate.status == "pending"
        })
        .collect::<Vec<_>>();
    let position = pending_for_session
        .iter()
        .position(|candidate| candidate.id == item.id)
        .map(|idx| idx + 1);
    let active_run_id = store
        .active_agent_run_for_conversation(session_id)?
        .map(|run| run.run_id);
    Ok(acp_queue_update_notification(
        session_id,
        item,
        position,
        pending_for_session.len(),
        active_run_id.as_deref(),
    ))
}

pub(super) fn acp_queue_update_notification(
    session_id: &str,
    item: &AgentQueuedRequest,
    position: Option<usize>,
    pending_count: usize,
    active_run_id: Option<&str>,
) -> Value {
    json!({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": {
            "sessionId": session_id,
            "update": {
                "sessionUpdate": "queue_update",
                "queueId": item.id,
                "status": item.status,
                "position": position,
                "pendingCount": pending_count,
                "activeRunId": active_run_id.unwrap_or(""),
                "content": {
                    "type": "text",
                    "text": item.content
                },
                "createdAt": item.created_at,
                "updatedAt": item.updated_at
            }
        }
    })
}

fn acp_enqueue_prompt_for_session(
    store: &AppStore,
    session_id: &str,
    prompt: &str,
) -> AppResult<AgentQueuedRequest> {
    let conversation = store.conversation(session_id)?;
    let persona = store.persona(conversation.persona_id.as_deref())?;
    let (_, queued) =
        super::enqueue_prompt_for_conversation(store, &conversation, &persona, prompt)?;
    Ok(queued)
}

fn acp_pending_queue_depth(store: &AppStore, session_id: &str) -> AppResult<usize> {
    Ok(store
        .agent_queue()?
        .into_iter()
        .filter(|item| item.conversation_id == session_id && item.status == "pending")
        .count())
}

fn acp_preview_text(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let keep = max_chars.saturating_sub(3);
    let mut preview = text.chars().take(keep).collect::<String>();
    preview.push_str("...");
    preview
}
