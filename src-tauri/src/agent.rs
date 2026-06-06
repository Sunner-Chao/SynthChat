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
        Conversation, EnhancedSkillSummary, LlmProvider, McpServer, MemoryEntry, Persona,
        ScheduledAgentJob, SearchProvider, SendChatRequest, ShortContextState, SkillPromptBlock,
        ToolApprovalRequest, ToolDefinition, ToolEvent, ToolTraceEntry, VideoProvider,
        VisionProvider,
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
#[path = "agent/context_references.rs"]
mod context_references;
#[path = "agent/control_commands.rs"]
mod control_commands;
#[path = "agent/cron.rs"]
mod cron;
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
#[path = "agent/file_tools.rs"]
mod file_tools;
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
#[path = "agent/prompt_builder.rs"]
mod prompt_builder;
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
#[path = "agent/state_tools.rs"]
mod state_tools;
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
pub use agent_loop::run_chat_turn;
use agent_loop::*;
use approval_gateway::*;
pub use approval_gateway::{
    approve_tool_call_always_and_resume, approve_tool_call_and_resume,
    approve_tool_call_server_and_resume, call_mcp_tool_with_retry, deny_tool_call_and_update_run,
};
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
use context_references::{
    collect_context_references, expand_context_references, read_context_reference_file,
    ContextReference, ContextReferenceKind,
};
use control_commands::*;
pub use control_commands::{list_agent_control_commands, AgentControlCommandView};
use cron::{apply_cron_schedule_input, cronjob_tool, parse_duration_minutes};
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
    stop_platform_adapter,
};
use iteration_budget::IterationBudget;
use kanban::{
    kanban_block_tool, kanban_comment_tool, kanban_complete_tool, kanban_create_tool,
    kanban_heartbeat_tool, kanban_link_tool, kanban_list_tool, kanban_show_tool,
    kanban_unblock_tool,
};
use llm_failure::{
    classify_llm_failure, format_rate_limit_usage, genuine_rate_limit_guard_state,
    llm_credential_variant_should_skip_retry, llm_failure_is_retryable, llm_retry_delay_ms,
};
use llm_recovery::*;
use media_tools::*;
use memory::{
    execute_manage_memory, manage_memory_tool, manage_memory_tool_for_run, memory_tool,
    memory_tool_for_run, recall_memory_tool, remember_fact_tool, remember_fact_tool_for_run,
};
use memory_manager::{
    build_memory_context_block, builtin_memory_prefetch, memory_pre_compress_context,
    on_memory_turn_start, on_memory_turn_synced, on_memory_write,
};
use mixture::{
    mixture_aggregator_system_prompt, mixture_of_agents_tool, mixture_reference_providers,
    mixture_reference_system_prompt,
};
use prompt_builder::{
    agent_planner_prompt, agent_planner_prompt_for_agent_context,
    agent_planner_prompt_for_agent_context_with_store, agent_planner_prompt_for_context,
    memory_prompt_blocks, memory_prompt_blocks_for_query,
};
use redact::{redact_json_value, redact_sensitive_text};
use run_management::*;
pub use run_management::{
    abort_agent_run, diagnose_agent_run, drain_all_agent_queues, export_agent_run_bundle,
    list_agent_run_artifacts, rerun_agent_run, resume_agent_run,
    spawn_background_chat_turn_for_job,
};
pub(crate) use runtime_events::emit_agent_queue_event;
use runtime_events::{
    append_planner_trace, emit_agent_run_record, push_tool_event_record, record_tool_event_for_run,
    record_tool_failed_for_run, record_tool_started_for_run, tool_failed_event, tool_started_event,
};
use security_tools::osv_check_tool;
use session_search::{
    execute_session_search, session_search_relevance_score, session_search_tool,
    sort_session_search_candidates, SessionSearchCandidate,
};
use shell_hooks::{
    handle_shell_hooks_control_command, inject_pre_llm_hook_context, list_python_plugin_skills,
    list_python_plugin_tools, run_post_approval_response_hooks, run_post_llm_call_hooks,
    run_post_tool_call_hooks, run_pre_approval_request_hooks, run_pre_llm_call_hooks,
    run_pre_tool_call_hooks, run_python_plugin_command, run_python_plugin_tool,
    run_session_finished_hooks, run_session_lifecycle_hooks, run_transform_llm_output_hooks,
    run_transform_terminal_output_hooks, run_transform_tool_result_hooks,
    spawn_post_approval_response_hooks, spawn_session_finished_hooks, spawn_session_reset_hooks,
};
use skills::{skill_manage_tool, skill_view_tool, skills_list_tool};
use state_tools::{
    artifact_tool, automatic_mutation_checkpoint, checkpoint_tool, file_state_tool,
    list_artifacts_tool, todo_tool,
};
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

pub fn reattach_managed_process_watchers(store: &AppStore, app: Option<&AppHandle>) -> usize {
    reattach_detached_process_watchers(store, app)
}

#[cfg(test)]
#[path = "agent/tests.rs"]
mod tests;
