use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use crate::{error::AppError, models::LlmProvider};

static LLM_RETRY_JITTER_COUNTER: AtomicU64 = AtomicU64::new(0);

pub(super) fn classify_llm_failure(error: &AppError) -> &'static str {
    let text = error.to_string().to_ascii_lowercase();
    if text_contains_any(
        &text,
        &[
            "no endpoints available matching your guardrail",
            "no endpoints available matching your data policy",
            "no endpoints found matching your data policy",
        ],
    ) {
        "provider_policy_blocked"
    } else if text_contains_any(
        &text,
        &[
            "flagged for possible cybersecurity risk",
            "trusted access for cyber",
            "violates our usage policies",
            "violates openai's usage policies",
            "your request was flagged by",
            "prompt was flagged by our safety",
            "responses cannot be generated due to safety",
            "content_filter",
            "responsibleaipolicyviolation",
        ],
    ) {
        "content_policy_blocked"
    } else if text_contains_any(
        &text,
        &[
            "invalid_encrypted_content",
            "encrypted content",
            "replay blob",
        ],
    ) {
        "invalid_encrypted_content"
    } else if text.contains("content[].thinking")
        || (text.contains("thinking")
            && (text.contains("must be passed back") || text.contains("passed back to the api")))
    {
        "thinking_replay_missing"
    } else if text.contains("thinking") && text.contains("signature") {
        "thinking_signature"
    } else if text_contains_any(
        &text,
        &[
            "image exceeds",
            "image too large",
            "image_too_large",
            "image size exceeds",
        ],
    ) {
        "image_too_large"
    } else if text_contains_any(
        &text,
        &[
            "out of extra usage",
            "extra usage",
            "long context tier",
            "long context is not enabled",
        ],
    ) {
        "long_context_tier"
    } else if text_contains_any(
        &text,
        &[
            "oauth long context",
            "1m context beta",
            "1m-context",
            "context-1m",
        ],
    ) && (text.contains("forbidden")
        || text.contains("not allowed")
        || text.contains("unauthorized"))
    {
        "oauth_long_context_beta_forbidden"
    } else if text.contains("grammar")
        && (text.contains("pattern") || text.contains("format"))
        && (text.contains("llama") || text.contains("json schema") || text.contains("schema"))
    {
        "llama_cpp_grammar_pattern"
    } else if text_contains_any(
        &text,
        &[
            "text is not set",
            "tool message content must be a string",
            "tool content must be a string",
            "tool message must be a string",
            "expected string, got list",
            "expected string, got array",
            "tool_call.content must be string",
        ],
    ) {
        "multimodal_tool_content_unsupported"
    } else if text_contains_any(
        &text,
        &[
            "assistant message with tool_calls must be followed by tool messages",
            "tool_calls must be followed by tool messages",
            "tool_use ids were found without tool_result",
            "tool_result without tool_use",
            "tool result without a corresponding tool use",
            "no tool output found for function call",
            "messages with role tool must be a response",
            "tool_call_id does not match",
            "invalid tool_call_id",
            "missing tool response",
            "missing tool_result",
        ],
    ) {
        "tool_replay_orphan"
    } else if text.contains("413")
        || text_contains_any(
            &text,
            &[
                "request entity too large",
                "payload too large",
                "error code: 413",
            ],
        )
    {
        "payload_too_large"
    } else if text_contains_any(
        &text,
        &[
            "context length",
            "context size",
            "maximum context",
            "token limit",
            "too many tokens",
            "reduce the length",
            "exceeds the limit",
            "context window",
            "prompt is too long",
            "prompt exceeds max length",
            "prompt length",
            "input is too long",
            "exceeds the max_model_len",
            "max_model_len",
            "maximum number of tokens",
            "maximum model length",
            "context length exceeded",
            "slot context",
            "n_ctx_slot",
            "超过最大长度",
            "上下文长度",
            "max input token",
            "input token",
            "exceeds the maximum number of input tokens",
        ],
    ) {
        "context_overflow"
    } else if text.contains("429")
        || text.contains("rate limit")
        || text.contains("ratelimit")
        || text_contains_any(
            &text,
            &[
                "too many requests",
                "throttled",
                "requests per minute",
                "tokens per minute",
                "requests per day",
                "try again in",
                "please retry after",
                "resource_exhausted",
                "rate increased too quickly",
                "throttlingexception",
                "too many concurrent requests",
                "servicequotaexceededexception",
            ],
        )
    {
        "rate_limit"
    } else if text_contains_any(
        &text,
        &[
            "insufficient credits",
            "insufficient_quota",
            "insufficient balance",
            "credit balance",
            "credits exhausted",
            "credits have been exhausted",
            "no usable credits",
            "top up your credits",
            "payment required",
            "billing hard limit",
            "exceeded your current quota",
            "account is deactivated",
            "plan does not include",
            "out of funds",
            "run out of funds",
            "balance_depleted",
            "model_not_supported_on_free_tier",
            "not available on the free tier",
        ],
    ) {
        "quota"
    } else if terminal_auth_failure_message(&text) {
        "terminal_auth"
    } else if text.contains("401")
        || text.contains("403")
        || text.contains("unauthorized")
        || text.contains("forbidden")
        || text.contains("invalid api key")
        || text.contains("authentication")
    {
        "auth"
    } else if text.contains("402") || text.contains("quota") || text.contains("billing") {
        if text_contains_any(
            &text,
            &[
                "try again",
                "retry",
                "resets at",
                "reset in",
                "wait",
                "requests remaining",
                "periodic",
                "window",
            ],
        ) {
            "rate_limit"
        } else {
            "quota"
        }
    } else if text.contains("404")
        || text_contains_any(
            &text,
            &[
                "is not a valid model",
                "invalid model",
                "model not found",
                "model_not_found",
                "does not exist",
                "no such model",
                "unknown model",
                "unsupported model",
            ],
        )
    {
        "model_not_found"
    } else if text_contains_any(
        &text,
        &[
            "unknown parameter",
            "unsupported parameter",
            "unrecognized request argument",
            "invalid_request_error",
            "unknown_parameter",
            "unsupported_parameter",
            "invalid request",
            "invalid_request",
        ],
    ) {
        "format_error"
    } else if text.contains("timeout") || text.contains("timed out") {
        "timeout"
    } else if text.contains("503")
        || text.contains("529")
        || text.contains("overload")
        || text.contains("overloaded")
    {
        "overloaded"
    } else if text.contains("500")
        || text.contains("502")
        || text.contains("server error")
        || text.contains("bad gateway")
        || text.contains("service unavailable")
    {
        "server_error"
    } else if text.contains("error sending request")
        || text.contains("connection")
        || text.contains("network")
        || text.contains("dns")
        || text.contains("tls")
        || text.contains("ssl")
        || text.contains("peer closed")
        || text.contains("connection reset")
        || text.contains("connection closed")
        || text.contains("error decoding response body")
        || text.contains("decode response body")
        || text.contains("failed to read llm response")
        || text.contains("invalid llm response")
        || text.contains("invalid recovered llm response")
        || text.contains("invalid anthropic response")
        || text.contains("invalid gemini response")
        || text.contains("invalid responses llm response")
        || text.contains("invalid bedrock response")
    {
        "transport"
    } else if text.contains("empty") || text.contains("missing assistant content") {
        "empty_response"
    } else {
        "error"
    }
}

