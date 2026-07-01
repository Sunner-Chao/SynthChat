use std::{
    collections::{HashMap, HashSet},
    env, fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use chrono::{Duration as ChronoDuration, Utc};
use futures::future::join_all;
use serde_json::{json, Value};
use tauri::{AppHandle, Manager};

use crate::{
    error::{AppError, AppResult},
    models::{
        new_id, now_iso, tool_event_kind, AgentCheckpointRecord, AgentDefinition,
        AgentRunPhaseRecord, AgentRunRecord, BrowserProvider, ChatConfig, ChatMessage,
        Conversation, EnhancedSkillSummary, ImageProvider, LlmProvider, McpServer, MemoryEntry,
        Persona, ScheduledAgentJob, SearchProvider, SendChatRequest, ShortContextState,
        SkillPromptBlock, ToolApprovalRequest, ToolDefinition, ToolEvent, ToolTraceEntry,
        VideoProvider, VisionProvider,
    },
    store::AppStore,
};

pub type AcpNotificationSink = Arc<dyn Fn(Value) -> AppResult<()> + Send + Sync>;

#[path = "agent/acp_auth.rs"]
mod acp_auth;
#[path = "agent/acp_child_events.rs"]
mod acp_child_events;
#[path = "agent/acp_client.rs"]
mod acp_client;
#[path = "agent/acp_client_fs.rs"]
mod acp_client_fs;
#[path = "agent/acp_commands.rs"]
mod acp_commands;
#[path = "agent/acp_edit_approval.rs"]
mod acp_edit_approval;
#[path = "agent/acp_events.rs"]
mod acp_events;
#[path = "agent/acp_history.rs"]
mod acp_history;
#[path = "agent/acp_permissions.rs"]
mod acp_permissions;
#[path = "agent/acp_prompt.rs"]
mod acp_prompt;
#[path = "agent/acp_prompt_runtime.rs"]
mod acp_prompt_runtime;
#[path = "agent/acp_queue.rs"]
mod acp_queue;
#[path = "agent/acp_server.rs"]
mod acp_server;
#[path = "agent/acp_session.rs"]
mod acp_session;
#[path = "agent/acp_session_env.rs"]
mod acp_session_env;
#[path = "agent/acp_subprocess.rs"]
mod acp_subprocess;
#[path = "agent/acp_tool_output.rs"]
mod acp_tool_output;
#[path = "agent/agent_loop.rs"]
mod agent_loop;
#[path = "agent/approval_gateway.rs"]
mod approval_gateway;
#[path = "agent/auxiliary_tasks.rs"]
mod auxiliary_tasks;
#[path = "agent/browser_plugins.rs"]
mod browser_plugins;
#[path = "agent/browser_tools.rs"]
mod browser_tools;
#[path = "agent/command_guard.rs"]
mod command_guard;
#[path = "agent/communication.rs"]
mod communication;
#[path = "agent/computer_use.rs"]
mod computer_use;
#[path = "agent/context_compression.rs"]
mod context_compression;
#[path = "agent/context_engine.rs"]
mod context_engine;
#[path = "agent/context_references.rs"]
mod context_references;
#[path = "agent/control_commands.rs"]
mod control_commands;
#[path = "agent/cron.rs"]
mod cron;
#[path = "agent/dashboard_auth.rs"]
mod dashboard_auth;
#[path = "agent/dashboard_plugins.rs"]
mod dashboard_plugins;
#[path = "agent/decision_parser.rs"]
mod decision_parser;
#[path = "agent/delegation.rs"]
mod delegation;
#[path = "agent/delegation_acp.rs"]
mod delegation_acp;
#[path = "agent/delegation_artifacts.rs"]
mod delegation_artifacts;
#[path = "agent/delegation_request.rs"]
mod delegation_request;
#[path = "agent/delegation_run_state.rs"]
mod delegation_run_state;
#[path = "agent/delegation_scope.rs"]
mod delegation_scope;
#[path = "agent/delegation_synthchat.rs"]
mod delegation_synthchat;
#[path = "agent/diagnostics.rs"]
mod diagnostics;
#[path = "agent/env_probe.rs"]
mod env_probe;
#[path = "agent/execution.rs"]
mod execution;
pub(crate) use execution::decode_terminal_output;
#[path = "agent/file_tools.rs"]
mod file_tools;
#[path = "agent/goal_judge.rs"]
mod goal_judge;
#[path = "agent/goal_state.rs"]
mod goal_state;
#[path = "agent/integrations.rs"]
mod integrations;
#[path = "agent/iteration_budget.rs"]
mod iteration_budget;
#[path = "agent/kanban.rs"]
mod kanban;
#[path = "agent/llm_failure.rs"]
mod llm_failure;
#[path = "agent/llm_recovery.rs"]
mod llm_recovery;
#[path = "agent/media_tools.rs"]
mod media_tools;
#[path = "agent/memory.rs"]
mod memory;
#[path = "agent/memory_manager.rs"]
mod memory_manager;
#[path = "agent/mixture.rs"]
mod mixture;
#[path = "agent/plugin_runtime.rs"]
mod plugin_runtime;
#[path = "agent/profile_describer.rs"]
mod profile_describer;
#[path = "agent/prompt_builder.rs"]
mod prompt_builder;
#[path = "agent/provider_plugins.rs"]
mod provider_plugins;
#[path = "agent/redact.rs"]
mod redact;
#[path = "agent/run_management.rs"]
mod run_management;
#[path = "agent/runtime_events.rs"]
mod runtime_events;
#[path = "agent/security_tools.rs"]
mod security_tools;
#[path = "agent/session_search.rs"]
mod session_search;
#[path = "agent/shell_hooks.rs"]
mod shell_hooks;
#[path = "agent/skills.rs"]
mod skills;
#[path = "agent/spotify_status.rs"]
mod spotify_status;
#[path = "agent/state_tools.rs"]
mod state_tools;
#[path = "agent/teams_pipeline.rs"]
mod teams_pipeline;
#[path = "agent/tool_dispatch.rs"]
mod tool_dispatch;
#[path = "agent/tool_guardrails.rs"]
mod tool_guardrails;
#[path = "agent/tool_policy.rs"]
mod tool_policy;
#[path = "agent/tool_registry.rs"]
mod tool_registry;
#[path = "agent/web_tools.rs"]
mod web_tools;
#[path = "agent/workspace.rs"]
mod workspace;

use acp_auth::*;
use acp_edit_approval::*;
use acp_events::*;
use acp_history::*;
use acp_permissions::*;
use acp_prompt::*;
use acp_server::acp_server_handle_json_rpc_async_with_sink as acp_server_handle_json_rpc_async_with_sink_inner;
use acp_server::*;
use acp_session::*;
pub(crate) use agent_loop::drain_queued_requests_for_conversation;
pub use agent_loop::run_chat_turn;
use agent_loop::*;
use approval_gateway::*;
pub use approval_gateway::{
    approve_tool_call_always_and_resume, approve_tool_call_and_resume,
    approve_tool_call_server_and_resume, call_mcp_tool_with_retry, deny_tool_call_and_update_run,
};
pub(crate) use auxiliary_tasks::{
    agent_auxiliary_task_defaults, list_agent_auxiliary_task_assignments,
    list_agent_auxiliary_tasks, reset_agent_auxiliary_task_assignments,
    save_agent_auxiliary_task_assignment,
};
use browser_plugins::browser_plugins_tool;
use browser_tools::{
    browser_back_tool, browser_cdp_tool, browser_click_tool, browser_close_session_tool,
    browser_console_tool, browser_create_session_tool, browser_dialog_tool,
    browser_get_images_tool, browser_navigate_tool, browser_press_tool, browser_provider_tool,
    browser_record_tool, browser_screenshot_format, browser_scroll_tool,
    browser_session_close_request, browser_session_create_url, browser_snapshot_tool,
    browser_supervisor_register_tool, browser_supervisor_remove_tool,
    browser_supervisor_state_tool, browser_target_from_payload, browser_target_resolver_script,
    browser_type_tool, browser_vision_tool, cdp_url_from_payload,
    dynamic_browser_snapshot_expression, extract_browser_cdp_url, extract_first_string_key,
    render_dynamic_browser_snapshot,
};
use command_guard::{dangerous_command_reason, hardline_command_reason, shell_disabled_message};
use communication::{clarify_tool, send_message_tool, send_message_tool_async};
use computer_use::{
    coerce_computer_use_max_elements, computer_use_action, computer_use_coordinate,
    computer_use_tool, ensure_computer_use_safe,
};
use context_compression::{
    compute_summary_token_budget, estimate_tokens, fallback_short_context_summary,
    handle_compact_control_command, normalize_short_context_summary, record_summary_failure,
    record_summary_success, render_messages_for_summary,
    summary_failure_cooldown_remaining_seconds,
};
use context_engine::context_engine_tool;
use context_references::{
    collect_context_references, expand_context_references, read_context_reference_file,
    ContextReference, ContextReferenceKind,
};
use control_commands::*;
pub use control_commands::{list_agent_control_commands, AgentControlCommandView};
use cron::{apply_cron_schedule_input, cronjob_tool, parse_duration_minutes};
use dashboard_auth::dashboard_auth_tool;
use dashboard_plugins::{dashboard_plugins_tool, kanban_dashboard_runtime_events};
use decision_parser::{
    parse_agent_decision, planned_tool_requests_from_decision, planner_decision_error,
    summarize_planner_step,
};
use delegation::{
    acp_list_sessions_for_store, acp_mcp_servers_for_agent, acp_path_within_cwd,
    acp_read_text_file_response, acp_session_cancel_request, acp_session_start_request,
    acp_session_update_record, acp_tool_event_update_from_value, acp_write_text_file_response,
    append_delegation_memory_observation, apply_delegation_runtime_config, delegate_task_requests,
    delegate_task_tool, delegation_child_toolsets, delegation_spawn_paused,
    set_delegation_spawn_paused,
};
use diagnostics::{
    build_line_shift, diagnostic_commands_for_workspace, diagnostics_mode, diagnostics_to_json,
    edit_diagnostics_for_paths, edit_diagnostics_for_paths_with_baselines,
    format_diagnostics_block, go_workspace_detected, parse_command_diagnostics,
    python_workspace_detected, workspace_diagnostics_mode_for_extension,
    workspace_diagnostics_tool,
};
use env_probe::env_probe_tool;
use execution::{
    execute_code_tool, process_tool, reattach_detached_process_watchers,
    sensitive_env_names_to_remove, terminal_tool, tool_env_passthrough,
};
use file_tools::{
    apply_v4a_hunks_to_content, delete_file_tool, move_file_tool, normalized_replacements,
    notify_file_tool_loop_other_call, patch_tool, read_file_tool, search_files_tool,
    write_file_tool, V4aHunk,
};
use integrations::*;
pub(crate) use integrations::{
    mattermost_adapter_status, platform_adapter_status, start_configured_platform_adapters,
    start_mattermost_adapter, start_platform_adapter, stop_mattermost_adapter,
    stop_platform_adapter, text_to_speech_payload_for_desktop, transcribe_audio_payload_for_desktop,
};
pub(crate) use media_tools::{desktop_voice_playback_start_path, desktop_voice_playback_stop};
use iteration_budget::IterationBudget;
use kanban::{
    kanban_block_tool, kanban_bulk_update_tool, kanban_comment_tool, kanban_complete_tool,
    kanban_create_tool, kanban_decompose_tool, kanban_delete_tool, kanban_heartbeat_tool,
    kanban_link_tool, kanban_list_tool, kanban_show_tool, kanban_specify_tool, kanban_unblock_tool,
    kanban_unlink_tool, kanban_update_tool,
};
use llm_failure::{
    classify_llm_failure, format_rate_limit_usage, genuine_rate_limit_guard_state,
    llm_credential_variant_should_skip_retry, llm_failure_is_retryable, llm_retry_delay_ms,
};
use llm_recovery::*;
use media_tools::*;
use memory::{
    execute_manage_memory, external_memory_provider_tool, fact_feedback_tool,
    fact_store_tool_for_run, holographic_memory_prefetch_facts, manage_memory_tool,
    manage_memory_tool_for_run, memory_provider_tool, memory_tool, memory_tool_for_run,
    recall_memory_tool, recall_memory_tool_for_run, remember_fact_tool, remember_fact_tool_for_run,
};
pub(crate) use memory_manager::sync_builtin_memory_markdown;
use memory_manager::{
    build_memory_context_block, builtin_memory_prefetch, memory_pre_compress_context,
    on_memory_turn_start, on_memory_turn_synced, on_memory_write, sanitize_memory_context,
};
use mixture::{
    mixture_aggregator_system_prompt, mixture_of_agents_tool, mixture_reference_providers,
    mixture_reference_system_prompt,
};
use plugin_runtime::plugin_runtime_tool;
pub(crate) use profile_describer::auto_describe_agent;
use prompt_builder::{
    agent_planner_prompt, agent_planner_prompt_for_agent_context,
    agent_planner_prompt_for_agent_context_with_store, agent_planner_prompt_for_context,
    memory_prompt_blocks, memory_prompt_blocks_for_query,
};
use provider_plugins::provider_plugins_tool;
use redact::{redact_json_value, redact_sensitive_text};
use run_management::*;
pub use run_management::{
    abort_agent_run, diagnose_agent_run, drain_all_agent_queues, export_agent_run_bundle,
    list_agent_run_artifacts, rerun_agent_run, resume_agent_run,
    spawn_background_chat_turn_for_job,
};
pub(crate) use runtime_events::emit_pet_assistant_event;
use runtime_events::{
    append_planner_trace, emit_agent_run_record, push_tool_event_record, record_tool_event_for_run,
    record_tool_failed_for_run, record_tool_started_for_run, subscribe_agent_run_record,
    tool_failed_event, tool_failed_transition_events, tool_started_event,
};
pub(crate) use runtime_events::{emit_agent_goal_event, emit_agent_queue_event};

pub(crate) fn agent_runtime_events(
    store: &AppStore,
    payload: &serde_json::Value,
) -> AppResult<serde_json::Value> {
    let action = payload
        .get("action")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("kanban-runtime-events");
    kanban_dashboard_runtime_events(store, payload, action)
}

pub(crate) async fn dispatch_kanban_and_drain_agent_queue(
    store: &AppStore,
    app: Option<&AppHandle>,
    payload: serde_json::Value,
) -> AppResult<serde_json::Value> {
    let mut dispatch_payload = payload.as_object().cloned().unwrap_or_default();
    dispatch_payload.insert("action".into(), json!("kanban-dispatch"));
    dispatch_payload
        .entry("dryRun")
        .or_insert_with(|| json!(false));
    dispatch_payload
        .entry("enqueueAgent")
        .or_insert_with(|| json!(true));
    let drain_requested = payload
        .get("drainAgent")
        .or_else(|| payload.get("drain_agent"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true);
    let dispatch_payload = serde_json::Value::Object(dispatch_payload);
    let dispatch: serde_json::Value =
        serde_json::from_str(&dashboard_plugins_tool(store, &dispatch_payload)?)?;
    let dry_run = dispatch
        .get("dry_run")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let drained = if drain_requested && !dry_run {
        drain_all_agent_queues(store, app).await?
    } else {
        Vec::new()
    };
    Ok(json!({
        "schema": "hermes_kanban_dispatch_drain_desktop_v1",
        "status": "ok",
        "action": "kanban-dispatch-drain",
        "dispatch": dispatch,
        "drainRequested": drain_requested,
        "drain_requested": drain_requested,
        "drained": drained,
        "drainedCount": drained.len(),
        "drained_count": drained.len(),
        "nativeDispatcherDrainBridge": true,
        "boundary": "SynthChat claims ready Kanban tasks, enqueues Hermes-style worker prompts into the native agent queue, then optionally drains that queue through the existing async agent runtime."
    }))
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationDeleteMemorySettlingResult {
    pub status: String,
    pub reason: Option<String>,
    pub memory_count: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct ConversationMemorySettlingSnapshot {
    conversation: Conversation,
    persona: Persona,
    agent: AgentDefinition,
    messages: Vec<ChatMessage>,
}

pub(crate) enum ConversationMemorySettlingPlan {
    Schedule(ConversationMemorySettlingSnapshot),
    Skip(ConversationDeleteMemorySettlingResult),
}

#[derive(Debug, Clone)]
struct DeleteMemoryCandidate {
    summary: String,
    importance: u8,
    target: String,
}

pub(crate) fn snapshot_conversation_memory_before_delete(
    store: &AppStore,
    conversation_id: &str,
) -> AppResult<ConversationMemorySettlingPlan> {
    let conversation = store.conversation(conversation_id)?;
    let persona = persona_for_conversation_agent(store, &conversation)?;
    let enabled = persona
        .memory
        .get("enabled")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    if !enabled {
        return Ok(ConversationMemorySettlingPlan::Skip(
            delete_memory_settling_skipped("persona memory disabled"),
        ));
    }
    let chat_config = store.config()?.chat;
    if !chat_config.background_memory_review_enabled {
        return Ok(ConversationMemorySettlingPlan::Skip(
            delete_memory_settling_skipped("session memory review disabled"),
        ));
    }
    let messages = store.messages(conversation_id, None)?;
    let visible_messages = messages
        .iter()
        .filter(|message| matches!(message.role.as_str(), "user" | "assistant"))
        .count();
    let min_messages = chat_config.background_memory_review_min_messages.max(2);
    if visible_messages < min_messages {
        return Ok(ConversationMemorySettlingPlan::Skip(
            delete_memory_settling_skipped(format!(
                "not enough visible messages: {visible_messages}/{min_messages}"
            )),
        ));
    }
    let agent = store.agent(Some(&conversation.agent_id))?;
    Ok(ConversationMemorySettlingPlan::Schedule(
        ConversationMemorySettlingSnapshot {
        conversation,
        persona,
        agent,
        messages,
        },
    ))
}

pub(crate) async fn settle_conversation_memory_snapshot(
    store: &AppStore,
    snapshot: ConversationMemorySettlingSnapshot,
) -> ConversationDeleteMemorySettlingResult {
    let transcript = render_messages_for_summary(&snapshot.messages);
    if transcript.trim().is_empty() {
        return delete_memory_settling_skipped("conversation transcript is empty");
    }
    let providers = match store.provider_candidates(selected_provider_id(&snapshot.persona, &snapshot.agent)) {
        Ok(providers) => providers,
        Err(error) => {
            return ConversationDeleteMemorySettlingResult {
                status: "failed".into(),
                reason: Some(error.to_string()),
                memory_count: 0,
            };
        }
    };
    if providers.is_empty() {
        return delete_memory_settling_skipped("no llm provider configured");
    }
    let mut effective_persona = effective_llm_persona(&snapshot.persona, &snapshot.agent);
    effective_persona.max_tokens = effective_persona.max_tokens.clamp(2048, 4096);
    let prompt = delete_memory_review_prompt(&snapshot.conversation, &transcript);
    let message = ChatMessage::new(
        snapshot.conversation.id.clone(),
        "user",
        prompt.clone(),
        "delete-memory-review",
    );
    let reply = match complete_chat_with_provider_failover_options(
        store,
        None,
        &providers,
        &effective_persona,
        delete_memory_review_system_prompt(),
        vec![message],
        &prompt,
        None,
        None,
        crate::llm::LlmCallOptions {
            responses_reasoning_replay_enabled: false,
            fast_mode_enabled: true,
            thinking_enabled: false,
            stream_delta_callback: None,
        },
    )
    .await
    {
        Ok(reply) => reply,
        Err(error) => {
            return ConversationDeleteMemorySettlingResult {
                status: "failed".into(),
                reason: Some(format!("memory review llm failed: {error}")),
                memory_count: 0,
            };
        }
    };
    let candidates = match parse_delete_memory_candidates(&reply.content) {
        Ok(candidates) => candidates,
        Err(error) => {
            return ConversationDeleteMemorySettlingResult {
                status: "failed".into(),
                reason: Some(format!("memory review parse failed: {error}")),
                memory_count: 0,
            };
        }
    };
    if candidates.is_empty() {
        let _ = sync_builtin_memory_markdown(store, &snapshot.persona);
        return ConversationDeleteMemorySettlingResult {
            status: "settled".into(),
            reason: Some("no durable memories found".into()),
            memory_count: 0,
        };
    }
    let mut saved = 0usize;
    for candidate in candidates {
        let summary = sanitize_memory_context(&candidate.summary);
        if summary.trim().is_empty() {
            continue;
        }
        if store
            .memories(Some(&snapshot.persona.id))
            .unwrap_or_default()
            .iter()
            .any(|memory| memory.summary.trim() == summary.trim() && memory.target == candidate.target)
        {
            continue;
        }
        match store.save_memory(MemoryEntry {
            id: String::new(),
            persona_id: snapshot.persona.id.clone(),
            target: candidate.target,
            summary,
            importance: candidate.importance,
            created_at: String::new(),
            updated_at: String::new(),
        }) {
            Ok(memory) => {
                let _ = on_memory_write(
                    store,
                    "",
                    &snapshot.persona,
                    "add",
                    &memory.id,
                    &memory.summary,
                );
                saved += 1;
            }
            Err(_) => {}
        }
    }
    let _ = sync_builtin_memory_markdown(store, &snapshot.persona);
    ConversationDeleteMemorySettlingResult {
        status: "settled".into(),
        reason: None,
        memory_count: saved,
    }
}

fn delete_memory_settling_skipped(
    reason: impl Into<String>,
) -> ConversationDeleteMemorySettlingResult {
    ConversationDeleteMemorySettlingResult {
        status: "skipped".into(),
        reason: Some(reason.into()),
        memory_count: 0,
    }
}

fn delete_memory_review_system_prompt() -> String {
    "You review a soon-to-be-deleted chat session for concise session memory. Do not think step by step. Do not explain. Do not include reasoning, analysis, markdown, or prose. Return compact valid JSON only. Store less, not more. Extract only facts, preferences, decisions, unresolved context, and useful continuity signals that would help future chats with this persona. Do not store secrets, credentials, transient implementation chatter, duplicate facts, or generic assistant behavior. JSON schema: {\"memories\":[{\"summary\":\"...\",\"importance\":1-5}]}.".into()
}

fn persona_for_conversation_agent(
    store: &AppStore,
    conversation: &Conversation,
) -> AppResult<Persona> {
    let conversation_persona = store.persona(conversation.persona_id.as_deref()).ok();
    if let Some(persona) = conversation_persona.as_ref() {
        if persona.agent_id == conversation.agent_id {
            return Ok(persona.clone());
        }
    }
    store
        .personas()?
        .into_iter()
        .find(|persona| persona.agent_id == conversation.agent_id)
        .or(conversation_persona)
        .map(Ok)
        .unwrap_or_else(|| store.persona(None))
}

fn delete_memory_review_prompt(conversation: &Conversation, transcript: &str) -> String {
    let transcript = transcript.chars().take(18_000).collect::<String>();
    format!(
        "Conversation title: {}\nConversation id: {}\n\nTranscript:\n{}\n\nReturn JSON only.",
        conversation.title, conversation.id, transcript
    )
}

fn parse_delete_memory_candidates(raw: &str) -> AppResult<Vec<DeleteMemoryCandidate>> {
    let value = parse_json_value_from_model_text(raw).ok_or_else(|| {
        AppError::BadRequest("memory review did not return valid JSON".into())
    })?;
    let items = value
        .get("memories")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut candidates = Vec::new();
    for item in items.into_iter().take(8) {
        let summary = item
            .get("summary")
            .or_else(|| item.get("content"))
            .or_else(|| item.get("fact"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        if summary.is_empty() {
            continue;
        }
        let importance = item
            .get("importance")
            .and_then(Value::as_u64)
            .unwrap_or(4)
            .clamp(1, 5) as u8;
        candidates.push(DeleteMemoryCandidate {
            summary,
            importance,
            target: "session".into(),
        });
    }
    Ok(candidates)
}

fn parse_json_value_from_model_text(text: &str) -> Option<Value> {
    let trimmed = text.trim();
    if let Ok(value) = serde_json::from_str(trimmed) {
        return Some(value);
    }
    let without_fence = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .and_then(|value| value.strip_suffix("```"))
        .map(str::trim);
    if let Some(value) = without_fence.and_then(|value| serde_json::from_str(value).ok()) {
        return Some(value);
    }
    let first = trimmed.find(|ch| ch == '{' || ch == '[')?;
    let last = trimmed.rfind(|ch| ch == '}' || ch == ']')?;
    if first <= last {
        serde_json::from_str(&trimmed[first..=last]).ok()
    } else {
        None
    }
}

#[cfg(test)]
mod delete_memory_review_tests {
    use super::*;

    #[test]
    fn parse_delete_memory_candidates_accepts_fenced_json() {
        let raw = r#"```json
{"memories":[{"summary":"User prefers concise answers.","importance":5,"target":"user"},{"summary":"Project uses SynthChat release builds.","importance":3}]}
```"#;

        let parsed = parse_delete_memory_candidates(raw).unwrap();

        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].summary, "User prefers concise answers.");
        assert_eq!(parsed[0].importance, 5);
        assert_eq!(parsed[0].target, "session");
        assert_eq!(parsed[1].target, "session");
    }
}
use security_tools::{osv_check_tool, security_scan_tool};
use session_search::{
    execute_session_search, session_search_relevance_score, session_search_tool,
    sort_session_search_candidates, SessionSearchCandidate,
};
pub(crate) use shell_hooks::list_python_plugin_auxiliary_tasks;
use shell_hooks::{
    handle_shell_hooks_control_command, inject_pre_llm_hook_context, list_python_plugin_commands,
    list_python_plugin_skills, list_python_plugin_tools, run_context_engine_compress,
    run_context_engine_should_compress, run_context_engine_update_from_response,
    run_context_engine_update_model, run_post_approval_response_hooks, run_post_llm_call_hooks,
    run_post_tool_call_hooks, run_pre_approval_request_hooks, run_pre_llm_call_hooks,
    run_pre_tool_call_hooks, run_python_plugin_command, run_python_plugin_tool,
    run_session_finished_hooks, run_session_lifecycle_hooks, run_transform_llm_output_hooks,
    run_transform_terminal_output_hooks, run_transform_tool_result_hooks,
    spawn_post_approval_response_hooks, spawn_session_finished_hooks, spawn_session_reset_hooks,
    ContextEngineCompressedMessage, PythonPluginBridgeContext,
};
use skills::{skill_manage_tool, skill_view_tool, skills_list_tool};
use spotify_status::spotify_status_tool;
use state_tools::{
    artifact_tool, automatic_mutation_checkpoint, checkpoint_tool, document_tool, file_state_tool,
    list_artifacts_tool, todo_tool,
};
use teams_pipeline::teams_pipeline_tool;
use tool_dispatch::*;
use tool_guardrails::{
    append_file_mutation_footer, file_mutation_result_landed, normalize_guardrail_halt_reply,
    record_file_mutation_result, ToolLoopGuardrails,
};
use tool_policy::*;
use tool_registry::{
    available_mcp_tool_definitions, credential_pool_tool, execute_recovery_mcp_tool,
    internal_tool_availability, internal_tool_available, internal_tool_prompt_lines,
    mcp_result_to_tool_event, render_internal_tool_prompt_block, render_mcp_tool_definitions,
    resolve_mcp_tool, resolve_tool_call_payload, tool_describe_tool, tool_search_tool,
    truncate_for_prompt, visible_tool_definitions_for_agent, InternalToolAvailability,
};
use web_tools::{
    build_browser_snapshot, build_x_search_query, extract_images, extract_readable_web_text,
    fetch_url_text_for_store, format_list, normalize_search_results, validate_web_url,
    web_extract_tool, web_extract_urls_from_payload, web_provider_tool, web_request_tool,
    web_search_tool, x_search_tool,
};
use workspace::{
    likely_binary, resolve_workspace_path, resolve_workspace_target_path, should_skip_dir,
    workspace_root,
};

pub fn recovery_agent_error() -> AppError {
    AppError::BadRequest("agent runtime recovery baseline is active".into())
}

fn sanitize_visible_assistant_reply(text: &str) -> String {
    let mut output = text.to_string();
    for needle in [
        "<tool_call>",
        "<tool_call ",
        "<tool_calls>",
        "<tool_calls ",
        "<function=",
        "<function_call>",
        "<function_call ",
        "<function_calls>",
        "<function_calls ",
        "<tool_result>",
        "<tool_result ",
    ] {
        if let Some(index) = find_ascii_case_insensitive(&output, needle, 0) {
            output.truncate(index);
        }
    }
    output
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

fn find_ascii_case_insensitive(haystack: &str, needle: &str, start: usize) -> Option<usize> {
    if start >= haystack.len() {
        return None;
    }
    let haystack_lower = haystack[start..].to_ascii_lowercase();
    let needle_lower = needle.to_ascii_lowercase();
    haystack_lower.find(&needle_lower).map(|idx| start + idx)
}

pub async fn browser_runtime_status(store: &AppStore) -> AppResult<Value> {
    let provider_status = browser_provider_tool(store, &json!({"action": "status"})).await?;
    let provider_status = serde_json::from_str::<Value>(&provider_status)
        .unwrap_or_else(|_| json!({"raw": provider_status}));
    let supervisor_status =
        browser_supervisor_state_tool(store, "ui-browser-status", &json!({})).await?;
    let supervisor_status = serde_json::from_str::<Value>(&supervisor_status)
        .unwrap_or_else(|_| json!({"raw": supervisor_status}));
    Ok(json!({
        "provider": provider_status,
        "supervisor": supervisor_status,
    }))
}

pub async fn computer_use_runtime_status(store: &AppStore) -> AppResult<Value> {
    let status = computer_use_tool(
        store,
        "ui-computer-use-status",
        &json!({"action": "status"}),
    )
    .await?;
    Ok(serde_json::from_str::<Value>(&status).unwrap_or_else(|_| json!({"raw": status})))
}

pub async fn judge_agent_goal(
    store: &AppStore,
    goal: &str,
    response: &str,
    subgoals: Vec<String>,
) -> AppResult<Value> {
    let providers = store.provider_candidates(None).unwrap_or_default();
    let persona = store.persona(None)?;
    let verdict =
        goal_judge::judge_goal_completion(store, goal, response, &subgoals, &providers, &persona)
            .await?;
    Ok(json!({
        "done": verdict.done,
        "reason": verdict.reason,
        "parseFailed": verdict.parse_failed,
        "model": verdict.model,
    }))
}

pub fn agent_goal_status(store: &AppStore, conversation_id: &str) -> AppResult<Value> {
    goal_state::agent_goal_status(store, conversation_id).map(goal_state::agent_goal_to_json)
}

pub fn set_agent_goal(
    store: &AppStore,
    conversation_id: &str,
    goal: &str,
    max_turns: Option<u32>,
) -> AppResult<Value> {
    goal_state::set_agent_goal(store, conversation_id, goal, max_turns)
        .map(Some)
        .map(goal_state::agent_goal_to_json)
}

pub fn pause_agent_goal(
    store: &AppStore,
    conversation_id: &str,
    reason: Option<&str>,
) -> AppResult<Value> {
    goal_state::pause_agent_goal(store, conversation_id, reason).map(goal_state::agent_goal_to_json)
}

pub fn resume_agent_goal(
    store: &AppStore,
    conversation_id: &str,
    reset_budget: bool,
) -> AppResult<Value> {
    goal_state::resume_agent_goal(store, conversation_id, reset_budget)
        .map(goal_state::agent_goal_to_json)
}

pub fn clear_agent_goal(store: &AppStore, conversation_id: &str) -> AppResult<Value> {
    goal_state::clear_agent_goal(store, conversation_id).map(goal_state::agent_goal_to_json)
}

pub fn add_agent_subgoal(store: &AppStore, conversation_id: &str, text: &str) -> AppResult<Value> {
    goal_state::add_agent_subgoal(store, conversation_id, text).map(goal_state::agent_goal_to_json)
}

pub fn remove_agent_subgoal(
    store: &AppStore,
    conversation_id: &str,
    index: usize,
) -> AppResult<Value> {
    goal_state::remove_agent_subgoal(store, conversation_id, index)
        .map(goal_state::agent_goal_to_json)
}

pub fn clear_agent_subgoals(store: &AppStore, conversation_id: &str) -> AppResult<Value> {
    goal_state::clear_agent_subgoals(store, conversation_id).map(goal_state::agent_goal_to_json)
}

pub fn handle_acp_json_rpc_request(
    store: &AppStore,
    request: &Value,
) -> AppResult<(Vec<Value>, Value)> {
    let handled = acp_server_handle_json_rpc(store, request)?;
    Ok((handled.notifications, handled.response))
}

pub async fn handle_acp_json_rpc_request_async(
    store: &AppStore,
    request: &Value,
) -> AppResult<(Vec<Value>, Value)> {
    let handled = acp_server_handle_json_rpc_async(store, request).await?;
    Ok((handled.notifications, handled.response))
}

pub async fn handle_acp_json_rpc_request_async_with_sink(
    store: &AppStore,
    request: &Value,
    notification_sink: Option<AcpNotificationSink>,
) -> AppResult<(Vec<Value>, Value)> {
    let handled =
        acp_server_handle_json_rpc_async_with_sink(store, request, notification_sink).await?;
    Ok((handled.notifications, handled.response))
}

pub(crate) fn synthchat_tools_mcp_definitions() -> Vec<Value> {
    let prompt_lines = tool_registry::internal_tool_prompt_lines();
    synthchat_tools_mcp_exposed_names()
        .iter()
        .filter_map(|name| {
            prompt_lines
                .iter()
                .find(|(tool_name, _)| tool_name == name)
                .map(|(_, description)| {
                    let input_schema = mcp_input_schema_from_prompt_line(description);
                    json!({
                        "name": name,
                        "description": mcp_tool_description(name, description),
                        "inputSchema": input_schema,
                        "annotations": {
                            "source": "synthchat-tools",
                            "serverId": "__internal",
                            "toolName": name
                        }
                    })
                })
        })
        .collect()
}

pub(crate) fn synthchat_tools_mcp_exposed_names() -> &'static [&'static str] {
    &[
        "web_search",
        "web_extract",
        "browser_navigate",
        "browser_click",
        "browser_type",
        "browser_press",
        "browser_snapshot",
        "browser_scroll",
        "browser_back",
        "browser_get_images",
        "browser_console",
        "browser_vision",
        "vision_analyze",
        "image_generate",
        "skill_view",
        "skills_list",
        "text_to_speech",
        "voice_status",
        "voice_playback",
        "voice_recording",
        "kanban_complete",
        "kanban_block",
        "kanban_comment",
        "kanban_heartbeat",
        "kanban_show",
        "kanban_list",
        "kanban_create",
        "kanban_unblock",
        "kanban_link",
        "kanban_unlink",
        "kanban_update",
        "kanban_delete",
        "kanban_bulk_update",
    ]
}

pub(crate) async fn synthchat_tools_mcp_call(
    store: &AppStore,
    tool_name: &str,
    arguments: Value,
) -> AppResult<String> {
    if !synthchat_tools_mcp_exposed_names().contains(&tool_name) {
        return Err(AppError::BadRequest(format!(
            "tool is not exposed by synthchat-tools MCP server: {tool_name}"
        )));
    }
    let arguments = normalize_synthchat_tools_mcp_arguments(tool_name, arguments)?;
    let mut agent = AgentDefinition::default();
    agent.id = "synthchat-tools-mcp".into();
    agent.name = "SynthChat Tools MCP".into();
    agent.workspace_dir = env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .to_string_lossy()
        .to_string();
    agent.allow_shell = false;
    agent.enabled_toolsets = vec![
        "web".into(),
        "browser".into(),
        "vision".into(),
        "image_gen".into(),
        "skills".into(),
        "audio".into(),
        "voice".into(),
        "todo".into(),
        "planning".into(),
    ];
    let conversation_id = new_id("mcp-conv");
    let run_id = new_id("mcp-run");
    let (text, _event) = execute_recovery_internal_tool(
        store,
        &agent,
        &conversation_id,
        &run_id,
        tool_name,
        arguments,
        ToolExecutionContext::Interactive,
        None,
    )
    .await?;
    Ok(text)
}

fn normalize_synthchat_tools_mcp_arguments(tool_name: &str, arguments: Value) -> AppResult<Value> {
    let arguments = match arguments {
        Value::Null => json!({}),
        Value::String(text) => {
            let text = text.trim();
            if text.is_empty() {
                json!({})
            } else {
                serde_json::from_str::<Value>(text).map_err(|error| {
                    AppError::BadRequest(format!(
                        "tools/call arguments for {tool_name} must be a JSON object or JSON object string: {error}"
                    ))
                })?
            }
        }
        other => other,
    };
    if arguments.is_object() {
        Ok(arguments)
    } else {
        Err(AppError::BadRequest(format!(
            "tools/call arguments for {tool_name} must be a JSON object"
        )))
    }
}

fn mcp_tool_description(name: &str, prompt_line: &str) -> String {
    prompt_line
        .trim()
        .strip_prefix(&format!("- {name}:"))
        .unwrap_or(prompt_line)
        .trim()
        .to_string()
}

fn mcp_input_schema_from_prompt_line(prompt_line: &str) -> Value {
    parse_payload_example(prompt_line)
        .and_then(|payload| payload.as_object().map(json_schema_for_object_example))
        .unwrap_or_else(|| {
            json!({
                "type": "object",
                "additionalProperties": true
            })
        })
}

fn parse_payload_example(prompt_line: &str) -> Option<Value> {
    let start = prompt_line.find("payload ")? + "payload ".len();
    let remainder = prompt_line[start..].trim_start();
    if !remainder.starts_with('{') {
        return None;
    }
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (idx, ch) in remainder.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return serde_json::from_str(&remainder[..=idx]).ok();
                }
            }
            _ => {}
        }
    }
    None
}

fn json_schema_for_object_example(object: &serde_json::Map<String, Value>) -> Value {
    let properties = object
        .iter()
        .map(|(key, value)| (key.clone(), json_schema_for_example_value(value)))
        .collect::<serde_json::Map<_, _>>();
    json!({
        "type": "object",
        "properties": properties,
        "additionalProperties": true
    })
}

fn json_schema_for_example_value(value: &Value) -> Value {
    match value {
        Value::Null => json!({}),
        Value::Bool(_) => json!({"type": "boolean"}),
        Value::Number(number) if number.is_i64() || number.is_u64() => {
            json!({"type": "integer"})
        }
        Value::Number(_) => json!({"type": "number"}),
        Value::String(_) => json!({"type": "string"}),
        Value::Array(items) => {
            let item_schema = items
                .first()
                .map(json_schema_for_example_value)
                .unwrap_or_else(|| json!({}));
            json!({
                "type": "array",
                "items": item_schema
            })
        }
        Value::Object(object) => json_schema_for_object_example(object),
    }
}

pub fn reattach_managed_process_watchers(store: &AppStore, app: Option<&AppHandle>) -> usize {
    reattach_detached_process_watchers(store, app)
}

#[cfg(test)]
#[path = "agent/tests.rs"]
mod tests;
