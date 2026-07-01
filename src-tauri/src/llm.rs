use std::sync::Arc;

use std::time::Duration;

use reqwest::header::HeaderMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{
    error::{AppError, AppResult},
    hermes_auth::{resolve_bitwarden_secret, resolve_hermes_runtime_credential},
    models::{ChatMessage, LlmProvider, Persona, ToolDefinition},
};

#[path = "llm/anthropic_transport.rs"]
mod anthropic_transport;
#[path = "llm/bedrock_transport.rs"]
mod bedrock_transport;
#[path = "llm/gemini_transport.rs"]
mod gemini_transport;
#[path = "llm/openai_transport.rs"]
mod openai_transport;
#[path = "llm/responses_transport.rs"]
mod responses_transport;
#[path = "llm/tool_schemas.rs"]
mod tool_schemas;

use anthropic_transport::*;
use bedrock_transport::*;
use gemini_transport::*;
use openai_transport::*;
use responses_transport::*;
use tool_schemas::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone)]
pub(super) struct ToolReplayMessage {
    pub call_id: String,
    pub name: String,
    pub arguments: Value,
    pub content: String,
    pub ok: bool,
    pub extra_content: Option<Value>,
}

#[derive(Debug, Clone)]
pub struct LlmFailoverAttempt {
    pub provider_id: String,
    pub model: String,
    pub kind: String,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct LlmCredentialBinding {
    pub provider: LlmProvider,
    pub source: Option<String>,
}

#[derive(Debug, Clone)]
pub struct LlmReply {
    pub content: String,
    pub prompt_tokens: usize,
    pub completion_tokens: usize,
    pub cache_read_tokens: usize,
    pub cache_write_tokens: usize,
    pub reasoning_tokens: usize,
    pub provider_id: Option<String>,
    pub provider_type: Option<String>,
    pub model: Option<String>,
    pub base_url: Option<String>,
    pub estimated_cost_usd: Option<f64>,
    pub cost_status: Option<String>,
    pub cost_source: Option<String>,
    pub rate_limit_state: Option<Value>,
    pub transport_diagnostics: Option<Value>,
    pub finish_reason: Option<String>,
    pub provider_data: Option<Value>,
    pub failover_attempts: Vec<LlmFailoverAttempt>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LlmStreamDeltaKind {
    Answer,
    Thinking,
}

pub type LlmDeltaCallback =
    Arc<dyn Fn(LlmStreamDeltaKind, &str) -> AppResult<()> + Send + Sync>;

#[derive(Clone)]
pub struct LlmCallOptions {
    pub responses_reasoning_replay_enabled: bool,
    pub fast_mode_enabled: bool,
    pub thinking_enabled: bool,
    pub stream_delta_callback: Option<LlmDeltaCallback>,
}

impl std::fmt::Debug for LlmCallOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LlmCallOptions")
            .field(
                "responses_reasoning_replay_enabled",
                &self.responses_reasoning_replay_enabled,
            )
            .field("fast_mode_enabled", &self.fast_mode_enabled)
            .field("thinking_enabled", &self.thinking_enabled)
            .field(
                "stream_delta_callback",
                &self.stream_delta_callback.as_ref().map(|_| true),
            )
            .finish()
    }
}

impl Default for LlmCallOptions {
    fn default() -> Self {
        Self {
            responses_reasoning_replay_enabled: true,
            fast_mode_enabled: false,
            thinking_enabled: true,
            stream_delta_callback: None,
        }
    }
}

pub async fn complete_chat(
    provider: &LlmProvider,
    persona: &Persona,
    system_prompt: String,
    history: Vec<ChatMessage>,
    user_content: &str,
    native_tools: Option<&[ToolDefinition]>,
) -> AppResult<LlmReply> {
    complete_chat_with_options(
        provider,
        persona,
        system_prompt,
        history,
        user_content,
        native_tools,
        &LlmCallOptions::default(),
    )
    .await
}

pub async fn complete_chat_with_options(
    provider: &LlmProvider,
    persona: &Persona,
    system_prompt: String,
    history: Vec<ChatMessage>,
    user_content: &str,
    native_tools: Option<&[ToolDefinition]>,
    options: &LlmCallOptions,
) -> AppResult<LlmReply> {
    if provider.provider_type == "echo"
        || (provider_base_url(provider).trim().is_empty()
            && !is_responses_compatible(provider)
            && !is_bedrock_compatible(provider)
            && !is_gemini_compatible(provider)
            && !is_anthropic_compatible(provider))
    {
        return Ok(echo_reply(user_content, history.len()));
    }

    if is_responses_compatible(provider) {
        return complete_responses_compatible(
            provider,
            persona,
            system_prompt,
            history,
            native_tools,
            options,
        )
        .await;
    }
    if is_bedrock_compatible(provider) {
        return complete_bedrock_compatible(
            provider,
            persona,
            system_prompt,
            history,
            native_tools,
            options,
        )
        .await;
    }
    if is_anthropic_compatible(provider) {
        return complete_anthropic_compatible(
            provider,
            persona,
            system_prompt,
            history,
            native_tools,
            options,
        )
        .await;
    }
    if is_gemini_compatible(provider) {
        return complete_gemini_compatible(
            provider,
            persona,
            system_prompt,
            history,
            native_tools,
            options,
        )
        .await;
    }

    complete_openai_compatible(
        provider,
        persona,
        system_prompt,
        history,
        native_tools,
        options,
    )
    .await
}

pub fn provider_request_timeout_duration(provider: &LlmProvider, model: &str) -> Duration {
    let seconds = provider_request_timeout_seconds(provider, model)
        .unwrap_or(provider.timeout_seconds as f64)
        .max(1.0);
    Duration::from_secs_f64(seconds)
}

pub fn provider_request_timeout_seconds(provider: &LlmProvider, model: &str) -> Option<f64> {
    if let Some(timeout) = provider_model_timeout(provider, model, "timeoutSeconds")
        .or_else(|| provider_model_timeout(provider, model, "timeout_seconds"))
    {
        return Some(timeout);
    }
    coerce_positive_timeout(provider.request_timeout_seconds)
}

pub fn provider_stale_timeout_seconds(provider: &LlmProvider, model: &str) -> Option<f64> {
    if let Some(timeout) = provider_model_timeout(provider, model, "staleTimeoutSeconds")
        .or_else(|| provider_model_timeout(provider, model, "stale_timeout_seconds"))
    {
        return Some(timeout);
    }
    coerce_positive_timeout(provider.stale_timeout_seconds)
}

pub fn provider_stream_stale_timeout_duration(
    provider: &LlmProvider,
    model: &str,
) -> Option<Duration> {
    provider_stale_timeout_seconds(provider, model)
        .filter(|seconds| seconds.is_finite() && *seconds > 0.0)
        .map(Duration::from_secs_f64)
}

pub async fn send_llm_request_with_stale_timeout(
    request: reqwest::RequestBuilder,
    provider: &LlmProvider,
    model: &str,
    label: &str,
) -> AppResult<reqwest::Response> {
    let send = request.send();
    if let Some(timeout) = provider_stream_stale_timeout_duration(provider, model) {
        tokio::time::timeout(timeout, send).await.map_err(|_| {
            AppError::Llm(format!(
                "{label} stale: no provider response for {}s",
                timeout.as_secs_f64()
            ))
        })?
    } else {
        send.await
    }
    .map_err(|e| AppError::Llm(e.to_string()))
}

fn provider_model_timeout(provider: &LlmProvider, model: &str, key: &str) -> Option<f64> {
    let model = model.trim();
    if model.is_empty() {
        return None;
    }
    let model_config = provider.models.as_object()?.get(model)?.as_object()?;
    coerce_timeout_value(model_config.get(key))
}

fn coerce_positive_timeout(timeout: Option<f64>) -> Option<f64> {
    timeout.filter(|timeout| timeout.is_finite() && *timeout > 0.0)
}

fn coerce_timeout_value(value: Option<&Value>) -> Option<f64> {
    match value? {
        Value::Number(number) => coerce_positive_timeout(number.as_f64()),
        Value::String(text) => text.trim().parse::<f64>().ok().and_then(|timeout| {
            if timeout.is_finite() && timeout > 0.0 {
                Some(timeout)
            } else {
                None
            }
        }),
        _ => None,
    }
}

pub(super) fn provider_supports_responses_thinking(provider: &LlmProvider) -> bool {
    if provider_supports_chat_completions_fallback_for_responses(provider) {
        return false;
    }
    let base = provider_base_url(provider).to_ascii_lowercase();
    let provider_type = provider.provider_type.to_ascii_lowercase();
    let preset = provider
        .preset
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let id = provider.id.to_ascii_lowercase();
    let haystack = format!("{id} {provider_type} {preset} {base}");
    haystack.contains("openai")
        || haystack.contains("chatgpt.com")
        || haystack.contains("x.ai")
        || haystack.contains("api.x.ai")
}

pub(super) fn provider_supports_chat_completions_fallback_for_responses(provider: &LlmProvider) -> bool {
    let base = provider_base_url(provider).to_ascii_lowercase();
    let provider_type = provider.provider_type.to_ascii_lowercase();
    let preset = provider
        .preset
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let id = provider.id.to_ascii_lowercase();
    let model = provider.model.to_ascii_lowercase();
    let haystack = format!("{id} {provider_type} {preset} {base} {model}");
    haystack.contains("deepseek") || base.contains("api.deepseek.com")
}