pub(super) fn llm_failure_is_retryable(kind: &str, message: &str) -> bool {
    if matches!(
        kind,
        "terminal_auth"
            | "auth"
            | "quota"
            | "context_overflow"
            | "payload_too_large"
            | "model_not_found"
            | "provider_policy_blocked"
            | "content_policy_blocked"
            | "format_error"
            | "invalid_encrypted_content"
            | "multimodal_tool_content_unsupported"
            | "thinking_signature"
            | "thinking_replay_missing"
            | "image_too_large"
            | "long_context_tier"
            | "oauth_long_context_beta_forbidden"
            | "llama_cpp_grammar_pattern"
    ) {
        return false;
    }
    let text = message.to_ascii_lowercase();
    if text.contains("400")
        || text.contains("404")
        || text.contains("invalid request")
        || text.contains("invalid_request")
        || text.contains("model not found")
        || text.contains("not found")
        || text.contains("unsupported")
        || text.contains("content policy")
        || text.contains("safety")
    {
        return false;
    }
    matches!(
        kind,
        "rate_limit"
            | "timeout"
            | "overloaded"
            | "server_error"
            | "transport"
            | "empty_response"
            | "error"
    )
}

pub(super) fn llm_failure_recovery_hints(kind: &str, message: &str) -> Value {
    let retryable = llm_failure_is_retryable(kind, message);
    let should_compress = matches!(
        kind,
        "context_overflow" | "payload_too_large" | "long_context_tier"
    );
    let should_strip_reasoning = matches!(kind, "invalid_encrypted_content" | "thinking_signature");
    let should_strip_image_payloads = matches!(
        kind,
        "image_too_large" | "multimodal_tool_content_unsupported"
    );
    let should_repair_tool_replay =
        matches!(kind, "tool_replay_orphan" | "thinking_replay_missing");
    let should_rotate_credential =
        matches!(kind, "rate_limit" | "quota" | "auth" | "terminal_auth");
    let should_fallback = matches!(
        kind,
        "quota"
            | "auth"
            | "terminal_auth"
            | "model_not_found"
            | "provider_policy_blocked"
            | "oauth_long_context_beta_forbidden"
            | "long_context_tier"
    );
    let action = if should_compress {
        "compact_context"
    } else if should_strip_reasoning {
        "strip_reasoning_replay"
    } else if should_strip_image_payloads {
        "strip_image_payloads"
    } else if should_repair_tool_replay {
        "repair_tool_replay"
    } else if kind == "rate_limit" {
        "backoff_or_rotate_credential"
    } else if should_fallback {
        "fallback_provider_or_model"
    } else if retryable {
        "retry_with_backoff"
    } else {
        "abort_or_adjust_request"
    };
    let hints = match kind {
        "context_overflow" => vec![
            "Compact older conversation history before retrying.",
            "Keep the latest user request in the protected tail.",
        ],
        "payload_too_large" => vec![
            "Strip or compact oversized media/tool payloads before retrying.",
            "Prefer artifact/file references over inline large content.",
        ],
        "long_context_tier" => {
            vec!["Compact context or switch to a provider/model with long-context access."]
        }
        "invalid_encrypted_content" | "thinking_signature" => {
            vec!["Remove stale reasoning replay/signature metadata and retry once."]
        }
        "thinking_replay_missing" => {
            vec!["Downgrade historical tool replay that cannot be paired with signed thinking blocks, then retry once."]
        }
        "multimodal_tool_content_unsupported" => {
            vec!["Downgrade historical multimodal tool messages to plain text."]
        }
        "image_too_large" => {
            vec!["Replace historical inline image payloads with text placeholders."]
        }
        "tool_replay_orphan" => {
            vec!["Repair historical tool replay by converting orphaned tool results to plain text."]
        }
        "rate_limit" => vec![
            "Back off before retrying.",
            "Rotate credential/provider when available.",
        ],
        "quota" => vec!["Switch credential/provider or top up billing before retrying."],
        "auth" | "terminal_auth" => {
            vec!["Refresh credentials or switch to another configured provider."]
        }
        "model_not_found" => vec!["Switch to an available model for this provider."],
        "provider_policy_blocked" => {
            vec!["Change provider routing or account data/privacy policy settings."]
        }
        "content_policy_blocked" => {
            vec!["Do not retry unchanged; revise the request or tool plan."]
        }
        "format_error" | "llama_cpp_grammar_pattern" => {
            vec!["Adjust request/schema formatting before retrying."]
        }
        "timeout" | "overloaded" | "server_error" | "transport" | "empty_response" => {
            vec!["Retry with backoff; fail over if retries are exhausted."]
        }
        _ => vec!["Retry if configured retries remain; otherwise fail over."],
    };
    json!({
        "action": action,
        "retryable": retryable,
        "shouldCompress": should_compress,
        "shouldRotateCredential": should_rotate_credential,
        "shouldFallback": should_fallback,
        "shouldStripReasoningReplay": should_strip_reasoning,
        "shouldStripImagePayloads": should_strip_image_payloads,
        "shouldRepairToolReplay": should_repair_tool_replay,
        "hints": hints,
    })
}

