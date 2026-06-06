use std::time::{Duration, Instant};

use futures::StreamExt;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde_json::{json, Value};

use crate::{
    error::{AppError, AppResult},
    models::{ChatMessage, LlmProvider, Persona, ToolDefinition},
};

use super::*;

pub(super) async fn complete_gemini_compatible(
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
    let url = gemini_generate_content_url(provider, model);
    let api_key = resolve_api_key(provider);

    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    if let Some(key) = api_key {
        let x_api_key = HeaderValue::from_str(&key)
            .map_err(|e| AppError::Llm(format!("invalid x-goog-api-key header: {e}")))?;
        let bearer = HeaderValue::from_str(&format!("Bearer {key}"))
            .map_err(|e| AppError::Llm(format!("invalid authorization header: {e}")))?;
        headers.insert("x-goog-api-key", x_api_key);
        headers.insert(AUTHORIZATION, bearer);
    }

    let contents = build_gemini_contents(history);
    let mut body = json!({
        "systemInstruction": {
            "parts": [{"text": system_prompt}]
        },
        "contents": contents,
        "generationConfig": {
            "temperature": persona.temperature,
            "maxOutputTokens": persona.max_tokens
        }
    });
    if let Some(tools) = native_tools.filter(|tools| !tools.is_empty()) {
        body["tools"] = json!(gemini_tool_schemas(tools));
    }

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(provider.timeout_seconds.max(1)))
        .default_headers(headers)
        .build()
        .map_err(|e| AppError::Llm(e.to_string()))?;

    let started_at = Instant::now();
    let request_url = if options.stream_delta_callback.is_some() {
        gemini_stream_generate_content_url(&url)
    } else {
        url.clone()
    };
    let response = client
        .post(request_url.clone())
        .json(&body)
        .send()
        .await
        .map_err(|e| AppError::Llm(e.to_string()))?;

    let status = response.status();
    let response_headers = response.headers().clone();
    if !status.is_success() {
        let text = response
            .text()
            .await
            .map_err(|e| AppError::Llm(format!("failed to read gemini response: {e}")))?;
        return Err(AppError::Llm(format!(
            "provider returned {status}: {}",
            response_preview(&text)
        )));
    }

    let transport = LlmTransportMetadata {
        transport: if options.stream_delta_callback.is_some() {
            "gemini_stream_generate_content"
        } else {
            "gemini_generate_content"
        },
        method: "POST",
        endpoint: request_url,
        status: Some(status.as_u16()),
        elapsed_ms: Some(started_at.elapsed().as_millis().min(u64::MAX as u128) as u64),
        retry_count: 0,
        retry_reason: None,
    };
    if let Some(callback) = options.stream_delta_callback.as_ref() {
        let reply = read_gemini_stream(response, callback).await?;
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
        .map_err(|e| AppError::Llm(format!("failed to read gemini response: {e}")))?;
    let payload: Value = serde_json::from_str(&text).map_err(|e| {
        AppError::Llm(format!(
            "invalid gemini response ({status}): {e}; body: {}",
            response_preview(&text)
        ))
    })?;

    parse_gemini_compatible(payload).map(|reply| {
        with_reply_metadata_and_transport(
            reply,
            provider,
            model,
            &response_headers,
            Some(transport),
        )
    })
}

