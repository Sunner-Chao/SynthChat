use std::collections::HashMap;

use serde_json::json;

use crate::{error::AppResult, store::AppStore};

use super::acp_session::{
    acp_advertised_command_specs, acp_available_commands_update_for_store,
    acp_estimate_session_context_tokens, acp_model_selection_from_id,
    acp_session_runtime_config_for_store, acp_update_session_runtime_config,
};
use super::shell_hooks::spawn_session_reset_hooks;

pub(super) struct AcpLocalCommandReply {
    pub(super) text: String,
    pub(super) include_usage: bool,
    pub(super) include_session_info: bool,
}

impl AcpLocalCommandReply {
    fn usage(text: String) -> Self {
        Self {
            text,
            include_usage: true,
            include_session_info: false,
        }
    }

    fn session_info(text: String) -> Self {
        Self {
            text,
            include_usage: false,
            include_session_info: true,
        }
    }

    fn usage_and_session_info(text: String) -> Self {
        Self {
            text,
            include_usage: true,
            include_session_info: true,
        }
    }
}

pub(super) async fn acp_local_command_reply_for_prompt(
    store: &AppStore,
    session_id: &str,
    text: &str,
) -> AppResult<Option<AcpLocalCommandReply>> {
    if let Some(help_text) = acp_help_text_for_prompt_with_store(store, text) {
        return Ok(Some(AcpLocalCommandReply::usage(help_text)));
    }
    if let Some(model_text) = acp_model_text_for_prompt(store, session_id, text)? {
        return Ok(Some(AcpLocalCommandReply::session_info(model_text)));
    }
    if let Some(context_text) = acp_context_text_for_prompt(store, session_id, text)? {
        return Ok(Some(AcpLocalCommandReply::usage(context_text)));
    }
    if let Some(compact_text) = acp_compact_text_for_prompt(store, session_id, text).await? {
        return Ok(Some(AcpLocalCommandReply::usage_and_session_info(
            compact_text,
        )));
    }
    if let Some(reset_text) = acp_reset_text_for_prompt(store, session_id, text)? {
        return Ok(Some(AcpLocalCommandReply::session_info(reset_text)));
    }
    if let Some(tools_text) = acp_tools_text_for_prompt(store, session_id, text)? {
        return Ok(Some(AcpLocalCommandReply::usage(tools_text)));
    }
    if let Some(version_text) = acp_version_text_for_prompt(text) {
        return Ok(Some(AcpLocalCommandReply::usage(version_text)));
    }
    Ok(None)
}

pub(super) fn acp_help_text_for_prompt(text: &str) -> Option<String> {
    let (command, _args) = acp_slash_command_parts(text)?;
    if command != "help" {
        return None;
    }
    let mut lines = vec!["Available commands:".to_string(), String::new()];
    for (name, description, _) in acp_advertised_command_specs() {
        lines.push(format!("  /{name:10}  {description}"));
    }
    lines.push(String::new());
    lines.push("Unrecognized /commands are sent to the model as normal messages.".into());
    Some(lines.join("\n"))
}

pub(super) fn acp_help_text_for_prompt_with_store(store: &AppStore, text: &str) -> Option<String> {
    let (command, _args) = acp_slash_command_parts(text)?;
    if command != "help" {
        return None;
    }
    let update = acp_available_commands_update_for_store(store);
    let commands = update["availableCommands"].as_array()?;
    let mut lines = vec!["Available commands:".to_string(), String::new()];
    for command in commands {
        let name = command["name"].as_str().unwrap_or_default();
        if name.trim().is_empty() {
            continue;
        }
        let description = command["description"].as_str().unwrap_or_default();
        lines.push(format!("  /{name:10}  {description}"));
    }
    lines.push(String::new());
    lines.push("Unrecognized /commands are sent to the model as normal messages.".into());
    Some(lines.join("\n"))
}

pub(super) fn acp_version_text_for_prompt(text: &str) -> Option<String> {
    let (command, _args) = acp_slash_command_parts(text)?;
    if command == "version" {
        Some(format!("SynthChat {}", env!("CARGO_PKG_VERSION")))
    } else {
        None
    }
}

