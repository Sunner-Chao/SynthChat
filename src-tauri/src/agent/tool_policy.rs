use std::collections::HashSet;

use serde_json::{json, Value};

use crate::{
    error::{AppError, AppResult},
    models::{
        new_id, now_iso, AgentDefinition, LlmProvider, Persona, ToolApprovalRequest, ToolDefinition,
    },
    store::AppStore,
};

use super::{
    append_parent_phase_event, complete_chat_with_provider_failover, computer_use_action,
    dangerous_command_reason, discord_action, redact_sensitive_text, resolve_tool_call_payload,
    shell_disabled_message, spotify_action, truncate_for_prompt,
};
pub(super) fn summarize_tool_text(text: &str) -> String {
    let compact = text.lines().take(3).collect::<Vec<_>>().join(" ");
    if compact.chars().count() > 160 {
        compact.chars().take(160).collect()
    } else if compact.is_empty() {
        "tool completed".into()
    } else {
        compact
    }
}

pub(super) fn append_tool_approval_request(
    store: &AppStore,
    conversation_id: &str,
    persona_id: &str,
    agent_id: &str,
    run_id: &str,
    server_id: &str,
    tool_name: &str,
    payload: Value,
    reason: String,
) -> AppResult<ToolApprovalRequest> {
    store.append_tool_approval(ToolApprovalRequest {
        id: new_id("approval"),
        created_at: now_iso(),
        updated_at: now_iso(),
        status: "pending".into(),
        conversation_id: Some(conversation_id.to_string()),
        persona_id: Some(persona_id.to_string()),
        agent_id: Some(agent_id.to_string()),
        run_id: Some(run_id.to_string()),
        server_id: server_id.to_string(),
        tool_name: tool_name.to_string(),
        payload,
        reason,
        result: None,
        error: None,
    })
}

pub(super) fn tool_approval_reason(
    store: &AppStore,
    server_id: &str,
    tool_name: &str,
    payload: &Value,
    risky_hint: bool,
) -> AppResult<Option<String>> {
    let config = store.config()?.chat;
    if trusted_tool_patterns_match(&config.trusted_tool_patterns, server_id, tool_name) {
        return Ok(None);
    }
    if let Some(command) = approval_command_text(tool_name, payload) {
        if trusted_command_patterns_match(&config.trusted_command_patterns, command) {
            return Ok(None);
        }
    }
    match config.tool_approval_mode.as_str() {
        "never" => Ok(None),
        "always" => Ok(Some("审批模式为全部审批".into())),
        _ if matches!(tool_name, "terminal" | "process" | "execute_code") => {
            if let Some(reason) = dangerous_tool_payload_reason(tool_name, payload) {
                Ok(Some(format!("命令需要审批：{reason}")))
            } else if risky_hint {
                Ok(Some("工具调用会写入、执行命令或可能改变外部状态".into()))
            } else {
                Ok(None)
            }
        }
        _ if risky_hint => Ok(Some("工具调用会写入、执行命令或可能改变外部状态".into())),
        _ => Ok(None),
    }
}

pub(super) fn apply_scheduled_approval_mode(
    store: &AppStore,
    context: ToolExecutionContext,
    approval_reason: Option<String>,
    tool_name: &str,
) -> AppResult<Option<String>> {
    let Some(reason) = approval_reason else {
        return Ok(None);
    };
    if context != ToolExecutionContext::ScheduledJob {
        return Ok(Some(reason));
    }
    let config = store.config()?.chat;
    match normalize_cron_approval_mode(&config.cron_approval_mode).as_str() {
        "approve" => Ok(None),
        _ => Err(AppError::BadRequest(format!(
            "BLOCKED: scheduled job tool call '{tool_name}' requires approval ({reason}), but cronApprovalMode=deny and no user is present to approve it. Use a safer approach or set cronApprovalMode=approve for trusted cron jobs."
        ))),
    }
}

