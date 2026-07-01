use std::{collections::BTreeMap, time::Instant};

use futures::StreamExt;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde_json::{json, Value};

use crate::{
    error::{AppError, AppResult},
    models::{ChatMessage, LlmProvider, Persona, ToolDefinition},
};

use super::*;
pub(super) async fn complete_responses_compatible(
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
    let url = responses_url(provider);
    let issuer_kind = responses_issuer_kind(provider);
    let tool_name_map = native_tools
        .filter(|tools| !tools.is_empty())
        .map(provider_tool_name_map)
        .unwrap_or_default();
    let fallback_system_prompt = system_prompt.clone();
    let fallback_history = history.clone();
    let (instructions, input) = build_responses_payload_with_tool_name_map(
        system_prompt,
        history,
        Some(issuer_kind.as_str()),
        options.responses_reasoning_replay_enabled,
        &tool_name_map,
    );

    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    if let Some(key) = api_key {
        let value = HeaderValue::from_str(&format!("Bearer {key}"))
            .map_err(|e| AppError::Llm(format!("invalid authorization header: {e}")))?;
        headers.insert(AUTHORIZATION, value);
    }

    let mut body = json!({
        "model": model,
        "input": input,
        "temperature": persona.temperature,
        "max_output_tokens": persona.max_tokens,
        "store": false
    });
    if options.fast_mode_enabled {
        body["service_tier"] = json!("priority");
    }
    let thinking_enabled = options.thinking_enabled
        && provider_thinking_enabled(provider)
        && provider_supports_responses_thinking(provider);
    if options.responses_reasoning_replay_enabled || thinking_enabled {
        let mut include = Vec::new();
        if options.responses_reasoning_replay_enabled {
            include.push("reasoning.encrypted_content");
        }
        if !include.is_empty() {
            body["include"] = json!(include);
        }
    }
    if thinking_enabled {
        body["reasoning"] = json!({
            "effort": "medium",
            "summary": "auto"
        });
    }
    if !instructions.trim().is_empty() {
        body["instructions"] = json!(instructions);
    }
    if let Some(tools) = native_tools.filter(|tools| !tools.is_empty()) {
        body["tools"] = json!(responses_tool_schemas(tools));
        body["tool_choice"] = json!("auto");
        body["parallel_tool_calls"] = json!(true);
    }
    if options.stream_delta_callback.is_some() {
        body["stream"] = json!(true);
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
        "responses request",
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
            .map_err(|e| AppError::Llm(format!("failed to read responses llm response: {e}")))?;
        if let Some(retry_body) = responses_unsupported_parameter_retry_body(&body, &text) {
            retry_count = 1;
            retry_reason = Some("unsupported_parameter_recovery".to_string());
            let retry_response = send_llm_request_with_stale_timeout(
                client.post(responses_url(provider)).json(&retry_body),
                provider,
                model,
                "responses retry request",
            )
            .await?;
            status = retry_response.status();
            response_headers = retry_response.headers().clone();
            let text = retry_response.text().await.map_err(|e| {
                AppError::Llm(format!(
                    "failed to read recovered responses llm response: {e}"
                ))
            })?;
            if !status.is_success() {
                return Err(AppError::Llm(format!(
                    "provider returned {status}: {}",
                    response_preview(&text)
                )));
            }
            let payload = serde_json::from_str::<Value>(&text).map_err(|error| {
                AppError::Llm(format!(
                    "invalid recovered responses llm response ({status}): {error}; {}",
                    invalid_response_body_detail(&text, &response_headers)
                ))
            })?;
            let transport = LlmTransportMetadata {
                transport: "responses",
                method: "POST",
                endpoint: url,
                status: Some(status.as_u16()),
                elapsed_ms: Some(started_at.elapsed().as_millis().min(u64::MAX as u128) as u64),
                retry_count,
                retry_reason,
            };
            return parse_responses_compatible_with_tool_name_map(payload, &tool_name_map)
                .map(|reply| stamp_responses_provider_data_issuer(reply, &issuer_kind))
                .map(|reply| {
                    with_reply_metadata_and_transport(
                        reply,
                        provider,
                        model,
                        &response_headers,
                        Some(transport),
                    )
                });
        }
        if provider_supports_chat_completions_fallback_for_responses(provider)
            && responses_failure_allows_chat_completions_fallback(status.as_u16(), &text)
        {
            let mut fallback_provider = provider.clone();
            fallback_provider.provider_type = "openai_compatible".into();
            fallback_provider.base_url = chat_completions_fallback_base_url_for_responses(provider);
            fallback_provider.append_chat_path = true;
            let mut fallback_reply = complete_openai_compatible(
                &fallback_provider,
                persona,
                fallback_system_prompt,
                fallback_history,
                native_tools,
                options,
            )
            .await?;
            fallback_reply.transport_diagnostics =
                merge_responses_fallback_diagnostics(
                    fallback_reply.transport_diagnostics.take(),
                    status.as_u16(),
                    &url,
                    &text,
                );
            return Ok(fallback_reply);
        }
        return Err(AppError::Llm(format!(
            "provider returned {status}: {}",
            response_preview(&text)
        )));
    }

    let transport = LlmTransportMetadata {
        transport: "responses",
        method: "POST",
        endpoint: url,
        status: Some(status.as_u16()),
        elapsed_ms: Some(started_at.elapsed().as_millis().min(u64::MAX as u128) as u64),
        retry_count,
        retry_reason,
    };
    if let Some(callback) = options.stream_delta_callback.as_ref() {
        let reply = read_responses_sse_stream(
            response,
            callback,
            provider_stream_stale_timeout_duration(provider, model),
            &tool_name_map,
        )
        .await?;
        return Ok(with_reply_metadata_and_transport(
            stamp_responses_provider_data_issuer(reply, &issuer_kind),
            provider,
            model,
            &response_headers,
            Some(transport),
        ));
    }

    let text = response
        .text()
        .await
        .map_err(|e| AppError::Llm(format!("failed to read responses llm response: {e}")))?;
    let payload = serde_json::from_str::<Value>(&text).map_err(|error| {
        AppError::Llm(format!(
            "invalid responses llm response ({status}): {error}; {}",
            invalid_response_body_detail(&text, &response_headers)
        ))
    })?;
    parse_responses_compatible_with_tool_name_map(payload, &tool_name_map)
        .map(|reply| stamp_responses_provider_data_issuer(reply, &issuer_kind))
        .map(|reply| {
            with_reply_metadata_and_transport(
                reply,
                provider,
                model,
                &response_headers,
                Some(transport),
            )
        })
}

