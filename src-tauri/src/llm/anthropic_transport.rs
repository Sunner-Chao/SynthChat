use std::{collections::BTreeMap, time::Instant};

use futures::StreamExt;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde_json::{json, Value};

use crate::{
    error::{AppError, AppResult},
    models::{ChatMessage, LlmProvider, Persona, ToolDefinition},
};

use super::*;

#[derive(Debug, Default)]
struct AnthropicStreamToolCall {
    id: String,
    name: String,
    input_json: String,
}

pub(super) async fn complete_anthropic_compatible(
    provider: &LlmProvider,
    persona: &Persona,
    system_prompt: String,
    history: Vec<ChatMessage>,
    native_tools: Option<&[ToolDefinition]>,
    options: &LlmCallOptions,
) -> AppResult<LlmReply> {
    let model = if !persona.llm_model.trim().is_empty() {
        persona.llm_model.trim()
    } else {
        provider.model.trim()
    };
    let api_key = resolve_api_key(provider);
    let effective_base_url = effective_anthropic_base_url(provider, api_key.as_deref());
    let url = anthropic_messages_url(provider, api_key.as_deref());

    let mut headers = anthropic_headers(provider, api_key.as_deref(), &effective_base_url)?;
    if options.fast_mode_enabled {
        append_anthropic_beta_header(&mut headers, "fast-mode-2026-02-01")?;
    }

    let messages = build_anthropic_messages(history);

    let cache_policy = prompt_cache_policy(provider, model);
    let system_value = anthropic_system_value(system_prompt, cache_policy.as_ref());
    let max_tokens = resolve_anthropic_messages_max_tokens(persona.max_tokens, model);
    let mut body = json!({
        "model": model,
        "system": system_value,
        "messages": messages,
        "temperature": persona.temperature,
        "max_tokens": max_tokens,
        "stream": options.stream_delta_callback.is_some()
    });
    if options.fast_mode_enabled {
        body["speed"] = json!("fast");
    }
    if anthropic_model_forbids_sampling_params(model) {
        if let Some(object) = body.as_object_mut() {
            object.remove("temperature");
        }
    }
    if let Some(tools) = native_tools.filter(|tools| !tools.is_empty()) {
        body["tools"] = json!(anthropic_tool_schemas(tools));
    }

    let client = reqwest::Client::builder()
        .timeout(provider_request_timeout_duration(provider, model))
        .default_headers(headers)
        .build()
        .map_err(|e| AppError::Llm(e.to_string()))?;

    let started_at = Instant::now();
    let response = send_llm_request_with_stale_timeout(
        client.post(url.clone()).json(&body),
        provider,
        model,
        "anthropic messages request",
    )
    .await?;

    let status = response.status();
    let response_headers = response.headers().clone();
    if !status.is_success() {
        let text = response
            .text()
            .await
            .map_err(|e| AppError::Llm(format!("failed to read llm response: {e}")))?;
        return Err(AppError::Llm(format!(
            "provider returned {status}: {}",
            response_preview(&text)
        )));
    }

    let transport = LlmTransportMetadata {
        transport: "anthropic_messages",
        method: "POST",
        endpoint: url,
        status: Some(status.as_u16()),
        elapsed_ms: Some(started_at.elapsed().as_millis().min(u64::MAX as u128) as u64),
        retry_count: 0,
        retry_reason: None,
    };
    if let Some(callback) = options.stream_delta_callback.as_ref() {
        let reply = read_anthropic_sse_stream(
            response,
            callback,
            provider_stream_stale_timeout_duration(provider, model),
        )
        .await?;
        return Ok(with_reply_metadata_and_transport(
            reply,
            provider,
            model,
            &response_headers,
            Some(transport),
        ));
    }

    let text = response
        .text()
        .await
        .map_err(|e| AppError::Llm(format!("failed to read llm response: {e}")))?;
    let payload: Value = serde_json::from_str(&text).map_err(|e| {
        AppError::Llm(format!(
            "invalid anthropic response ({status}): {e}; {}",
            invalid_response_body_detail(&text, &response_headers)
        ))
    })?;

    parse_anthropic_compatible(payload).map(|reply| {
        with_reply_metadata_and_transport(
            reply,
            provider,
            model,
            &response_headers,
            Some(transport),
        )
    })
}