pub(super) async fn apply_smart_approval_mode(
    store: &AppStore,
    run_id: &str,
    providers: &[LlmProvider],
    persona: &Persona,
    approval_reason: Option<String>,
    tool_name: &str,
    payload: &Value,
) -> AppResult<Option<String>> {
    let Some(reason) = approval_reason else {
        return Ok(None);
    };
    if store.config()?.chat.tool_approval_mode.trim() != "smart" {
        return Ok(Some(reason));
    }
    let Some(command) = approval_command_text(tool_name, payload) else {
        return Ok(Some(reason));
    };
    if providers.is_empty() {
        return Ok(Some(reason));
    }
    let system_prompt = "You are a security reviewer for an AI coding agent. Decide whether a flagged tool command can be auto-approved. Respond with exactly one word: APPROVE, DENY, or ESCALATE.".to_string();
    let user_prompt = format!(
        "Command:\n{}\n\nFlagged reason:\n{}\n\nRules:\n- APPROVE only if the command is clearly safe, such as benign script execution, inspection, development tooling, or harmless file operations.\n- DENY if it could damage the system, overwrite secrets/config, wipe data, kill critical processes, drop databases, or bypass security.\n- ESCALATE if uncertain or context-dependent.\n\nRespond with exactly one word.",
        redact_sensitive_text(command),
        reason
    );
    let reply = match complete_chat_with_provider_failover(
        store,
        Some(run_id),
        providers,
        persona,
        system_prompt,
        Vec::new(),
        &user_prompt,
        None,
        None,
    )
    .await
    {
        Ok(reply) => reply,
        Err(error) => {
            append_parent_phase_event(
                store,
                run_id,
                "smart_approval",
                json!({
                    "toolName": tool_name,
                    "verdict": "escalate",
                    "reason": reason,
                    "error": error.to_string(),
                }),
            )?;
            return Ok(Some(reason));
        }
    };
    let verdict = parse_smart_approval_verdict(&reply.content);
    append_parent_phase_event(
        store,
        run_id,
        "smart_approval",
        json!({
            "toolName": tool_name,
            "verdict": verdict,
            "reason": reason,
            "reply": truncate_for_prompt(&redact_sensitive_text(&reply.content), 200),
        }),
    )?;
    match verdict {
        "approve" => Ok(None),
        "deny" => Err(AppError::BadRequest(format!(
            "BLOCKED by smart approval: {reason}. The command was assessed as genuinely dangerous. Do not retry."
        ))),
        _ => Ok(Some(reason)),
    }
}

pub(super) fn parse_smart_approval_verdict(content: &str) -> &'static str {
    let answer = content.trim().to_ascii_uppercase();
    if answer == "APPROVE" || answer.starts_with("APPROVE\n") {
        "approve"
    } else if answer == "DENY" || answer.starts_with("DENY\n") {
        "deny"
    } else {
        "escalate"
    }
}

pub(super) fn normalize_cron_approval_mode(mode: &str) -> String {
    match mode.trim().to_ascii_lowercase().as_str() {
        "approve" | "allow" | "allowed" | "yes" | "true" | "on" => "approve".into(),
        _ => "deny".into(),
    }
}

pub(super) fn dangerous_tool_payload_reason(tool_name: &str, payload: &Value) -> Option<String> {
    approval_command_text(tool_name, payload).and_then(dangerous_command_reason)
}

fn approval_command_text<'a>(tool_name: &str, payload: &'a Value) -> Option<&'a str> {
    match tool_name {
        "terminal" => payload.get("command").and_then(Value::as_str),
        "process" => {
            let action = payload
                .get("action")
                .and_then(Value::as_str)
                .unwrap_or("list")
                .trim()
                .to_ascii_lowercase();
            if matches!(action.as_str(), "start" | "run") {
                payload.get("command").and_then(Value::as_str)
            } else {
                None
            }
        }
        "execute_code" => payload.get("code").and_then(Value::as_str),
        _ => None,
    }
}

