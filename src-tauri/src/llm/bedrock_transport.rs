use std::{collections::BTreeMap, time::Instant};

use chrono::Utc;
use futures::StreamExt;
use hmac::{Hmac, Mac};
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::{
    error::{AppError, AppResult},
    models::{ChatMessage, LlmProvider, Persona, ToolDefinition},
};

use super::*;

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Clone)]
struct AwsCredentials {
    access_key: String,
    secret_key: String,
    session_token: Option<String>,
}

#[derive(Debug, Default)]
struct BedrockStreamToolUse {
    id: String,
    name: String,
    input_json: String,
}

pub(super) async fn complete_bedrock_compatible(
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
    let region = bedrock_region(provider);
    let url = bedrock_converse_url(provider, model, &region)?;
    let request_url = if options.stream_delta_callback.is_some() {
        bedrock_converse_stream_url(url.clone())
    } else {
        url
    };
    let credentials = bedrock_aws_credentials(provider)?;

    let mut body = json!({
        "system": [{"text": sanitize_provider_text(&system_prompt)}],
        "messages": build_bedrock_messages(history),
        "inferenceConfig": {
            "temperature": persona.temperature,
            "maxTokens": persona.max_tokens
        }
    });
    if let Some(tool_config) =
        native_tools.and_then(|tools| bedrock_tool_config(tools).filter(|_| !tools.is_empty()))
    {
        body["toolConfig"] = tool_config;
    }
    let body_text = serde_json::to_string(&body)
        .map_err(|error| AppError::Llm(format!("failed to serialize bedrock payload: {error}")))?;
    let headers = bedrock_sigv4_headers(&request_url, &region, &credentials, &body_text)?;

    let client = reqwest::Client::builder()
        .timeout(provider_request_timeout_duration(provider, model))
        .build()
        .map_err(|error| AppError::Llm(error.to_string()))?;
    let started_at = Instant::now();
    let response = send_llm_request_with_stale_timeout(
        client
            .post(request_url.clone())
            .headers(headers)
            .body(body_text),
        provider,
        model,
        "bedrock converse request",
    )
    .await?;

    let status = response.status();
    let response_headers = response.headers().clone();
    if !status.is_success() {
        let text = response
            .text()
            .await
            .map_err(|error| AppError::Llm(format!("failed to read bedrock response: {error}")))?;
        return Err(AppError::Llm(format!(
            "provider returned {status}: {}",
            response_preview(&text)
        )));
    }

    let transport = LlmTransportMetadata {
        transport: if options.stream_delta_callback.is_some() {
            "bedrock_converse_stream"
        } else {
            "bedrock_converse"
        },
        method: "POST",
        endpoint: request_url.to_string(),
        status: Some(status.as_u16()),
        elapsed_ms: Some(started_at.elapsed().as_millis().min(u64::MAX as u128) as u64),
        retry_count: 0,
        retry_reason: None,
    };
    if let Some(callback) = options.stream_delta_callback.as_ref() {
        let reply = read_bedrock_event_stream(
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
        .map_err(|error| AppError::Llm(format!("failed to read bedrock response: {error}")))?;
    let payload = serde_json::from_str::<Value>(&text).map_err(|error| {
        AppError::Llm(format!(
            "invalid bedrock response ({status}): {error}; {}",
            invalid_response_body_detail(&text, &response_headers)
        ))
    })?;
    parse_bedrock_converse(payload).map(|reply| {
        with_reply_metadata_and_transport(
            reply,
            provider,
            model,
            &response_headers,
            Some(transport),
        )
    })
}

async fn read_bedrock_event_stream(
    response: reqwest::Response,
    callback: &LlmDeltaCallback,
    stale_timeout: Option<std::time::Duration>,
) -> AppResult<LlmReply> {
    let mut buffer = Vec::<u8>::new();
    let mut content = String::new();
    let mut prompt_tokens = 0usize;
    let mut completion_tokens = 0usize;
    let mut finish_reason = None::<String>;
    let mut tool_uses = BTreeMap::<usize, BedrockStreamToolUse>::new();
    let mut stream = response.bytes_stream();

    loop {
        let next_chunk = if let Some(timeout) = stale_timeout {
            tokio::time::timeout(timeout, stream.next())
                .await
                .map_err(|_| {
                    AppError::Llm(format!(
                        "bedrock stream stale: no provider bytes for {}s",
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
            chunk.map_err(|e| AppError::Llm(format!("failed to read bedrock stream: {e}")))?;
        buffer.extend_from_slice(&chunk);
        while let Some(payload) = next_aws_event_stream_payload(&mut buffer)? {
            handle_bedrock_stream_payload(
                &payload,
                callback,
                &mut content,
                &mut prompt_tokens,
                &mut completion_tokens,
                &mut finish_reason,
                &mut tool_uses,
            )?;
        }
    }

    let tool_calls = bedrock_stream_tool_calls(tool_uses)?;
    let has_tool_calls = !tool_calls.is_empty();
    if has_tool_calls {
        let tool_json = json!({"tool_calls": tool_calls}).to_string();
        if content.trim().is_empty() {
            content = tool_json;
        } else {
            content = format!("{content}\n\n{tool_json}");
        }
    }
    let content = scrub_reasoning_blocks(&content);
    if content.trim().is_empty() {
        return Err(AppError::Llm(
            "bedrock stream produced no visible text".into(),
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

fn next_aws_event_stream_payload(buffer: &mut Vec<u8>) -> AppResult<Option<Value>> {
    if buffer.len() < 12 {
        return Ok(None);
    }
    let total_len = u32::from_be_bytes([buffer[0], buffer[1], buffer[2], buffer[3]]) as usize;
    let headers_len = u32::from_be_bytes([buffer[4], buffer[5], buffer[6], buffer[7]]) as usize;
    if total_len < 16 || 12 + headers_len > total_len.saturating_sub(4) {
        return Err(AppError::Llm("invalid bedrock event stream frame".into()));
    }
    if buffer.len() < total_len {
        return Ok(None);
    }
    let payload_start = 12 + headers_len;
    let payload_end = total_len - 4;
    let payload_bytes = buffer[payload_start..payload_end].to_vec();
    buffer.drain(..total_len);
    if payload_bytes.is_empty() {
        return Ok(Some(json!({})));
    }
    serde_json::from_slice::<Value>(&payload_bytes)
        .map(Some)
        .map_err(|error| AppError::Llm(format!("invalid bedrock stream event: {error}")))
}

fn handle_bedrock_stream_payload(
    payload: &Value,
    callback: &LlmDeltaCallback,
    content: &mut String,
    prompt_tokens: &mut usize,
    completion_tokens: &mut usize,
    finish_reason: &mut Option<String>,
    tool_uses: &mut BTreeMap<usize, BedrockStreamToolUse>,
) -> AppResult<()> {
    if let Some(delta) = payload
        .pointer("/contentBlockDelta/delta/text")
        .and_then(Value::as_str)
        .filter(|delta| !delta.is_empty())
    {
        content.push_str(delta);
        callback(LlmStreamDeltaKind::Answer, delta)?;
    }
    track_bedrock_stream_tool_use(payload, tool_uses);
    if let Some(usage) = payload.pointer("/metadata/usage") {
        if *prompt_tokens == 0 {
            *prompt_tokens = usage
                .get("inputTokens")
                .and_then(Value::as_u64)
                .unwrap_or(0) as usize;
        }
        if let Some(output_tokens) = usage.get("outputTokens").and_then(Value::as_u64) {
            *completion_tokens = output_tokens as usize;
        }
    }
    if let Some(reason) = payload
        .pointer("/messageStop/stopReason")
        .and_then(Value::as_str)
        .filter(|reason| !reason.is_empty())
    {
        *finish_reason = Some(if reason == "tool_use" {
            "tool_calls".into()
        } else {
            reason.to_string()
        });
    }
    Ok(())
}

fn track_bedrock_stream_tool_use(
    payload: &Value,
    tool_uses: &mut BTreeMap<usize, BedrockStreamToolUse>,
) {
    let index = payload
        .get("contentBlockIndex")
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;
    if let Some(tool_use) = payload.pointer("/contentBlockStart/start/toolUse") {
        let call = tool_uses.entry(index).or_default();
        if let Some(id) = tool_use.get("toolUseId").and_then(Value::as_str) {
            call.id = id.to_string();
        }
        if let Some(name) = tool_use.get("name").and_then(Value::as_str) {
            call.name = name.to_string();
        }
    }
    if let Some(input) = payload
        .pointer("/contentBlockDelta/delta/toolUse/input")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    {
        tool_uses
            .entry(index)
            .or_default()
            .input_json
            .push_str(input);
    }
}

fn bedrock_stream_tool_calls(
    tool_uses: BTreeMap<usize, BedrockStreamToolUse>,
) -> AppResult<Vec<Value>> {
    tool_uses
        .into_values()
        .filter(|call| !call.name.trim().is_empty())
        .map(|call| {
            let arguments = if call.input_json.trim().is_empty() {
                json!({})
            } else {
                serde_json::from_str::<Value>(&call.input_json).map_err(|error| {
                    AppError::Llm(format!(
                        "invalid bedrock streamed tool arguments for {}: {error}; body: {}",
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

pub(super) fn is_bedrock_compatible(provider: &LlmProvider) -> bool {
    let provider_type = provider.provider_type.to_ascii_lowercase();
    let preset = provider
        .preset
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let base_url = provider_base_url(provider).to_ascii_lowercase();
    provider_type.contains("bedrock")
        || preset.contains("bedrock")
        || base_url.contains("bedrock-runtime.")
        || base_url.contains(".bedrock-runtime.")
}

pub(super) fn bedrock_converse_url(
    provider: &LlmProvider,
    model: &str,
    region: &str,
) -> AppResult<reqwest::Url> {
    let base_url = provider_base_url(provider);
    let base = base_url.trim().trim_end_matches('/');
    if base.is_empty() {
        return reqwest::Url::parse(&format!(
            "https://bedrock-runtime.{region}.amazonaws.com/model/{}/converse",
            aws_uri_encode_path_segment(bedrock_model_id(model))
        ))
        .map_err(|error| AppError::Llm(format!("invalid bedrock url: {error}")));
    }

    let mut url = reqwest::Url::parse(base)
        .map_err(|error| AppError::Llm(format!("invalid bedrock base URL: {error}")))?;
    if url.path().contains("/model/") && url.path().ends_with("/converse") {
        return Ok(url);
    }

    url.set_path(&format!(
        "{}/model/{}/converse",
        url.path().trim_end_matches('/'),
        aws_uri_encode_path_segment(bedrock_model_id(model))
    ));
    Ok(url)
}

fn bedrock_converse_stream_url(mut url: reqwest::Url) -> reqwest::Url {
    let path = url.path().trim_end_matches('/');
    if path.ends_with("/converse-stream") {
        return url;
    }
    if let Some(prefix) = path.strip_suffix("/converse") {
        url.set_path(&format!("{prefix}/converse-stream"));
    }
    url
}

pub(super) fn build_bedrock_messages(history: Vec<ChatMessage>) -> Vec<Value> {
    let messages = history
        .into_iter()
        .filter_map(|item| {
            if let Some(tool_replay) = tool_replay_message(&item) {
                return Some(vec![
                    json!({
                        "role": "assistant",
                        "content": [{
                            "toolUse": {
                                "toolUseId": tool_replay.call_id,
                                "name": tool_replay.name,
                                "input": tool_replay.arguments,
                            }
                        }]
                    }),
                    json!({
                        "role": "user",
                        "content": [{
                            "toolResult": {
                                "toolUseId": tool_replay.call_id,
                                "content": [{"text": tool_replay.content}],
                                "status": if tool_replay.ok { "success" } else { "error" },
                            }
                        }]
                    }),
                ]);
            }
            sanitized_wire_message(item, false).map(|message| {
                vec![json!({
                    "role": message.role,
                    "content": [{"text": message.content}]
                })]
            })
        })
        .flatten()
        .collect::<Vec<_>>();
    merge_adjacent_bedrock_messages(messages)
}

fn merge_adjacent_bedrock_messages(messages: Vec<Value>) -> Vec<Value> {
    let mut merged: Vec<Value> = Vec::new();
    for message in messages {
        let role = message.get("role").and_then(Value::as_str).unwrap_or("");
        let Some(text) = message
            .pointer("/content/0/text")
            .and_then(Value::as_str)
            .map(str::to_string)
        else {
            merged.push(message);
            continue;
        };
        if let Some(previous) = merged.last_mut() {
            if previous.get("role").and_then(Value::as_str) == Some(role) {
                let Some(previous_text) = previous
                    .pointer("/content/0/text")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                else {
                    merged.push(message);
                    continue;
                };
                let separator = if previous_text.trim().is_empty() || text.trim().is_empty() {
                    ""
                } else {
                    "\n\n"
                };
                previous["content"][0]["text"] = json!(format!("{previous_text}{separator}{text}"));
                continue;
            }
        }
        merged.push(message);
    }
    merged
}

pub(super) fn parse_bedrock_converse(payload: Value) -> AppResult<LlmReply> {
    let mut content = payload
        .pointer("/output/message/content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .filter(|text| !text.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    let tool_calls = bedrock_tool_calls(&payload);
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
            "bedrock response has no visible text or tool-call content: {payload}"
        )));
    }

    let prompt_tokens = payload
        .pointer("/usage/inputTokens")
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;
    let completion_tokens = payload
        .pointer("/usage/outputTokens")
        .and_then(Value::as_u64)
        .unwrap_or_else(|| estimate_tokens(&content) as u64) as usize;

    Ok(LlmReply {
        content,
        prompt_tokens,
        completion_tokens,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
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

fn bedrock_tool_calls(payload: &Value) -> Vec<Value> {
    payload
        .pointer("/output/message/content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|part| {
            let tool_use = part.get("toolUse")?;
            let name = tool_use
                .get("name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|name| !name.is_empty())?;
            let arguments = tool_use.get("input").cloned().unwrap_or_else(|| json!({}));
            Some(json!({
                "type": "function",
                "id": tool_use.get("toolUseId").cloned().unwrap_or(Value::Null),
                "function": {
                    "name": name,
                    "arguments": arguments
                }
            }))
        })
        .collect()
}

fn bedrock_sigv4_headers(
    url: &reqwest::Url,
    region: &str,
    credentials: &AwsCredentials,
    body_text: &str,
) -> AppResult<HeaderMap> {
    let host = url
        .host_str()
        .ok_or_else(|| AppError::Llm("bedrock URL is missing host".into()))?;
    let now = Utc::now();
    let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
    let short_date = now.format("%Y%m%d").to_string();
    let payload_hash = hex_sha256(body_text.as_bytes());

    let mut canonical_headers = vec![
        ("content-type", "application/json"),
        ("host", host),
        ("x-amz-content-sha256", payload_hash.as_str()),
        ("x-amz-date", amz_date.as_str()),
    ];
    if let Some(token) = credentials.session_token.as_deref() {
        canonical_headers.push(("x-amz-security-token", token));
    }
    canonical_headers.sort_by(|left, right| left.0.cmp(right.0));

    let canonical_headers_text = canonical_headers
        .iter()
        .map(|(name, value)| format!("{name}:{value}\n"))
        .collect::<String>();
    let signed_headers = canonical_headers
        .iter()
        .map(|(name, _)| *name)
        .collect::<Vec<_>>()
        .join(";");
    let canonical_query = url.query().unwrap_or("");
    let canonical_request = format!(
        "POST\n{}\n{}\n{}{}\n{}",
        url.path(),
        canonical_query,
        canonical_headers_text,
        signed_headers,
        payload_hash
    );

    let credential_scope = format!("{short_date}/{region}/bedrock/aws4_request");
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{credential_scope}\n{}",
        hex_sha256(canonical_request.as_bytes())
    );
    let signing_key = aws_signing_key(&credentials.secret_key, &short_date, region);
    let signature = hex::encode(hmac_sha256(&signing_key, string_to_sign.as_bytes()));
    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential={}/{}, SignedHeaders={}, Signature={}",
        credentials.access_key, credential_scope, signed_headers, signature
    );

    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    insert_header(&mut headers, "x-amz-date", &amz_date)?;
    insert_header(&mut headers, "x-amz-content-sha256", &payload_hash)?;
    insert_header(&mut headers, "Authorization", &authorization)?;
    if let Some(token) = credentials.session_token.as_deref() {
        insert_header(&mut headers, "x-amz-security-token", token)?;
    }
    Ok(headers)
}

fn bedrock_aws_credentials(provider: &LlmProvider) -> AppResult<AwsCredentials> {
    if let Some(value) = provider
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        if let Some(credentials) = credentials_from_compound(value, None) {
            return Ok(credentials);
        }
    }

    if !provider.api_key_env.trim().is_empty() {
        if let Ok(value) = std::env::var(provider.api_key_env.trim()) {
            if let Some(credentials) =
                credentials_from_compound(&value, Some(provider.api_key_env.trim()))
            {
                return Ok(credentials);
            }
        }
        let secret_env = format!("{}_SECRET", provider.api_key_env.trim());
        if let (Ok(access_key), Ok(secret_key)) = (
            std::env::var(provider.api_key_env.trim()),
            std::env::var(&secret_env),
        ) {
            let session_token =
                std::env::var(format!("{}_SESSION_TOKEN", provider.api_key_env.trim())).ok();
            return Ok(AwsCredentials {
                access_key,
                secret_key,
                session_token,
            });
        }
    }

    if let (Ok(access_key), Ok(secret_key)) = (
        std::env::var("AWS_ACCESS_KEY_ID"),
        std::env::var("AWS_SECRET_ACCESS_KEY"),
    ) {
        return Ok(AwsCredentials {
            access_key,
            secret_key,
            session_token: std::env::var("AWS_SESSION_TOKEN").ok(),
        });
    }

    if let Some(credentials) = credentials_from_aws_shared_credentials(&bedrock_aws_profile()) {
        return Ok(credentials);
    }

    Err(AppError::Llm(
        "Bedrock requires AWS_ACCESS_KEY_ID/AWS_SECRET_ACCESS_KEY or static credentials in AWS_PROFILE".into(),
    ))
}

fn credentials_from_compound(value: &str, env_prefix: Option<&str>) -> Option<AwsCredentials> {
    let mut parts = value.splitn(3, ':');
    let access_key = parts.next()?.trim();
    let secret_key = parts.next()?.trim();
    if access_key.is_empty() || secret_key.is_empty() {
        return None;
    }
    let token_from_value = parts
        .next()
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(str::to_string);
    let token_from_env = env_prefix
        .and_then(|prefix| std::env::var(format!("{prefix}_SESSION_TOKEN")).ok())
        .or_else(|| std::env::var("AWS_SESSION_TOKEN").ok());
    Some(AwsCredentials {
        access_key: access_key.to_string(),
        secret_key: secret_key.to_string(),
        session_token: token_from_value.or(token_from_env),
    })
}

fn bedrock_aws_profile() -> String {
    std::env::var("AWS_PROFILE")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "default".into())
}

fn bedrock_shared_credentials_path() -> Option<std::path::PathBuf> {
    std::env::var_os("AWS_SHARED_CREDENTIALS_FILE")
        .filter(|value| !value.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| home_dir().map(|dir| dir.join(".aws").join("credentials")))
}

fn credentials_from_aws_shared_credentials(profile: &str) -> Option<AwsCredentials> {
    let path = bedrock_shared_credentials_path()?;
    let text = std::fs::read_to_string(path).ok()?;
    credentials_from_aws_shared_credentials_text(profile, &text)
}

fn credentials_from_aws_shared_credentials_text(
    profile: &str,
    text: &str,
) -> Option<AwsCredentials> {
    let target = profile.trim();
    if target.is_empty() {
        return None;
    }
    let mut in_target = false;
    let mut access_key: Option<String> = None;
    let mut secret_key: Option<String> = None;
    let mut session_token: Option<String> = None;

    for raw_line in text.lines() {
        let mut line = raw_line;
        if let Some((before, _)) = line.split_once('#') {
            line = before;
        }
        if let Some((before, _)) = line.split_once(';') {
            line = before;
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            let section = line.trim_start_matches('[').trim_end_matches(']').trim();
            in_target = section == target;
            continue;
        }
        if !in_target {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        match key.trim().to_ascii_lowercase().as_str() {
            "aws_access_key_id" => access_key = Some(value.to_string()),
            "aws_secret_access_key" => secret_key = Some(value.to_string()),
            "aws_session_token" => session_token = Some(value.to_string()),
            _ => {}
        }
    }

    Some(AwsCredentials {
        access_key: access_key?,
        secret_key: secret_key?,
        session_token,
    })
}

fn bedrock_region(provider: &LlmProvider) -> String {
    std::env::var("AWS_REGION")
        .ok()
        .or_else(|| std::env::var("AWS_DEFAULT_REGION").ok())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| {
            let base_url = provider_base_url(provider);
            reqwest::Url::parse(&base_url)
                .ok()
                .and_then(|url| url.host_str().map(str::to_string))
                .and_then(|host| {
                    let parts = host.split('.').collect::<Vec<_>>();
                    parts
                        .iter()
                        .position(|part| *part == "bedrock-runtime")
                        .and_then(|idx| parts.get(idx + 1))
                        .map(|part| part.to_string())
                })
                .unwrap_or_else(|| "us-east-1".into())
        })
}

fn bedrock_model_id(model: &str) -> &str {
    let value = model.trim();
    value.strip_prefix("models/").unwrap_or(value)
}

fn aws_signing_key(secret_key: &str, short_date: &str, region: &str) -> Vec<u8> {
    let k_date = hmac_sha256(
        format!("AWS4{secret_key}").as_bytes(),
        short_date.as_bytes(),
    );
    let k_region = hmac_sha256(&k_date, region.as_bytes());
    let k_service = hmac_sha256(&k_region, b"bedrock");
    hmac_sha256(&k_service, b"aws4_request")
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts keys of any size");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

fn hex_sha256(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}

fn insert_header(headers: &mut HeaderMap, name: &'static str, value: &str) -> AppResult<()> {
    headers.insert(
        name,
        HeaderValue::from_str(value)
            .map_err(|error| AppError::Llm(format!("invalid bedrock header {name}: {error}")))?,
    );
    Ok(())
}

fn aws_uri_encode_path_segment(value: &str) -> String {
    value
        .bytes()
        .flat_map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                vec![byte as char]
            }
            _ => format!("%{byte:02X}").chars().collect(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bedrock_stream_tool_calls_reconstruct_tool_use_input() {
        let mut tool_uses = BTreeMap::<usize, BedrockStreamToolUse>::new();
        track_bedrock_stream_tool_use(
            &json!({
                "contentBlockIndex": 0,
                "contentBlockStart": {
                    "start": {
                        "toolUse": {
                            "toolUseId": "tooluse_123",
                            "name": "terminal"
                        }
                    }
                }
            }),
            &mut tool_uses,
        );
        track_bedrock_stream_tool_use(
            &json!({
                "contentBlockIndex": 0,
                "contentBlockDelta": {
                    "delta": {
                        "toolUse": {
                            "input": "{\"command\":\"pwd\"}"
                        }
                    }
                }
            }),
            &mut tool_uses,
        );

        let calls = bedrock_stream_tool_calls(tool_uses).unwrap();
        assert_eq!(calls[0]["id"], "tooluse_123");
        assert_eq!(calls[0]["function"]["name"], "terminal");
        assert_eq!(calls[0]["function"]["arguments"], json!({"command": "pwd"}));
    }

    #[test]
    fn bedrock_shared_credentials_parser_reads_selected_profile() {
        let text = r#"
            [default]
            aws_access_key_id = default-key
            aws_secret_access_key = default-secret

            [prod]
            aws_access_key_id = prod-key # trailing comment
            aws_secret_access_key = prod-secret ; another comment
            aws_session_token = prod-token
        "#;

        let credentials = credentials_from_aws_shared_credentials_text("prod", text).unwrap();

        assert_eq!(credentials.access_key, "prod-key");
        assert_eq!(credentials.secret_key, "prod-secret");
        assert_eq!(credentials.session_token.as_deref(), Some("prod-token"));
        assert!(credentials_from_aws_shared_credentials_text("missing", text).is_none());
    }
}