pub(super) fn provider_thinking_enabled(provider: &LlmProvider) -> bool {
    provider
        .models
        .pointer("/__provider/thinkingEnabled")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

pub(super) fn strip_thinking_cards_from_visible_content(
    content: &str,
    provider_data: &Option<Value>,
) -> String {
    let mut output = content.to_string();
    for summary in thinking_card_summaries(provider_data) {
        output = strip_exact_thinking_summary(&output, &summary);
    }
    output
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
        .replace("\n\n\n", "\n\n")
        .trim()
        .to_string()
}

fn thinking_card_summaries(provider_data: &Option<Value>) -> Vec<String> {
    let Some(data) = provider_data.as_ref() else {
        return Vec::new();
    };
    let mut summaries = Vec::new();
    collect_thinking_card_summaries(data.get("thinkingCards"), &mut summaries);
    collect_thinking_card_summaries(data.pointer("/responses/thinkingCards"), &mut summaries);
    collect_thinking_card_summaries(data.pointer("/anthropic/thinkingCards"), &mut summaries);
    summaries
}

fn collect_thinking_card_summaries(value: Option<&Value>, summaries: &mut Vec<String>) {
    let Some(cards) = value.and_then(Value::as_array) else {
        return;
    };
    for card in cards {
        let Some(summary) = card
            .get("summary")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        if summary.chars().count() >= 8 && !summaries.iter().any(|item| item == summary) {
            summaries.push(summary.to_string());
        }
    }
}

fn strip_exact_thinking_summary(content: &str, summary: &str) -> String {
    let summary = summary.trim();
    if summary.is_empty() {
        return content.to_string();
    }
    if content.trim() == summary {
        return String::new();
    }
    let mut output = content.to_string();
    while let Some(index) = output.find(summary) {
        let end = index + summary.len();
        output.replace_range(index..end, "");
    }
    output
}

pub async fn complete_text_prompt(
    provider: &LlmProvider,
    persona: &Persona,
    system_prompt: String,
    user_prompt: String,
) -> AppResult<LlmReply> {
    let message = ChatMessage::new(
        "__planner__".into(),
        "user",
        user_prompt.clone(),
        "internal",
    );
    complete_chat(
        provider,
        persona,
        system_prompt,
        vec![message],
        &user_prompt,
        None,
    )
    .await
}

#[derive(Debug, Clone)]
struct PromptCachePolicy {
    native_layout: bool,
    ttl: String,
}

fn build_openai_wire_messages(
    system_prompt: String,
    history: Vec<ChatMessage>,
    cache_policy: Option<&PromptCachePolicy>,
) -> Vec<Value> {
    build_openai_wire_messages_with_tool_name_map(
        system_prompt,
        history,
        cache_policy,
        &serde_json::Map::new(),
    )
}

fn build_openai_wire_messages_with_tool_name_map(
    system_prompt: String,
    history: Vec<ChatMessage>,
    cache_policy: Option<&PromptCachePolicy>,
    tool_name_map: &serde_json::Map<String, Value>,
) -> Vec<Value> {
    let mut system_content = sanitize_provider_text(&system_prompt);
    let mut conversation_messages = Vec::new();
    for item in history {
        if item.role == "system" {
            let content = sanitize_provider_text(&item.content);
            if !content.trim().is_empty() {
                if !system_content.trim().is_empty() {
                    system_content.push_str("\n\n");
                }
                system_content.push_str(&content);
            }
            continue;
        }
        if let Some(mut tool_replay) = tool_replay_message(&item) {
            tool_replay.name =
                safe_provider_tool_name_for_original(&tool_replay.name, tool_name_map);
            conversation_messages.push(openai_assistant_tool_call_message(&tool_replay));
            conversation_messages.push(openai_tool_result_message(&tool_replay));
            continue;
        }
        if let Some(content) = openai_provider_user_content(&item) {
            conversation_messages.push(json!({
                "role": item.role,
                "content": content,
            }));
            continue;
        }
        let provider_replay = openai_provider_replay_fields(&item);
        if let Some(message) = sanitized_wire_message(item, false) {
            if provider_replay.is_empty() {
                push_openai_text_message(
                    &mut conversation_messages,
                    &message.role,
                    &message.content,
                );
            } else {
                conversation_messages.push(openai_text_message_with_provider_fields(
                    &message.role,
                    &message.content,
                    provider_replay,
                ));
            }
        }
    }

    let mut system = json!({
        "role": "system",
        "content": system_content,
    });
    if let Some(policy) = cache_policy.filter(|policy| !policy.native_layout) {
        system["cache_control"] = cache_control_value(policy);
    }
    let mut messages = Vec::with_capacity(conversation_messages.len() + 1);
    messages.push(system);
    messages.extend(conversation_messages);
    messages
}

fn push_openai_text_message(messages: &mut Vec<Value>, role: &str, content: &str) {
    let content = sanitize_provider_text(content);
    if content.trim().is_empty() {
        return;
    }
    if let Some(previous) = messages.last_mut() {
        if previous.get("role").and_then(Value::as_str) == Some(role)
            && previous.get("tool_calls").is_none()
        {
            if let Some(previous_content) = previous.get("content").and_then(Value::as_str) {
                let separator = if previous_content.trim().is_empty() || content.trim().is_empty() {
                    ""
                } else {
                    "\n\n"
                };
                previous["content"] = json!(format!("{previous_content}{separator}{content}"));
                return;
            }
        }
    }
    messages.push(json!({
        "role": role,
        "content": content,
    }));
}

fn openai_provider_user_content(message: &ChatMessage) -> Option<Value> {
    if message.role != "user" {
        return None;
    }
    let provider_data = message.provider_data.as_ref()?;
    let openai = provider_data.get("openai").unwrap_or(provider_data);
    let content = openai
        .get("content")
        .or_else(|| openai.get("contentParts"))
        .or_else(|| openai.get("content_parts"))?;
    match content {
        Value::Array(items) if !items.is_empty() => Some(Value::Array(items.clone())),
        _ => None,
    }
}

fn openai_text_message_with_provider_fields(
    role: &str,
    content: &str,
    provider_fields: Vec<(&'static str, Value)>,
) -> Value {
    let mut message = json!({
        "role": role,
        "content": sanitize_provider_text(content),
    });
    for (key, value) in provider_fields {
        message[key] = value;
    }
    message
}

fn openai_provider_replay_fields(message: &ChatMessage) -> Vec<(&'static str, Value)> {
    if message.role != "assistant" {
        return Vec::new();
    }
    let Some(provider_data) = message.provider_data.as_ref() else {
        return Vec::new();
    };
    let openai = provider_data.get("openai").unwrap_or(provider_data);
    let mut fields = Vec::new();
    for key in ["reasoning_content", "reasoning", "reasoning_details"] {
        if let Some(value) = openai
            .get(key)
            .filter(|value| provider_replay_value_present(value))
        {
            fields.push((key, value.clone()));
        }
    }
    fields
}

fn provider_replay_value_present(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::String(text) => !text.trim().is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(items) => !items.is_empty(),
        _ => true,
    }
}

pub(super) fn tool_replay_message(item: &ChatMessage) -> Option<ToolReplayMessage> {
    if item.role != "tool" {
        return None;
    }
    let value = serde_json::from_str::<Value>(&item.content).ok()?;
    if value.get("type").and_then(Value::as_str) != Some("toolEvent") {
        return None;
    }
    let event = value.get("event")?;
    let name = event
        .get("toolName")
        .or_else(|| event.get("tool_name"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())?
        .to_string();
    let mut arguments = event
        .pointer("/raw/payload")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let provider_tool_call = remove_provider_tool_call_metadata(&mut arguments);
    let call_id = provider_tool_call
        .as_ref()
        .and_then(|metadata| {
            metadata
                .get("id")
                .or_else(|| metadata.get("call_id"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|id| !id.is_empty())
                .map(str::to_string)
        })
        .or_else(|| {
            event
                .get("callId")
                .or_else(|| event.get("call_id"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|id| !id.is_empty())
                .map(str::to_string)
        })
        .unwrap_or_else(|| item.id.clone());
    let extra_content = provider_tool_call
        .as_ref()
        .and_then(|metadata| metadata.get("extra_content").cloned());
    let ok = event.get("ok").and_then(Value::as_bool).unwrap_or(true);
    let content = event
        .get("text")
        .or_else(|| event.get("error"))
        .or_else(|| event.get("summary"))
        .and_then(Value::as_str)
        .map(sanitize_provider_text)
        .filter(|content| !content.trim().is_empty())
        .unwrap_or_else(|| {
            if ok {
                "Tool completed without textual output.".into()
            } else {
                "Tool failed without textual output.".into()
            }
        });
    Some(ToolReplayMessage {
        call_id,
        name,
        arguments,
        content,
        ok,
        extra_content,
    })
}

const PROVIDER_TOOL_CALL_META_KEY: &str = "__agentProviderToolCall";
const TOOL_CALL_ARGUMENTS_CORRUPTION_KEY: &str = "__agentToolArgumentsCorruption";
const TOOL_CALL_ARGUMENTS_CORRUPTION_MARKER: &str =
    "Tool call arguments were corrupted and replaced with an empty object. Reissue the tool call with valid JSON arguments.";

fn remove_provider_tool_call_metadata(arguments: &mut Value) -> Option<Value> {
    arguments
        .as_object_mut()
        .and_then(|object| object.remove(PROVIDER_TOOL_CALL_META_KEY))
        .filter(|value| value.is_object())
}

pub(super) fn normalize_provider_tool_arguments(arguments: Value, tool_name: &str) -> Value {
    let Some(raw) = arguments.as_str() else {
        return arguments;
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Value::String("{}".into());
    }
    if trimmed == "None" {
        return Value::String("{}".into());
    }
    if valid_json_text(trimmed) {
        return arguments;
    }
    if let Some(repaired) = repair_tool_arguments_json(trimmed) {
        return Value::String(repaired);
    }
    Value::String(
        json!({
            TOOL_CALL_ARGUMENTS_CORRUPTION_KEY: {
                "tool": tool_name,
                "message": TOOL_CALL_ARGUMENTS_CORRUPTION_MARKER,
                "originalPreview": response_preview(trimmed)
            }
        })
        .to_string(),
    )
}

fn repair_tool_arguments_json(raw: &str) -> Option<String> {
    let mut fixed = strip_trailing_json_commas(raw);
    let open_curly = fixed.matches('{').count();
    let close_curly = fixed.matches('}').count();
    if open_curly > close_curly {
        fixed.push_str(&"}".repeat(open_curly - close_curly));
    }
    let open_bracket = fixed.matches('[').count();
    let close_bracket = fixed.matches(']').count();
    if open_bracket > close_bracket {
        fixed.push_str(&"]".repeat(open_bracket - close_bracket));
    }
    for _ in 0..50 {
        if valid_json_text(&fixed) {
            return normalize_json_text(&fixed);
        }
        let trimmed = fixed.trim_end();
        if trimmed.ends_with('}') && trimmed.matches('}').count() > trimmed.matches('{').count() {
            fixed.truncate(trimmed.len().saturating_sub(1));
            continue;
        }
        if trimmed.ends_with(']') && trimmed.matches(']').count() > trimmed.matches('[').count() {
            fixed.truncate(trimmed.len().saturating_sub(1));
            continue;
        }
        break;
    }
    let escaped = escape_invalid_chars_in_json_strings(&fixed);
    if escaped != fixed && valid_json_text(&escaped) {
        return normalize_json_text(&escaped);
    }
    None
}

fn strip_trailing_json_commas(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    let mut in_string = false;
    let mut escaped = false;
    while let Some(ch) = chars.next() {
        if in_string {
            out.push(ch);
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        if ch == '"' {
            in_string = true;
            out.push(ch);
            continue;
        }
        if ch == ',' {
            let mut lookahead = chars.clone();
            while matches!(lookahead.peek(), Some(next) if next.is_whitespace()) {
                lookahead.next();
            }
            if matches!(lookahead.peek(), Some('}' | ']')) {
                continue;
            }
        }
        out.push(ch);
    }
    out
}

fn escape_invalid_chars_in_json_strings(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut in_string = false;
    let mut escaped = false;
    for ch in raw.chars() {
        if in_string {
            if escaped {
                out.push(ch);
                escaped = false;
                continue;
            }
            if ch == '\\' {
                out.push(ch);
                escaped = true;
                continue;
            }
            if ch == '"' {
                out.push(ch);
                in_string = false;
                continue;
            }
            if (ch as u32) < 0x20 {
                out.push_str(&format!("\\u{:04x}", ch as u32));
            } else {
                out.push(ch);
            }
            continue;
        }
        if ch == '"' {
            in_string = true;
        }
        out.push(ch);
    }
    out
}

fn valid_json_text(raw: &str) -> bool {
    serde_json::from_str::<Value>(raw).is_ok()
}

fn normalize_json_text(raw: &str) -> Option<String> {
    serde_json::from_str::<Value>(raw)
        .ok()
        .and_then(|value| serde_json::to_string(&value).ok())
}

pub(super) fn openai_assistant_tool_call_message(tool: &ToolReplayMessage) -> Value {
    let mut message = json!({
        "role": "assistant",
        "content": "",
        "tool_calls": [{
            "id": tool.call_id,
            "type": "function",
            "function": {
                "name": tool.name,
                "arguments": tool.arguments.to_string(),
            }
        }]
    });
    if let Some(extra_content) = tool.extra_content.as_ref() {
        message["tool_calls"][0]["extra_content"] = extra_content.clone();
    }
    message
}

pub(super) fn openai_tool_result_message(tool: &ToolReplayMessage) -> Value {
    json!({
        "role": "tool",
        "tool_call_id": tool.call_id,
        "name": tool.name,
        "content": tool.content,
    })
}

fn sanitized_wire_message(item: ChatMessage, anthropic: bool) -> Option<WireMessage> {
    let role = match item.role.as_str() {
        "user" => "user",
        "assistant" => {
            if anthropic {
                "assistant"
            } else {
                "assistant"
            }
        }
        _ => return None,
    };
    let content = sanitize_provider_text(&item.content);
    if content.trim().is_empty() {
        return None;
    }
    Some(WireMessage {
        role: role.into(),
        content,
    })
}

fn merge_adjacent_wire_messages(messages: Vec<WireMessage>) -> Vec<WireMessage> {
    let mut merged: Vec<WireMessage> = Vec::new();
    for message in messages {
        if let Some(previous) = merged.last_mut() {
            if previous.role == message.role {
                if !previous.content.trim().is_empty() && !message.content.trim().is_empty() {
                    previous.content.push_str("\n\n");
                }
                previous.content.push_str(&message.content);
                continue;
            }
        }
        merged.push(message);
    }
    merged
}

fn sanitize_provider_text(content: &str) -> String {
    let scrubbed = scrub_reasoning_blocks(content);
    scrubbed
        .chars()
        .map(|ch| {
            if ch.is_control() && !matches!(ch, '\n' | '\r' | '\t') {
                '\u{fffd}'
            } else {
                ch
            }
        })
        .collect::<String>()
        .trim()
        .to_string()
}

fn cache_control_value(policy: &PromptCachePolicy) -> Value {
    let mut value = json!({"type": "ephemeral"});
    if policy.ttl == "1h" {
        value["ttl"] = json!("1h");
    }
    value
}

fn prompt_cache_policy(provider: &LlmProvider, model: &str) -> Option<PromptCachePolicy> {
    let mode = provider.prompt_cache_mode.trim().to_ascii_lowercase();
    if mode == "off" || mode == "disabled" || mode == "false" {
        return None;
    }
    let auto = auto_prompt_cache_policy(provider, model);
    let forced = matches!(mode.as_str(), "on" | "enabled" | "true" | "force");
    if !forced && auto.is_none() {
        return None;
    }
    let layout = provider.prompt_cache_layout.trim().to_ascii_lowercase();
    let native_layout = match layout.as_str() {
        "native" | "anthropic" | "content" => true,
        "envelope" | "openai" | "openrouter" => false,
        _ => auto.unwrap_or_else(|| is_anthropic_compatible(provider)),
    };
    Some(PromptCachePolicy {
        native_layout,
        ttl: normalized_prompt_cache_ttl(&provider.prompt_cache_ttl),
    })
}

fn auto_prompt_cache_policy(provider: &LlmProvider, model: &str) -> Option<bool> {
    let model_lower = model.to_ascii_lowercase();
    let provider_type = provider.provider_type.to_ascii_lowercase();
    let preset = provider
        .preset
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let base_url = provider_base_url(provider).to_ascii_lowercase();
    let is_claude = model_lower.contains("claude");
    let is_qwen = model_lower.contains("qwen");
    let anthropic_wire = is_anthropic_compatible(provider);
    let native_anthropic = anthropic_wire
        && (provider_type == "anthropic" || host_matches(&base_url, "api.anthropic.com"));
    if native_anthropic {
        return Some(true);
    }
    if (host_matches(&base_url, "openrouter.ai") || base_url.contains("nousresearch"))
        && (is_claude || is_qwen)
    {
        return Some(false);
    }
    if anthropic_wire && is_claude {
        return Some(true);
    }
    if anthropic_wire
        && (provider_type.contains("minimax")
            || preset.contains("minimax")
            || host_matches(&base_url, "api.minimax.io")
            || host_matches(&base_url, "api.minimaxi.com"))
    {
        return Some(true);
    }
    if is_qwen
        && (provider_type.contains("opencode")
            || provider_type.contains("alibaba")
            || preset.contains("opencode")
            || preset.contains("alibaba"))
    {
        return Some(false);
    }
    None
}

fn normalized_prompt_cache_ttl(value: &str) -> String {
    if value.trim().eq_ignore_ascii_case("1h") {
        "1h".into()
    } else {
        "5m".into()
    }
}

fn host_matches(base_url: &str, expected: &str) -> bool {
    reqwest::Url::parse(base_url)
        .ok()
        .and_then(|url| {
            url.host_str()
                .map(|host| host.eq_ignore_ascii_case(expected))
        })
        .unwrap_or(false)
}

fn chat_url(provider: &LlmProvider) -> String {
    let base_url = provider_base_url(provider);
    let base = base_url.trim().trim_end_matches('/');
    if provider.append_chat_path && is_ollama_root_url(base) {
        return format!("{base}/v1/chat/completions");
    }
    if provider.append_chat_path && !base.ends_with("/chat/completions") {
        format!("{base}/chat/completions")
    } else {
        base.to_string()
    }
}

fn is_ollama_root_url(base: &str) -> bool {
    let value = base.to_lowercase();
    (value.contains("127.0.0.1:11434")
        || value.contains("localhost:11434")
        || value.contains("[::1]:11434"))
        && !value.ends_with("/v1")
        && !value.ends_with("/api")
        && !value.ends_with("/chat/completions")
}

fn provider_base_url(provider: &LlmProvider) -> String {
    let configured = provider.base_url.trim();
    if !configured.is_empty() {
        return configured.to_string();
    }
    provider_base_url_env_candidates(provider)
        .into_iter()
        .find_map(|env_name| {
            std::env::var(env_name)
                .ok()
                .map(|value| value.trim().trim_end_matches('/').to_string())
                .filter(|value| !value.is_empty())
        })
        .or_else(|| {
            resolve_hermes_runtime_credential(provider)
                .and_then(|credential| credential.base_url)
                .map(|value| value.trim().trim_end_matches('/').to_string())
                .filter(|value| !value.is_empty())
        })
        .or_else(|| provider_default_base_url(provider).map(str::to_string))
        .unwrap_or_default()
}

pub fn bind_runtime_credential_for_attempt(provider: &LlmProvider) -> LlmCredentialBinding {
    if provider_has_inline_or_env_api_key(provider) || provider_env_api_key(provider).is_some() {
        return LlmCredentialBinding {
            provider: provider.clone(),
            source: None,
        };
    }
    let Some(credential) = resolve_hermes_runtime_credential(provider) else {
        return LlmCredentialBinding {
            provider: provider.clone(),
            source: None,
        };
    };
    let mut provider = provider.clone();
    provider.api_key = Some(credential.api_key);
    if provider.base_url.trim().is_empty() {
        if let Some(base_url) = credential.base_url.as_deref().map(str::trim) {
            if !base_url.is_empty() {
                provider.base_url = base_url.trim_end_matches('/').to_string();
            }
        }
    }
    LlmCredentialBinding {
        provider,
        source: Some(credential.source),
    }
}

fn provider_base_url_env_candidates(provider: &LlmProvider) -> Vec<&'static str> {
    let id = provider.id.to_ascii_lowercase();
    let provider_type = provider.provider_type.to_ascii_lowercase();
    let preset = provider
        .preset
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let model = provider.model.to_ascii_lowercase();
    let haystack = format!("{id} {provider_type} {preset} {model}");

    if haystack.contains("openrouter") {
        return vec!["OPENROUTER_BASE_URL"];
    }
    if haystack.contains("anthropic") || model.contains("claude") {
        return vec!["ANTHROPIC_BASE_URL"];
    }
    if haystack.contains("gemini") || haystack.contains("google") {
        return vec!["GEMINI_BASE_URL"];
    }
    if haystack.contains("kimi") || haystack.contains("moonshot") {
        return vec!["KIMI_BASE_URL"];
    }
    if haystack.contains("minimax") {
        return vec!["MINIMAX_BASE_URL", "MINIMAX_CN_BASE_URL"];
    }
    if haystack.contains("xai") || haystack.contains("x.ai") || haystack.contains("grok") {
        return vec!["HERMES_XAI_BASE_URL", "XAI_BASE_URL"];
    }
    if haystack.contains("zai") || haystack.contains("z.ai") || haystack.contains("glm") {
        return vec!["GLM_BASE_URL"];
    }
    if haystack.contains("deepseek") {
        return vec!["DEEPSEEK_BASE_URL"];
    }
    if haystack.contains("groq") {
        return vec!["GROQ_BASE_URL"];
    }
    if haystack.contains("mistral") {
        return vec!["MISTRAL_BASE_URL"];
    }
    if haystack.contains("dashscope") || haystack.contains("alibaba") || haystack.contains("qwen") {
        return vec![
            "HERMES_QWEN_BASE_URL",
            "DASHSCOPE_BASE_URL",
            "ALIBABA_CODING_PLAN_BASE_URL",
        ];
    }
    if haystack.contains("stepfun") || haystack.contains("step-plan") {
        return vec!["STEPFUN_BASE_URL"];
    }
    if haystack.contains("copilot-acp") {
        return vec!["COPILOT_ACP_BASE_URL"];
    }
    if haystack.contains("opencode-go") {
        return vec!["OPENCODE_GO_BASE_URL"];
    }
    if haystack.contains("opencode") {
        return vec!["OPENCODE_ZEN_BASE_URL"];
    }
    if haystack.contains("kilo") {
        return vec!["KILOCODE_BASE_URL"];
    }
    if haystack.contains("huggingface") || haystack.contains("hugging-face") {
        return vec!["HF_BASE_URL"];
    }
    if haystack.contains("novita") {
        return vec!["NOVITA_BASE_URL"];
    }
    if haystack.contains("nvidia") || haystack.contains("nemotron") {
        return vec!["NVIDIA_BASE_URL"];
    }
    if haystack.contains("xiaomi") || haystack.contains("mimo") {
        return vec!["XIAOMI_BASE_URL"];
    }
    if haystack.contains("tencent") || haystack.contains("tokenhub") {
        return vec!["TOKENHUB_BASE_URL"];
    }
    if haystack.contains("arcee") {
        return vec!["ARCEE_BASE_URL"];
    }
    if haystack.contains("gmi") {
        return vec!["GMI_BASE_URL"];
    }
    if haystack.contains("ollama") {
        return vec!["OLLAMA_BASE_URL"];
    }
    if haystack.contains("azure-foundry") {
        return vec!["AZURE_FOUNDRY_BASE_URL"];
    }
    if haystack.contains("lmstudio") || haystack.contains("lm-studio") {
        return vec!["LM_BASE_URL"];
    }
    if id.contains("openai") || preset.contains("openai") || provider_type == "openai" {
        return vec!["OPENAI_BASE_URL", "OPENROUTER_BASE_URL"];
    }
    Vec::new()
}

fn provider_default_base_url(provider: &LlmProvider) -> Option<&'static str> {
    let id = provider.id.to_ascii_lowercase();
    let provider_type = provider.provider_type.to_ascii_lowercase();
    let preset = provider
        .preset
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let model = provider.model.to_ascii_lowercase();
    let haystack = format!("{id} {provider_type} {preset} {model}");

    if haystack.contains("openrouter") {
        return Some("https://openrouter.ai/api/v1");
    }
    if haystack.contains("openai-codex") {
        return Some("https://chatgpt.com/backend-api/codex");
    }
    if id.contains("openai") || preset.contains("openai") {
        return Some("https://api.openai.com/v1");
    }
    if haystack.contains("nous") {
        return Some("https://inference-api.nousresearch.com/v1");
    }
    if haystack.contains("qwen-oauth") {
        return Some("https://portal.qwen.ai/v1");
    }
    if haystack.contains("minimax-oauth") {
        return Some("https://api.minimax.io/anthropic");
    }
    if haystack.contains("stepfun") || haystack.contains("step-plan") {
        return Some("https://api.stepfun.ai/step_plan/v1");
    }
    if haystack.contains("lmstudio") || haystack.contains("lm-studio") {
        return Some("http://127.0.0.1:1234/v1");
    }
    if haystack.contains("nvidia") || haystack.contains("nemotron") {
        return Some("https://integrate.api.nvidia.com/v1");
    }
    if haystack.contains("arcee") {
        return Some("https://api.arcee.ai/api/v1");
    }
    if haystack.contains("gmi") {
        return Some("https://api.gmi-serving.com/v1");
    }
    if haystack.contains("ollama-cloud") {
        return Some("https://ollama.com/v1");
    }
    if haystack.contains("xai") || haystack.contains("x.ai") || haystack.contains("grok") {
        return Some("https://api.x.ai/v1");
    }
    if haystack.contains("deepseek") {
        return Some("https://api.deepseek.com/v1");
    }
    if haystack.contains("groq") {
        return Some("https://api.groq.com/openai/v1");
    }
    if haystack.contains("mistral") {
        return Some("https://api.mistral.ai/v1");
    }
    if id == "gemini"
        || id == "google"
        || id.contains("google-gemini-cli")
        || preset.contains("gemini")
        || preset.contains("google")
    {
        return None;
    }
    if haystack.contains("anthropic") || haystack.contains("claude") {
        return Some("https://api.anthropic.com");
    }
    None
}

fn resolve_api_key(provider: &LlmProvider) -> Option<String> {
    if let Some(key) = provider
        .api_key
        .as_ref()
        .map(|v| v.trim())
        .filter(|v| usable_secret_value(v))
    {
        return Some(key.to_string());
    }
    let env_name = provider.api_key_env.trim();
    let configured_env_key = if env_name.is_empty() {
        None
    } else if looks_like_inline_api_key(env_name) {
        usable_secret_value(env_name).then(|| env_name.to_string())
    } else {
        std::env::var(env_name)
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| usable_secret_value(v))
    };
    configured_env_key
        .or_else(|| provider_env_api_key(provider))
        .or_else(|| {
            resolve_hermes_runtime_credential(provider).map(|credential| credential.api_key)
        })
        .or_else(|| {
            let candidates = provider_api_key_env_candidates(provider);
            resolve_bitwarden_secret(&candidates)
        })
        .or_else(|| resolve_claude_code_oauth_token(provider))
}

fn provider_has_inline_or_env_api_key(provider: &LlmProvider) -> bool {
    if provider
        .api_key
        .as_ref()
        .map(|value| value.trim())
        .is_some_and(usable_secret_value)
    {
        return true;
    }
    let env_name = provider.api_key_env.trim();
    if env_name.is_empty() {
        return false;
    }
    if looks_like_inline_api_key(env_name) {
        return usable_secret_value(env_name);
    }
    std::env::var(env_name)
        .ok()
        .map(|value| value.trim().to_string())
        .is_some_and(|value| usable_secret_value(&value))
}

fn provider_env_api_key(provider: &LlmProvider) -> Option<String> {
    provider_api_key_env_candidates(provider)
        .into_iter()
        .find_map(|env_name| {
            std::env::var(env_name)
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| usable_secret_value(value))
        })
}

fn provider_api_key_env_candidates(provider: &LlmProvider) -> Vec<&'static str> {
    let id = provider.id.to_ascii_lowercase();
    let provider_type = provider.provider_type.to_ascii_lowercase();
    let preset = provider
        .preset
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let base = provider_base_url(provider).to_ascii_lowercase();
    let model = provider.model.to_ascii_lowercase();
    let haystack = format!("{id} {provider_type} {preset} {base} {model}");

    if haystack.contains("openrouter") {
        return vec!["OPENROUTER_API_KEY", "OPENAI_API_KEY"];
    }
    if haystack.contains("gemini") || haystack.contains("google") {
        return vec!["GOOGLE_API_KEY", "GEMINI_API_KEY"];
    }
    if haystack.contains("xiaomi")
        || haystack.contains("mimo")
        || haystack.contains("xiaomimimo.com")
    {
        return vec!["MIMO_API_KEY", "XIAOMI_API_KEY"];
    }
    if haystack.contains("anthropic") || model.contains("claude") {
        return vec![
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_TOKEN",
            "CLAUDE_CODE_OAUTH_TOKEN",
        ];
    }
    if haystack.contains("kimi") || haystack.contains("moonshot") {
        return vec![
            "KIMI_API_KEY",
            "KIMI_CODING_API_KEY",
            "KIMI_CN_API_KEY",
            "MOONSHOT_API_KEY",
        ];
    }
    if haystack.contains("minimax") {
        return vec!["MINIMAX_API_KEY", "MINIMAX_CN_API_KEY"];
    }
    if haystack.contains("xai") || haystack.contains("x.ai") || haystack.contains("grok") {
        return vec!["XAI_API_KEY"];
    }
    if haystack.contains("zai") || haystack.contains("z.ai") || haystack.contains("glm") {
        return vec!["GLM_API_KEY", "ZAI_API_KEY", "Z_AI_API_KEY"];
    }
    if haystack.contains("deepseek") {
        return vec!["DEEPSEEK_API_KEY"];
    }
    if haystack.contains("groq") {
        return vec!["GROQ_API_KEY"];
    }
    if haystack.contains("mistral") {
        return vec!["MISTRAL_API_KEY"];
    }
    if haystack.contains("stepfun") || haystack.contains("step-plan") {
        return vec!["STEPFUN_API_KEY"];
    }
    if haystack.contains("copilot") || haystack.contains("github") {
        return vec!["COPILOT_GITHUB_TOKEN", "GH_TOKEN", "GITHUB_TOKEN"];
    }
    if haystack.contains("opencode") {
        return vec!["OPENCODE_API_KEY"];
    }
    if haystack.contains("kilo") {
        return vec!["KILOCODE_API_KEY"];
    }
    if haystack.contains("huggingface") || haystack.contains("hugging-face") {
        return vec!["HF_TOKEN", "HF_API_KEY", "HUGGINGFACE_API_KEY"];
    }
    if haystack.contains("novita") {
        return vec!["NOVITA_API_KEY"];
    }
    if haystack.contains("nvidia") || haystack.contains("nemotron") {
        return vec!["NVIDIA_API_KEY"];
    }
    if haystack.contains("tencent") || haystack.contains("tokenhub") {
        return vec!["TOKENHUB_API_KEY"];
    }
    if haystack.contains("arcee") {
        return vec!["ARCEE_API_KEY"];
    }
    if haystack.contains("gmi") {
        return vec!["GMI_API_KEY"];
    }
    if haystack.contains("cohere") {
        return vec!["COHERE_API_KEY"];
    }
    if haystack.contains("dashscope") || haystack.contains("alibaba") || haystack.contains("qwen") {
        return vec!["DASHSCOPE_API_KEY", "ALIBABA_CODING_PLAN_API_KEY"];
    }
    if id.contains("openai")
        || preset.contains("openai")
        || base.contains("api.openai.com")
        || provider_type == "openai"
    {
        return vec!["OPENAI_API_KEY", "OPENROUTER_API_KEY"];
    }
    Vec::new()
}