async fn read_anthropic_sse_stream(
    response: reqwest::Response,
    callback: &LlmDeltaCallback,
    stale_timeout: Option<std::time::Duration>,
) -> AppResult<LlmReply> {
    let mut buffer = String::new();
    let mut content = String::new();
    let mut prompt_tokens = 0usize;
    let mut completion_tokens = 0usize;
    let mut cache_read_tokens = 0usize;
    let mut cache_write_tokens = 0usize;
    let mut stop_reason = None::<String>;
    let mut tool_calls = BTreeMap::<usize, AnthropicStreamToolCall>::new();
    let mut stream = response.bytes_stream();

    loop {
        let next_chunk = if let Some(timeout) = stale_timeout {
            tokio::time::timeout(timeout, stream.next())
                .await
                .map_err(|_| {
                    AppError::Llm(format!(
                        "anthropic stream stale: no provider bytes for {}s",
                        timeout.as_secs_f64()
                    ))
                })?
        } else {
            stream.next().await
        };
        let Some(chunk) = next_chunk else {
            break;
        };
        let chunk =
            chunk.map_err(|e| AppError::Llm(format!("failed to read anthropic stream: {e}")))?;
        buffer.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(newline) = buffer.find('\n') {
            let mut line = buffer[..newline].trim().to_string();
            buffer.replace_range(..=newline, "");
            if line.ends_with('\r') {
                line.pop();
            }
            handle_anthropic_sse_line(
                &line,
                callback,
                &mut content,
                &mut prompt_tokens,
                &mut completion_tokens,
                &mut cache_read_tokens,
                &mut cache_write_tokens,
                &mut stop_reason,
                &mut tool_calls,
            )?;
        }
    }
    if !buffer.trim().is_empty() {
        let line = buffer.trim().to_string();
        handle_anthropic_sse_line(
            &line,
            callback,
            &mut content,
            &mut prompt_tokens,
            &mut completion_tokens,
            &mut cache_read_tokens,
            &mut cache_write_tokens,
            &mut stop_reason,
            &mut tool_calls,
        )?;
    }

    let stream_tool_calls = anthropic_stream_tool_calls(tool_calls)?;
    let has_tool_calls = !stream_tool_calls.is_empty();
    if has_tool_calls {
        let tool_json = json!({"tool_calls": stream_tool_calls}).to_string();
        if content.trim().is_empty() {
            content = tool_json;
        } else {
            content = format!("{content}\n\n{tool_json}");
        }
    }
    let content = scrub_reasoning_blocks(&content);
    if content.trim().is_empty() {
        return Err(AppError::Llm(
            "anthropic stream produced no visible text".into(),
        ));
    }
    Ok(LlmReply {
        prompt_tokens,
        completion_tokens: if completion_tokens == 0 {
            estimate_tokens(&content)
        } else {
            completion_tokens
        },
        cache_read_tokens,
        cache_write_tokens,
        reasoning_tokens: 0,
        content,
        provider_id: None,
        provider_type: None,
        model: None,
        base_url: None,
        estimated_cost_usd: None,
        cost_status: None,
        cost_source: None,
        rate_limit_state: None,
        transport_diagnostics: None,
        finish_reason: if has_tool_calls {
            Some("tool_calls".into())
        } else {
            stop_reason.or_else(|| Some("stop".into()))
        },
        provider_data: None,
        failover_attempts: Vec::new(),
    })
}