pub(super) fn llm_classified_error_detail(
    kind: &str,
    message: &str,
    provider: Option<&str>,
    model: Option<&str>,
) -> Value {
    let mut detail = json!({
        "reason": kind,
        "message": message,
        "recovery": llm_failure_recovery_hints(kind, message),
    });
    if let Some(status_code) = llm_failure_status_code(message) {
        detail["statusCode"] = json!(status_code);
    }
    if let Some(provider) = provider.filter(|value| !value.trim().is_empty()) {
        detail["provider"] = json!(provider);
    }
    if let Some(model) = model.filter(|value| !value.trim().is_empty()) {
        detail["model"] = json!(model);
    }
    detail
}

fn llm_failure_status_code(message: &str) -> Option<u16> {
    message
        .split(|ch: char| !ch.is_ascii_digit())
        .filter(|part| part.len() == 3)
        .filter_map(|part| part.parse::<u16>().ok())
        .find(|code| (400..=599).contains(code))
}

pub(super) fn llm_credential_variant_should_skip_retry(provider: &LlmProvider, kind: &str) -> bool {
    provider.id.contains(":cred-")
        && matches!(
            kind,
            "rate_limit"
                | "terminal_auth"
                | "auth"
                | "quota"
                | "long_context_tier"
                | "oauth_long_context_beta_forbidden"
        )
}