pub(super) fn acp_reset_text_for_prompt(
    store: &AppStore,
    session_id: &str,
    text: &str,
) -> AppResult<Option<String>> {
    let Some((command, _args)) = acp_slash_command_parts(text) else {
        return Ok(None);
    };
    if command != "reset" {
        return Ok(None);
    }
    if !session_id.is_empty() {
        if let Ok(conversation) = store.conversation(session_id) {
            let removed = store.clear_conversation_history(session_id)?;
            spawn_session_reset_hooks(
                store,
                conversation,
                json!({
                    "source": "acp_reset",
                    "action": command,
                    "removed_messages": removed,
                }),
            );
        }
    }
    Ok(Some("Conversation history cleared.".into()))
}

pub(super) fn acp_model_text_for_prompt(
    store: &AppStore,
    session_id: &str,
    text: &str,
) -> AppResult<Option<String>> {
    let Some((command, args)) = acp_slash_command_parts(text) else {
        return Ok(None);
    };
    if command != "model" {
        return Ok(None);
    }
    if session_id.is_empty() || store.conversation(session_id).is_err() {
        return Ok(Some("Current model: unknown\nProvider: auto".into()));
    }
    let argument = args.trim();
    if argument.is_empty() || matches!(argument.to_ascii_lowercase().as_str(), "show" | "status") {
        let (provider, model) = acp_session_runtime_provider_model(store, session_id)?;
        return Ok(Some(format!(
            "Current model: {}\nProvider: {}",
            model.unwrap_or_else(|| "unknown".into()),
            provider.unwrap_or_else(|| "auto".into())
        )));
    }

    let (provider, model) = acp_model_selection_from_id(argument);
    acp_update_session_runtime_config(store, session_id, |runtime| {
        if provider.is_some() {
            runtime.provider = provider.clone();
        }
        runtime.model = Some(model.clone());
    })?;
    let provider = provider
        .or_else(|| {
            acp_session_runtime_config_for_store(store, session_id)
                .ok()
                .flatten()
                .and_then(|runtime| runtime.provider)
        })
        .unwrap_or_else(|| "auto".into());
    Ok(Some(format!(
        "Model switched to: {model}\nProvider: {provider}"
    )))
}

pub(super) fn acp_context_text_for_prompt(
    store: &AppStore,
    session_id: &str,
    text: &str,
) -> AppResult<Option<String>> {
    let Some((command, _args)) = acp_slash_command_parts(text) else {
        return Ok(None);
    };
    if command != "context" {
        return Ok(None);
    }

    let messages = store.messages(session_id, None)?;
    let mut roles = HashMap::<String, usize>::new();
    for message in &messages {
        *roles.entry(message.role.clone()).or_insert(0) += 1;
    }

    let mut lines = vec![
        if messages.is_empty() {
            "Conversation is empty (no messages yet).".into()
        } else {
            format!("Conversation: {} messages", messages.len())
        },
        format!(
            "  user: {}, assistant: {}, tool: {}, system: {}",
            roles.get("user").copied().unwrap_or(0),
            roles.get("assistant").copied().unwrap_or(0),
            roles.get("tool").copied().unwrap_or(0),
            roles.get("system").copied().unwrap_or(0)
        ),
    ];

    let (provider, model) = if !session_id.is_empty() && store.conversation(session_id).is_ok() {
        acp_session_runtime_provider_model(store, session_id)?
    } else {
        (None, None)
    };
    if let Some(model) = model.filter(|value| !value.trim().is_empty()) {
        lines.push(format!("Model: {model}"));
    }
    lines.push(format!(
        "Provider: {}",
        provider.unwrap_or_else(|| "auto".into())
    ));

    let config = store.config()?.chat;
    let context_length = config.short_context_token_budget.max(0) as usize;
    let threshold_tokens = if context_length > 0 {
        context_length.saturating_mul(80) / 100
    } else {
        0
    };
    let short_context = store.short_context(session_id).ok();
    let approx_tokens = short_context
        .as_ref()
        .map(|context| context.last_real_prompt_tokens)
        .filter(|tokens| *tokens > 0)
        .unwrap_or(acp_estimate_session_context_tokens(
            store,
            session_id,
            short_context.as_ref(),
        )?);

    if approx_tokens > 0 {
        if context_length > 0 {
            let usage_pct = (approx_tokens as f64 / context_length as f64) * 100.0;
            lines.push(format!(
                "Context usage: ~{} / {} tokens ({usage_pct:.1}%)",
                acp_format_count(approx_tokens),
                acp_format_count(context_length)
            ));
        } else {
            lines.push(format!(
                "Context usage: ~{} tokens",
                acp_format_count(approx_tokens)
            ));
        }
    }

    if threshold_tokens > 0 {
        let threshold_pct = if context_length > 0 {
            format!(", {}%", (threshold_tokens * 100) / context_length)
        } else {
            String::new()
        };
        if approx_tokens >= threshold_tokens {
            lines.push(format!(
                "Compression: due now (threshold ~{}{}). Run /compact.",
                acp_format_count(threshold_tokens),
                threshold_pct
            ));
        } else {
            let remaining = threshold_tokens.saturating_sub(approx_tokens);
            lines.push(format!(
                "Compression: ~{} tokens until threshold (~{}{}).",
                acp_format_count(remaining),
                acp_format_count(threshold_tokens),
                threshold_pct
            ));
        }
    }

    if config.short_context_mode.trim().eq_ignore_ascii_case("off") {
        lines.push("Compression is disabled for this agent.".into());
    } else {
        lines.push("Tip: run /compact to compress manually before the threshold.".into());
    }

    Ok(Some(lines.join("\n")))
}