async fn read_gemini_stream(
    response: reqwest::Response,
    callback: &LlmDeltaCallback,
) -> AppResult<LlmReply> {
    let mut buffer = String::new();
    let mut content = String::new();
    let mut last_payload = None::<Value>;
    let mut prompt_tokens = 0usize;
    let mut completion_tokens = 0usize;
    let mut cache_read_tokens = 0usize;
    let mut reasoning_tokens = 0usize;
    let mut tool_calls = Vec::<Value>::new();
    let mut stream = response.bytes_stream();

    while let Some(chunk) = stream.next().await {
        let chunk =
            chunk.map_err(|e| AppError::Llm(format!("failed to read gemini stream: {e}")))?;
        buffer.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(newline) = buffer.find('\n') {
            let mut line = buffer[..newline].trim().to_string();
            buffer.replace_range(..=newline, "");
            if line.ends_with('\r') {
                line.pop();
            }
            handle_gemini_stream_line(
                &line,
                callback,
                &mut content,
                &mut last_payload,
                &mut prompt_tokens,
                &mut completion_tokens,
                &mut cache_read_tokens,
                &mut reasoning_tokens,
                &mut tool_calls,
            )?;
        }
    }
    let remainder = buffer.trim();
    if !remainder.is_empty() {
        handle_gemini_stream_line(
            remainder,
            callback,
            &mut content,
            &mut last_payload,
            &mut prompt_tokens,
            &mut completion_tokens,
            &mut cache_read_tokens,
            &mut reasoning_tokens,
            &mut tool_calls,
        )?;
    }

    let has_tool_calls = !tool_calls.is_empty();
    if has_tool_calls {
        let tool_json = json!({"tool_calls": tool_calls}).to_string();
        if content.trim().is_empty() {
            content = tool_json;
        } else {
            content = format!("{content}\n\n{tool_json}");
        }
    }
    if content.trim().is_empty() {
        if let Some(payload) = last_payload {
            return parse_gemini_compatible(payload);
        }
        return Err(AppError::Llm(
            "gemini stream produced no visible text".into(),
        ));
    }

    let content = scrub_reasoning_blocks(&content);
    Ok(LlmReply {
        prompt_tokens,
        completion_tokens: if completion_tokens == 0 {
            estimate_tokens(&content)
        } else {
            completion_tokens
        },
        cache_read_tokens,
        cache_write_tokens: 0,
        reasoning_tokens,
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
        finish_reason: Some(if has_tool_calls {
            "tool_calls".into()
        } else {
            "stop".into()
        }),
        provider_data: None,
        failover_attempts: Vec::new(),
    })
}

fn handle_gemini_stream_line(
    line: &str,
    callback: &LlmDeltaCallback,
    content: &mut String,
    last_payload: &mut Option<Value>,
    prompt_tokens: &mut usize,
    completion_tokens: &mut usize,
    cache_read_tokens: &mut usize,
    reasoning_tokens: &mut usize,
    tool_calls: &mut Vec<Value>,
) -> AppResult<()> {
    let line = line.trim();
    if line.is_empty() || line == "[DONE]" || line.starts_with("event:") {
        return Ok(());
    }
    let data = line.strip_prefix("data:").unwrap_or(line).trim();
    if data.is_empty() || data == "[DONE]" {
        return Ok(());
    }
    let Ok(payload) = serde_json::from_str::<Value>(data) else {
        return Ok(());
    };
    for item in gemini_stream_payload_items(payload) {
        if let Some(delta) = gemini_payload_text(&item).filter(|delta| !delta.is_empty()) {
            content.push_str(&delta);
            callback(&delta)?;
        }
        update_gemini_usage(
            &item,
            prompt_tokens,
            completion_tokens,
            cache_read_tokens,
            reasoning_tokens,
        );
        extend_unique_gemini_tool_calls(&item, tool_calls);
        *last_payload = Some(item);
    }
    Ok(())
}

fn extend_unique_gemini_tool_calls(payload: &Value, tool_calls: &mut Vec<Value>) {
    let Some(candidates) = payload.get("candidates").and_then(Value::as_array) else {
        return;
    };
    for call in gemini_tool_calls(candidates) {
        if !tool_calls.iter().any(|existing| existing == &call) {
            tool_calls.push(call);
        }
    }
}

fn gemini_stream_payload_items(payload: Value) -> Vec<Value> {
    match payload {
        Value::Array(items) => items,
        item => vec![item],
    }
}

