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
struct OpenAiStreamToolCall {
    id: Option<String>,
    name: String,
    arguments: String,
}

pub(super) async fn complete_openai_compatible(
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
    let tool_name_map = native_tools
        .filter(|tools| !tools.is_empty())
        .map(provider_tool_name_map)
        .unwrap_or_default();
    let cache_policy = prompt_cache_policy(provider, model);
    let messages = build_openai_wire_messages_with_tool_name_map(
        system_prompt,
        history,
        cache_policy.as_ref(),
        &tool_name_map,
    );
    let url = chat_url(provider);
    let api_key = resolve_api_key(provider);

    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    if let Some(key) = api_key {
        let value = HeaderValue::from_str(&format!("Bearer {key}"))
            .map_err(|e| AppError::Llm(format!("invalid authorization header: {e}")))?;
        headers.insert(AUTHORIZATION, value);
    }

    let mut body = json!({
        "model": model,
        "messages": messages,
        "temperature": persona.temperature,
        "max_tokens": persona.max_tokens
    });
    if !options.thinking_enabled {
        body["reasoning_effort"] = json!("none");
        body["enable_thinking"] = json!(false);
    }
    if options.fast_mode_enabled {
        body["service_tier"] = json!("priority");
    }
    if let Some(tools) = native_tools.filter(|tools| !tools.is_empty()) {
        body["tools"] = json!(openai_tool_schemas(tools));
        body["tool_choice"] = json!("auto");
        body["parallel_tool_calls"] = json!(true);
    }
    if options.stream_delta_callback.is_some() {
        body["stream"] = json!(true);
        body["stream_options"] = json!({"include_usage": true});
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
        "openai chat request",
    )
    .await?;

    let mut status = response.status();
    let mut response_headers = response.headers().clone();
    let mut retry_count = 0;
    let mut retry_reason = None;

    if !status.is_success() {
        let text = response
            .text()
            .await
            .map_err(|e| AppError::Llm(format!("failed to read llm response: {e}")))?;
        if let Some(retry_body) = openai_unsupported_parameter_retry_body(&body, &text) {
            retry_count = 1;
            retry_reason = Some("unsupported_parameter_recovery".to_string());
            let retry_response = send_llm_request_with_stale_timeout(
                client.post(chat_url(provider)).json(&retry_body),
                provider,
                model,
                "openai chat retry request",
            )
            .await?;
            status = retry_response.status();
            response_headers = retry_response.headers().clone();
            let text = retry_response.text().await.map_err(|e| {
                AppError::Llm(format!("failed to read recovered llm response: {e}"))
            })?;
            if !status.is_success() {
                return Err(AppError::Llm(format!(
                    "provider returned {status}: {}",
                    response_preview(&text)
                )));
            }
            let payload: Value = serde_json::from_str(&text).map_err(|error| {
                AppError::Llm(format!(
                    "invalid recovered llm response ({status}): {error}; {}",
                    invalid_response_body_detail(&text, &response_headers)
                ))
            })?;
            let transport = LlmTransportMetadata {
                transport: "openai_chat",
                method: "POST",
                endpoint: url,
                status: Some(status.as_u16()),
                elapsed_ms: Some(started_at.elapsed().as_millis().min(u64::MAX as u128) as u64),
                retry_count,
                retry_reason,
            };
            return parse_openai_compatible_with_tool_name_map(payload, &tool_name_map).map(|reply| {
                with_reply_metadata_and_transport(
                    reply,
                    provider,
                    model,
                    &response_headers,
                    Some(transport),
                )
            });
        }
        return Err(AppError::Llm(format!(
            "provider returned {status}: {}",
            response_preview(&text)
        )));
    }

    let transport = LlmTransportMetadata {
        transport: "openai_chat",
        method: "POST",
        endpoint: url,
        status: Some(status.as_u16()),
        elapsed_ms: Some(started_at.elapsed().as_millis().min(u64::MAX as u128) as u64),
        retry_count,
        retry_reason,
    };

    if let Some(callback) = options.stream_delta_callback.as_ref() {
        let reply = read_openai_sse_stream(
            response,
            callback,
            provider_stream_stale_timeout_duration(provider, model),
            &tool_name_map,
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
    let payload: Value = match serde_json::from_str(&text) {
        Ok(payload) => payload,
        Err(error) => {
            if let Some(reply) = parse_openai_sse(&text, &tool_name_map) {
                return Ok(with_reply_metadata_and_transport(
                    reply,
                    provider,
                    model,
                    &response_headers,
                    Some(transport),
                ));
            }
            return Err(AppError::Llm(format!(
                "invalid llm response ({status}): {error}; {}",
                invalid_response_body_detail(&text, &response_headers)
            )));
        }
    };

    parse_openai_compatible_with_tool_name_map(payload, &tool_name_map).map(|reply| {
        with_reply_metadata_and_transport(
            reply,
            provider,
            model,
            &response_headers,
            Some(transport),
        )
    })
}

async fn read_openai_sse_stream(
    response: reqwest::Response,
    callback: &LlmDeltaCallback,
    stale_timeout: Option<std::time::Duration>,
    tool_name_map: &serde_json::Map<String, Value>,
) -> AppResult<LlmReply> {
    let mut buffer = String::new();
    let mut content = String::new();
    let mut prompt_tokens = 0usize;
    let mut completion_tokens = 0usize;
    let mut finish_reason = None;
    let mut tool_calls = BTreeMap::<usize, OpenAiStreamToolCall>::new();
    let mut stream = response.bytes_stream();

    loop {
        let next_chunk = if let Some(timeout) = stale_timeout {
            tokio::time::timeout(timeout, stream.next())
                .await
                .map_err(|_| {
                    AppError::Llm(format!(
                        "llm stream stale: no provider bytes for {}s",
                        timeout.as_secs_f64()
                    ))
                })?
        } else {
            stream.next().await
        };
        let Some(chunk) = next_chunk else {
            break;
        };
        let chunk = chunk.map_err(|e| AppError::Llm(format!("failed to read llm stream: {e}")))?;
        buffer.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(newline) = buffer.find('\n') {
            let mut line = buffer[..newline].trim().to_string();
            buffer.replace_range(..=newline, "");
            if line.ends_with('\r') {
                line.pop();
            }
            handle_openai_sse_line(
                &line,
                callback,
                &mut content,
                &mut prompt_tokens,
                &mut completion_tokens,
                &mut finish_reason,
                &mut tool_calls,
            )?;
        }
    }
    if !buffer.trim().is_empty() {
        let line = buffer.trim().to_string();
        handle_openai_sse_line(
            &line,
            callback,
            &mut content,
            &mut prompt_tokens,
            &mut completion_tokens,
            &mut finish_reason,
            &mut tool_calls,
        )?;
    }

    let stream_tool_calls = openai_stream_tool_calls(tool_calls, tool_name_map)?;
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
            "openai stream produced no visible text".into(),
        ));
    }
    Ok(LlmReply {
        prompt_tokens,
        completion_tokens: if completion_tokens == 0 {
            estimate_tokens(&content)
        } else {
            completion_tokens
        },
        cache_read_tokens: 0,
        cache_write_tokens: 0,
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
            finish_reason.or_else(|| Some("stop".into()))
        },
        provider_data: None,
        failover_attempts: Vec::new(),
    })
}