pub(super) async fn acp_compact_text_for_prompt(
    store: &AppStore,
    session_id: &str,
    text: &str,
) -> AppResult<Option<String>> {
    let Some((command, args)) = acp_slash_command_parts(text) else {
        return Ok(None);
    };
    if command != "compact" {
        return Ok(None);
    }
    let result = async {
        let conversation = store.conversation(session_id)?;
        let persona = store.persona(conversation.persona_id.as_deref())?;
        let mut agent = store.agent(Some(&conversation.agent_id))?;
        super::apply_acp_session_mcp_scope(store, &conversation, &mut agent)?;
        super::handle_compact_control_command(store, &conversation, &persona, &agent, &args).await
    }
    .await;
    Ok(Some(match result {
        Ok(text) => text,
        Err(error) => format!("Compression failed: {error}"),
    }))
}

pub(super) fn acp_tools_text_for_prompt(
    store: &AppStore,
    session_id: &str,
    text: &str,
) -> AppResult<Option<String>> {
    let Some((command, _args)) = acp_slash_command_parts(text) else {
        return Ok(None);
    };
    if command != "tools" {
        return Ok(None);
    }
    let Ok(conversation) = store.conversation(session_id) else {
        return Ok(Some("No tools available.".into()));
    };
    let agent = store.agent(Some(&conversation.agent_id))?;
    let tools = super::visible_tool_definitions_for_agent(
        store,
        &agent,
        super::ToolExecutionContext::Interactive,
    )?;
    if tools.is_empty() {
        return Ok(Some("No tools available.".into()));
    }
    let mut lines = vec![format!("Available tools ({}):", tools.len())];
    for tool in tools {
        let description = acp_truncate_tool_description(&tool.description.replace('\n', " "));
        lines.push(format!("  {}: {}", tool.name, description));
    }
    Ok(Some(lines.join("\n")))
}

pub(super) fn acp_slash_command_parts(text: &str) -> Option<(String, String)> {
    let trimmed = text.trim();
    let rest = trimmed
        .strip_prefix('/')
        .or_else(|| trimmed.strip_prefix('／'))?;
    let mut parts = rest.splitn(2, char::is_whitespace);
    let command = parts.next().unwrap_or("").to_ascii_lowercase();
    let args = parts.next().unwrap_or("").trim().to_string();
    Some((command, args))
}

fn acp_session_runtime_provider_model(
    store: &AppStore,
    session_id: &str,
) -> AppResult<(Option<String>, Option<String>)> {
    let conversation = store.conversation(session_id)?;
    let agent = store.agent(Some(&conversation.agent_id)).ok();
    let runtime = acp_session_runtime_config_for_store(store, session_id)?;
    let provider = runtime
        .as_ref()
        .and_then(|runtime| runtime.provider.clone())
        .or_else(|| agent.as_ref().map(|agent| agent.llm_provider.clone()))
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let model = runtime
        .as_ref()
        .and_then(|runtime| runtime.model.clone())
        .or_else(|| agent.as_ref().map(|agent| agent.llm_model.clone()))
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    Ok((provider, model))
}

fn acp_format_count(value: usize) -> String {
    let text = value.to_string();
    let mut out = String::new();
    for (idx, ch) in text.chars().rev().enumerate() {
        if idx > 0 && idx % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out.chars().rev().collect()
}

fn acp_truncate_tool_description(description: &str) -> String {
    const LIMIT: usize = 80;
    const PREFIX: usize = 77;
    if description.chars().count() <= LIMIT {
        return description.to_string();
    }
    let mut truncated = description.chars().take(PREFIX).collect::<String>();
    truncated.push_str("...");
    truncated
}