pub(super) fn is_internal_tool(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "tool_search"
            | "tool_describe"
            | "tool_call"
            | "read_file"
            | "file_state"
            | "search_files"
            | "write_file"
            | "delete_file"
            | "move_file"
            | "patch"
            | "terminal"
            | "process"
            | "execute_code"
            | "workspace_diagnostics"
            | "env_probe"
            | "credential_pool"
            | "osv_check"
            | "computer_use"
            | "delegate_task"
            | "mixture_of_agents"
            | "kanban_create"
            | "kanban_list"
            | "kanban_show"
            | "kanban_complete"
            | "kanban_block"
            | "kanban_unblock"
            | "kanban_heartbeat"
            | "kanban_comment"
            | "kanban_link"
            | "send_message"
            | "session_search"
            | "clarify"
            | "cronjob"
            | "recall_memory"
            | "remember_fact"
            | "manage_memory"
            | "memory"
            | "skills_list"
            | "skill_view"
            | "skill_manage"
            | "image_generate"
            | "video_generate"
            | "text_to_speech"
            | "transcribe_audio"
            | "vision_analyze"
            | "video_analyze"
            | "weather"
            | "ha_list_entities"
            | "ha_get_state"
            | "ha_list_services"
            | "ha_call_service"
            | "feishu_doc_read"
            | "feishu_drive_list_comments"
            | "feishu_drive_list_comment_replies"
            | "feishu_drive_reply_comment"
            | "feishu_drive_add_comment"
            | "yb_query_group_info"
            | "yb_query_group_members"
            | "yb_send_dm"
            | "yb_search_sticker"
            | "yb_send_sticker"
            | "spotify_playback"
            | "spotify_devices"
            | "spotify_queue"
            | "spotify_search"
            | "spotify_playlists"
            | "spotify_albums"
            | "spotify_library"
            | "discord"
            | "discord_admin"
            | "todo"
            | "update_todo"
            | "checkpoint"
            | "artifact"
            | "list_artifacts"
            | "browser_navigate"
            | "browser_snapshot"
            | "browser_back"
            | "browser_get_images"
            | "browser_provider"
            | "browser_create_session"
            | "browser_close_session"
            | "browser_cdp"
            | "browser_click"
            | "browser_type"
            | "browser_press"
            | "browser_scroll"
            | "browser_dialog"
            | "browser_record"
            | "browser_vision"
            | "browser_console"
            | "browser_supervisor_register"
            | "browser_supervisor_state"
            | "browser_supervisor_remove"
            | "web_provider"
            | "web_search"
            | "x_search"
            | "web_extract"
            | "web_request"
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub(super) enum ToolExecutionContext {
    Interactive,
    ScheduledJob,
    SubagentLeaf,
    SubagentOrchestrator,
}

#[allow(dead_code)]
pub(super) fn apply_tool_context_policy(
    tools: Vec<ToolDefinition>,
    context: ToolExecutionContext,
) -> Vec<ToolDefinition> {
    tools
        .into_iter()
        .filter(|tool| tool_allowed_in_context(tool, context))
        .collect()
}

pub(super) fn apply_agent_toolset_policy(
    tools: Vec<ToolDefinition>,
    agent: &AgentDefinition,
) -> Vec<ToolDefinition> {
    tools
        .into_iter()
        .filter(|tool| tool_allowed_by_agent_toolsets(tool, agent))
        .collect()
}

pub(super) fn tool_allowed_by_agent_toolsets(
    tool: &ToolDefinition,
    agent: &AgentDefinition,
) -> bool {
    let enabled = normalized_toolset_names(&agent.enabled_toolsets);
    let disabled = normalized_toolset_names(&agent.disabled_toolsets);
    if enabled.is_empty() && disabled.is_empty() {
        return true;
    }
    let toolsets = tool_toolsets(tool);
    let explicitly_enabled =
        enabled.is_empty() || toolsets.iter().any(|name| enabled.contains(name));
    let explicitly_disabled = toolsets.iter().any(|name| disabled.contains(name));
    explicitly_enabled && !explicitly_disabled
}

pub(super) fn tool_allowed_by_agent_capabilities(
    tool: &ToolDefinition,
    agent: &AgentDefinition,
) -> bool {
    if tool.source == "internal" && is_shell_execution_tool(&tool.tool_name) {
        return agent.allow_shell;
    }
    true
}

pub(super) fn is_shell_execution_tool(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "terminal" | "process" | "execute_code" | "workspace_diagnostics" | "env_probe"
    )
}

pub(super) fn normalized_toolset_names(names: &[String]) -> HashSet<String> {
    names
        .iter()
        .map(|name| normalize_toolset_name(name))
        .filter(|name| !name.is_empty())
        .collect()
}

pub(super) fn normalize_toolset_name(name: &str) -> String {
    name.trim().to_lowercase().replace('-', "_")
}

pub(super) fn tool_toolsets(tool: &ToolDefinition) -> HashSet<String> {
    let mut names = HashSet::new();
    names.insert("all".into());
    names.insert(normalize_toolset_name(&tool.source));
    names.insert(format!(
        "server:{}",
        normalize_toolset_name(&tool.server_id)
    ));
    names.insert(format!("tool:{}", normalize_toolset_name(&tool.tool_name)));
    for name in semantic_toolsets_for_tool(tool) {
        names.insert(name.into());
    }
    names
}