fn handle_openai_sse_line(
    line: &str,
    callback: &LlmDeltaCallback,
    content: &mut String,
    prompt_tokens: &mut usize,
    completion_tokens: &mut usize,
    finish_reason: &mut Option<String>,
    tool_calls: &mut BTreeMap<usize, OpenAiStreamToolCall>,
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
        .pointer("/choices/0/delta/content")
        .and_then(Value::as_str)
        .or_else(|| {
            payload
                .pointer("/choices/0/message/content")
                .and_then(Value::as_str)
        })
        .filter(|delta| !delta.is_empty())
    {
        content.push_str(delta);
        callback(LlmStreamDeltaKind::Answer, delta)?;
    }
    track_openai_stream_tool_calls(&payload, tool_calls);
    if let Some(reason) = payload
        .pointer("/choices/0/finish_reason")
        .and_then(Value::as_str)
        .filter(|reason| !reason.is_empty())
    {
        *finish_reason = Some(reason.to_string());
    }
    if *prompt_tokens == 0 {
        *prompt_tokens = payload
            .pointer("/usage/prompt_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
    }
    if *completion_tokens == 0 {
        *completion_tokens = payload
            .pointer("/usage/completion_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
    }
    Ok(())
}

fn track_openai_stream_tool_calls(
    payload: &Value,
    tool_calls: &mut BTreeMap<usize, OpenAiStreamToolCall>,
) {
    let Some(items) = payload
        .pointer("/choices/0/delta/tool_calls")
        .and_then(Value::as_array)
    else {
        return;
    };
    for item in items {
        let index = item.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
        let call = tool_calls.entry(index).or_default();
        if let Some(id) = item
            .get("id")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        {
            call.id = Some(id.to_string());
        }
        if let Some(name) = item
            .pointer("/function/name")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        {
            call.name = name.to_string();
        }
        if let Some(arguments) = item
            .pointer("/function/arguments")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        {
            call.arguments.push_str(arguments);
        }
    }
}

fn openai_stream_tool_calls(
    tool_calls: BTreeMap<usize, OpenAiStreamToolCall>,
    tool_name_map: &serde_json::Map<String, Value>,
) -> AppResult<Vec<Value>> {
    tool_calls
        .into_values()
        .filter(|call| !call.name.trim().is_empty())
        .map(|call| {
            let name = original_provider_tool_name(&call.name, tool_name_map);
            let arguments = if call.arguments.trim().is_empty() {
                json!({})
            } else {
                let parsed = serde_json::from_str::<Value>(&call.arguments).map_err(|error| {
                    AppError::Llm(format!(
                        "invalid openai streamed tool arguments for {}: {error}; body: {}",
                        name,
                        response_preview(&call.arguments)
                    ))
                })?;
                normalize_provider_tool_arguments(parsed, &name)
            };
            Ok(json!({
                "type": "function",
                "id": call.id.map(Value::String).unwrap_or(Value::Null),
                "function": {
                    "name": name,
                    "arguments": arguments
                }
            }))
        })
        .collect()
}

fn parse_openai_sse(
    text: &str,
    _tool_name_map: &serde_json::Map<String, Value>,
) -> Option<LlmReply> {
    let mut content = String::new();
    let mut prompt_tokens = 0;
    let mut completion_tokens = 0;

    for line in text.lines() {
        let line = line.trim();
        let Some(data) = line.strip_prefix("data:") else {
            continue;
        };
        let data = data.trim();
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        let Ok(payload) = serde_json::from_str::<Value>(data) else {
            continue;
        };
        if let Some(delta) = payload
            .pointer("/choices/0/delta/content")
            .and_then(Value::as_str)
            .or_else(|| {
                payload
                    .pointer("/choices/0/message/content")
                    .and_then(Value::as_str)
            })
        {
            content.push_str(delta);
        }
        if prompt_tokens == 0 {
            prompt_tokens = payload
                .pointer("/usage/prompt_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0) as usize;
        }
        if completion_tokens == 0 {
            completion_tokens = payload
                .pointer("/usage/completion_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0) as usize;
        }
    }

    let content = scrub_reasoning_blocks(&content);
    if content.trim().is_empty() {
        None
    } else {
        Some(LlmReply {
            prompt_tokens,
            completion_tokens: if completion_tokens == 0 {
                estimate_tokens(&content)
            } else {
                completion_tokens
            },
            cache_read_tokens: 0,
            cache_write_tokens: 0,
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
            finish_reason: Some("stop".into()),
            provider_data: None,
            failover_attempts: Vec::new(),
        })
    }
}

pub(super) fn parse_openai_compatible(payload: Value) -> AppResult<LlmReply> {
    parse_openai_compatible_with_tool_name_map(payload, &serde_json::Map::new())
}

fn parse_openai_compatible_with_tool_name_map(
    payload: Value,
    tool_name_map: &serde_json::Map<String, Value>,
) -> AppResult<LlmReply> {
    let content_value = payload
        .pointer("/choices/0/message/content")
        .or_else(|| payload.pointer("/message/content"));
    let mut content = content_value
        .and_then(extract_openai_message_content)
        .unwrap_or_default();
    let tool_calls = openai_tool_calls(&payload, tool_name_map);
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
            "openai response has no visible text or tool-call content: {payload}"
        )));
    }

    let prompt_tokens = payload
        .pointer("/usage/prompt_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;
    let completion_tokens = payload
        .pointer("/usage/completion_tokens")
        .and_then(Value::as_u64)
        .unwrap_or_else(|| estimate_tokens(&content) as u64) as usize;
    let cache_read_tokens = payload
        .pointer("/usage/prompt_tokens_details/cached_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;
    let reasoning_tokens = payload
        .pointer("/usage/completion_tokens_details/reasoning_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;
    let provider_data = openai_provider_data(&payload);

    Ok(LlmReply {
        content,
        prompt_tokens,
        completion_tokens,
        cache_read_tokens,
        cache_write_tokens: 0,
        reasoning_tokens,
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
        provider_data,
        failover_attempts: Vec::new(),
    })
}

fn extract_openai_message_content(value: &Value) -> Option<String> {
    if value.is_null() {
        return None;
    }
    if let Some(text) = value.as_str() {
        return (!text.trim().is_empty()).then(|| text.to_string());
    }
    let parts = value.as_array()?;
    let text = parts
        .iter()
        .filter_map(|part| {
            part.get("text")
                .and_then(Value::as_str)
                .or_else(|| part.pointer("/text/value").and_then(Value::as_str))
                .or_else(|| part.pointer("/content/text").and_then(Value::as_str))
        })
        .filter(|part| !part.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    (!text.trim().is_empty()).then_some(text)
}

fn openai_provider_data(payload: &Value) -> Option<Value> {
    let message = payload
        .pointer("/choices/0/message")
        .or_else(|| payload.get("message"))?;
    let mut openai = serde_json::Map::new();
    for key in ["reasoning_content", "reasoning", "reasoning_details"] {
        if let Some(value) = message
            .get(key)
            .filter(|value| provider_data_value_present(value))
        {
            openai.insert(key.into(), value.clone());
        }
    }
    if openai.is_empty() {
        None
    } else {
        Some(json!({ "openai": Value::Object(openai) }))
    }
}

fn provider_data_value_present(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::String(text) => !text.trim().is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(items) => !items.is_empty(),
        _ => true,
    }
}

fn openai_tool_calls(
    payload: &Value,
    tool_name_map: &serde_json::Map<String, Value>,
) -> Vec<Value> {
    payload
        .pointer("/choices/0/message/tool_calls")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let function = item.get("function")?;
            let name = function
                .get("name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|name| !name.is_empty())?;
            let name = original_provider_tool_name(name, tool_name_map);
            let arguments = function
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            let arguments = normalize_provider_tool_arguments(arguments, &name);
            let mut call = json!({
                "type": "function",
                "id": item.get("id").cloned().unwrap_or(Value::Null),
                "function": {
                    "name": name,
                    "arguments": arguments
                }
            });
            if let Some(extra_content) = item.get("extra_content").filter(|value| !value.is_null())
            {
                call["extra_content"] = extra_content.clone();
            }
            Some(call)
        })
        .collect()
}

pub(super) fn openai_unsupported_parameter_retry_body(
    body: &Value,
    error_text: &str,
) -> Option<Value> {
    let mut retry = body.clone();
    let object = retry.as_object_mut()?;
    let rejected = rejected_openai_parameter(error_text);
    let lower = error_text.to_ascii_lowercase();

    match rejected.as_deref() {
        Some("temperature") => {
            object.remove("temperature")?;
        }
        Some("max_tokens") => {
            let value = object.remove("max_tokens")?;
            if lower.contains("max_completion_tokens")
                && !object.contains_key("max_completion_tokens")
            {
                object.insert("max_completion_tokens".into(), value);
            }
        }
        Some("max_completion_tokens") => {
            let value = object.remove("max_completion_tokens")?;
            if lower.contains("max_tokens") && !object.contains_key("max_tokens") {
                object.insert("max_tokens".into(), value);
            }
        }
        Some("parallel_tool_calls") => {
            object.remove("parallel_tool_calls")?;
        }
        Some("service_tier") => {
            object.remove("service_tier")?;
        }
        Some("reasoning_effort") => {
            object.remove("reasoning_effort")?;
            object.remove("enable_thinking");
        }
        Some("enable_thinking") => {
            object.remove("enable_thinking")?;
            object.remove("reasoning_effort");
        }
        _ => {
            if unsupported_parameter_error_mentions(&lower, "temperature") {
                object.remove("temperature")?;
            } else if unsupported_parameter_error_mentions(&lower, "max_tokens") {
                let value = object.remove("max_tokens")?;
                if lower.contains("max_completion_tokens")
                    && !object.contains_key("max_completion_tokens")
                {
                    object.insert("max_completion_tokens".into(), value);
                }
            } else if unsupported_parameter_error_mentions(&lower, "parallel_tool_calls") {
                object.remove("parallel_tool_calls")?;
            } else if unsupported_parameter_error_mentions(&lower, "service_tier") {
                object.remove("service_tier")?;
            } else if unsupported_parameter_error_mentions(&lower, "reasoning_effort") {
                object.remove("reasoning_effort")?;
                object.remove("enable_thinking");
            } else if unsupported_parameter_error_mentions(&lower, "enable_thinking") {
                object.remove("enable_thinking")?;
                object.remove("reasoning_effort");
            } else {
                return None;
            }
        }
    }

    (retry != *body).then_some(retry)
}

pub(super) fn rejected_openai_parameter(error_text: &str) -> Option<String> {
    let payload = serde_json::from_str::<Value>(error_text).ok()?;
    for pointer in ["/error/param", "/error/parameter", "/param", "/parameter"] {
        if let Some(param) = payload
            .pointer(pointer)
            .and_then(Value::as_str)
            .map(normalize_openai_parameter_name)
            .filter(|value| !value.is_empty())
        {
            return Some(param);
        }
    }
    None
}

fn normalize_openai_parameter_name(value: &str) -> String {
    value
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .trim_start_matches("messages.")
        .to_ascii_lowercase()
}

pub(super) fn unsupported_parameter_error_mentions(error_lower: &str, parameter: &str) -> bool {
    error_lower.contains(parameter)
        && (error_lower.contains("unsupported")
            || error_lower.contains("unknown parameter")
            || error_lower.contains("unrecognized request")
            || error_lower.contains("invalid_request")
            || error_lower.contains("unsupported_parameter")
            || error_lower.contains("not supported"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openai_stream_tool_calls_reconstruct_chunked_arguments() {
        let mut tool_calls = BTreeMap::<usize, OpenAiStreamToolCall>::new();
        track_openai_stream_tool_calls(
            &json!({
                "choices": [{
                    "delta": {
                        "tool_calls": [{
                            "index": 0,
                            "id": "call_123",
                            "function": {
                                "name": "terminal",
                                "arguments": "{\"command\":"
                            }
                        }]
                    }
                }]
            }),
            &mut tool_calls,
        );
        track_openai_stream_tool_calls(
            &json!({
                "choices": [{
                    "delta": {
                        "tool_calls": [{
                            "index": 0,
                            "function": {
                                "arguments": "\"pwd\"}"
                            }
                        }]
                    }
                }]
            }),
            &mut tool_calls,
        );

        let calls = openai_stream_tool_calls(tool_calls, &serde_json::Map::new()).unwrap();
        assert_eq!(calls[0]["id"], "call_123");
        assert_eq!(calls[0]["function"]["name"], "terminal");
        assert_eq!(calls[0]["function"]["arguments"], json!({"command": "pwd"}));
    }
}