fn gemini_payload_text(payload: &Value) -> Option<String> {
    let text = payload
        .get("candidates")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .flat_map(|candidate| {
            candidate
                .pointer("/content/parts")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
        })
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("");
    (!text.is_empty()).then_some(text)
}

fn update_gemini_usage(
    payload: &Value,
    prompt_tokens: &mut usize,
    completion_tokens: &mut usize,
    cache_read_tokens: &mut usize,
    reasoning_tokens: &mut usize,
) {
    if *prompt_tokens == 0 {
        *prompt_tokens = payload
            .pointer("/usageMetadata/promptTokenCount")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
    }
    if let Some(value) = payload
        .pointer("/usageMetadata/candidatesTokenCount")
        .and_then(Value::as_u64)
    {
        *completion_tokens = value as usize;
    }
    if *cache_read_tokens == 0 {
        *cache_read_tokens = payload
            .pointer("/usageMetadata/cachedContentTokenCount")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
    }
    if *reasoning_tokens == 0 {
        *reasoning_tokens = payload
            .pointer("/usageMetadata/thoughtsTokenCount")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
    }
}

pub(super) fn is_gemini_compatible(provider: &LlmProvider) -> bool {
    let provider_type = provider.provider_type.to_lowercase();
    let preset = provider
        .preset
        .as_deref()
        .unwrap_or_default()
        .to_lowercase();
    let base_url = provider_base_url(provider).to_lowercase();
    provider_type == "gemini"
        || preset.contains("google")
        || preset.contains("gemini")
        || base_url.contains("generativelanguage.googleapis.com")
        || base_url.ends_with(":generatecontent")
}

pub(super) fn build_gemini_contents(history: Vec<ChatMessage>) -> Vec<Value> {
    let contents = history
        .into_iter()
        .filter_map(|item| {
            if let Some(tool_replay) = tool_replay_message(&item) {
                return Some(vec![
                    json!({
                        "role": "model",
                        "parts": [{
                            "functionCall": {
                                "name": tool_replay.name,
                                "args": tool_replay.arguments,
                            }
                        }]
                    }),
                    json!({
                        "role": "user",
                        "parts": [{
                            "functionResponse": {
                                "name": tool_replay.name,
                                "response": {
                                    "result": tool_replay.content,
                                    "ok": tool_replay.ok,
                                }
                            }
                        }]
                    }),
                ]);
            }
            let role = match item.role.as_str() {
                "assistant" => "model",
                "user" => "user",
                _ => return None,
            };
            let content = sanitize_provider_text(&item.content);
            if content.trim().is_empty() {
                return None;
            }
            Some(vec![json!({
                "role": role,
                "parts": [{"text": content}]
            })])
        })
        .flatten()
        .collect::<Vec<_>>();
    merge_adjacent_gemini_contents(contents)
}

pub(super) fn merge_adjacent_gemini_contents(contents: Vec<Value>) -> Vec<Value> {
    let mut merged: Vec<Value> = Vec::new();
    for content in contents {
        let role = content.get("role").and_then(Value::as_str).unwrap_or("");
        let Some(text) = content
            .pointer("/parts/0/text")
            .and_then(Value::as_str)
            .map(str::to_string)
        else {
            merged.push(content);
            continue;
        };
        if let Some(previous) = merged.last_mut() {
            if previous.get("role").and_then(Value::as_str) == Some(role) {
                let Some(previous_text) = previous
                    .pointer("/parts/0/text")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                else {
                    merged.push(content);
                    continue;
                };
                let separator = if previous_text.trim().is_empty() || text.trim().is_empty() {
                    ""
                } else {
                    "\n\n"
                };
                previous["parts"][0]["text"] = json!(format!("{previous_text}{separator}{text}"));
                continue;
            }
        }
        merged.push(content);
    }
    merged
}