pub(super) fn semantic_toolsets_for_tool(tool: &ToolDefinition) -> Vec<&'static str> {
    let name = tool.tool_name.as_str();
    if tool.source == "internal" {
        return match name {
            "read_file" | "file_state" | "search_files" | "write_file" | "delete_file"
            | "move_file" | "patch" => vec!["file"],
            "terminal" | "process" | "execute_code" | "workspace_diagnostics" => {
                vec!["terminal", "code_execution"]
            }
            "env_probe" => vec!["terminal", "config"],
            "credential_pool" => vec!["config"],
            "osv_check" => vec!["security", "web"],
            "browser_navigate"
            | "browser_snapshot"
            | "browser_back"
            | "browser_get_images"
            | "browser_provider"
            | "browser_create_session"
            | "browser_close_session"
            | "browser_cdp"
            | "browser_click"
            | "browser_type"
            | "browser_press"
            | "browser_scroll"
            | "browser_dialog"
            | "browser_record"
            | "browser_vision"
            | "browser_console"
            | "browser_supervisor_register"
            | "browser_supervisor_state"
            | "browser_supervisor_remove" => vec!["browser"],
            "web_provider" => vec!["web", "config"],
            "web_search" | "x_search" => vec!["web", "search"],
            "web_extract" | "web_request" | "weather" => vec!["web"],
            "vision_analyze" | "video_analyze" => vec!["vision"],
            "image_generate" => vec!["image_gen"],
            "video_generate" => vec!["video_gen"],
            "text_to_speech" => vec!["tts", "audio"],
            "transcribe_audio" => vec!["stt", "audio"],
            "delegate_task" => vec!["delegation"],
            "mixture_of_agents" => vec!["moa", "delegation"],
            "clarify" => vec!["clarify"],
            "cronjob" => vec!["cronjob"],
            "session_search" => vec!["session_search", "search"],
            "todo" | "update_todo" | "checkpoint" | "kanban_create" | "kanban_list"
            | "kanban_show" | "kanban_complete" | "kanban_block" | "kanban_unblock"
            | "kanban_heartbeat" | "kanban_comment" | "kanban_link" => vec!["todo", "planning"],
            "recall_memory" | "remember_fact" | "manage_memory" | "memory" => vec!["memory"],
            "skills_list" | "skill_view" | "skill_manage" => vec!["skills"],
            "computer_use" => vec!["computer_use"],
            "homeassistant" | "ha_list_entities" | "ha_get_state" | "ha_list_services"
            | "ha_call_service" => vec!["homeassistant"],
            "feishu_doc_read"
            | "feishu_drive_list_comments"
            | "feishu_drive_list_comment_replies"
            | "feishu_drive_reply_comment"
            | "feishu_drive_add_comment" => vec!["feishu"],
            "yb_query_group_info"
            | "yb_query_group_members"
            | "yb_send_dm"
            | "yb_search_sticker"
            | "yb_send_sticker" => vec!["yuanbao"],
            "spotify_playback" | "spotify_devices" | "spotify_queue" | "spotify_search"
            | "spotify_playlists" | "spotify_albums" | "spotify_library" => vec!["spotify"],
            "discord" | "discord_admin" => vec!["discord"],
            "send_message" => vec!["messaging"],
            "artifact" | "list_artifacts" => vec!["artifact"],
            "tool_search" | "tool_describe" | "tool_call" => vec!["tools"],
            _ => vec!["internal"],
        };
    }
    let text = format!(
        "{} {} {} {} {}",
        tool.name.to_lowercase(),
        tool.display_name.to_lowercase(),
        tool.tool_name.to_lowercase(),
        tool.server_id.to_lowercase(),
        tool.description.to_lowercase()
    );
    let mut toolsets = Vec::new();
    if contains_any(
        &text,
        &["browser", "page", "dom", "snapshot", "click", "scroll"],
    ) {
        toolsets.push("browser");
    }
    if contains_any(&text, &["web", "http", "url", "search", "extract", "fetch"]) {
        toolsets.push("web");
    }
    if contains_any(&text, &["search", "query", "find"]) {
        toolsets.push("search");
    }
    if contains_any(
        &text,
        &["file", "read", "write", "patch", "directory", "path"],
    ) {
        toolsets.push("file");
    }
    if contains_any(&text, &["terminal", "shell", "command", "process", "exec"]) {
        toolsets.push("terminal");
    }
    if contains_any(&text, &["image", "vision", "screenshot", "ocr"]) {
        toolsets.push("vision");
    }
    if contains_any(&text, &["audio", "speech", "tts", "voice"]) {
        toolsets.push("audio");
    }
    if toolsets.is_empty() {
        toolsets.push("mcp");
    }
    toolsets
}