fn resolve_claude_code_oauth_token(provider: &LlmProvider) -> Option<String> {
    if !provider_allows_claude_code_credentials(provider) {
        return None;
    }
    let path = home_dir()?.join(".claude").join(".credentials.json");
    let data = std::fs::read_to_string(path).ok()?;
    let payload = serde_json::from_str::<Value>(&data).ok()?;
    claude_code_oauth_token_from_credentials(&payload)
}

fn claude_code_oauth_token_from_credentials(payload: &Value) -> Option<String> {
    let oauth = payload.get("claudeAiOauth")?;
    let token = oauth
        .get("accessToken")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|token| usable_secret_value(token))?;
    if claude_code_credentials_expired(oauth) {
        return None;
    }
    Some(token.to_string())
}

fn provider_allows_claude_code_credentials(provider: &LlmProvider) -> bool {
    let provider_type = provider.provider_type.to_ascii_lowercase();
    let preset = provider
        .preset
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let provider_id = provider.id.to_ascii_lowercase();
    let base = provider_base_url(provider).trim().to_ascii_lowercase();
    let explicit_anthropic = provider_type == "anthropic"
        || preset.contains("anthropic")
        || provider_id.contains("anthropic");
    if !explicit_anthropic {
        return false;
    }
    base.is_empty() || host_matches(&base, "api.anthropic.com")
}

fn claude_code_credentials_expired(oauth: &Value) -> bool {
    let Some(expires_at) = oauth.get("expiresAt").and_then(Value::as_i64) else {
        return false;
    };
    if expires_at <= 0 {
        return false;
    }
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(i64::MAX);
    now_ms >= expires_at.saturating_sub(60_000)
}

fn home_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("USERPROFILE")
                .filter(|value| !value.is_empty())
                .map(std::path::PathBuf::from)
        })
}

fn looks_like_inline_api_key(value: &str) -> bool {
    value.starts_with("sk-")
        || value.starts_with("sk_")
        || value.starts_with("tp-")
        || value.starts_with("pk-")
        || value.starts_with("AIza")
        || value.len() > 48 && !value.chars().any(char::is_whitespace)
}

fn usable_secret_value(value: &str) -> bool {
    let normalized = value.trim().to_ascii_lowercase();
    !normalized.is_empty()
        && !matches!(
            normalized.as_str(),
            "*" | "**"
                | "***"
                | "changeme"
                | "your_api_key"
                | "your_api_key_here"
                | "your-api-key"
                | "placeholder"
                | "example"
                | "dummy"
                | "null"
                | "none"
        )
}

fn response_preview(text: &str) -> String {
    let clean = text.replace('\n', " ").replace('\r', " ");
    clean.chars().take(500).collect()
}

fn invalid_response_body_detail(text: &str, headers: &HeaderMap) -> String {
    let trimmed = text.trim_start();
    let body_kind = if text.trim().is_empty() {
        "empty"
    } else if trimmed.starts_with('{') || trimmed.starts_with('[') {
        "json_like"
    } else if trimmed.starts_with("<!DOCTYPE")
        || trimmed.starts_with("<!doctype")
        || trimmed.starts_with("<html")
        || trimmed.starts_with("<HTML")
    {
        "html"
    } else if trimmed.starts_with("data:") || trimmed.starts_with("event:") {
        "sse_like"
    } else {
        "text"
    };
    let content_type = header_text(headers, "content-type").unwrap_or_else(|| "unknown".into());
    format!(
        "bodyKind={body_kind}; bodyBytes={}; contentType={}; preview={}",
        text.as_bytes().len(),
        response_preview(&content_type),
        response_preview(text)
    )
}

fn scrub_reasoning_blocks(content: &str) -> String {
    let mut output = content.to_string();
    for tag in [
        "think",
        "thinking",
        "reasoning",
        "thought",
        "REASONING_SCRATCHPAD",
    ] {
        output = remove_closed_reasoning_pairs(&output, tag);
    }
    for tag in [
        "think",
        "thinking",
        "reasoning",
        "thought",
        "REASONING_SCRATCHPAD",
    ] {
        output = remove_unterminated_reasoning_blocks(&output, tag);
    }
    for tag in [
        "think",
        "thinking",
        "reasoning",
        "thought",
        "REASONING_SCRATCHPAD",
    ] {
        output = strip_reasoning_close_tags(&output, tag);
    }
    for tag in [
        "tool_calls",
        "tool_call",
        "tool_result",
        "function_calls",
        "function_call",
    ] {
        output = remove_closed_xml_blocks_with_attrs(&output, tag, false);
    }
    output = remove_closed_xml_blocks_with_attrs(&output, "function", true);
    for tag in [
        "tool_calls",
        "tool_call",
        "tool_result",
        "function_calls",
        "function_call",
        "function",
    ] {
        output = strip_xml_close_tags(&output, tag);
    }
    normalize_visible_assistant_text(&output)
}