fn terminal_auth_failure_message(text: &str) -> bool {
    (text.contains("401") || text.contains("oauth") || text.contains("bearer"))
        && text_contains_any(
            text,
            &[
                "token_invalidated",
                "token invalidated",
                "token_revoked",
                "token revoked",
                "invalid_token",
                "invalid token",
                "invalid_grant",
                "invalid grant",
                "unauthorized_client",
                "unauthorized client",
                "refresh_token_reused",
                "refresh token reused",
            ],
        )
}

fn text_contains_any(text: &str, patterns: &[&str]) -> bool {
    patterns.iter().any(|pattern| text.contains(pattern))
}

pub(super) fn llm_retry_delay_ms(base_ms: usize, attempt: usize, kind: &str) -> u64 {
    let base = base_ms.max(100) as u64;
    let exponent = attempt.saturating_sub(1).min(10);
    let delay = base.saturating_mul(1u64 << exponent).min(60_000);
    let kind_floor = if kind == "rate_limit" { 1_500 } else { 0 };
    let delay = delay.max(kind_floor).min(60_000);
    let jitter_window = (delay / 2).max(1);
    delay
        .saturating_add(llm_retry_jitter_ms(jitter_window, attempt, kind))
        .min(60_000)
}

fn llm_retry_jitter_ms(window_ms: u64, attempt: usize, kind: &str) -> u64 {
    if window_ms <= 1 {
        return 0;
    }
    let tick = LLM_RETRY_JITTER_COUNTER.fetch_add(1, Ordering::Relaxed) + 1;
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos() as u64)
        .unwrap_or(0);
    let mut seed = nanos ^ tick.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    seed ^= (attempt as u64).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    for byte in kind.as_bytes() {
        seed = seed.rotate_left(5) ^ (*byte as u64);
    }
    seed ^= seed >> 30;
    seed = seed.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    seed ^= seed >> 27;
    seed = seed.wrapping_mul(0x94D0_49BB_1331_11EB);
    seed ^= seed >> 31;
    seed % window_ms
}

pub(super) fn format_rate_limit_usage(state: &Value) -> String {
    let provider = state
        .get("provider")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .unwrap_or("provider");
    let captured = state
        .get("capturedAt")
        .and_then(Value::as_str)
        .unwrap_or("unknown time");
    let mut lines = vec![format!("{provider} captured at {captured}")];
    for (label, key) in [
        ("RPM", "requestsMin"),
        ("RPH", "requestsHour"),
        ("TPM", "tokensMin"),
        ("TPH", "tokensHour"),
    ] {
        let Some(bucket) = state.get(key) else {
            continue;
        };
        let limit = bucket.get("limit").and_then(Value::as_u64).unwrap_or(0);
        if limit == 0 {
            continue;
        }
        let remaining = bucket.get("remaining").and_then(Value::as_u64).unwrap_or(0);
        let reset = bucket
            .get("resetSeconds")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        lines.push(format!(
            "{label}: {remaining}/{limit} remaining, reset in {reset:.0}s"
        ));
    }
    lines.join("\n")
}