pub(super) fn contains_any(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| text.contains(needle))
}

#[allow(dead_code)]
pub(super) fn tool_allowed_in_context(
    tool: &ToolDefinition,
    context: ToolExecutionContext,
) -> bool {
    match context {
        ToolExecutionContext::Interactive => true,
        ToolExecutionContext::ScheduledJob => {
            !(tool.source == "internal"
                && matches!(
                    tool.tool_name.as_str(),
                    "cronjob"
                        | "clarify"
                        | "send_message"
                        | "remember_fact"
                        | "recall_memory"
                        | "memory"
                ))
        }
        ToolExecutionContext::SubagentLeaf => {
            !(tool.source == "internal"
                && matches!(
                    tool.tool_name.as_str(),
                    "delegate_task"
                        | "cronjob"
                        | "clarify"
                        | "remember_fact"
                        | "recall_memory"
                        | "memory"
                ))
        }
        ToolExecutionContext::SubagentOrchestrator => {
            !(tool.source == "internal"
                && matches!(
                    tool.tool_name.as_str(),
                    "cronjob" | "clarify" | "remember_fact" | "recall_memory" | "memory"
                ))
        }
    }
}

pub(super) fn ensure_internal_tool_allowed_in_context(
    tool_name: &str,
    context: ToolExecutionContext,
) -> AppResult<()> {
    let tool = ToolDefinition {
        name: tool_name.into(),
        display_name: tool_name.into(),
        description: String::new(),
        source: "internal".into(),
        server_id: "__internal".into(),
        tool_name: tool_name.into(),
        input_schema: json!({}),
        requires_approval: false,
    };
    if tool_allowed_in_context(&tool, context) {
        Ok(())
    } else {
        Err(AppError::BadRequest(format!(
            "internal tool '{tool_name}' is not allowed in {context:?} context"
        )))
    }
}

pub(super) fn ensure_internal_tool_allowed(
    agent: &AgentDefinition,
    tool_name: &str,
    context: ToolExecutionContext,
) -> AppResult<()> {
    let tool = ToolDefinition {
        name: tool_name.into(),
        display_name: tool_name.into(),
        description: String::new(),
        source: "internal".into(),
        server_id: "__internal".into(),
        tool_name: tool_name.into(),
        input_schema: json!({}),
        requires_approval: false,
    };
    ensure_internal_tool_allowed_in_context(tool_name, context)?;
    if !tool_allowed_by_agent_capabilities(&tool, agent) {
        return Err(AppError::BadRequest(shell_disabled_message(
            agent,
            Some(tool_name),
        )));
    }
    if tool_allowed_by_agent_toolsets(&tool, agent) {
        Ok(())
    } else {
        Err(AppError::BadRequest(format!(
            "internal tool '{tool_name}' is disabled by this agent's toolset policy"
        )))
    }
}

pub(super) fn trusted_tool_patterns_match(
    patterns: &[String],
    server_id: &str,
    tool_name: &str,
) -> bool {
    let exact = format!("{server_id}.{tool_name}");
    let server_wildcard = format!("{server_id}.*");
    patterns
        .iter()
        .any(|pattern| pattern == "*" || pattern == &exact || pattern == &server_wildcard)
}

pub(super) fn trusted_command_patterns_match(patterns: &[String], command: &str) -> bool {
    let command = command.trim();
    patterns
        .iter()
        .any(|pattern| wildcard_pattern_match(pattern.trim(), command))
}

fn wildcard_pattern_match(pattern: &str, value: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if !pattern.contains('*') {
        return pattern == value;
    }
    let mut remainder = value;
    let anchored_start = !pattern.starts_with('*');
    let anchored_end = !pattern.ends_with('*');
    let parts = pattern
        .split('*')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    if let Some(first) = parts.first() {
        if anchored_start {
            let Some(after) = remainder.strip_prefix(first) else {
                return false;
            };
            remainder = after;
        } else if let Some(index) = remainder.find(first) {
            remainder = &remainder[index + first.len()..];
        } else {
            return false;
        }
    }
    for part in parts.iter().skip(1) {
        if let Some(index) = remainder.find(part) {
            remainder = &remainder[index + part.len()..];
        } else {
            return false;
        }
    }
    !anchored_end || parts.last().is_none_or(|last| value.ends_with(last))
}