fn remove_closed_reasoning_pairs(content: &str, tag: &str) -> String {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut output = content.to_string();
    loop {
        let Some(open_idx) = find_ascii_case_insensitive(&output, &open, 0) else {
            break;
        };
        let close_search_start = open_idx + open.len();
        let Some(close_idx) = find_ascii_case_insensitive(&output, &close, close_search_start)
        else {
            break;
        };
        output.replace_range(open_idx..close_idx + close.len(), "");
    }
    output
}

fn remove_unterminated_reasoning_blocks(content: &str, tag: &str) -> String {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut output = content.to_string();
    let mut cursor = 0usize;
    while let Some(open_idx) = find_ascii_case_insensitive(&output, &open, cursor) {
        if !reasoning_open_at_block_boundary(&output[..open_idx]) {
            cursor = open_idx + open.len();
            continue;
        }
        let close_search_start = open_idx + open.len();
        if let Some(close_idx) = find_ascii_case_insensitive(&output, &close, close_search_start) {
            output.replace_range(open_idx..close_idx + close.len(), "");
            cursor = open_idx;
        } else {
            output.truncate(open_idx);
            break;
        }
    }
    output
}

fn strip_reasoning_close_tags(content: &str, tag: &str) -> String {
    let close = format!("</{tag}>");
    let mut output = content.to_string();
    while let Some(close_idx) = find_ascii_case_insensitive(&output, &close, 0) {
        output.replace_range(close_idx..close_idx + close.len(), "");
    }
    output
}

fn remove_closed_xml_blocks_with_attrs(
    content: &str,
    tag: &str,
    require_function_name_boundary: bool,
) -> String {
    let close = format!("</{tag}>");
    let mut output = content.to_string();
    let mut cursor = 0usize;
    while let Some((open_idx, open_end)) = find_xml_open_tag(&output, tag, cursor) {
        if require_function_name_boundary {
            let open_tag = &output[open_idx..open_end];
            if !function_xml_open_is_tool_call(&output[..open_idx], open_tag) {
                cursor = open_end;
                continue;
            }
        }
        let Some(close_idx) = find_ascii_case_insensitive(&output, &close, open_end) else {
            break;
        };
        if closed_xml_block_should_survive_for_tool_parser(
            tag,
            &output[open_idx..close_idx + close.len()],
        ) {
            cursor = open_end;
            continue;
        }
        output.replace_range(open_idx..close_idx + close.len(), "");
        cursor = open_idx;
    }
    output
}

fn closed_xml_block_should_survive_for_tool_parser(tag: &str, block: &str) -> bool {
    if !matches!(
        tag,
        "tool_calls" | "tool_call" | "function_calls" | "function_call"
    ) {
        return false;
    }
    let lower = block.to_ascii_lowercase();
    lower.contains("<function=")
}

fn find_xml_open_tag(content: &str, tag: &str, start: usize) -> Option<(usize, usize)> {
    let needle = format!("<{tag}");
    let mut cursor = start;
    while let Some(open_idx) = find_ascii_case_insensitive(content, &needle, cursor) {
        let after_tag = open_idx + needle.len();
        let next = content[after_tag..].chars().next();
        if !matches!(next, Some('>' | ' ' | '\t' | '\r' | '\n')) {
            cursor = after_tag;
            continue;
        }
        let Some(relative_end) = content[after_tag..].find('>') else {
            return None;
        };
        return Some((open_idx, after_tag + relative_end + 1));
    }
    None
}

fn function_xml_open_is_tool_call(prefix: &str, open_tag: &str) -> bool {
    if !open_tag.to_ascii_lowercase().contains("name") {
        return false;
    }
    if prefix.trim().is_empty() {
        return true;
    }
    if let Some(index) = prefix.rfind(['\n', '\r']) {
        if prefix[index + 1..].trim().is_empty() {
            return true;
        }
    }
    let previous = prefix.chars().rev().find(|ch| !ch.is_whitespace());
    matches!(previous, Some('.' | '!' | '?' | ':'))
}

fn strip_xml_close_tags(content: &str, tag: &str) -> String {
    let close = format!("</{tag}>");
    let mut output = content.to_string();
    while let Some(close_idx) = find_ascii_case_insensitive(&output, &close, 0) {
        output.replace_range(close_idx..close_idx + close.len(), "");
    }
    output
}

fn find_ascii_case_insensitive(haystack: &str, needle: &str, start: usize) -> Option<usize> {
    if start >= haystack.len() {
        return None;
    }
    let haystack_lower = haystack[start..].to_ascii_lowercase();
    let needle_lower = needle.to_ascii_lowercase();
    haystack_lower.find(&needle_lower).map(|idx| start + idx)
}

fn reasoning_open_at_block_boundary(prefix: &str) -> bool {
    if prefix.trim().is_empty() {
        return true;
    }
    match prefix.rfind('\n') {
        Some(index) => prefix[index + 1..].trim().is_empty(),
        None => false,
    }
}

fn normalize_visible_assistant_text(value: &str) -> String {
    let mut lines = value.lines().map(str::trim_end).collect::<Vec<_>>();
    while lines
        .first()
        .map(|line| line.trim().is_empty())
        .unwrap_or(false)
    {
        lines.remove(0);
    }
    while lines
        .last()
        .map(|line| line.trim().is_empty())
        .unwrap_or(false)
    {
        lines.pop();
    }
    let mut output = lines.join("\n");
    while output.contains("\n\n\n") {
        output = output.replace("\n\n\n", "\n\n");
    }
    output.trim().to_string()
}

fn echo_reply(user_content: &str, history_len: usize) -> LlmReply {
    let content = format!(
        "收到：{}\n\n当前 Rust 对话链已接管会话，已读取最近 {} 条上下文。配置 LLM Provider 后会切换为真实模型回复。",
        user_content,
        history_len.saturating_sub(1)
    );
    LlmReply {
        prompt_tokens: estimate_tokens(user_content),
        completion_tokens: estimate_tokens(&content),
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        reasoning_tokens: 0,
        content,
        provider_id: Some("local-echo".into()),
        provider_type: Some("echo".into()),
        model: Some("echo".into()),
        base_url: None,
        estimated_cost_usd: Some(0.0),
        cost_status: Some("included".into()),
        cost_source: Some("none".into()),
        rate_limit_state: None,
        transport_diagnostics: None,
        finish_reason: Some("stop".into()),
        provider_data: None,
        failover_attempts: Vec::new(),
    }
}

fn with_reply_metadata(
    reply: LlmReply,
    provider: &LlmProvider,
    model: &str,
    headers: &HeaderMap,
) -> LlmReply {
    with_reply_metadata_and_transport(reply, provider, model, headers, None)
}

#[derive(Clone, Debug, Default)]
pub(super) struct LlmTransportMetadata {
    pub transport: &'static str,
    pub method: &'static str,
    pub endpoint: String,
    pub status: Option<u16>,
    pub elapsed_ms: Option<u64>,
    pub retry_count: u32,
    pub retry_reason: Option<String>,
}

fn with_reply_metadata_and_transport(
    mut reply: LlmReply,
    provider: &LlmProvider,
    model: &str,
    headers: &HeaderMap,
    transport: Option<LlmTransportMetadata>,
) -> LlmReply {
    reply.provider_id = Some(provider.id.clone());
    reply.provider_type = Some(provider.provider_type.clone());
    reply.model = Some(model.to_string());
    let base_url = provider_base_url(provider);
    reply.base_url = if base_url.trim().is_empty() {
        None
    } else {
        Some(base_url)
    };
    reply.rate_limit_state = parse_rate_limit_headers(headers, &provider.provider_type);
    reply.transport_diagnostics =
        llm_transport_diagnostics(headers, transport.as_ref(), provider, model);
    let (amount, status, source) = estimate_usage_cost(
        &provider.provider_type,
        model,
        reply.prompt_tokens,
        reply.completion_tokens,
        reply.cache_read_tokens,
        reply.cache_write_tokens,
    );
    reply.estimated_cost_usd = amount;
    reply.cost_status = Some(status.into());
    reply.cost_source = Some(source.into());
    reply
}

fn llm_transport_diagnostics(
    headers: &HeaderMap,
    transport: Option<&LlmTransportMetadata>,
    provider: &LlmProvider,
    model: &str,
) -> Option<Value> {
    let mut captured = serde_json::Map::new();
    for name in [
        "cf-ray",
        "x-request-id",
        "request-id",
        "x-openai-request-id",
        "x-openrouter-provider",
        "x-openrouter-cache",
        "x-ratelimit-reset",
        "retry-after",
    ] {
        if let Some(value) = header_text(headers, name).filter(|value| !value.trim().is_empty()) {
            captured.insert(name.to_string(), json!(value));
        }
    }
    if captured.is_empty() && transport.is_none() {
        return None;
    }
    let mut diagnostics = json!({
        "capturedAt": crate::models::now_iso(),
        "headers": captured
    });
    if let Some(transport) = transport {
        diagnostics["transport"] = json!(transport.transport);
        diagnostics["method"] = json!(transport.method);
        diagnostics["endpoint"] = json!(safe_diagnostic_endpoint(&transport.endpoint));
        if let Some(status) = transport.status {
            diagnostics["status"] = json!(status);
        }
        if let Some(elapsed_ms) = transport.elapsed_ms {
            diagnostics["elapsedMs"] = json!(elapsed_ms);
        }
        if transport.retry_count > 0 {
            diagnostics["retryCount"] = json!(transport.retry_count);
        }
        if let Some(reason) = transport.retry_reason.as_deref() {
            diagnostics["retryReason"] = json!(reason);
        }
    }
    diagnostics["timeoutPolicy"] = llm_timeout_policy_diagnostics(provider, model);
    diagnostics["timeout_policy"] = llm_timeout_policy_diagnostics(provider, model);
    Some(diagnostics)
}

fn llm_timeout_policy_diagnostics(provider: &LlmProvider, model: &str) -> Value {
    let request_timeout = provider_request_timeout_duration(provider, model).as_secs_f64();
    let stale_timeout = provider_stale_timeout_seconds(provider, model);
    json!({
        "schema": "hermes_provider_timeout_policy_desktop_v1",
        "requestTimeoutSeconds": request_timeout,
        "request_timeout_seconds": request_timeout,
        "staleTimeoutSeconds": stale_timeout,
        "stale_timeout_seconds": stale_timeout,
        "source": provider_timeout_source(provider, model, false),
        "staleSource": provider_timeout_source(provider, model, true),
        "stale_source": provider_timeout_source(provider, model, true)
    })
}

fn provider_timeout_source(provider: &LlmProvider, model: &str, stale: bool) -> &'static str {
    let model = model.trim();
    if !model.is_empty() {
        let model_config = provider.models.as_object().and_then(|models| {
            models
                .get(model)
                .and_then(|model_config| model_config.as_object())
        });
        if let Some(model_config) = model_config {
            let keys = if stale {
                ["staleTimeoutSeconds", "stale_timeout_seconds"]
            } else {
                ["timeoutSeconds", "timeout_seconds"]
            };
            if keys
                .iter()
                .any(|key| coerce_timeout_value(model_config.get(*key)).is_some())
            {
                return "model";
            }
        }
    }
    if stale {
        if coerce_positive_timeout(provider.stale_timeout_seconds).is_some() {
            return "provider";
        }
        "unset"
    } else if coerce_positive_timeout(provider.request_timeout_seconds).is_some() {
        "provider"
    } else {
        "legacy_timeout_seconds"
    }
}

fn safe_diagnostic_endpoint(url: &str) -> String {
    let without_fragment = url.split('#').next().unwrap_or(url);
    without_fragment
        .split('?')
        .next()
        .unwrap_or(without_fragment)
        .to_string()
}

fn parse_rate_limit_headers(headers: &HeaderMap, provider: &str) -> Option<Value> {
    let has_any = headers.keys().any(|key| {
        key.as_str()
            .to_ascii_lowercase()
            .starts_with("x-ratelimit-")
    });
    if !has_any {
        return None;
    }

    let bucket = |resource: &str, suffix: &str| {
        let tag = format!("{resource}{suffix}");
        let limit = header_u64(headers, &format!("x-ratelimit-limit-{tag}"));
        let remaining = header_u64(headers, &format!("x-ratelimit-remaining-{tag}"));
        let reset_seconds = header_f64(headers, &format!("x-ratelimit-reset-{tag}"));
        let used = limit.saturating_sub(remaining);
        let usage_pct = if limit == 0 {
            0.0
        } else {
            (used as f64 / limit as f64) * 100.0
        };
        json!({
            "limit": limit,
            "remaining": remaining,
            "used": used,
            "usagePct": usage_pct,
            "resetSeconds": reset_seconds
        })
    };

    Some(json!({
        "provider": provider,
        "capturedAt": crate::models::now_iso(),
        "requestsMin": bucket("requests", ""),
        "requestsHour": bucket("requests", "-1h"),
        "tokensMin": bucket("tokens", ""),
        "tokensHour": bucket("tokens", "-1h")
    }))
}

fn header_u64(headers: &HeaderMap, name: &str) -> u64 {
    header_text(headers, name)
        .and_then(|value| value.parse::<f64>().ok())
        .map(|value| value.max(0.0) as u64)
        .unwrap_or(0)
}

fn header_f64(headers: &HeaderMap, name: &str) -> f64 {
    header_text(headers, name)
        .and_then(|value| value.parse::<f64>().ok())
        .unwrap_or(0.0)
}