fn handle_anthropic_sse_line(
    line: &str,
    callback: &LlmDeltaCallback,
    content: &mut String,
    prompt_tokens: &mut usize,
    completion_tokens: &mut usize,
    cache_read_tokens: &mut usize,
    cache_write_tokens: &mut usize,
    stop_reason: &mut Option<String>,
    tool_calls: &mut BTreeMap<usize, AnthropicStreamToolCall>,
) -> AppResult<()> {
    let Some(data) = line.trim().strip_prefix("data:") else {
        return Ok(());
    };
    let data = data.trim();
    if data.is_empty() || data == "[DONE]" {
        return Ok(());
    }
    let Ok(payload) = serde_json::from_str::<Value>(data) else {
        return Ok(());
    };
    if let Some(delta) = payload
        .pointer("/delta/text")
        .and_then(Value::as_str)
        .filter(|delta| !delta.is_empty())
    {
        content.push_str(delta);
        callback(delta)?;
    }
    track_anthropic_stream_tool_call(&payload, tool_calls);
    let usage = payload
        .pointer("/message/usage")
        .or_else(|| payload.pointer("/usage"));
    if *prompt_tokens == 0 {
        *prompt_tokens = usage
            .and_then(|usage| usage.get("input_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
    }
    if let Some(output_tokens) = usage
        .and_then(|usage| usage.get("output_tokens"))
        .and_then(Value::as_u64)
    {
        *completion_tokens = output_tokens as usize;
    }
    if *cache_read_tokens == 0 {
        *cache_read_tokens = usage
            .and_then(|usage| usage.get("cache_read_input_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
    }
    if *cache_write_tokens == 0 {
        *cache_write_tokens = usage
            .and_then(|usage| usage.get("cache_creation_input_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
    }
    if let Some(reason) = payload
        .pointer("/delta/stop_reason")
        .and_then(Value::as_str)
        .filter(|reason| !reason.is_empty())
    {
        *stop_reason = Some(if reason == "tool_use" {
            "tool_calls".into()
        } else {
            reason.to_string()
        });
    }
    Ok(())
}

fn track_anthropic_stream_tool_call(
    payload: &Value,
    tool_calls: &mut BTreeMap<usize, AnthropicStreamToolCall>,
) {
    let index = payload.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
    if let Some(block) = payload.get("content_block") {
        if block.get("type").and_then(Value::as_str) == Some("tool_use") {
            let call = tool_calls.entry(index).or_default();
            if let Some(id) = block.get("id").and_then(Value::as_str) {
                call.id = id.to_string();
            }
            if let Some(name) = block.get("name").and_then(Value::as_str) {
                call.name = name.to_string();
            }
        }
    }
    if let Some(partial_json) = payload
        .pointer("/delta/partial_json")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    {
        tool_calls
            .entry(index)
            .or_default()
            .input_json
            .push_str(partial_json);
    }
}

fn anthropic_stream_tool_calls(
    tool_calls: BTreeMap<usize, AnthropicStreamToolCall>,
) -> AppResult<Vec<Value>> {
    tool_calls
        .into_values()
        .filter(|call| !call.name.trim().is_empty())
        .map(|call| {
            let arguments = if call.input_json.trim().is_empty() {
                json!({})
            } else {
                serde_json::from_str::<Value>(&call.input_json).map_err(|error| {
                    AppError::Llm(format!(
                        "invalid anthropic streamed tool arguments for {}: {error}; body: {}",
                        call.name,
                        response_preview(&call.input_json)
                    ))
                })?
            };
            Ok(json!({
                "type": "function",
                "id": if call.id.trim().is_empty() { Value::Null } else { json!(call.id) },
                "function": {
                    "name": call.name,
                    "arguments": arguments
                }
            }))
        })
        .collect()
}

pub(super) fn build_anthropic_messages(history: Vec<ChatMessage>) -> Vec<Value> {
    let mut messages = Vec::new();
    for item in history {
        if let Some(tool_replay) = tool_replay_message(&item) {
            messages.push(json!({
                "role": "assistant",
                "content": [{
                    "type": "tool_use",
                    "id": tool_replay.call_id,
                    "name": tool_replay.name,
                    "input": tool_replay.arguments,
                }]
            }));
            messages.push(json!({
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": tool_replay.call_id,
                    "content": tool_replay.content,
                    "is_error": !tool_replay.ok,
                }]
            }));
            continue;
        }
        if let Some(message) = sanitized_wire_message(item, true) {
            push_anthropic_text_message(&mut messages, &message.role, &message.content);
        }
    }
    messages
}

fn push_anthropic_text_message(messages: &mut Vec<Value>, role: &str, content: &str) {
    let content = sanitize_provider_text(content);
    if content.trim().is_empty() {
        return;
    }
    if let Some(previous) = messages.last_mut() {
        if previous.get("role").and_then(Value::as_str) == Some(role) {
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

pub(super) fn is_anthropic_compatible(provider: &LlmProvider) -> bool {
    let provider_id = provider.id.to_lowercase();
    let provider_type = provider.provider_type.to_lowercase();
    let preset = provider
        .preset
        .as_deref()
        .unwrap_or_default()
        .to_lowercase();
    let base_url = provider_base_url(provider).to_lowercase();
    provider_id.contains("minimax")
        || provider_type == "anthropic"
        || provider_type.contains("minimax")
        || preset.contains("anthropic")
        || preset.contains("minimax")
        || base_url.contains("/anthropic")
        || base_url.ends_with("/v1/messages")
        || provider_uses_kimi_code_endpoint(provider, resolve_api_key(provider).as_deref())
}

pub(super) fn anthropic_system_value(
    system_prompt: String,
    cache_policy: Option<&PromptCachePolicy>,
) -> Value {
    if let Some(policy) = cache_policy.filter(|policy| policy.native_layout) {
        json!([{
            "type": "text",
            "text": system_prompt,
            "cache_control": cache_control_value(policy),
        }])
    } else {
        json!(system_prompt)
    }
}

pub(super) fn anthropic_messages_url(provider: &LlmProvider, api_key: Option<&str>) -> String {
    let effective_base = effective_anthropic_base_url(provider, api_key);
    let base = effective_base.trim().trim_end_matches('/');
    let url = if base.ends_with("/v1/messages") {
        base.to_string()
    } else if base.ends_with("/v1") {
        format!("{base}/messages")
    } else if provider.append_chat_path {
        format!("{base}/v1/messages")
    } else {
        base.to_string()
    };
    if is_azure_anthropic_endpoint(base) && !url.to_ascii_lowercase().contains("api-version=") {
        let separator = if url.contains('?') { '&' } else { '?' };
        format!("{url}{separator}api-version=2025-04-15")
    } else {
        url
    }
}

pub(super) fn effective_anthropic_base_url(
    provider: &LlmProvider,
    api_key: Option<&str>,
) -> String {
    if provider_uses_kimi_code_endpoint(provider, api_key) {
        "https://api.kimi.com/coding".into()
    } else {
        provider_base_url(provider)
    }
}

fn provider_uses_kimi_code_endpoint(provider: &LlmProvider, api_key: Option<&str>) -> bool {
    let Some(key) = api_key
        .map(str::trim)
        .filter(|key| key.starts_with("sk-kimi-"))
    else {
        return false;
    };
    if key.is_empty() {
        return false;
    }
    let base_url = provider_base_url(provider);
    let base = base_url.trim();
    if base.is_empty() || is_kimi_coding_endpoint(base) || host_matches(base, "api.moonshot.ai") {
        return true;
    }
    let provider_type = provider.provider_type.to_ascii_lowercase();
    let preset = provider
        .preset
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let model = provider.model.to_ascii_lowercase();
    provider_type.contains("kimi")
        || provider_type.contains("moonshot")
        || preset.contains("kimi")
        || preset.contains("moonshot")
        || model.contains("kimi")
        || model.contains("moonshot")
}

pub(super) fn anthropic_headers(
    provider: &LlmProvider,
    api_key: Option<&str>,
    effective_base_url: &str,
) -> AppResult<HeaderMap> {
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));

    if let Some(beta) = anthropic_beta_header(provider) {
        let value = HeaderValue::from_str(&beta)
            .map_err(|e| AppError::Llm(format!("invalid anthropic-beta header: {e}")))?;
        headers.insert("anthropic-beta", value);
    }

    if is_kimi_coding_endpoint(effective_base_url) {
        headers.insert("User-Agent", HeaderValue::from_static("claude-code/0.1.0"));
    }

    let Some(key) = api_key else {
        return Ok(headers);
    };
    if anthropic_uses_oauth_bearer_auth(effective_base_url, key) {
        let bearer = HeaderValue::from_str(&format!("Bearer {key}"))
            .map_err(|e| AppError::Llm(format!("invalid authorization header: {e}")))?;
        headers.insert(AUTHORIZATION, bearer);
        headers.insert(
            "anthropic-beta",
            HeaderValue::from_static(
                "interleaved-thinking-2025-05-14,fine-grained-tool-streaming-2025-05-14,claude-code-20250219,oauth-2025-04-20",
            ),
        );
        headers.insert(
            "user-agent",
            HeaderValue::from_static("claude-cli/2.1.74 (external, cli)"),
        );
        headers.insert("x-app", HeaderValue::from_static("cli"));
    } else if anthropic_requires_bearer_auth(effective_base_url) {
        let bearer = HeaderValue::from_str(&format!("Bearer {key}"))
            .map_err(|e| AppError::Llm(format!("invalid authorization header: {e}")))?;
        headers.insert(AUTHORIZATION, bearer);
    } else {
        let x_api_key = HeaderValue::from_str(key)
            .map_err(|e| AppError::Llm(format!("invalid x-api-key header: {e}")))?;
        headers.insert("x-api-key", x_api_key);
    }
    Ok(headers)
}

fn append_anthropic_beta_header(headers: &mut HeaderMap, beta: &str) -> AppResult<()> {
    let existing = headers
        .get("anthropic-beta")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    let combined = if existing
        .split(',')
        .any(|item| item.trim().eq_ignore_ascii_case(beta))
    {
        existing.to_string()
    } else if existing.trim().is_empty() {
        beta.to_string()
    } else {
        format!("{existing},{beta}")
    };
    let value = HeaderValue::from_str(&combined)
        .map_err(|e| AppError::Llm(format!("invalid anthropic-beta header: {e}")))?;
    headers.insert("anthropic-beta", value);
    Ok(())
}

pub(super) fn anthropic_uses_oauth_bearer_auth(base_url: &str, key: &str) -> bool {
    anthropic_oauth_token(key) && direct_anthropic_endpoint(base_url)
}

pub(super) fn anthropic_oauth_token(key: &str) -> bool {
    let key = key.trim();
    !key.is_empty()
        && !key.starts_with("sk-ant-api")
        && (key.starts_with("sk-ant-") || key.starts_with("eyJ") || key.starts_with("cc-"))
}

pub(super) fn direct_anthropic_endpoint(base_url: &str) -> bool {
    let normalized = normalized_url_text(base_url);
    normalized.is_empty() || host_matches(&normalized, "api.anthropic.com")
}

pub(super) fn anthropic_beta_header(provider: &LlmProvider) -> Option<String> {
    let base_url = provider_base_url(provider);
    let betas = anthropic_betas_for_base_url(&base_url);
    if betas.is_empty() {
        None
    } else {
        Some(betas.join(","))
    }
}

pub(super) fn resolve_anthropic_messages_max_tokens(requested: u32, model: &str) -> u32 {
    if requested > 0 {
        return requested;
    }
    anthropic_model_max_output_tokens(model)
}

pub(super) fn anthropic_model_max_output_tokens(model: &str) -> u32 {
    let normalized = model.trim().to_ascii_lowercase().replace('.', "-");
    let limits = [
        ("claude-opus-4-8", 128_000),
        ("claude-opus-4-7", 128_000),
        ("claude-opus-4-6", 128_000),
        ("claude-sonnet-4-6", 64_000),
        ("claude-opus-4-5", 64_000),
        ("claude-sonnet-4-5", 64_000),
        ("claude-haiku-4-5", 64_000),
        ("claude-opus-4", 32_000),
        ("claude-sonnet-4", 64_000),
        ("claude-3-7-sonnet", 128_000),
        ("claude-3-5-sonnet", 8_192),
        ("claude-3-5-haiku", 8_192),
        ("claude-3-opus", 4_096),
        ("claude-3-sonnet", 4_096),
        ("claude-3-haiku", 4_096),
        ("minimax", 131_072),
        ("qwen3", 65_536),
    ];
    limits
        .iter()
        .filter(|(needle, _)| normalized.contains(*needle))
        .max_by_key(|(needle, _)| needle.len())
        .map(|(_, limit)| *limit)
        .unwrap_or(128_000)
}

pub(super) fn anthropic_model_forbids_sampling_params(model: &str) -> bool {
    let normalized = model.trim().to_ascii_lowercase().replace('.', "-");
    ["4-7", "4-8"]
        .iter()
        .any(|needle| normalized.contains(needle))
}

pub(super) fn anthropic_betas_for_base_url(base_url: &str) -> Vec<&'static str> {
    const INTERLEAVED_THINKING: &str = "interleaved-thinking-2025-05-14";
    const FINE_GRAINED_TOOL_STREAMING: &str = "fine-grained-tool-streaming-2025-05-14";
    const CONTEXT_1M: &str = "context-1m-2025-08-07";
    if is_minimax_anthropic_endpoint(base_url) {
        return vec![INTERLEAVED_THINKING];
    }
    let mut betas = vec![INTERLEAVED_THINKING, FINE_GRAINED_TOOL_STREAMING];
    if base_url_needs_context_1m_beta(base_url) {
        betas.push(CONTEXT_1M);
    }
    betas
}

pub(super) fn anthropic_requires_bearer_auth(base_url: &str) -> bool {
    let normalized = normalized_url_text(base_url);
    normalized.starts_with("https://api.minimax.io/anthropic")
        || normalized.starts_with("https://api.minimaxi.com/anthropic")
        || normalized.contains("azure.com")
}

pub(super) fn is_minimax_anthropic_endpoint(base_url: &str) -> bool {
    let normalized = normalized_url_text(base_url);
    normalized.starts_with("https://api.minimax.io/anthropic")
        || normalized.starts_with("https://api.minimaxi.com/anthropic")
}

pub(super) fn base_url_needs_context_1m_beta(base_url: &str) -> bool {
    normalized_url_text(base_url).contains("azure.com")
}

pub(super) fn is_kimi_coding_endpoint(base_url: &str) -> bool {
    normalized_url_text(base_url).starts_with("https://api.kimi.com/coding")
}

pub(super) fn is_azure_anthropic_endpoint(base_url: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(base_url) else {
        return false;
    };
    let host = url.host_str().unwrap_or("").to_ascii_lowercase();
    let path = url.path().to_ascii_lowercase();
    let padded = format!(".{}.", host.trim_end_matches('.'));
    (padded.contains(".services.ai.azure.") || padded.contains(".openai.azure."))
        && path.contains("/anthropic")
}

fn normalized_url_text(value: &str) -> String {
    value.trim().trim_end_matches('/').to_ascii_lowercase()
}

pub(super) fn parse_anthropic_compatible(payload: Value) -> AppResult<LlmReply> {
    let content_blocks = payload
        .get("content")
        .and_then(Value::as_array)
        .ok_or_else(|| AppError::Llm(format!("missing anthropic content: {payload}")))?;

    let mut content = content_blocks
        .iter()
        .filter_map(|block| {
            let ty = block.get("type").and_then(Value::as_str).unwrap_or("");
            if ty == "text" || block.get("text").is_some() {
                block.get("text").and_then(Value::as_str)
            } else {
                None
            }
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    let tool_calls = anthropic_tool_calls(content_blocks);
    let has_tool_calls = !tool_calls.is_empty();
    if !tool_calls.is_empty() {
        let tool_json = json!({"tool_calls": tool_calls}).to_string();
        if content.trim().is_empty() {
            content = tool_json;
        } else {
            content = format!("{content}\n\n{tool_json}");
        }
    }
    let content = scrub_reasoning_blocks(&content);

    if content.trim().is_empty() {
        return Err(AppError::Llm(format!(
            "anthropic response has no visible text or tool-call content: {payload}"
        )));
    }

    let prompt_tokens = payload
        .pointer("/usage/input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;
    let completion_tokens = payload
        .pointer("/usage/output_tokens")
        .and_then(Value::as_u64)
        .unwrap_or_else(|| estimate_tokens(&content) as u64) as usize;
    let cache_read_tokens = payload
        .pointer("/usage/cache_read_input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;
    let cache_write_tokens = payload
        .pointer("/usage/cache_creation_input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;

    Ok(LlmReply {
        content,
        prompt_tokens,
        completion_tokens,
        cache_read_tokens,
        cache_write_tokens,
        reasoning_tokens: 0,
        provider_id: None,
        provider_type: None,
        model: None,
        base_url: None,
        estimated_cost_usd: None,
        cost_status: None,
        cost_source: None,
        rate_limit_state: None,
        transport_diagnostics: None,
        finish_reason: Some(if has_tool_calls {
            "tool_calls".into()
        } else {
            "stop".into()
        }),
        provider_data: None,
        failover_attempts: Vec::new(),
    })
}

fn anthropic_tool_calls(content_blocks: &[Value]) -> Vec<Value> {
    content_blocks
        .iter()
        .filter_map(|block| {
            if block.get("type").and_then(Value::as_str) != Some("tool_use") {
                return None;
            }
            let name = block
                .get("name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|name| !name.is_empty())?;
            let arguments = block.get("input").cloned().unwrap_or_else(|| json!({}));
            Some(json!({
                "type": "function",
                "id": block.get("id").cloned().unwrap_or(Value::Null),
                "function": {
                    "name": name,
                    "arguments": arguments
                }
            }))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anthropic_stream_tool_calls_reconstruct_partial_json() {
        let mut tool_calls = BTreeMap::<usize, AnthropicStreamToolCall>::new();
        track_anthropic_stream_tool_call(
            &json!({
                "type": "content_block_start",
                "index": 1,
                "content_block": {
                    "type": "tool_use",
                    "id": "toolu_123",
                    "name": "terminal",
                    "input": {}
                }
            }),
            &mut tool_calls,
        );
        track_anthropic_stream_tool_call(
            &json!({
                "type": "content_block_delta",
                "index": 1,
                "delta": {
                    "type": "input_json_delta",
                    "partial_json": "{\"command\":\"pwd\"}"
                }
            }),
            &mut tool_calls,
        );

        let calls = anthropic_stream_tool_calls(tool_calls).unwrap();
        assert_eq!(calls[0]["id"], "toolu_123");
        assert_eq!(calls[0]["function"]["name"], "terminal");
        assert_eq!(calls[0]["function"]["arguments"], json!({"command": "pwd"}));
    }
}