pub(super) fn is_risky_tool_call(tool_name: &str, payload: &Value) -> bool {
    match tool_name {
        "write_file"
        | "delete_file"
        | "move_file"
        | "patch"
        | "terminal"
        | "execute_code"
        | "skill_manage"
        | "cronjob"
        | "ha_call_service"
        | "kanban_create"
        | "kanban_complete"
        | "kanban_block"
        | "kanban_unblock"
        | "kanban_heartbeat"
        | "kanban_comment"
        | "kanban_link"
        | "feishu_drive_reply_comment"
        | "feishu_drive_add_comment"
        | "yb_send_dm"
        | "yb_send_sticker" => true,
        "spotify_playback" => spotify_action(payload)
            .map(|action| {
                !matches!(
                    action.as_str(),
                    "get_state" | "get_currently_playing" | "recently_played"
                )
            })
            .unwrap_or(false),
        "spotify_devices" => spotify_action(payload)
            .map(|action| action == "transfer")
            .unwrap_or(false),
        "spotify_queue" => spotify_action(payload)
            .map(|action| action == "add")
            .unwrap_or(false),
        "spotify_search" | "spotify_albums" => false,
        "spotify_playlists" => spotify_action(payload)
            .map(|action| !matches!(action.as_str(), "list" | "get"))
            .unwrap_or(false),
        "spotify_library" => spotify_action(payload)
            .map(|action| action != "list")
            .unwrap_or(false),
        "tool_call" => resolve_tool_call_payload(payload)
            .map(|(target_name, target_payload)| is_risky_tool_call(&target_name, &target_payload))
            .unwrap_or(true),
        "discord" => discord_action(payload)
            .map(|action| matches!(action.as_str(), "create_thread" | "send_message"))
            .unwrap_or(false),
        "discord_admin" => discord_action(payload)
            .map(|action| {
                matches!(
                    action.as_str(),
                    "pin_message" | "unpin_message" | "delete_message" | "add_role" | "remove_role"
                )
            })
            .unwrap_or(false),
        "computer_use" => computer_use_action(payload)
            .map(|action| {
                !matches!(
                    action.as_str(),
                    "status"
                        | "capabilities"
                        | "capability"
                        | "backend_status"
                        | "requirements"
                        | "setup_schema"
                        | "reset_backend"
                        | "mcp_probe"
                        | "capture"
                        | "list_apps"
                        | "wait"
                )
            })
            .unwrap_or(true),
        "mixture_of_agents" => true,
        "send_message" => payload
            .get("action")
            .and_then(Value::as_str)
            .map(|action| !matches!(action.trim().to_lowercase().as_str(), "list" | "targets"))
            .unwrap_or(true),
        "process" => payload
            .get("action")
            .and_then(Value::as_str)
            .map(|action| {
                matches!(
                    action.trim().to_lowercase().as_str(),
                    "start" | "run" | "stop" | "kill"
                )
            })
            .unwrap_or(false),
        "web_request" => payload
            .get("method")
            .and_then(Value::as_str)
            .map(|method| !matches!(method.trim().to_uppercase().as_str(), "GET" | "HEAD"))
            .unwrap_or(false),
        "browser_click" | "browser_type" | "browser_press" | "browser_dialog"
        | "browser_record" => true,
        "browser_cdp" | "browser_console" => cdp_payload_may_mutate(payload),
        "credential_pool" => payload
            .get("action")
            .and_then(Value::as_str)
            .map(|action| matches!(action.trim().to_lowercase().as_str(), "reset" | "clear"))
            .unwrap_or(false),
        _ => false,
    }
}

pub(super) fn cdp_payload_may_mutate(payload: &Value) -> bool {
    let method = payload
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !matches!(method, "Runtime.evaluate" | "Runtime.callFunctionOn") {
        return false;
    }
    let expression = payload
        .get("params")
        .and_then(|params| params.get("expression"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_lowercase();
    let mutation_markers = [
        "click(",
        "submit(",
        "fetch(",
        "xmlhttprequest",
        "localstorage.",
        "sessionstorage.",
        "document.cookie",
        "innerhtml",
        "textcontent",
        "value =",
        "remove(",
        "append",
        "dispatchEvent",
    ];
    mutation_markers
        .iter()
        .any(|marker| expression.contains(&marker.to_lowercase()))
}