fn header_text(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

fn estimate_usage_cost(
    provider_type: &str,
    model: &str,
    input_tokens: usize,
    output_tokens: usize,
    cache_read_tokens: usize,
    cache_write_tokens: usize,
) -> (Option<f64>, &'static str, &'static str) {
    let Some((
        input_per_million,
        output_per_million,
        cache_read_per_million,
        cache_write_per_million,
    )) = pricing_for_model(provider_type, model)
    else {
        return (None, "unknown", "none");
    };
    let input = input_tokens.saturating_sub(cache_read_tokens + cache_write_tokens) as f64;
    let amount = (input * input_per_million
        + output_tokens as f64 * output_per_million
        + cache_read_tokens as f64 * cache_read_per_million
        + cache_write_tokens as f64 * cache_write_per_million)
        / 1_000_000.0;
    (Some(amount), "estimated", "official_docs_snapshot")
}

fn pricing_for_model(provider_type: &str, model: &str) -> Option<(f64, f64, f64, f64)> {
    let provider = provider_type.to_ascii_lowercase();
    let model = model.to_ascii_lowercase();
    let openai_like = provider.contains("openai") || provider.contains("compatible");
    if openai_like || provider.contains("openrouter") {
        if model.contains("gpt-4o-mini") {
            return Some((0.15, 0.60, 0.075, 0.0));
        }
        if model.contains("gpt-4o") {
            return Some((2.50, 10.00, 1.25, 0.0));
        }
        if model.contains("gpt-4.1-mini") {
            return Some((0.40, 1.60, 0.10, 0.0));
        }
        if model.contains("gpt-4.1") {
            return Some((2.00, 8.00, 0.50, 0.0));
        }
    }
    if provider.contains("anthropic") || model.contains("claude") {
        if model.contains("haiku") {
            return Some((1.00, 5.00, 0.10, 1.25));
        }
        if model.contains("sonnet") {
            return Some((3.00, 15.00, 0.30, 3.75));
        }
        if model.contains("opus") {
            return Some((5.00, 25.00, 0.50, 6.25));
        }
    }
    if provider.contains("gemini") || provider.contains("google") || model.contains("gemini") {
        if model.contains("flash") {
            return Some((0.10, 0.40, 0.025, 0.0));
        }
        if model.contains("pro") {
            return Some((1.25, 5.00, 0.31, 0.0));
        }
    }
    None
}

pub fn estimate_tokens(text: &str) -> usize {
    let chars = text.chars().count();
    if chars == 0 {
        0
    } else {
        (chars / 3).max(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::ToolDefinition;
    use reqwest::header::HeaderMap;
    use reqwest::header::AUTHORIZATION;

    fn restore_env_var(name: &str, value: Option<std::ffi::OsString>) {
        if let Some(value) = value {
            std::env::set_var(name, value);
        } else {
            std::env::remove_var(name);
        }
    }

    fn schema_test_tool(input_schema: Value) -> ToolDefinition {
        ToolDefinition {
            name: "terminal".into(),
            display_name: "terminal".into(),
            description: "Run a command".into(),
            source: "internal".into(),
            server_id: "__internal".into(),
            tool_name: "terminal".into(),
            input_schema,
            requires_approval: true,
        }
    }

    #[test]
    fn rate_limit_headers_parse_into_usage_buckets() {
        let mut headers = HeaderMap::new();
        headers.insert("x-ratelimit-limit-requests", "100".parse().unwrap());
        headers.insert("x-ratelimit-remaining-requests", "80".parse().unwrap());
        headers.insert("x-ratelimit-reset-requests", "30".parse().unwrap());
        headers.insert("x-ratelimit-limit-tokens-1h", "1000000".parse().unwrap());
        headers.insert("x-ratelimit-remaining-tokens-1h", "750000".parse().unwrap());
        headers.insert("x-ratelimit-reset-tokens-1h", "3600".parse().unwrap());

        let state = parse_rate_limit_headers(&headers, "openai-compatible").unwrap();

        assert_eq!(state["requestsMin"]["limit"], 100);
        assert_eq!(state["requestsMin"]["used"], 20);
        assert_eq!(state["tokensHour"]["remaining"], 750000);
    }

    #[test]
    fn transport_diagnostics_capture_upstream_headers() {
        let mut headers = HeaderMap::new();
        headers.insert("cf-ray", "ray-123".parse().unwrap());
        headers.insert("x-openrouter-provider", "upstream-a".parse().unwrap());
        headers.insert("authorization", "secret".parse().unwrap());
        let mut provider = LlmProvider::default();
        provider.timeout_seconds = 60;
        provider.request_timeout_seconds = Some(45.0);
        provider.stale_timeout_seconds = Some(30.0);
        provider.models = json!({
            "diagnostic-model": {
                "timeout_seconds": 12,
                "stale_timeout_seconds": 6
            }
        });

        let diagnostics = llm_transport_diagnostics(
            &headers,
            Some(&LlmTransportMetadata {
                transport: "openai_chat",
                method: "POST",
                endpoint: "https://example.test/v1/chat/completions?api_key=secret#frag".into(),
                status: Some(200),
                elapsed_ms: Some(42),
                retry_count: 1,
                retry_reason: Some("unsupported_parameter_recovery".into()),
            }),
            &provider,
            "diagnostic-model",
        )
        .unwrap();

        assert_eq!(diagnostics["headers"]["cf-ray"], "ray-123");
        assert_eq!(
            diagnostics["headers"]["x-openrouter-provider"],
            "upstream-a"
        );
        assert_eq!(diagnostics["transport"], "openai_chat");
        assert_eq!(diagnostics["method"], "POST");
        assert_eq!(
            diagnostics["endpoint"],
            "https://example.test/v1/chat/completions"
        );
        assert_eq!(diagnostics["status"], 200);
        assert_eq!(diagnostics["elapsedMs"], 42);
        assert_eq!(diagnostics["retryCount"], 1);
        assert_eq!(diagnostics["retryReason"], "unsupported_parameter_recovery");
        assert_eq!(
            diagnostics["timeoutPolicy"]["schema"],
            "hermes_provider_timeout_policy_desktop_v1"
        );
        assert_eq!(diagnostics["timeoutPolicy"]["requestTimeoutSeconds"], 12.0);
        assert_eq!(diagnostics["timeoutPolicy"]["staleTimeoutSeconds"], 6.0);
        assert_eq!(diagnostics["timeoutPolicy"]["source"], "model");
        assert_eq!(diagnostics["timeoutPolicy"]["staleSource"], "model");
        assert!(diagnostics["headers"].get("authorization").is_none());
    }

    #[test]
    fn invalid_response_body_detail_classifies_common_provider_shapes() {
        let mut headers = HeaderMap::new();
        headers.insert("content-type", "text/html".parse().unwrap());

        let html = invalid_response_body_detail("<html>bad gateway</html>", &headers);
        assert!(html.contains("bodyKind=html"));
        assert!(html.contains("contentType=text/html"));
        assert!(html.contains("preview=<html>bad gateway</html>"));

        let empty = invalid_response_body_detail("   \n", &HeaderMap::new());
        assert!(empty.contains("bodyKind=empty"));
        assert!(empty.contains("contentType=unknown"));

        let sse = invalid_response_body_detail("data: {\"delta\":\"partial\"}", &HeaderMap::new());
        assert!(sse.contains("bodyKind=sse_like"));
    }

    #[test]
    fn native_tool_schema_converters_match_provider_shapes() {
        let tool = schema_test_tool(json!({
            "type": "object",
            "properties": {
                "command": {
                    "anyOf": [{"type": "string"}, {"type": "null"}],
                    "description": "Shell command",
                    "pattern": ".*"
                }
            },
            "required": ["command"],
            "oneOf": [{"required": ["command"]}]
        }));
        let tools = vec![tool];

        let openai = openai_tool_schemas(&tools);
        assert_eq!(openai[0]["type"], "function");
        assert_eq!(openai[0]["function"]["name"], "terminal");
        assert_eq!(
            openai[0]["function"]["parameters"]["properties"]["command"]["type"],
            "string"
        );
        assert!(openai[0]["function"]["parameters"].get("oneOf").is_none());

        let responses = responses_tool_schemas(&tools);
        assert_eq!(responses[0]["type"], "function");
        assert_eq!(responses[0]["name"], "terminal");

        let anthropic = anthropic_tool_schemas(&tools);
        assert_eq!(anthropic[0]["name"], "terminal");
        assert_eq!(
            anthropic[0]["input_schema"]["properties"]["command"]["type"],
            "string"
        );

        let bedrock = bedrock_tool_config(&tools).unwrap();
        assert_eq!(
            bedrock["tools"][0]["toolSpec"]["inputSchema"]["json"]["properties"]["command"]["type"],
            "string"
        );

        let gemini = gemini_tool_schemas(&tools);
        assert_eq!(gemini[0]["functionDeclarations"][0]["name"], "terminal");
        assert_eq!(
            gemini[0]["functionDeclarations"][0]["parameters"]["properties"]["command"]["type"],
            "string"
        );
        assert!(
            gemini[0]["functionDeclarations"][0]["parameters"]["properties"]["command"]
                .get("pattern")
                .is_none()
        );

        let mut mcp_tool = schema_test_tool(json!({"type": "object"}));
        mcp_tool.name = "ai.exa/exa.search-docs".into();
        mcp_tool.display_name = "search-docs".into();
        mcp_tool.source = "mcp".into();
        mcp_tool.server_id = "ai.exa/exa".into();
        mcp_tool.tool_name = "search-docs".into();
        let mcp_tools = vec![mcp_tool];
        assert_eq!(
            openai_tool_schemas(&mcp_tools)[0]["function"]["name"],
            "ai_exa_exa_search-docs"
        );
        assert_eq!(
            responses_tool_schemas(&mcp_tools)[0]["name"],
            "ai_exa_exa_search-docs"
        );
        assert_eq!(
            anthropic_tool_schemas(&mcp_tools)[0]["name"],
            "ai_exa_exa_search-docs"
        );
    }

    #[test]
    fn known_model_usage_cost_is_estimated() {
        let (amount, status, source) =
            estimate_usage_cost("openai-compatible", "gpt-4o-mini", 1_000_000, 500_000, 0, 0);

        assert_eq!(status, "estimated");
        assert_eq!(source, "official_docs_snapshot");
        assert!((amount.unwrap() - 0.45).abs() < 0.000001);
    }

    #[test]
    fn reasoning_blocks_are_scrubbed_from_visible_text() {
        let scrubbed = scrub_reasoning_blocks(
            "先说一句。\n<think>hidden plan</think>\n\n<REASONING_SCRATCHPAD>secret",
        );
        assert_eq!(scrubbed, "先说一句。");

        let inline = scrub_reasoning_blocks("普通说明：请不要手写 <think> 标签。");
        assert_eq!(inline, "普通说明：请不要手写 <think> 标签。");
    }

    #[test]
    fn tool_xml_blocks_are_scrubbed_from_visible_text() {
        let scrubbed = scrub_reasoning_blocks(
            "先说明。\n<tool_call>{\"name\":\"terminal\"}</tool_call>\n<function name=\"read_file\">{\"path\":\"Cargo.toml\"}</function>\n最终回答</function>",
        );
        assert!(scrubbed.contains("先说明。"));
        assert!(scrubbed.contains("最终回答"));
        assert!(!scrubbed.contains("tool_call"));
        assert!(!scrubbed.contains("function name"));
        assert!(!scrubbed.contains("</function>"));

        let prose = scrub_reasoning_blocks("在 JavaScript 里可以讨论 <function> 标签。");
        assert_eq!(prose, "在 JavaScript 里可以讨论 <function> 标签。");
    }

    #[test]
    fn openai_retry_body_removes_unsupported_temperature() {
        let body = json!({
            "model": "fixed-temp-model",
            "messages": [],
            "temperature": 0.7,
            "max_tokens": 1024
        });

        let retry = openai_unsupported_parameter_retry_body(
            &body,
            r#"{"error":{"message":"Unsupported parameter: temperature","param":"temperature","code":"unsupported_parameter"}}"#,
        )
        .unwrap();

        assert!(retry.get("temperature").is_none());
        assert_eq!(retry["max_tokens"], 1024);
    }

    #[test]
    fn openai_retry_body_converts_max_tokens_when_provider_requests_completion_tokens() {
        let body = json!({
            "model": "gpt-next",
            "messages": [],
            "temperature": 0.7,
            "max_tokens": 1024
        });

        let retry = openai_unsupported_parameter_retry_body(
            &body,
            "Unsupported parameter: max_tokens is not supported with this model. Use max_completion_tokens instead.",
        )
        .unwrap();

        assert!(retry.get("max_tokens").is_none());
        assert_eq!(retry["max_completion_tokens"], 1024);
        assert_eq!(retry["temperature"], 0.7);
    }

    #[test]
    fn openai_retry_body_removes_max_tokens_for_generic_unsupported_error() {
        let body = json!({
            "model": "vision-route",
            "messages": [],
            "temperature": 0.7,
            "max_tokens": 1024
        });

        let retry = openai_unsupported_parameter_retry_body(
            &body,
            r#"{"error":{"message":"unsupported_parameter","param":"max_tokens"}}"#,
        )
        .unwrap();

        assert!(retry.get("max_tokens").is_none());
        assert!(retry.get("max_completion_tokens").is_none());
        assert_eq!(retry["temperature"], 0.7);
    }

    #[test]
    fn openai_retry_body_removes_unsupported_service_tier() {
        let body = json!({
            "model": "gpt-test",
            "messages": [],
            "service_tier": "priority",
            "temperature": 0.7
        });

        let retry = openai_unsupported_parameter_retry_body(
            &body,
            r#"{"error":{"message":"Unsupported parameter: service_tier","param":"service_tier","code":"unsupported_parameter"}}"#,
        )
        .unwrap();

        assert!(retry.get("service_tier").is_none());
        assert_eq!(retry["temperature"], 0.7);
    }

    #[test]
    fn openai_parser_strips_reasoning_blocks() {
        let reply = parse_openai_compatible(json!({
            "choices": [{
                "message": {
                    "content": "<think>do not show</think>\n最终回答"
                }
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 8
            }
        }))
        .unwrap();

        assert_eq!(reply.content, "最终回答");
    }

    #[test]
    fn openai_parser_preserves_reasoning_provider_data() {
        let reply = parse_openai_compatible(json!({
            "choices": [{
                "message": {
                    "content": "done",
                    "reasoning_content": "hidden chain",
                    "reasoning_details": [{"type": "reasoning.text", "text": "signed"}]
                }
            }]
        }))
        .unwrap();

        let data = reply.provider_data.unwrap();
        assert_eq!(data["openai"]["reasoning_content"], "hidden chain");
        assert_eq!(
            data["openai"]["reasoning_details"][0]["type"],
            "reasoning.text"
        );
    }

    #[test]
    fn openai_payload_replays_assistant_reasoning_provider_data() {
        let mut message =
            ChatMessage::new("conv".into(), "assistant", "done".into(), "desktop-agent");
        message.provider_data = Some(json!({
            "openai": {
                "reasoning_content": "hidden chain",
                "reasoning_details": [{"type": "reasoning.text", "text": "signed"}]
            }
        }));

        let input = build_openai_wire_messages(String::new(), vec![message], None);

        assert_eq!(input[1]["role"], "assistant");
        assert_eq!(input[1]["content"], "done");
        assert_eq!(input[1]["reasoning_content"], "hidden chain");
        assert_eq!(input[1]["reasoning_details"][0]["type"], "reasoning.text");
    }

    #[test]
    fn openai_payload_uses_user_provider_content_parts() {
        let mut message = ChatMessage::new("conv".into(), "user", "Review image".into(), "desktop");
        message.provider_data = Some(json!({
            "openai": {
                "content": [
                    {"type": "text", "text": "Review image"},
                    {"type": "image_url", "image_url": {"url": "data:image/png;base64,AQID"}}
                ]
            }
        }));

        let input = build_openai_wire_messages(String::new(), vec![message], None);

        assert_eq!(input[1]["role"], "user");
        assert_eq!(input[1]["content"][0]["type"], "text");
        assert_eq!(input[1]["content"][1]["type"], "image_url");
        assert_eq!(
            input[1]["content"][1]["image_url"]["url"],
            "data:image/png;base64,AQID"
        );
    }

    #[test]
    fn responses_parser_extracts_output_text_and_usage() {
        let reply = parse_responses_compatible(json!({
            "output": [{
                "type": "message",
                "content": [
                    {"type": "output_text", "text": "第一段"},
                    {"type": "output_text", "text": "第二段"}
                ]
            }],
            "usage": {
                "input_tokens": 11,
                "output_tokens": 7,
                "output_tokens_details": {"reasoning_tokens": 3}
            }
        }))
        .unwrap();

        assert_eq!(reply.content, "第一段第二段");
        assert_eq!(reply.prompt_tokens, 11);
        assert_eq!(reply.completion_tokens, 7);
        assert_eq!(reply.reasoning_tokens, 3);
    }

    #[test]
    fn responses_parser_converts_function_calls_to_agent_tool_calls() {
        let reply = parse_responses_compatible(json!({
            "output": [{
                "type": "function_call",
                "id": "fc_123",
                "call_id": "call_123",
                "name": "terminal",
                "arguments": "{\"command\":\"pwd\"}"
            }],
            "usage": {
                "input_tokens": 5,
                "output_tokens": 2
            }
        }))
        .unwrap();

        let value = serde_json::from_str::<Value>(&reply.content).unwrap();
        assert_eq!(value["tool_calls"][0]["function"]["name"], "terminal");
        assert_eq!(
            value["tool_calls"][0]["function"]["arguments"],
            "{\"command\":\"pwd\"}"
        );
        assert_eq!(reply.finish_reason.as_deref(), Some("tool_calls"));
    }

    #[test]
    fn responses_parser_repairs_malformed_function_arguments() {
        let reply = parse_responses_compatible(json!({
            "output": [{
                "type": "function_call",
                "id": "fc_bad",
                "call_id": "call_bad",
                "name": "terminal",
                "arguments": "{\"command\":\"pwd\""
            }]
        }))
        .unwrap();

        let value = serde_json::from_str::<Value>(&reply.content).unwrap();
        let raw_arguments = value["tool_calls"][0]["function"]["arguments"]
            .as_str()
            .unwrap();
        let arguments = serde_json::from_str::<Value>(raw_arguments).unwrap();
        assert_eq!(arguments["command"], "pwd");
        assert_eq!(reply.finish_reason.as_deref(), Some("tool_calls"));
    }

    #[test]
    fn provider_tool_arguments_repairs_common_json_damage() {
        let trailing =
            normalize_provider_tool_arguments(json!("{\"command\":\"pwd\",}"), "terminal");
        assert_eq!(trailing.as_str().unwrap(), r#"{"command":"pwd"}"#);

        let raw_control =
            normalize_provider_tool_arguments(json!("{\"command\":\"line\nnext\"}"), "terminal");
        let parsed = serde_json::from_str::<Value>(raw_control.as_str().unwrap()).unwrap();
        assert_eq!(parsed["command"], "line\nnext");

        let none = normalize_provider_tool_arguments(json!("None"), "terminal");
        assert_eq!(none.as_str().unwrap(), "{}");
    }

    #[test]
    fn responses_parser_converts_custom_tool_calls_to_agent_tool_calls() {
        let reply = parse_responses_compatible(json!({
            "output": [{
                "type": "custom_tool_call",
                "id": "fc_custom",
                "call_id": "call_custom",
                "name": "terminal",
                "input": "{\"command\":\"pwd\"}"
            }]
        }))
        .unwrap();

        let value = serde_json::from_str::<Value>(&reply.content).unwrap();
        assert_eq!(value["tool_calls"][0]["id"], "fc_custom");
        assert_eq!(value["tool_calls"][0]["function"]["name"], "terminal");
        assert_eq!(
            value["tool_calls"][0]["function"]["arguments"],
            "{\"command\":\"pwd\"}"
        );
        assert_eq!(reply.finish_reason.as_deref(), Some("tool_calls"));
    }

    #[test]
    fn responses_parser_marks_reasoning_only_output_incomplete() {
        let reply = parse_responses_compatible(json!({
            "output": [{
                "type": "reasoning",
                "summary": [{"type": "summary_text", "text": "still working"}]
            }],
            "usage": {
                "input_tokens": 3,
                "output_tokens": 4
            }
        }))
        .unwrap();

        assert!(reply.content.trim().is_empty());
        assert_eq!(reply.finish_reason.as_deref(), Some("incomplete"));
    }

    #[test]
    fn responses_parser_marks_commentary_without_final_phase_incomplete() {
        let reply = parse_responses_compatible(json!({
            "output": [{
                "type": "message",
                "phase": "commentary",
                "content": [{"type": "output_text", "text": "status update"}]
            }]
        }))
        .unwrap();

        assert_eq!(reply.content, "status update");
        assert_eq!(reply.finish_reason.as_deref(), Some("incomplete"));
    }

    #[test]
    fn responses_parser_preserves_provider_replay_items() {
        let reply = parse_responses_compatible(json!({
            "output": [
                {
                    "type": "reasoning",
                    "encrypted_content": "sealed",
                    "summary": [{"type": "summary_text", "text": "short"}]
                },
                {
                    "type": "message",
                    "id": "msg_1",
                    "phase": "final_answer",
                    "status": "completed",
                    "content": [{"type": "output_text", "text": "done"}]
                }
            ]
        }))
        .unwrap();

        let data = reply.provider_data.unwrap();
        assert_eq!(
            data["responses"]["reasoningItems"][0]["encrypted_content"],
            "sealed"
        );
        assert_eq!(data["responses"]["messageItems"][0]["id"], "msg_1");
        assert_eq!(
            data["responses"]["messageItems"][0]["phase"],
            "final_answer"
        );
    }

    #[test]
    fn responses_payload_replays_provider_items_from_assistant_history() {
        let mut message =
            ChatMessage::new("conv".into(), "assistant", "done".into(), "desktop-agent");
        message.provider_data = Some(json!({
            "responses": {
                "reasoningItems": [{
                    "type": "reasoning",
                    "encrypted_content": "sealed",
                    "summary": [{"type": "summary_text", "text": "short"}]
                }],
                "messageItems": [{
                    "type": "message",
                    "role": "assistant",
                    "status": "completed",
                    "phase": "final_answer",
                    "content": [{"type": "output_text", "text": "done"}]
                }]
            }
        }));

        let (_instructions, input) =
            build_responses_payload(String::new(), vec![message], Some("codex_backend"), true);

        assert_eq!(input[0]["type"], "reasoning");
        assert_eq!(input[0]["encrypted_content"], "sealed");
        assert_eq!(input[1]["type"], "message");
        assert_eq!(input[1]["phase"], "final_answer");
        assert_eq!(input.len(), 2);
    }

    #[test]
    fn responses_payload_can_disable_reasoning_replay_only() {
        let mut message =
            ChatMessage::new("conv".into(), "assistant", "done".into(), "desktop-agent");
        message.provider_data = Some(json!({
            "responses": {
                "reasoningItems": [{
                    "type": "reasoning",
                    "encrypted_content": "sealed",
                    "_issuerKind": "codex_backend"
                }],
                "messageItems": [{
                    "type": "message",
                    "role": "assistant",
                    "status": "completed",
                    "content": [{"type": "output_text", "text": "done"}]
                }]
            }
        }));

        let (_instructions, input) =
            build_responses_payload(String::new(), vec![message], Some("codex_backend"), false);

        assert_eq!(input[0]["type"], "message");
        assert_eq!(input[0]["content"][0]["text"], "done");
        assert_eq!(input.len(), 1);
    }

    #[test]
    fn responses_payload_skips_cross_issuer_reasoning_items() {
        let mut message =
            ChatMessage::new("conv".into(), "assistant", "done".into(), "desktop-agent");
        message.provider_data = Some(json!({
            "responses": {
                "reasoningItems": [{
                    "type": "reasoning",
                    "encrypted_content": "sealed",
                    "_issuerKind": "codex_backend"
                }]
            }
        }));

        let (_instructions, input) =
            build_responses_payload(String::new(), vec![message], Some("xai_responses"), true);

        assert_eq!(input[0]["role"], "assistant");
        assert_eq!(input[0]["content"][0]["text"], "done");
        assert_eq!(input.len(), 1);
    }

    #[test]
    fn responses_payload_merges_adjacent_plain_text_turns() {
        let first = ChatMessage::new("conv".into(), "user", "first".into(), "desktop-user");
        let second = ChatMessage::new("conv".into(), "user", "second".into(), "desktop-user");
        let tool = ChatMessage::new(
            "conv".into(),
            "tool",
            json!({
                "type": "toolEvent",
                "event": {
                    "toolName": "terminal",
                    "callId": "call_1",
                    "ok": true,
                    "text": "done",
                    "raw": {"payload": {"command": "pwd"}}
                }
            })
            .to_string(),
            "desktop-agent-tool",
        );
        let third = ChatMessage::new("conv".into(), "user", "third".into(), "desktop-user");

        let (_instructions, input) =
            build_responses_payload(String::new(), vec![first, second, tool, third], None, true);

        assert_eq!(input[0]["role"], "user");
        assert_eq!(input[0]["content"][0]["text"], "first\n\nsecond");
        assert_eq!(input[1]["type"], "function_call");
        assert_eq!(input[2]["type"], "function_call_output");
        assert_eq!(input[3]["role"], "user");
        assert_eq!(input[3]["content"][0]["text"], "third");
    }

    #[test]
    fn responses_unsupported_retry_removes_reasoning_include() {
        let body = json!({
            "model": "gpt-test",
            "input": [],
            "include": ["reasoning.encrypted_content"],
            "temperature": 0.2
        });
        let retry = responses_unsupported_parameter_retry_body(
            &body,
            r#"{"error":{"message":"Unsupported parameter: include","param":"include","code":"unsupported_parameter"}}"#,
        )
        .unwrap();

        assert!(retry.get("include").is_none());
        assert_eq!(retry["temperature"], 0.2);
    }

    #[test]
    fn responses_unsupported_retry_removes_service_tier() {
        let body = json!({
            "model": "gpt-test",
            "input": [],
            "service_tier": "priority",
            "temperature": 0.2
        });
        let retry = responses_unsupported_parameter_retry_body(
            &body,
            r#"{"error":{"message":"Unsupported parameter: service_tier","param":"service_tier","code":"unsupported_parameter"}}"#,
        )
        .unwrap();

        assert!(retry.get("service_tier").is_none());
        assert_eq!(retry["temperature"], 0.2);
    }

    #[test]
    fn openai_parser_converts_tool_calls_to_agent_tool_calls() {
        let reply = parse_openai_compatible(json!({
            "choices": [{
                "message": {
                    "tool_calls": [{
                        "id": "call_123",
                        "type": "function",
                        "extra_content": {"google": {"thought_signature": "sig"}},
                        "function": {
                            "name": "terminal",
                            "arguments": "{\"command\":\"pwd\"}"
                        }
                    }]
                }
            }],
            "usage": {
                "prompt_tokens": 5,
                "completion_tokens": 2
            }
        }))
        .unwrap();

        let value = serde_json::from_str::<Value>(&reply.content).unwrap();
        assert_eq!(value["tool_calls"][0]["id"], "call_123");
        assert_eq!(value["tool_calls"][0]["function"]["name"], "terminal");
        assert_eq!(
            value["tool_calls"][0]["extra_content"]["google"]["thought_signature"],
            "sig"
        );
        assert_eq!(reply.finish_reason.as_deref(), Some("tool_calls"));
    }

    #[test]
    fn openai_parser_repairs_malformed_tool_call_arguments() {
        let reply = parse_openai_compatible(json!({
            "choices": [{
                "message": {
                    "tool_calls": [{
                        "id": "call_bad",
                        "type": "function",
                        "function": {
                            "name": "terminal",
                            "arguments": "{\"command\":\"pwd\""
                        }
                    }]
                }
            }]
        }))
        .unwrap();

        let value = serde_json::from_str::<Value>(&reply.content).unwrap();
        let raw_arguments = value["tool_calls"][0]["function"]["arguments"]
            .as_str()
            .unwrap();
        let arguments = serde_json::from_str::<Value>(raw_arguments).unwrap();
        assert_eq!(arguments["command"], "pwd");
        assert_eq!(reply.finish_reason.as_deref(), Some("tool_calls"));
    }

    #[test]
    fn openai_parser_marks_unrepairable_tool_call_arguments() {
        let reply = parse_openai_compatible(json!({
            "choices": [{
                "message": {
                    "tool_calls": [{
                        "id": "call_bad",
                        "type": "function",
                        "function": {
                            "name": "terminal",
                            "arguments": "not-json"
                        }
                    }]
                }
            }]
        }))
        .unwrap();

        let value = serde_json::from_str::<Value>(&reply.content).unwrap();
        let raw_arguments = value["tool_calls"][0]["function"]["arguments"]
            .as_str()
            .unwrap();
        let arguments = serde_json::from_str::<Value>(raw_arguments).unwrap();
        assert_eq!(
            arguments["__agentToolArgumentsCorruption"]["tool"],
            "terminal"
        );
        assert!(
            arguments["__agentToolArgumentsCorruption"]["originalPreview"]
                .as_str()
                .unwrap()
                .contains("not-json")
        );
        assert_eq!(reply.finish_reason.as_deref(), Some("tool_calls"));
    }

    #[test]
    fn openai_tool_replay_restores_provider_tool_call_metadata() {
        let message = ChatMessage::new(
            "conv".into(),
            "tool",
            json!({
                "type": "toolEvent",
                "event": {
                    "toolName": "terminal",
                    "callId": "local_call",
                    "ok": true,
                    "text": "done",
                    "raw": {
                        "payload": {
                            "command": "pwd",
                            "__agentProviderToolCall": {
                                "id": "provider_call",
                                "extra_content": {"google": {"thought_signature": "sig"}}
                            }
                        }
                    }
                }
            })
            .to_string(),
            "desktop-agent-tool",
        );

        let input = build_openai_wire_messages(String::new(), vec![message], None);

        assert_eq!(input[1]["tool_calls"][0]["id"], "provider_call");
        assert_eq!(
            input[1]["tool_calls"][0]["extra_content"]["google"]["thought_signature"],
            "sig"
        );
        assert_eq!(
            input[1]["tool_calls"][0]["function"]["arguments"],
            "{\"command\":\"pwd\"}"
        );
        assert_eq!(input[2]["tool_call_id"], "provider_call");
    }

    #[test]
    fn anthropic_parser_converts_tool_use_blocks_to_agent_tool_calls() {
        let reply = parse_anthropic_compatible(json!({
            "content": [{
                "type": "tool_use",
                "id": "toolu_123",
                "name": "terminal",
                "input": {"command": "pwd"}
            }],
            "usage": {
                "input_tokens": 5,
                "output_tokens": 2
            }
        }))
        .unwrap();

        let value = serde_json::from_str::<Value>(&reply.content).unwrap();
        assert_eq!(value["tool_calls"][0]["id"], "toolu_123");
        assert_eq!(value["tool_calls"][0]["function"]["name"], "terminal");
        assert_eq!(
            value["tool_calls"][0]["function"]["arguments"],
            json!({"command": "pwd"})
        );
        assert_eq!(reply.finish_reason.as_deref(), Some("tool_calls"));
    }

    #[test]
    fn gemini_parser_converts_function_calls_to_agent_tool_calls() {
        let reply = parse_gemini_compatible(json!({
            "candidates": [{
                "content": {
                    "parts": [{
                        "functionCall": {
                            "name": "terminal",
                            "args": {"command": "pwd"}
                        }
                    }]
                }
            }],
            "usageMetadata": {
                "promptTokenCount": 5,
                "candidatesTokenCount": 2
            }
        }))
        .unwrap();

        let value = serde_json::from_str::<Value>(&reply.content).unwrap();
        assert_eq!(value["tool_calls"][0]["function"]["name"], "terminal");
        assert_eq!(
            value["tool_calls"][0]["function"]["arguments"],
            json!({"command": "pwd"})
        );
        assert_eq!(reply.finish_reason.as_deref(), Some("tool_calls"));
    }

    #[test]
    fn provider_timeout_helpers_follow_hermes_provider_and_model_precedence() {
        let mut provider = LlmProvider::default();
        provider.timeout_seconds = 60;
        provider.request_timeout_seconds = Some(45.0);
        provider.stale_timeout_seconds = Some(30.0);
        provider.models = json!({
            "gpt-large": {
                "timeout_seconds": "12.5",
                "stale_timeout_seconds": 8
            },
            "gpt-fast": {
                "timeoutSeconds": 9,
                "staleTimeoutSeconds": "4.5"
            },
            "bad": {
                "timeout_seconds": 0,
                "stale_timeout_seconds": "nope"
            }
        });

        assert_eq!(
            provider_request_timeout_duration(&provider, "gpt-large").as_secs_f64(),
            12.5
        );
        assert_eq!(
            provider_request_timeout_duration(&provider, "gpt-fast").as_secs_f64(),
            9.0
        );
        assert_eq!(
            provider_request_timeout_seconds(&provider, "missing"),
            Some(45.0)
        );
        assert_eq!(
            provider_request_timeout_seconds(&provider, "bad"),
            Some(45.0)
        );
        assert_eq!(
            provider_stale_timeout_seconds(&provider, "gpt-large"),
            Some(8.0)
        );
        assert_eq!(
            provider_stale_timeout_seconds(&provider, "gpt-fast"),
            Some(4.5)
        );
        assert_eq!(
            provider_stale_timeout_seconds(&provider, "missing"),
            Some(30.0)
        );
        assert_eq!(
            provider_stream_stale_timeout_duration(&provider, "gpt-fast")
                .unwrap()
                .as_secs_f64(),
            4.5
        );

        provider.request_timeout_seconds = None;
        assert_eq!(
            provider_request_timeout_duration(&provider, "missing").as_secs(),
            60
        );
    }

    #[test]
    fn bedrock_parser_converts_tool_use_blocks_to_agent_tool_calls() {
        let reply = parse_bedrock_converse(json!({
            "output": {
                "message": {
                    "content": [{
                        "toolUse": {
                            "toolUseId": "tooluse_123",
                            "name": "terminal",
                            "input": {"command": "pwd"}
                        }
                    }]
                }
            },
            "usage": {
                "inputTokens": 5,
                "outputTokens": 2
            }
        }))
        .unwrap();

        let value = serde_json::from_str::<Value>(&reply.content).unwrap();
        assert_eq!(value["tool_calls"][0]["id"], "tooluse_123");
        assert_eq!(value["tool_calls"][0]["function"]["name"], "terminal");
        assert_eq!(
            value["tool_calls"][0]["function"]["arguments"],
            json!({"command": "pwd"})
        );
        assert_eq!(reply.finish_reason.as_deref(), Some("tool_calls"));
    }

    #[test]
    fn responses_parser_failed_status_surfaces_code_and_message() {
        let error = parse_responses_compatible(json!({
            "status": "failed",
            "output": [{
                "type": "message",
                "content": [{"type": "output_text", "text": "partial"}]
            }],
            "error": {"code": "rate_limit_exceeded", "message": "Slow down"}
        }))
        .unwrap_err();

        assert_eq!(
            error.to_string(),
            "llm error: rate_limit_exceeded: Slow down"
        );
    }

    #[test]
    fn responses_error_formatter_falls_back_to_status() {
        assert_eq!(
            format_responses_error(None, "cancelled"),
            "Responses API returned status 'cancelled'"
        );
        assert_eq!(
            format_responses_error(Some(&json!({"code": "server_error"})), "failed"),
            "server_error"
        );
    }

    #[test]
    fn responses_transport_detection_and_url_follow_codex_shape() {
        let mut provider = LlmProvider {
            id: "openai-codex".into(),
            name: "OpenAI Codex".into(),
            provider_type: "codex".into(),
            preset: Some("responses".into()),
            base_url: "https://chatgpt.com/backend-api/codex".into(),
            append_chat_path: true,
            api_key_env: String::new(),
            api_key: None,
            model: "gpt-5-codex".into(),
            enabled: true,
            timeout_seconds: 60,
            request_timeout_seconds: None,
            stale_timeout_seconds: None,
            models: json!({}),
            prompt_cache_mode: "off".into(),
            prompt_cache_ttl: "5m".into(),
            prompt_cache_layout: "native".into(),
        };

        assert!(is_responses_compatible(&provider));
        assert_eq!(
            responses_url(&provider),
            "https://chatgpt.com/backend-api/codex/responses"
        );

        provider.base_url = "https://api.openai.com/v1".into();
        assert_eq!(
            responses_url(&provider),
            "https://api.openai.com/v1/responses"
        );

        provider.provider_type = "openai".into();
        provider.preset = Some("openai-codex".into());
        provider.base_url.clear();
        assert!(is_responses_compatible(&provider));
        assert_eq!(
            responses_url(&provider),
            "https://chatgpt.com/backend-api/codex/responses"
        );

        provider.id = "xai".into();
        provider.name = "xAI".into();
        provider.provider_type = "openai".into();
        provider.preset = Some("xai".into());
        provider.base_url.clear();
        provider.model = "grok-4".into();
        assert!(is_responses_compatible(&provider));
        assert_eq!(responses_url(&provider), "https://api.x.ai/v1/responses");
    }

    #[test]
    fn wire_message_builder_sanitizes_history_before_provider_send() {
        let messages = build_openai_wire_messages(
            "root system".into(),
            vec![
                ChatMessage::new("conv".into(), "system", "extra system".into(), "test"),
                ChatMessage::new(
                    "conv".into(),
                    "assistant",
                    "<think>hidden only</think>".into(),
                    "test",
                ),
                ChatMessage::new("conv".into(), "user", "visible\u{0007}text".into(), "test"),
                ChatMessage::new("conv".into(), "user", "second user".into(), "test"),
            ],
            None,
        );

        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[0]["content"], "root system\n\nextra system");
        assert_eq!(messages[1]["role"], "user");
        assert_eq!(messages[1]["content"], "visible\u{fffd}text\n\nsecond user");
    }

    #[test]
    fn gemini_builder_merges_adjacent_same_role_messages() {
        let contents = build_gemini_contents(vec![
            ChatMessage::new("conv".into(), "user", "first".into(), "test"),
            ChatMessage::new("conv".into(), "user", "second".into(), "test"),
            ChatMessage::new("conv".into(), "assistant", "reply".into(), "test"),
        ]);

        assert_eq!(contents.len(), 2);
        assert_eq!(contents[0]["role"], "user");
        assert_eq!(contents[0]["parts"][0]["text"], "first\n\nsecond");
        assert_eq!(contents[1]["role"], "model");
        assert_eq!(contents[1]["parts"][0]["text"], "reply");
    }

    #[test]
    fn anthropic_headers_follow_hermes_compatible_endpoint_rules() {
        let mut provider = LlmProvider {
            id: "minimax".into(),
            name: "MiniMax".into(),
            provider_type: "anthropic".into(),
            preset: Some("anthropic".into()),
            base_url: "https://api.minimaxi.com/anthropic/v1/messages".into(),
            append_chat_path: true,
            api_key_env: String::new(),
            api_key: None,
            model: "claude-compatible".into(),
            enabled: true,
            timeout_seconds: 60,
            request_timeout_seconds: None,
            stale_timeout_seconds: None,
            models: json!({}),
            prompt_cache_mode: "off".into(),
            prompt_cache_ttl: "5m".into(),
            prompt_cache_layout: "native".into(),
        };

        let minimax_base = effective_anthropic_base_url(&provider, Some("mm-key"));
        let minimax = anthropic_headers(&provider, Some("mm-key"), &minimax_base).unwrap();
        assert!(minimax.get(AUTHORIZATION).is_some());
        assert!(minimax.get("x-api-key").is_none());
        assert_eq!(
            minimax.get("anthropic-beta").unwrap().to_str().unwrap(),
            "interleaved-thinking-2025-05-14"
        );

        provider.base_url =
            "https://demo.services.ai.azure.com/models/anthropic/v1/messages".into();
        let azure_base = effective_anthropic_base_url(&provider, Some("az-key"));
        let azure = anthropic_headers(&provider, Some("az-key"), &azure_base).unwrap();
        assert!(azure.get(AUTHORIZATION).is_some());
        assert!(azure
            .get("anthropic-beta")
            .unwrap()
            .to_str()
            .unwrap()
            .contains("context-1m-2025-08-07"));
        assert!(
            anthropic_messages_url(&provider, Some("az-key")).contains("api-version=2025-04-15")
        );

        provider.base_url = "https://api.kimi.com/coding/v1/messages".into();
        let kimi_base = effective_anthropic_base_url(&provider, Some("kimi-key"));
        let kimi = anthropic_headers(&provider, Some("kimi-key"), &kimi_base).unwrap();
        assert_eq!(
            kimi.get("User-Agent").unwrap().to_str().unwrap(),
            "claude-code/0.1.0"
        );
        assert!(kimi.get("x-api-key").is_some());

        provider.id = "minimax-oauth".into();
        provider.provider_type = "openai".into();
        provider.preset = Some("minimax-oauth".into());
        provider.base_url.clear();
        provider.model = "MiniMax-M2.7".into();
        assert!(is_anthropic_compatible(&provider));
        assert_eq!(
            anthropic_messages_url(&provider, Some("mm-oauth-token")),
            "https://api.minimax.io/anthropic/v1/messages"
        );
    }

    #[test]
    fn anthropic_headers_use_claude_code_oauth_bearer_rules() {
        let provider = LlmProvider {
            id: "anthropic".into(),
            name: "Anthropic".into(),
            provider_type: "anthropic".into(),
            preset: Some("anthropic".into()),
            base_url: "https://api.anthropic.com".into(),
            append_chat_path: true,
            api_key_env: String::new(),
            api_key: None,
            model: "claude-sonnet-4-6".into(),
            enabled: true,
            timeout_seconds: 60,
            request_timeout_seconds: None,
            stale_timeout_seconds: None,
            models: json!({}),
            prompt_cache_mode: "off".into(),
            prompt_cache_ttl: "5m".into(),
            prompt_cache_layout: "native".into(),
        };

        let headers =
            anthropic_headers(&provider, Some("cc-valid-token"), &provider.base_url).unwrap();
        assert_eq!(
            headers.get(AUTHORIZATION).unwrap().to_str().unwrap(),
            "Bearer cc-valid-token"
        );
        assert!(headers.get("x-api-key").is_none());
        assert_eq!(headers.get("x-app").unwrap().to_str().unwrap(), "cli");
        assert!(headers
            .get("anthropic-beta")
            .unwrap()
            .to_str()
            .unwrap()
            .contains("oauth-2025-04-20"));
    }

    #[test]
    fn claude_code_credentials_payload_resolves_non_expired_token() {
        let payload = json!({
            "claudeAiOauth": {
                "accessToken": "cc-from-file",
                "refreshToken": "refresh",
                "expiresAt": 4_102_444_800_000i64
            }
        });
        assert_eq!(
            claude_code_oauth_token_from_credentials(&payload).as_deref(),
            Some("cc-from-file")
        );

        let expired = json!({
            "claudeAiOauth": {
                "accessToken": "cc-expired",
                "expiresAt": 1_000i64
            }
        });
        assert!(claude_code_oauth_token_from_credentials(&expired).is_none());
    }

    #[test]
    fn kimi_code_key_routes_to_anthropic_coding_endpoint() {
        let provider = LlmProvider {
            id: "kimi".into(),
            name: "Kimi".into(),
            provider_type: "openai".into(),
            preset: Some("moonshot".into()),
            base_url: "https://api.moonshot.ai/v1".into(),
            append_chat_path: true,
            api_key_env: String::new(),
            api_key: Some("sk-kimi-example".into()),
            model: "kimi-k2".into(),
            enabled: true,
            timeout_seconds: 60,
            request_timeout_seconds: None,
            stale_timeout_seconds: None,
            models: json!({}),
            prompt_cache_mode: "off".into(),
            prompt_cache_ttl: "5m".into(),
            prompt_cache_layout: "native".into(),
        };

        assert!(is_anthropic_compatible(&provider));
        assert_eq!(
            effective_anthropic_base_url(&provider, Some("sk-kimi-example")),
            "https://api.kimi.com/coding"
        );
        assert_eq!(
            anthropic_messages_url(&provider, Some("sk-kimi-example")),
            "https://api.kimi.com/coding/v1/messages"
        );
        let headers = anthropic_headers(
            &provider,
            Some("sk-kimi-example"),
            &effective_anthropic_base_url(&provider, Some("sk-kimi-example")),
        )
        .unwrap();
        assert_eq!(
            headers.get("User-Agent").unwrap().to_str().unwrap(),
            "claude-code/0.1.0"
        );
    }

    #[test]
    fn api_key_resolution_ignores_placeholder_secrets() {
        let mut provider = LlmProvider {
            id: "placeholder".into(),
            name: "Placeholder".into(),
            provider_type: "openai".into(),
            preset: None,
            base_url: "https://example.test/v1".into(),
            append_chat_path: true,
            api_key_env: String::new(),
            api_key: Some("your_api_key".into()),
            model: "test".into(),
            enabled: true,
            timeout_seconds: 60,
            request_timeout_seconds: None,
            stale_timeout_seconds: None,
            models: json!({}),
            prompt_cache_mode: "off".into(),
            prompt_cache_ttl: "5m".into(),
            prompt_cache_layout: "native".into(),
        };

        assert!(resolve_api_key(&provider).is_none());
        assert!(!usable_secret_value("changeme"));
        assert!(!usable_secret_value("none"));
        assert!(usable_secret_value("sk-real-key"));
        assert!(looks_like_inline_api_key("tp-real-key"));

        provider.api_key = Some("sk-real-key".into());
        assert_eq!(resolve_api_key(&provider).as_deref(), Some("sk-real-key"));
        provider.api_key = None;
        provider.api_key_env = "tp-real-key".into();
        assert_eq!(resolve_api_key(&provider).as_deref(), Some("tp-real-key"));
    }

    #[test]
    fn api_key_resolution_reads_hermes_auth_store_tokens() {
        let _guard = crate::hermes_auth::HERMES_AUTH_TEST_ENV_LOCK
            .lock()
            .unwrap();
        let dir = std::env::temp_dir().join(format!(
            "synthchat-llm-hermes-auth-{}",
            crate::models::new_id("test")
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("auth.json"),
            json!({
                "providers": {
                    "openai-codex": {
                        "access_token": "codex-runtime-token",
                        "base_url": "https://chatgpt.com/backend-api/codex",
                        "expires_at": "2999-01-01T00:00:00Z"
                    }
                }
            })
            .to_string(),
        )
        .unwrap();
        std::env::set_var("HERMES_HOME", &dir);

        let provider = LlmProvider {
            id: "openai-codex".into(),
            name: "OpenAI Codex".into(),
            provider_type: "codex".into(),
            preset: Some("openai-codex".into()),
            base_url: String::new(),
            append_chat_path: true,
            api_key_env: String::new(),
            api_key: None,
            model: "gpt-5-codex".into(),
            enabled: true,
            timeout_seconds: 60,
            request_timeout_seconds: None,
            stale_timeout_seconds: None,
            models: json!({}),
            prompt_cache_mode: "off".into(),
            prompt_cache_ttl: "5m".into(),
            prompt_cache_layout: "native".into(),
        };

        assert_eq!(
            resolve_api_key(&provider).as_deref(),
            Some("codex-runtime-token")
        );
        assert_eq!(
            provider_base_url(&provider),
            "https://chatgpt.com/backend-api/codex"
        );

        std::env::remove_var("HERMES_HOME");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn runtime_credential_binding_freezes_hermes_pool_source() {
        let _guard = crate::hermes_auth::HERMES_AUTH_TEST_ENV_LOCK
            .lock()
            .unwrap();
        let old_hermes_home = std::env::var_os("HERMES_HOME");
        let old_strategy = std::env::var_os("SYNTHCHAT_LLM_CREDENTIAL_POOL_STRATEGY");
        let dir = std::env::temp_dir().join(format!(
            "synthchat-llm-hermes-binding-{}",
            crate::models::new_id("test")
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("auth.json"),
            json!({
                "credential_pool": {
                    "openrouter": [{
                        "label": "first",
                        "access_token": "first-token",
                        "base_url": "https://first.example/v1",
                        "priority": 0
                    }, {
                        "label": "second",
                        "access_token": "second-token",
                        "base_url": "https://second.example/v1",
                        "priority": 1
                    }]
                }
            })
            .to_string(),
        )
        .unwrap();
        std::env::set_var("HERMES_HOME", &dir);
        std::env::set_var("SYNTHCHAT_LLM_CREDENTIAL_POOL_STRATEGY", "round_robin");

        let provider = LlmProvider {
            id: "openrouter".into(),
            name: "OpenRouter".into(),
            provider_type: "openai".into(),
            preset: Some("openrouter".into()),
            base_url: String::new(),
            append_chat_path: true,
            api_key_env: String::new(),
            api_key: None,
            model: "openrouter/test".into(),
            enabled: true,
            timeout_seconds: 60,
            request_timeout_seconds: None,
            stale_timeout_seconds: None,
            models: json!({}),
            prompt_cache_mode: "off".into(),
            prompt_cache_ttl: "5m".into(),
            prompt_cache_layout: "native".into(),
        };

        let binding = bind_runtime_credential_for_attempt(&provider);
        let bound_source = binding.source.as_deref().unwrap();
        let (bound_key, bound_base_url) = match bound_source {
            "hermes-pool:openrouter:first" => ("first-token", "https://first.example/v1"),
            "hermes-pool:openrouter:second" => ("second-token", "https://second.example/v1"),
            source => panic!("unexpected credential source: {source}"),
        };
        assert_eq!(binding.provider.api_key.as_deref(), Some(bound_key));
        assert_eq!(binding.provider.base_url, bound_base_url);

        assert_eq!(
            resolve_api_key(&binding.provider).as_deref(),
            Some(bound_key)
        );

        restore_env_var("SYNTHCHAT_LLM_CREDENTIAL_POOL_STRATEGY", old_strategy);
        restore_env_var("HERMES_HOME", old_hermes_home);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn provider_env_candidates_follow_hermes_registry_aliases() {
        let mut provider = LlmProvider {
            id: "anthropic".into(),
            name: "Anthropic".into(),
            provider_type: "anthropic".into(),
            preset: Some("anthropic".into()),
            base_url: "https://api.anthropic.com".into(),
            append_chat_path: true,
            api_key_env: String::new(),
            api_key: None,
            model: "claude-sonnet-4-6".into(),
            enabled: true,
            timeout_seconds: 60,
            request_timeout_seconds: None,
            stale_timeout_seconds: None,
            models: json!({}),
            prompt_cache_mode: "off".into(),
            prompt_cache_ttl: "5m".into(),
            prompt_cache_layout: "native".into(),
        };
        assert_eq!(
            provider_api_key_env_candidates(&provider),
            vec![
                "ANTHROPIC_API_KEY",
                "ANTHROPIC_TOKEN",
                "CLAUDE_CODE_OAUTH_TOKEN"
            ]
        );

        provider.id = "gemini".into();
        provider.provider_type = "gemini".into();
        provider.preset = Some("google".into());
        provider.base_url = "https://generativelanguage.googleapis.com/v1beta".into();
        provider.model = "gemini-2.5-pro".into();
        assert_eq!(
            provider_api_key_env_candidates(&provider),
            vec!["GOOGLE_API_KEY", "GEMINI_API_KEY"]
        );

        provider.id = "local-echo".into();
        provider.provider_type = "anthropic".into();
        provider.preset = Some("anthropic".into());
        provider.base_url = "https://token-plan-sgp.xiaomimimo.com/anthropic".into();
        provider.model = "mimo-v2.5".into();
        assert_eq!(
            provider_api_key_env_candidates(&provider),
            vec!["MIMO_API_KEY", "XIAOMI_API_KEY"]
        );

        provider.id = "kimi".into();
        provider.provider_type = "openai".into();
        provider.preset = Some("moonshot".into());
        provider.base_url = "https://api.moonshot.ai/v1".into();
        provider.model = "kimi-k2".into();
        assert_eq!(
            provider_api_key_env_candidates(&provider),
            vec![
                "KIMI_API_KEY",
                "KIMI_CODING_API_KEY",
                "KIMI_CN_API_KEY",
                "MOONSHOT_API_KEY"
            ]
        );

        provider.id = "openrouter".into();
        provider.provider_type = "openai".into();
        provider.preset = Some("openrouter".into());
        provider.base_url = "https://openrouter.ai/api/v1".into();
        provider.model = "openai/gpt-5".into();
        assert_eq!(
            provider_api_key_env_candidates(&provider),
            vec!["OPENROUTER_API_KEY", "OPENAI_API_KEY"]
        );

        provider.model = "anthropic/claude-sonnet-4.6".into();
        assert_eq!(
            provider_api_key_env_candidates(&provider),
            vec!["OPENROUTER_API_KEY", "OPENAI_API_KEY"]
        );

        provider.id = "copilot-acp".into();
        provider.provider_type = "codex".into();
        provider.preset = Some("github".into());
        provider.base_url = "acp://copilot".into();
        provider.model = "gpt-5".into();
        assert_eq!(
            provider_api_key_env_candidates(&provider),
            vec!["COPILOT_GITHUB_TOKEN", "GH_TOKEN", "GITHUB_TOKEN"]
        );

        provider.id = "huggingface".into();
        provider.provider_type = "openai".into();
        provider.preset = Some("huggingface".into());
        provider.base_url = "https://router.huggingface.co/v1".into();
        provider.model = "qwen/qwen3-coder".into();
        assert_eq!(
            provider_api_key_env_candidates(&provider),
            vec!["HF_TOKEN", "HF_API_KEY", "HUGGINGFACE_API_KEY"]
        );

        provider.id = "tencent-tokenhub".into();
        provider.provider_type = "openai".into();
        provider.preset = Some("tokenhub".into());
        provider.base_url.clear();
        provider.model = "hunyuan-code".into();
        assert_eq!(
            provider_api_key_env_candidates(&provider),
            vec!["TOKENHUB_API_KEY"]
        );

        provider.id = "groq".into();
        provider.provider_type = "openai".into();
        provider.preset = Some("groq".into());
        provider.base_url = "https://api.groq.com/openai/v1".into();
        provider.model = "llama-3.3-70b-versatile".into();
        assert_eq!(
            provider_api_key_env_candidates(&provider),
            vec!["GROQ_API_KEY"]
        );

        provider.id = "mistral".into();
        provider.preset = Some("mistral".into());
        provider.base_url = "https://api.mistral.ai/v1".into();
        provider.model = "mistral-large-latest".into();
        assert_eq!(
            provider_api_key_env_candidates(&provider),
            vec!["MISTRAL_API_KEY"]
        );
    }

    #[test]
    fn provider_base_url_candidates_follow_hermes_registry_aliases() {
        let mut provider = LlmProvider {
            id: "anthropic".into(),
            name: "Anthropic".into(),
            provider_type: "anthropic".into(),
            preset: Some("anthropic".into()),
            base_url: "https://configured.example/v1".into(),
            append_chat_path: true,
            api_key_env: String::new(),
            api_key: None,
            model: "claude-sonnet-4-6".into(),
            enabled: true,
            timeout_seconds: 60,
            request_timeout_seconds: None,
            stale_timeout_seconds: None,
            models: json!({}),
            prompt_cache_mode: "off".into(),
            prompt_cache_ttl: "5m".into(),
            prompt_cache_layout: "native".into(),
        };
        assert_eq!(
            provider_base_url(&provider),
            "https://configured.example/v1"
        );
        assert_eq!(
            provider_base_url_env_candidates(&provider),
            vec!["ANTHROPIC_BASE_URL"]
        );

        provider.base_url.clear();
        provider.id = "kimi".into();
        provider.provider_type = "openai".into();
        provider.preset = Some("moonshot".into());
        provider.model = "kimi-k2".into();
        assert_eq!(
            provider_base_url_env_candidates(&provider),
            vec!["KIMI_BASE_URL"]
        );

        provider.id = "xai".into();
        provider.provider_type = "openai".into();
        provider.preset = Some("xai".into());
        provider.model = "grok-4".into();
        assert_eq!(
            provider_base_url_env_candidates(&provider),
            vec!["HERMES_XAI_BASE_URL", "XAI_BASE_URL"]
        );

        provider.id = "openrouter".into();
        provider.provider_type = "openai".into();
        provider.preset = Some("openrouter".into());
        provider.model = "anthropic/claude-sonnet-4.6".into();
        assert_eq!(
            provider_base_url_env_candidates(&provider),
            vec!["OPENROUTER_BASE_URL"]
        );

        provider.id = "copilot-acp".into();
        provider.provider_type = "codex".into();
        provider.preset = Some("github".into());
        provider.model = "gpt-5".into();
        assert_eq!(
            provider_base_url_env_candidates(&provider),
            vec!["COPILOT_ACP_BASE_URL"]
        );

        provider.id = "opencode-go".into();
        provider.provider_type = "openai".into();
        provider.preset = Some("opencode-go".into());
        assert_eq!(
            provider_base_url_env_candidates(&provider),
            vec!["OPENCODE_GO_BASE_URL"]
        );

        provider.id = "novita".into();
        provider.provider_type = "openai".into();
        provider.preset = Some("novita".into());
        assert_eq!(
            provider_base_url_env_candidates(&provider),
            vec!["NOVITA_BASE_URL"]
        );

        provider.id = "azure-foundry".into();
        provider.provider_type = "openai".into();
        provider.preset = Some("azure-foundry".into());
        assert_eq!(
            provider_base_url_env_candidates(&provider),
            vec!["AZURE_FOUNDRY_BASE_URL"]
        );

        provider.id = "lmstudio".into();
        provider.provider_type = "openai".into();
        provider.preset = Some("lmstudio".into());
        assert_eq!(
            provider_base_url_env_candidates(&provider),
            vec!["LM_BASE_URL"]
        );

        provider.id = "qwen-oauth".into();
        provider.provider_type = "openai".into();
        provider.preset = Some("qwen-oauth".into());
        provider.model = "qwen3-coder-plus".into();
        assert_eq!(
            provider_base_url_env_candidates(&provider),
            vec![
                "HERMES_QWEN_BASE_URL",
                "DASHSCOPE_BASE_URL",
                "ALIBABA_CODING_PLAN_BASE_URL"
            ]
        );

        provider.id = "groq".into();
        provider.provider_type = "openai".into();
        provider.preset = Some("groq".into());
        provider.model = "llama-3.3-70b-versatile".into();
        assert_eq!(
            provider_base_url_env_candidates(&provider),
            vec!["GROQ_BASE_URL"]
        );

        provider.id = "mistral".into();
        provider.preset = Some("mistral".into());
        provider.model = "mistral-large-latest".into();
        assert_eq!(
            provider_base_url_env_candidates(&provider),
            vec!["MISTRAL_BASE_URL"]
        );
    }

    #[test]
    fn provider_default_base_urls_follow_hermes_overlays() {
        let mut provider = LlmProvider {
            id: "nous".into(),
            name: "Nous".into(),
            provider_type: "openai".into(),
            preset: Some("nous".into()),
            base_url: String::new(),
            append_chat_path: true,
            api_key_env: String::new(),
            api_key: None,
            model: "hermes-4".into(),
            enabled: true,
            timeout_seconds: 60,
            request_timeout_seconds: None,
            stale_timeout_seconds: None,
            models: json!({}),
            prompt_cache_mode: "off".into(),
            prompt_cache_ttl: "5m".into(),
            prompt_cache_layout: "native".into(),
        };

        assert_eq!(
            provider_default_base_url(&provider),
            Some("https://inference-api.nousresearch.com/v1")
        );

        provider.id = "openrouter".into();
        provider.preset = Some("openrouter".into());
        provider.model = "anthropic/claude-sonnet-4.6".into();
        assert_eq!(
            provider_default_base_url(&provider),
            Some("https://openrouter.ai/api/v1")
        );

        provider.id = "openai".into();
        provider.preset = Some("openai".into());
        provider.model = "gpt-5".into();
        assert_eq!(
            provider_default_base_url(&provider),
            Some("https://api.openai.com/v1")
        );

        provider.id = "anthropic".into();
        provider.provider_type = "anthropic".into();
        provider.preset = Some("anthropic".into());
        provider.model = "claude-sonnet-4.6".into();
        assert_eq!(
            provider_default_base_url(&provider),
            Some("https://api.anthropic.com")
        );

        provider.id = "qwen-oauth".into();
        provider.provider_type = "openai".into();
        provider.preset = Some("qwen-oauth".into());
        assert_eq!(
            provider_default_base_url(&provider),
            Some("https://portal.qwen.ai/v1")
        );

        provider.id = "minimax-oauth".into();
        provider.preset = Some("minimax-oauth".into());
        assert_eq!(
            provider_default_base_url(&provider),
            Some("https://api.minimax.io/anthropic")
        );

        provider.id = "stepfun".into();
        provider.preset = Some("stepfun".into());
        assert_eq!(
            provider_default_base_url(&provider),
            Some("https://api.stepfun.ai/step_plan/v1")
        );

        provider.id = "ollama-cloud".into();
        provider.preset = Some("ollama-cloud".into());
        assert_eq!(
            provider_default_base_url(&provider),
            Some("https://ollama.com/v1")
        );

        provider.id = "deepseek".into();
        provider.preset = Some("deepseek".into());
        assert_eq!(
            provider_default_base_url(&provider),
            Some("https://api.deepseek.com/v1")
        );

        provider.id = "groq".into();
        provider.preset = Some("groq".into());
        assert_eq!(
            provider_default_base_url(&provider),
            Some("https://api.groq.com/openai/v1")
        );

        provider.id = "mistral".into();
        provider.preset = Some("mistral".into());
        assert_eq!(
            provider_default_base_url(&provider),
            Some("https://api.mistral.ai/v1")
        );

        provider.id = "google-gemini-cli".into();
        provider.preset = Some("google-gemini-cli".into());
        assert_eq!(provider_default_base_url(&provider), None);
    }

    #[test]
    fn anthropic_max_tokens_follows_hermes_model_limits() {
        assert_eq!(
            resolve_anthropic_messages_max_tokens(2048, "claude-sonnet-4-6-20260415"),
            2048
        );
        assert_eq!(
            resolve_anthropic_messages_max_tokens(0, "claude-sonnet-4.6-20260415"),
            64_000
        );
        assert_eq!(
            resolve_anthropic_messages_max_tokens(0, "claude-3-5-sonnet-20241022"),
            8_192
        );
        assert_eq!(
            resolve_anthropic_messages_max_tokens(0, "MiniMax-M2.7"),
            131_072
        );
        assert_eq!(
            resolve_anthropic_messages_max_tokens(0, "unknown-model"),
            128_000
        );
    }

    #[test]
    fn anthropic_sampling_params_follow_hermes_model_gate() {
        assert!(anthropic_model_forbids_sampling_params(
            "claude-opus-4.7-20260416"
        ));
        assert!(anthropic_model_forbids_sampling_params(
            "anthropic/claude-opus-4-8"
        ));
        assert!(!anthropic_model_forbids_sampling_params(
            "claude-sonnet-4.6-20260415"
        ));
        assert!(!anthropic_model_forbids_sampling_params(
            "claude-3-5-sonnet-20241022"
        ));
    }
}