pub(super) fn genuine_rate_limit_guard_state(state: Option<&Value>) -> Value {
    let Some(state) = state else {
        return json!({
            "hasRateLimitState": false,
            "genuineAccountLimit": false,
            "reason": "missing_rate_limit_state"
        });
    };
    let exhausted = rate_limit_exhausted_buckets(state);
    let meaningful = exhausted
        .iter()
        .filter(|bucket| bucket.reset_seconds >= 60.0)
        .collect::<Vec<_>>();
    json!({
        "hasRateLimitState": true,
        "provider": state.get("provider").cloned().unwrap_or(Value::Null),
        "genuineAccountLimit": !meaningful.is_empty(),
        "reason": if meaningful.is_empty() { "no_exhausted_bucket_with_meaningful_reset" } else { "exhausted_bucket_with_meaningful_reset" },
        "minResetSecondsForBreaker": 60.0,
        "exhaustedBuckets": exhausted.iter().map(|bucket| {
            json!({
                "name": bucket.name,
                "remaining": bucket.remaining,
                "resetSeconds": bucket.reset_seconds
            })
        }).collect::<Vec<_>>()
    })
}

struct ExhaustedRateLimitBucket<'a> {
    name: &'a str,
    remaining: u64,
    reset_seconds: f64,
}

fn rate_limit_exhausted_buckets(state: &Value) -> Vec<ExhaustedRateLimitBucket<'static>> {
    [
        ("requestsMin", "requests_min"),
        ("requestsHour", "requests_hour"),
        ("tokensMin", "tokens_min"),
        ("tokensHour", "tokens_hour"),
    ]
    .into_iter()
    .filter_map(|(camel, snake)| {
        let bucket = state.get(camel).or_else(|| state.get(snake))?;
        let limit = bucket.get("limit").and_then(Value::as_u64).unwrap_or(0);
        let remaining = bucket.get("remaining").and_then(Value::as_u64).unwrap_or(0);
        let reset_seconds = bucket
            .get("resetSeconds")
            .or_else(|| bucket.get("reset_seconds"))
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        if limit > 0 && remaining == 0 {
            Some(ExhaustedRateLimitBucket {
                name: camel,
                remaining,
                reset_seconds,
            })
        } else {
            None
        }
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{genuine_rate_limit_guard_state, llm_retry_delay_ms};

    #[test]
    fn llm_retry_delay_uses_jittered_exponential_range() {
        let delay = llm_retry_delay_ms(800, 2, "transport");
        assert!(
            (1_600..=2_400).contains(&delay),
            "delay should be base exponential delay plus <=50% jitter, got {delay}"
        );
    }

    #[test]
    fn llm_retry_delay_rate_limit_keeps_minimum_floor() {
        let delay = llm_retry_delay_ms(100, 1, "rate_limit");
        assert!(
            (1_500..=2_250).contains(&delay),
            "rate-limit delay should keep Hermes-style floor plus jitter, got {delay}"
        );
    }

    #[test]
    fn llm_retry_delay_caps_at_sixty_seconds() {
        for attempt in [8, 16, 64] {
            let delay = llm_retry_delay_ms(10_000, attempt, "server_error");
            assert!(delay <= 60_000, "attempt {attempt} delay was {delay}");
        }
    }

    #[test]
    fn genuine_rate_limit_guard_requires_exhausted_meaningful_bucket() {
        let state = json!({
            "provider": "nous",
            "requestsHour": {"limit": 100, "remaining": 0, "resetSeconds": 3600.0},
            "requestsMin": {"limit": 10, "remaining": 8, "resetSeconds": 20.0}
        });
        let guard = genuine_rate_limit_guard_state(Some(&state));
        assert_eq!(guard["genuineAccountLimit"], true);
        assert_eq!(guard["exhaustedBuckets"][0]["name"], "requestsHour");

        let transient = json!({
            "provider": "nous",
            "requestsMin": {"limit": 10, "remaining": 0, "resetSeconds": 15.0}
        });
        let guard = genuine_rate_limit_guard_state(Some(&transient));
        assert_eq!(guard["genuineAccountLimit"], false);

        let missing = genuine_rate_limit_guard_state(None);
        assert_eq!(missing["hasRateLimitState"], false);
    }
}