pub(super) fn gemini_generate_content_url(provider: &LlmProvider, model: &str) -> String {
    let base_url = provider_base_url(provider);
    let base = base_url.trim().trim_end_matches('/');
    if base.is_empty() {
        return format!(
            "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent",
            gemini_model_id(model)
        );
    }
    if base.to_lowercase().ends_with(":generatecontent") {
        base.to_string()
    } else if base.ends_with("/v1") || base.ends_with("/v1beta") {
        format!("{base}/models/{}:generateContent", gemini_model_id(model))
    } else if provider.append_chat_path {
        format!("{base}/models/{}:generateContent", gemini_model_id(model))
    } else {
        base.to_string()
    }
}

pub(super) fn gemini_stream_generate_content_url(generate_url: &str) -> String {
    let trimmed = generate_url.trim();
    if trimmed
        .to_ascii_lowercase()
        .ends_with(":streamgeneratecontent")
    {
        trimmed.to_string()
    } else if trimmed.to_ascii_lowercase().ends_with(":generatecontent") {
        let cutoff = trimmed.len() - ":generateContent".len();
        format!("{}:streamGenerateContent", &trimmed[..cutoff])
    } else {
        trimmed.to_string()
    }
}

pub(super) fn gemini_model_id(model: &str) -> String {
    let value = model.trim();
    let value = value.strip_prefix("models/").unwrap_or(value);
    if value.is_empty() {
        "gemini-2.0-flash".into()
    } else {
        value.to_string()
    }
}

pub(super) fn parse_gemini_compatible(payload: Value) -> AppResult<LlmReply> {
    let candidates = payload
        .get("candidates")
        .and_then(Value::as_array)
        .ok_or_else(|| AppError::Llm(format!("missing gemini candidates: {payload}")))?;

    let mut content = candidates
        .iter()
        .flat_map(|candidate| {
            candidate
                .pointer("/content/parts")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
        })
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .filter(|text| !text.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    let tool_calls = gemini_tool_calls(candidates);
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
            "gemini response has no visible text or tool-call content: {payload}"
        )));
    }

    let prompt_tokens = payload
        .pointer("/usageMetadata/promptTokenCount")
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;
    let completion_tokens = payload
        .pointer("/usageMetadata/candidatesTokenCount")
        .and_then(Value::as_u64)
        .unwrap_or_else(|| estimate_tokens(&content) as u64) as usize;
    let cached_content_tokens = payload
        .pointer("/usageMetadata/cachedContentTokenCount")
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;
    let thoughts_tokens = payload
        .pointer("/usageMetadata/thoughtsTokenCount")
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;

    Ok(LlmReply {
        content,
        prompt_tokens,
        completion_tokens,
        cache_read_tokens: cached_content_tokens,
        cache_write_tokens: 0,
        reasoning_tokens: thoughts_tokens,
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

fn gemini_tool_calls(candidates: &[Value]) -> Vec<Value> {
    candidates
        .iter()
        .flat_map(|candidate| {
            candidate
                .pointer("/content/parts")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
        })
        .filter_map(|part| {
            let function_call = part.get("functionCall")?;
            let name = function_call
                .get("name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|name| !name.is_empty())?;
            let arguments = function_call
                .get("args")
                .cloned()
                .unwrap_or_else(|| json!({}));
            Some(json!({
                "type": "function",
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
    fn gemini_stream_tool_calls_collect_function_call_chunks() {
        let payload = json!({
            "candidates": [{
                "content": {
                    "parts": [{
                        "functionCall": {
                            "name": "terminal",
                            "args": {"command": "pwd"}
                        }
                    }]
                }
            }]
        });
        let mut calls = Vec::new();
        extend_unique_gemini_tool_calls(&payload, &mut calls);
        extend_unique_gemini_tool_calls(&payload, &mut calls);

        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["function"]["name"], "terminal");
        assert_eq!(calls[0]["function"]["arguments"], json!({"command": "pwd"}));
    }
}