async fn read_responses_sse_stream(
    response: reqwest::Response,
    callback: &LlmDeltaCallback,
    stale_timeout: Option<std::time::Duration>,
    tool_name_map: &serde_json::Map<String, Value>,
) -> AppResult<LlmReply> {
    let mut buffer = String::new();
    let mut content = String::new();
    let mut final_response = None::<Value>;
    let mut prompt_tokens = 0usize;
    let mut completion_tokens = 0usize;
    let mut reasoning_tokens = 0usize;
    let mut output_items = Vec::<Value>::new();
    let mut tool_state = ResponsesStreamToolCallState::default();
    let mut stream = response.bytes_stream();

    loop {
        let next_chunk = if let Some(timeout) = stale_timeout {
            tokio::time::timeout(timeout, stream.next())
                .await
                .map_err(|_| {
                    AppError::Llm(format!(
                        "responses stream stale: no provider bytes for {}s",
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
            chunk.map_err(|e| AppError::Llm(format!("failed to read responses stream: {e}")))?;
        buffer.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(newline) = buffer.find('\n') {
            let mut line = buffer[..newline].trim().to_string();
            buffer.replace_range(..=newline, "");
            if line.ends_with('\r') {
                line.pop();
            }
            handle_responses_sse_line(
                &line,
                callback,
                &mut content,
                &mut final_response,
                &mut prompt_tokens,
                &mut completion_tokens,
                &mut reasoning_tokens,
                &mut output_items,
                &mut tool_state,
            )?;
        }
    }
    if !buffer.trim().is_empty() {
        let line = buffer.trim().to_string();
        handle_responses_sse_line(
            &line,
            callback,
            &mut content,
            &mut final_response,
            &mut prompt_tokens,
            &mut completion_tokens,
            &mut reasoning_tokens,
            &mut output_items,
            &mut tool_state,
        )?;
    }

    if let Some(mut payload) = final_response {
        merge_responses_stream_fallback_output(&mut payload, &output_items, &content);
        let mut reply = parse_responses_compatible_with_tool_name_map(payload, tool_name_map)?;
        if !content.trim().is_empty() {
            let cleaned = strip_thinking_cards_from_visible_content(&content, &reply.provider_data);
            reply.content = scrub_reasoning_blocks(&cleaned);
            reply.completion_tokens = if completion_tokens == 0 {
                estimate_tokens(&reply.content)
            } else {
                completion_tokens
            };
        }
        return Ok(reply);
    }
    if !output_items.is_empty() {
        let payload = json!({
            "output": output_items,
            "usage": {
                "input_tokens": prompt_tokens,
                "output_tokens": completion_tokens,
                "output_tokens_details": {
                    "reasoning_tokens": reasoning_tokens
                }
            }
        });
        if let Ok(reply) = parse_responses_compatible_with_tool_name_map(payload, tool_name_map) {
            return Ok(reply);
        }
    }
    let content = scrub_reasoning_blocks(&content);
    if content.trim().is_empty() {
        return Err(AppError::Llm(
            "responses stream produced no visible text".into(),
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
        finish_reason: Some("stop".into()),
        provider_data: None,
        failover_attempts: Vec::new(),
    })
}

fn handle_responses_sse_line(
    line: &str,
    callback: &LlmDeltaCallback,
    content: &mut String,
    final_response: &mut Option<Value>,
    prompt_tokens: &mut usize,
    completion_tokens: &mut usize,
    reasoning_tokens: &mut usize,
    output_items: &mut Vec<Value>,
    tool_state: &mut ResponsesStreamToolCallState,
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
    let event_type = payload
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if event_type == "error" {
        return Err(AppError::Llm(format_responses_stream_error(&payload)));
    }
    if responses_stream_event_is_reasoning_delta(event_type) {
        if let Some(delta) = responses_stream_text_delta(&payload).filter(|delta| !delta.is_empty())
        {
            callback(LlmStreamDeltaKind::Thinking, delta)?;
        }
    } else if responses_stream_event_is_answer_delta(event_type) {
        if let Some(delta) = responses_stream_text_delta(&payload).filter(|delta| !delta.is_empty())
        {
            content.push_str(delta);
            callback(LlmStreamDeltaKind::Answer, delta)?;
        }
    }
    if matches!(
        event_type,
        "response.completed" | "response.failed" | "response.incomplete"
    ) {
        if let Some(response) = payload.get("response") {
            *final_response = Some(response.clone());
        }
    }
    if event_type == "response.output_item.done" {
        if let Some(item) = payload.get("item") {
            output_items.push(item.clone());
            tool_state.remove_item(item);
        }
    }
    tool_state.handle_event(event_type, &payload, output_items);
    let usage = payload
        .get("usage")
        .or_else(|| payload.pointer("/response/usage"));
    if *prompt_tokens == 0 {
        *prompt_tokens = usage
            .and_then(|usage| usage.get("input_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
    }
    if *completion_tokens == 0 {
        *completion_tokens = usage
            .and_then(|usage| usage.get("output_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
    }
    if *reasoning_tokens == 0 {
        *reasoning_tokens = usage
            .and_then(|usage| usage.pointer("/output_tokens_details/reasoning_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
    }
    Ok(())
}

fn responses_stream_text_delta(payload: &Value) -> Option<&str> {
    payload
        .get("delta")
        .and_then(Value::as_str)
        .or_else(|| {
            payload
                .pointer("/response/output_text/delta")
                .and_then(Value::as_str)
        })
}

fn responses_stream_event_is_reasoning_delta(event_type: &str) -> bool {
    event_type.contains("reasoning") && event_type.contains(".delta")
}

fn responses_stream_event_is_answer_delta(event_type: &str) -> bool {
    if event_type.is_empty() {
        return true;
    }
    event_type == "response.output_text.delta"
        || event_type == "response.message.delta"
        || event_type == "response.content_part.delta"
        || event_type.ends_with(".output_text.delta")
}

#[derive(Default)]
struct ResponsesStreamToolCallState {
    items: BTreeMap<String, Value>,
    argument_deltas: BTreeMap<String, String>,
}

impl ResponsesStreamToolCallState {
    fn handle_event(&mut self, event_type: &str, payload: &Value, output_items: &mut Vec<Value>) {
        match event_type {
            "response.output_item.added" => self.remember_added_item(payload),
            "response.function_call_arguments.delta" => self.append_arguments_delta(payload),
            "response.function_call_arguments.done" => self.finish_arguments(payload, output_items),
            _ => {}
        }
    }

    fn remember_added_item(&mut self, payload: &Value) {
        let Some(item) = payload.get("item").filter(|item| {
            matches!(
                item.get("type").and_then(Value::as_str),
                Some("function_call" | "custom_tool_call")
            )
        }) else {
            return;
        };
        let Some(key) =
            responses_stream_item_key(item).or_else(|| responses_stream_event_key(payload))
        else {
            return;
        };
        self.items.insert(key, item.clone());
    }

    fn append_arguments_delta(&mut self, payload: &Value) {
        let Some(key) = responses_stream_event_key(payload) else {
            return;
        };
        let Some(delta) = payload.get("delta").and_then(Value::as_str) else {
            return;
        };
        self.argument_deltas.entry(key).or_default().push_str(delta);
    }

    fn finish_arguments(&mut self, payload: &Value, output_items: &mut Vec<Value>) {
        let Some(key) = responses_stream_event_key(payload) else {
            return;
        };
        let arguments = payload
            .get("arguments")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| self.argument_deltas.remove(&key))
            .unwrap_or_default();
        let mut item = self.items.remove(&key).unwrap_or_else(|| {
            json!({
                "type": "function_call",
                "id": key,
                "call_id": payload.get("call_id").cloned().unwrap_or_else(|| json!(key)),
                "name": payload.get("name").cloned().unwrap_or(Value::Null),
            })
        });
        if item.get("call_id").is_none() {
            item["call_id"] = payload
                .get("call_id")
                .cloned()
                .unwrap_or_else(|| json!(key));
        }
        if item
            .get("arguments")
            .is_none_or(|value| value.is_null() || value.as_str() == Some(""))
        {
            item["arguments"] = json!(arguments);
        }
        item["status"] = json!("completed");
        let duplicate = responses_stream_item_key(&item).is_some_and(|item_key| {
            output_items
                .iter()
                .filter_map(responses_stream_item_key)
                .any(|existing| existing == item_key)
        });
        if !duplicate {
            output_items.push(item);
        }
    }

    fn remove_item(&mut self, item: &Value) {
        if let Some(key) = responses_stream_item_key(item) {
            self.items.remove(&key);
            self.argument_deltas.remove(&key);
        }
    }
}

fn responses_stream_event_key(payload: &Value) -> Option<String> {
    first_response_stream_string(payload, &["item_id", "output_item_id", "call_id", "id"])
}

fn responses_stream_item_key(item: &Value) -> Option<String> {
    first_response_stream_string(item, &["id", "call_id"])
}

fn first_response_stream_string(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        value
            .get(*key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    })
}

fn merge_responses_stream_fallback_output(
    payload: &mut Value,
    output_items: &[Value],
    content: &str,
) {
    let output_missing_or_empty = payload
        .get("output")
        .and_then(Value::as_array)
        .is_none_or(|items| items.is_empty());
    if output_missing_or_empty && !output_items.is_empty() {
        payload["output"] = json!(output_items);
    }
    if payload
        .get("output_text")
        .and_then(Value::as_str)
        .is_none_or(|text| text.trim().is_empty())
        && !content.trim().is_empty()
    {
        payload["output_text"] = json!(content);
    }
}

fn responses_failure_allows_chat_completions_fallback(status: u16, body: &str) -> bool {
    if matches!(status, 404 | 405 | 410) {
        return true;
    }
    let lower = body.to_ascii_lowercase();
    lower.contains("not found")
        || lower.contains("unsupported")
        || lower.contains("unknown url")
        || lower.contains("unknown endpoint")
        || lower.contains("invalid path")
        || lower.contains("responses")
}

fn chat_completions_fallback_base_url_for_responses(provider: &LlmProvider) -> String {
    let base = provider_base_url(provider);
    let trimmed = base.trim().trim_end_matches('/');
    if trimmed.eq_ignore_ascii_case("https://api.deepseek.com") {
        "https://api.deepseek.com/v1".into()
    } else {
        trimmed.to_string()
    }
}

fn merge_responses_fallback_diagnostics(
    diagnostics: Option<Value>,
    responses_status: u16,
    responses_url: &str,
    responses_body: &str,
) -> Option<Value> {
    let mut root = diagnostics
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    root.insert(
        "responsesFallback".into(),
        json!({
            "reason": "responses_request_failed",
            "status": responses_status,
            "endpoint": safe_diagnostic_endpoint(responses_url),
            "preview": response_preview(responses_body),
        }),
    );
    Some(Value::Object(root))
}

fn format_responses_stream_error(payload: &Value) -> String {
    if let Some(error) = payload.get("error") {
        return format_responses_error(Some(error), "error");
    }
    let code = payload
        .get("code")
        .map(value_to_compact_error_part)
        .filter(|value| !value.trim().is_empty());
    let message = payload
        .get("message")
        .map(value_to_compact_error_part)
        .filter(|value| !value.trim().is_empty());
    match (code, message) {
        (Some(code), Some(message)) if code != message => format!("{code}: {message}"),
        (Some(code), _) => code,
        (_, Some(message)) => message,
        _ => "Responses stream emitted an error event".into(),
    }
}

pub(super) fn responses_unsupported_parameter_retry_body(
    body: &Value,
    error_text: &str,
) -> Option<Value> {
    let mut retry = body.clone();
    let object = retry.as_object_mut()?;
    let lower = error_text.to_ascii_lowercase();
    let rejected = rejected_openai_parameter(error_text);
    match rejected.as_deref() {
        Some("temperature") => {
            object.remove("temperature")?;
        }
        Some("max_output_tokens") => {
            object.remove("max_output_tokens")?;
        }
        Some("tool_choice") => {
            object.remove("tool_choice")?;
        }
        Some("parallel_tool_calls") => {
            object.remove("parallel_tool_calls")?;
        }
        Some(parameter) if parameter == "reasoning" || parameter.starts_with("reasoning.") => {
            object.remove("reasoning");
            remove_reasoning_include_values(object);
        }
        Some("include") => {
            object.remove("include")?;
        }
        Some("service_tier") => {
            object.remove("service_tier")?;
        }
        _ => {
            if unsupported_parameter_error_mentions(&lower, "temperature") {
                object.remove("temperature")?;
            } else if unsupported_parameter_error_mentions(&lower, "max_output_tokens") {
                object.remove("max_output_tokens")?;
            } else if unsupported_parameter_error_mentions(&lower, "tool_choice") {
                object.remove("tool_choice")?;
            } else if unsupported_parameter_error_mentions(&lower, "parallel_tool_calls") {
                object.remove("parallel_tool_calls")?;
            } else if unsupported_parameter_error_mentions(&lower, "reasoning") {
                object.remove("reasoning");
                remove_reasoning_include_values(object);
            } else if unsupported_parameter_error_mentions(&lower, "include")
                || lower.contains("reasoning.encrypted_content")
                || lower.contains("reasoning.summary")
            {
                object.remove("include")?;
            } else if unsupported_parameter_error_mentions(&lower, "service_tier") {
                object.remove("service_tier")?;
            } else {
                return None;
            }
        }
    }
    (retry != *body).then_some(retry)
}

fn remove_reasoning_include_values(object: &mut serde_json::Map<String, Value>) {
    let Some(include) = object.get_mut("include") else {
        return;
    };
    let Some(items) = include.as_array_mut() else {
        return;
    };
    items.retain(|item| {
        !item
            .as_str()
            .map(|value| value.to_ascii_lowercase().starts_with("reasoning."))
            .unwrap_or(false)
    });
    if items.is_empty() {
        object.remove("include");
    }
}

pub(super) fn is_responses_compatible(provider: &LlmProvider) -> bool {
    let provider_id = provider.id.to_lowercase();
    let provider_type = provider.provider_type.to_lowercase();
    let preset = provider
        .preset
        .as_deref()
        .unwrap_or_default()
        .to_lowercase();
    let base_url = provider_base_url(provider).to_lowercase();
    provider_id.contains("openai-codex")
        || provider_id.contains("xai")
        || provider_id.contains("grok")
        || provider_id.contains("copilot-acp")
        || provider_type == "openai_responses"
        || provider_type == "openai-responses"
        || provider_type.contains("responses")
        || provider_type.contains("codex")
        || preset == "openai_responses"
        || preset == "openai-responses"
        || preset.contains("xai")
        || preset.contains("grok")
        || preset.contains("copilot-acp")
        || preset.contains("responses")
        || preset.contains("codex")
        || host_matches(&base_url, "api.x.ai")
        || base_url.ends_with("/responses")
        || base_url.contains("/backend-api/codex")
}

pub(super) fn build_responses_payload(
    system_prompt: String,
    history: Vec<ChatMessage>,
    current_issuer_kind: Option<&str>,
    reasoning_replay_enabled: bool,
) -> (String, Vec<Value>) {
    build_responses_payload_with_tool_name_map(
        system_prompt,
        history,
        current_issuer_kind,
        reasoning_replay_enabled,
        &serde_json::Map::new(),
    )
}

fn build_responses_payload_with_tool_name_map(
    system_prompt: String,
    history: Vec<ChatMessage>,
    current_issuer_kind: Option<&str>,
    reasoning_replay_enabled: bool,
    tool_name_map: &serde_json::Map<String, Value>,
) -> (String, Vec<Value>) {
    let mut instructions = sanitize_provider_text(&system_prompt);
    let mut input = Vec::new();
    for item in history {
        if item.role == "system" {
            let content = sanitize_provider_text(&item.content);
            if !content.trim().is_empty() {
                if !instructions.trim().is_empty() {
                    instructions.push_str("\n\n");
                }
                instructions.push_str(&content);
            }
            continue;
        }
        if let Some(mut tool_replay) = tool_replay_message(&item) {
            tool_replay.name =
                safe_provider_tool_name_for_original(&tool_replay.name, tool_name_map);
            input.push(json!({
                "type": "function_call",
                "call_id": tool_replay.call_id,
                "name": tool_replay.name,
                "arguments": tool_replay.arguments.to_string(),
            }));
            input.push(json!({
                "type": "function_call_output",
                "call_id": tool_replay.call_id,
                "output": tool_replay.content,
            }));
            continue;
        }
        if !matches!(item.role.as_str(), "user" | "assistant") {
            continue;
        }
        let role = item.role.clone();
        let replayed_provider_items = (role == "assistant").then(|| {
            responses_provider_replay_items(&item, current_issuer_kind, reasoning_replay_enabled)
        });
        if let Some(provider_items) = replayed_provider_items.as_ref() {
            input.extend(provider_items.iter().cloned());
        }
        let content = sanitize_provider_text(&item.content);
        let replayed_message_item = replayed_provider_items
            .as_ref()
            .map(|items| {
                items.iter().any(|item| {
                    item.get("type").and_then(Value::as_str) == Some("message")
                        && item.get("role").and_then(Value::as_str) == Some("assistant")
                })
            })
            .unwrap_or(false);
        if content.trim().is_empty() {
            if role == "assistant"
                && replayed_provider_items
                    .as_ref()
                    .map(|items| !items.is_empty())
                    .unwrap_or(false)
            {
                input.push(json!({"role": "assistant", "content": ""}));
            }
            continue;
        }
        if role == "assistant" && replayed_message_item {
            continue;
        }
        if role == "user" {
            if let Some(content) = responses_provider_user_content(&item) {
                input.push(json!({
                    "role": role,
                    "content": content
                }));
                continue;
            }
        }
        let part_type = if role == "assistant" {
            "output_text"
        } else {
            "input_text"
        };
        push_responses_text_item(&mut input, &role, part_type, &content);
    }
    (instructions, input)
}

fn responses_provider_user_content(message: &ChatMessage) -> Option<Vec<Value>> {
    let provider_data = message.provider_data.as_ref()?;
    let responses = provider_data.get("responses").unwrap_or(provider_data);
    let content = responses
        .get("content")
        .or_else(|| responses.get("contentParts"))
        .or_else(|| responses.get("content_parts"))?
        .as_array()?;
    let content = content
        .iter()
        .filter(|part| {
            matches!(
                part.get("type").and_then(Value::as_str),
                Some("input_text" | "input_image")
            )
        })
        .cloned()
        .collect::<Vec<_>>();
    (!content.is_empty()).then_some(content)
}

fn push_responses_text_item(input: &mut Vec<Value>, role: &str, part_type: &str, content: &str) {
    let content = sanitize_provider_text(content);
    if content.trim().is_empty() {
        return;
    }
    if let Some(previous) = input.last_mut() {
        if previous.get("role").and_then(Value::as_str) == Some(role)
            && previous.pointer("/content/0/type").and_then(Value::as_str) == Some(part_type)
            && previous
                .get("content")
                .and_then(Value::as_array)
                .map(|items| items.len() == 1)
                .unwrap_or(false)
        {
            if let Some(previous_text) = previous
                .pointer("/content/0/text")
                .and_then(Value::as_str)
                .map(str::to_string)
            {
                let separator = if previous_text.trim().is_empty() || content.trim().is_empty() {
                    ""
                } else {
                    "\n\n"
                };
                previous["content"][0]["text"] =
                    json!(format!("{previous_text}{separator}{content}"));
                return;
            }
        }
    }
    input.push(json!({
        "role": role,
        "content": [{
            "type": part_type,
            "text": content
        }]
    }));
}

fn responses_issuer_kind(provider: &LlmProvider) -> String {
    let provider_id = provider.id.to_ascii_lowercase();
    let provider_type = provider.provider_type.to_ascii_lowercase();
    let preset = provider
        .preset
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let base_url = provider_base_url(provider).trim().to_ascii_lowercase();
    if provider_id.contains("xai")
        || provider_id.contains("grok")
        || provider_type.contains("xai")
        || preset.contains("xai")
        || base_url.contains("api.x.ai")
        || base_url.contains("grok")
    {
        "xai_responses".into()
    } else if provider_id.contains("copilot")
        || provider_id.contains("github")
        || provider_type.contains("github")
        || preset.contains("github")
        || preset.contains("copilot")
        || base_url.contains("models.github.ai")
    {
        "github_responses".into()
    } else if provider_id.contains("openai-codex")
        || provider_type.contains("codex")
        || preset.contains("codex")
        || base_url.contains("/backend-api/codex")
        || base_url.contains("chatgpt.com")
    {
        "codex_backend".into()
    } else if base_url.is_empty() {
        "other".into()
    } else {
        format!("other:{base_url}")
    }
}

pub(super) fn responses_url(provider: &LlmProvider) -> String {
    let base_url = provider_base_url(provider);
    let base = base_url.trim().trim_end_matches('/');
    if base.is_empty() {
        return "https://api.openai.com/v1/responses".into();
    }
    if base.ends_with("/responses") {
        base.to_string()
    } else if base.ends_with("/v1") {
        format!("{base}/responses")
    } else if provider.append_chat_path {
        format!("{base}/responses")
    } else {
        base.to_string()
    }
}

pub(super) fn parse_responses_compatible(payload: Value) -> AppResult<LlmReply> {
    parse_responses_compatible_with_tool_name_map(payload, &serde_json::Map::new())
}

fn parse_responses_compatible_with_tool_name_map(
    payload: Value,
    tool_name_map: &serde_json::Map<String, Value>,
) -> AppResult<LlmReply> {
    if let Some(status) = payload.get("status").and_then(Value::as_str) {
        let normalized = status.trim().to_ascii_lowercase();
        if matches!(normalized.as_str(), "failed" | "cancelled") {
            return Err(AppError::Llm(format_responses_error(
                payload.get("error"),
                &normalized,
            )));
        }
    }
    let mut content = payload
        .get("output_text")
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|text| !text.trim().is_empty())
        .unwrap_or_else(|| responses_output_text(&payload));
    let tool_calls = responses_tool_calls(&payload, tool_name_map);
    let finish_reason =
        responses_finish_reason(&payload, !content.trim().is_empty(), !tool_calls.is_empty());
    let provider_data = responses_provider_data(&payload);
    content = strip_thinking_cards_from_visible_content(&content, &provider_data);
    if !tool_calls.is_empty() {
        let tool_json = json!({"tool_calls": tool_calls}).to_string();
        if content.trim().is_empty() {
            content = tool_json;
        } else {
            content = format!("{content}\n\n{tool_json}");
        }
    }
    let content = scrub_reasoning_blocks(&content);
    if content.trim().is_empty() && finish_reason.as_deref() != Some("incomplete") {
        return Err(AppError::Llm(format!(
            "responses response has no visible text or tool-call content: {payload}"
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
    let reasoning_tokens = payload
        .pointer("/usage/output_tokens_details/reasoning_tokens")
        .and_then(Value::as_u64)
        .or_else(|| {
            payload
                .pointer("/usage/completion_tokens_details/reasoning_tokens")
                .and_then(Value::as_u64)
        })
        .unwrap_or(0) as usize;

    Ok(LlmReply {
        content,
        prompt_tokens,
        completion_tokens,
        cache_read_tokens: 0,
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
        finish_reason,
        provider_data,
        failover_attempts: Vec::new(),
    })
}

pub(super) fn format_responses_error(error: Option<&Value>, status: &str) -> String {
    let Some(error) = error else {
        return format!("Responses API returned status '{status}'");
    };
    let code = error
        .get("code")
        .map(value_to_compact_error_part)
        .filter(|value| !value.trim().is_empty());
    let message = error
        .get("message")
        .map(value_to_compact_error_part)
        .filter(|value| !value.trim().is_empty());
    match (code, message) {
        (Some(code), Some(message)) if code != message => format!("{code}: {message}"),
        (Some(code), _) => code,
        (_, Some(message)) => message,
        _ => value_to_compact_error_part(error)
            .trim()
            .to_string()
            .if_empty_then(|| format!("Responses API returned status '{status}'")),
    }
}

fn value_to_compact_error_part(value: &Value) -> String {
    match value {
        Value::String(text) => text.trim().to_string(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

trait EmptyStringFallback {
    fn if_empty_then(self, fallback: impl FnOnce() -> String) -> String;
}

impl EmptyStringFallback for String {
    fn if_empty_then(self, fallback: impl FnOnce() -> String) -> String {
        if self.is_empty() {
            fallback()
        } else {
            self
        }
    }
}

fn responses_output_text(payload: &Value) -> String {
    payload
        .get("output")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| item.get("content").and_then(Value::as_array))
        .flatten()
        .filter_map(|part| {
            let part_type = part.get("type").and_then(Value::as_str).unwrap_or_default();
            if matches!(part_type, "output_text" | "text") {
                part.get("text").and_then(Value::as_str)
            } else {
                None
            }
        })
        .collect::<Vec<_>>()
        .join("")
}

fn responses_tool_calls(
    payload: &Value,
    tool_name_map: &serde_json::Map<String, Value>,
) -> Vec<Value> {
    payload
        .get("output")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let item_status = normalized_responses_status(item.get("status"));
            if matches!(
                item_status.as_deref(),
                Some("queued" | "in_progress" | "incomplete")
            ) {
                return None;
            }
            let item_type = item.get("type").and_then(Value::as_str)?;
            if !matches!(item_type, "function_call" | "custom_tool_call") {
                return None;
            }
            let name = item
                .get("name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|name| !name.is_empty())?;
            let name = original_provider_tool_name(name, tool_name_map);
            let arguments = if item_type == "custom_tool_call" {
                item.get("input").cloned().unwrap_or_else(|| json!({}))
            } else {
                item.get("arguments").cloned().unwrap_or_else(|| json!({}))
            };
            let arguments = normalize_provider_tool_arguments(arguments, &name);
            Some(json!({
                "type": "function",
                "id": item.get("id").cloned().unwrap_or(Value::Null),
                "call_id": item.get("call_id").cloned().unwrap_or(Value::Null),
                "function": {
                    "name": name,
                    "arguments": arguments
                }
            }))
        })
        .collect()
}

fn responses_provider_data(payload: &Value) -> Option<Value> {
    let reasoning_items = responses_reasoning_replay_items(payload);
    let message_items = responses_message_replay_items(payload);
    let thinking_cards = responses_thinking_cards(payload);
    if reasoning_items.is_empty() && message_items.is_empty() && thinking_cards.is_empty() {
        return None;
    }
    Some(json!({
        "responses": {
            "reasoningItems": reasoning_items,
            "messageItems": message_items,
            "thinkingCards": thinking_cards
        }
    }))
}

fn stamp_responses_provider_data_issuer(mut reply: LlmReply, issuer_kind: &str) -> LlmReply {
    let Some(provider_data) = reply.provider_data.as_mut() else {
        return reply;
    };
    let Some(reasoning_items) = provider_data
        .get_mut("responses")
        .and_then(|responses| responses.get_mut("reasoningItems"))
        .and_then(Value::as_array_mut)
    else {
        return reply;
    };
    for item in reasoning_items {
        if item
            .get("encrypted_content")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .is_some()
        {
            item["_issuerKind"] = json!(issuer_kind);
        }
    }
    reply
}

fn responses_reasoning_replay_items(payload: &Value) -> Vec<Value> {
    payload
        .get("output")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| {
            if item.get("type").and_then(Value::as_str) != Some("reasoning") {
                return None;
            }
            let encrypted = item
                .get("encrypted_content")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())?;
            if item
                .get("id")
                .and_then(Value::as_str)
                .map(|id| id.starts_with("rs_tmp_"))
                .unwrap_or(false)
            {
                return None;
            }
            let mut replay = json!({
                "type": "reasoning",
                "encrypted_content": encrypted
            });
            if let Some(summary) = normalized_responses_reasoning_summary(item) {
                replay["summary"] = summary;
            }
            Some(replay)
        })
        .collect()
}

fn normalized_responses_reasoning_summary(item: &Value) -> Option<Value> {
    let summary = item.get("summary")?.as_array()?;
    let parts = summary
        .iter()
        .filter_map(|part| {
            let text = part
                .get("text")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())?;
            Some(json!({
                "type": "summary_text",
                "text": text
            }))
        })
        .collect::<Vec<_>>();
    (!parts.is_empty()).then(|| json!(parts))
}

fn responses_thinking_cards(payload: &Value) -> Vec<Value> {
    payload
        .get("output")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| {
            if item.get("type").and_then(Value::as_str) != Some("reasoning") {
                return None;
            }
            let summary = responses_reasoning_summary_text(item);
            let encrypted = item
                .get("encrypted_content")
                .and_then(Value::as_str)
                .map(str::trim)
                .is_some_and(|value| !value.is_empty());
            if summary.trim().is_empty() && !encrypted {
                return None;
            }
            Some(json!({
                "provider": "openai_responses",
                "kind": "reasoning",
                "title": "模型思考",
                "summary": summary,
                "redacted": summary.trim().is_empty(),
                "encrypted": encrypted,
                "itemId": item.get("id").cloned().unwrap_or(Value::Null)
            }))
        })
        .collect()
}

fn responses_reasoning_summary_text(item: &Value) -> String {
    let mut texts = Vec::new();
    collect_responses_reasoning_texts(item.get("summary"), &mut texts);
    collect_responses_reasoning_texts(item.get("content"), &mut texts);
    collect_responses_reasoning_texts(item.get("text"), &mut texts);
    texts
        .into_iter()
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn collect_responses_reasoning_texts(value: Option<&Value>, texts: &mut Vec<String>) {
    match value {
        Some(Value::String(text)) => texts.push(text.clone()),
        Some(Value::Array(items)) => {
            for item in items {
                collect_responses_reasoning_texts(Some(item), texts);
            }
        }
        Some(Value::Object(object)) => {
            for key in ["text", "summary", "content"] {
                collect_responses_reasoning_texts(object.get(key), texts);
            }
        }
        _ => {}
    }
}

fn responses_message_replay_items(payload: &Value) -> Vec<Value> {
    payload
        .get("output")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| {
            if item.get("type").and_then(Value::as_str) != Some("message") {
                return None;
            }
            let content = item
                .get("content")
                .and_then(Value::as_array)?
                .iter()
                .filter_map(|part| {
                    let part_type = part.get("type").and_then(Value::as_str).unwrap_or_default();
                    if !matches!(part_type, "output_text" | "text") {
                        return None;
                    }
                    let text = part.get("text").and_then(Value::as_str)?;
                    Some(json!({
                        "type": "output_text",
                        "text": text
                    }))
                })
                .collect::<Vec<_>>();
            if content.is_empty() {
                return None;
            }
            let status = normalized_responses_status(item.get("status"))
                .filter(|status| {
                    matches!(status.as_str(), "completed" | "incomplete" | "in_progress")
                })
                .unwrap_or_else(|| "completed".into());
            let mut replay = json!({
                "type": "message",
                "role": "assistant",
                "status": status,
                "content": content
            });
            if let Some(id) = item
                .get("id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                replay["id"] = json!(id);
            }
            if let Some(phase) = item
                .get("phase")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                replay["phase"] = json!(phase);
            }
            Some(replay)
        })
        .collect()
}

fn responses_provider_replay_items(
    message: &ChatMessage,
    current_issuer_kind: Option<&str>,
    reasoning_replay_enabled: bool,
) -> Vec<Value> {
    let Some(provider_data) = message.provider_data.as_ref() else {
        return Vec::new();
    };
    let responses = provider_data.get("responses").unwrap_or(provider_data);
    let mut items = Vec::new();
    if reasoning_replay_enabled {
        items.extend(provider_replay_array(
            responses,
            &[
                "reasoningItems",
                "reasoning_items",
                "codexReasoningItems",
                "codex_reasoning_items",
            ],
            |item| normalize_reasoning_replay_item(item, current_issuer_kind),
        ));
    }
    items.extend(provider_replay_array(
        responses,
        &[
            "messageItems",
            "message_items",
            "codexMessageItems",
            "codex_message_items",
        ],
        normalize_message_replay_item,
    ));
    items
}

fn provider_replay_array(
    data: &Value,
    keys: &[&str],
    normalize: impl Fn(&Value) -> Option<Value>,
) -> Vec<Value> {
    for key in keys {
        if let Some(items) = data.get(*key).and_then(Value::as_array) {
            return items.iter().filter_map(normalize).collect();
        }
    }
    Vec::new()
}

fn normalize_reasoning_replay_item(
    item: &Value,
    current_issuer_kind: Option<&str>,
) -> Option<Value> {
    if item.get("type").and_then(Value::as_str) != Some("reasoning") {
        return None;
    }
    let item_issuer = item
        .get("_issuerKind")
        .or_else(|| item.get("_issuer_kind"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if let (Some(current), Some(item_issuer)) = (current_issuer_kind, item_issuer) {
        if current != item_issuer {
            return None;
        }
    }
    let encrypted = item
        .get("encrypted_content")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    let mut replay = json!({
        "type": "reasoning",
        "encrypted_content": encrypted
    });
    if let Some(summary) = normalized_responses_reasoning_summary(item) {
        replay["summary"] = summary;
    }
    Some(replay)
}

fn normalize_message_replay_item(item: &Value) -> Option<Value> {
    if item.get("type").and_then(Value::as_str) != Some("message") {
        return None;
    }
    let content = item.get("content").and_then(Value::as_array)?;
    if content.is_empty() {
        return None;
    }
    let mut replay = json!({
        "type": "message",
        "role": "assistant",
        "status": normalized_responses_status(item.get("status"))
            .unwrap_or_else(|| "completed".into()),
        "content": content
    });
    if let Some(id) = item.get("id").and_then(Value::as_str) {
        replay["id"] = json!(id);
    }
    if let Some(phase) = item.get("phase").and_then(Value::as_str) {
        replay["phase"] = json!(phase);
    }
    Some(replay)
}

fn responses_finish_reason(
    payload: &Value,
    has_visible_text: bool,
    has_tool_calls: bool,
) -> Option<String> {
    if has_tool_calls {
        return Some("tool_calls".into());
    }
    if matches!(
        normalized_responses_status(payload.get("status")).as_deref(),
        Some("queued" | "in_progress" | "incomplete")
    ) {
        return Some("incomplete".into());
    }

    let mut saw_reasoning_item = false;
    let mut saw_commentary_phase = false;
    let mut saw_final_phase = false;
    for item in payload
        .get("output")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if matches!(
            normalized_responses_status(item.get("status")).as_deref(),
            Some("queued" | "in_progress" | "incomplete")
        ) {
            return Some("incomplete".into());
        }
        match item.get("type").and_then(Value::as_str) {
            Some("reasoning") => saw_reasoning_item = true,
            Some("message") => {
                let phase = item
                    .get("phase")
                    .and_then(Value::as_str)
                    .map(|value| value.trim().to_ascii_lowercase().replace('-', "_"))
                    .unwrap_or_default();
                if matches!(phase.as_str(), "commentary" | "analysis") {
                    saw_commentary_phase = true;
                } else if matches!(phase.as_str(), "final" | "final_answer") {
                    saw_final_phase = true;
                }
            }
            _ => {}
        }
    }
    if saw_commentary_phase && !saw_final_phase {
        return Some("incomplete".into());
    }
    if saw_reasoning_item && !has_visible_text {
        return Some("incomplete".into());
    }
    Some("stop".into())
}

fn normalized_responses_status(value: Option<&Value>) -> Option<String> {
    value.and_then(Value::as_str).map(|status| {
        status
            .trim()
            .to_ascii_lowercase()
            .replace('-', "_")
            .replace(' ', "_")
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    #[test]
    fn responses_stream_collects_output_item_done_fallback() {
        let callback: LlmDeltaCallback = Arc::new(|_, _| Ok(()));
        let mut content = String::new();
        let mut final_response = None;
        let mut prompt_tokens = 0;
        let mut completion_tokens = 0;
        let mut reasoning_tokens = 0;
        let mut output_items = Vec::new();
        let mut tool_state = ResponsesStreamToolCallState::default();

        handle_responses_sse_line(
            r#"data: {"type":"response.output_item.done","item":{"type":"function_call","id":"fc_123","call_id":"call_123","name":"terminal","arguments":{"command":"pwd"},"status":"completed"}}"#,
            &callback,
            &mut content,
            &mut final_response,
            &mut prompt_tokens,
            &mut completion_tokens,
            &mut reasoning_tokens,
            &mut output_items,
            &mut tool_state,
        )
        .unwrap();

        let reply = parse_responses_compatible(json!({"output": output_items})).unwrap();
        let value = serde_json::from_str::<Value>(&reply.content).unwrap();
        assert_eq!(value["tool_calls"][0]["id"], "fc_123");
        assert_eq!(value["tool_calls"][0]["call_id"], "call_123");
        assert_eq!(value["tool_calls"][0]["function"]["name"], "terminal");
        assert_eq!(
            value["tool_calls"][0]["function"]["arguments"],
            json!({"command": "pwd"})
        );
    }

    #[test]
    fn responses_stream_error_event_surfaces_code_and_message() {
        let callback: LlmDeltaCallback = Arc::new(|_, _| Ok(()));
        let mut content = String::new();
        let mut final_response = None;
        let mut prompt_tokens = 0;
        let mut completion_tokens = 0;
        let mut reasoning_tokens = 0;
        let mut output_items = Vec::new();
        let mut tool_state = ResponsesStreamToolCallState::default();

        let error = handle_responses_sse_line(
            r#"data: {"type":"error","code":"rate_limit_exceeded","message":"quota exhausted"}"#,
            &callback,
            &mut content,
            &mut final_response,
            &mut prompt_tokens,
            &mut completion_tokens,
            &mut reasoning_tokens,
            &mut output_items,
            &mut tool_state,
        )
        .unwrap_err();

        let text = error.to_string();
        assert!(text.contains("rate_limit_exceeded"));
        assert!(text.contains("quota exhausted"));
    }

    #[test]
    fn responses_stream_failed_terminal_frame_is_not_downgraded_to_empty_text() {
        let callback: LlmDeltaCallback = Arc::new(|_, _| Ok(()));
        let mut content = String::new();
        let mut final_response = None;
        let mut prompt_tokens = 0;
        let mut completion_tokens = 0;
        let mut reasoning_tokens = 0;
        let mut output_items = Vec::new();
        let mut tool_state = ResponsesStreamToolCallState::default();

        handle_responses_sse_line(
            r#"data: {"type":"response.failed","response":{"status":"failed","error":{"code":"invalid_request_error","message":"bad reasoning replay"}}}"#,
            &callback,
            &mut content,
            &mut final_response,
            &mut prompt_tokens,
            &mut completion_tokens,
            &mut reasoning_tokens,
            &mut output_items,
            &mut tool_state,
        )
        .unwrap();

        let error = parse_responses_compatible(final_response.unwrap()).unwrap_err();
        let text = error.to_string();
        assert!(text.contains("invalid_request_error"));
        assert!(text.contains("bad reasoning replay"));
    }

    #[test]
    fn responses_stream_reconstructs_function_call_from_argument_deltas() {
        let callback: LlmDeltaCallback = Arc::new(|_, _| Ok(()));
        let mut content = String::new();
        let mut final_response = None;
        let mut prompt_tokens = 0;
        let mut completion_tokens = 0;
        let mut reasoning_tokens = 0;
        let mut output_items = Vec::new();
        let mut tool_state = ResponsesStreamToolCallState::default();

        for line in [
            r#"data: {"type":"response.output_item.added","item_id":"fc_delta","item":{"type":"function_call","id":"fc_delta","call_id":"call_delta","name":"terminal","arguments":"","status":"in_progress"}}"#,
            r#"data: {"type":"response.function_call_arguments.delta","item_id":"fc_delta","delta":"{\"command\":"}"#,
            r#"data: {"type":"response.function_call_arguments.delta","item_id":"fc_delta","delta":"\"pwd\"}"}"#,
            r#"data: {"type":"response.function_call_arguments.done","item_id":"fc_delta"}"#,
        ] {
            handle_responses_sse_line(
                line,
                &callback,
                &mut content,
                &mut final_response,
                &mut prompt_tokens,
                &mut completion_tokens,
                &mut reasoning_tokens,
                &mut output_items,
                &mut tool_state,
            )
            .unwrap();
        }

        let reply = parse_responses_compatible(json!({"output": output_items})).unwrap();
        let value = serde_json::from_str::<Value>(&reply.content).unwrap();
        assert_eq!(value["tool_calls"][0]["id"], "fc_delta");
        assert_eq!(value["tool_calls"][0]["call_id"], "call_delta");
        assert_eq!(value["tool_calls"][0]["function"]["name"], "terminal");
        let arguments = value["tool_calls"][0]["function"]["arguments"]
            .as_str()
            .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
            .unwrap();
        assert_eq!(arguments, json!({"command": "pwd"}));
    }
}
