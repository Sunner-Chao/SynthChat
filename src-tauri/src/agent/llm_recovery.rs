use std::{
    collections::{HashMap, HashSet},
    fs,
    future::Future,
    path::PathBuf,
    time::{Duration, Instant},
};

use chrono::{DateTime, Utc};
use serde_json::{json, Map, Value};

use crate::{
    error::{AppError, AppResult},
    hermes_auth::{
        mark_hermes_credential_pool_failure, mark_hermes_credential_pool_failure_for_source,
    },
    model_catalog,
    models::{
        new_id, now_iso, AgentCheckpointRecord, AgentRunRecord, ChatConfig, ChatMessage,
        LlmProvider, Persona, ShortContextState, ToolDefinition,
    },
    store::AppStore,
};

use super::context_compression::{
    compression_anti_thrash_skip_note, context_engine_messages_to_summary,
    record_compression_effectiveness, record_summary_success, selected_context_engine_name,
};
use super::llm_failure::{llm_classified_error_detail, llm_failure_recovery_hints};
use super::shell_hooks::{run_post_api_request_hooks, run_pre_api_request_hooks};
use super::{
    append_parent_phase_event, classify_llm_failure, contains_any, estimate_tokens,
    fallback_short_context_summary, genuine_rate_limit_guard_state,
    llm_credential_variant_should_skip_retry, llm_failure_is_retryable, llm_retry_delay_ms,
    memory_pre_compress_context, redact_json_value, redact_sensitive_text,
    render_messages_for_summary, run_context_engine_compress, run_context_engine_should_compress,
    run_context_engine_update_from_response, run_context_engine_update_model,
    spawn_session_finished_hooks, truncate_for_prompt,
};

fn llm_failure_kind_should_rotate_credential(kind: &str) -> bool {
    matches!(
        kind,
        "rate_limit"
            | "terminal_auth"
            | "auth"
            | "quota"
            | "long_context_tier"
            | "oauth_long_context_beta_forbidden"
    )
}

fn persona_with_max_tokens_override(
    persona: &Persona,
    max_tokens_override: Option<u32>,
) -> Persona {
    let mut persona = persona.clone();
    if let Some(max_tokens) = max_tokens_override {
        persona.max_tokens = max_tokens.max(1);
    }
    persona
}

fn provider_base_id(provider_id: &str) -> &str {
    provider_id
        .split_once(":cred-")
        .map(|(base, _)| base)
        .unwrap_or(provider_id)
}

fn provider_ids_match(left: &str, right: &str) -> bool {
    provider_base_id(left.trim()) == provider_base_id(right.trim())
}

fn persona_for_provider_attempt(
    persona: &Persona,
    provider: &LlmProvider,
    max_tokens_override: Option<u32>,
) -> Persona {
    let mut attempt_persona = persona_with_max_tokens_override(persona, max_tokens_override);
    let requested_provider = attempt_persona.llm_provider.trim();
    if !requested_provider.is_empty() && !provider_ids_match(requested_provider, &provider.id) {
        attempt_persona.llm_provider = provider_base_id(&provider.id).to_string();
        attempt_persona.llm_model.clear();
    }
    attempt_persona
}

pub(super) fn next_max_tokens_override(
    current_override: Option<u32>,
    configured_max_tokens: u32,
    message: &str,
) -> Option<u32> {
    let available = parse_available_output_tokens_from_error(message)?;
    let available = available.min(u32::MAX as usize) as u32;
    let ceiling = current_override.unwrap_or(configured_max_tokens).max(1);
    Some(available.max(1).min(ceiling))
}

pub(super) async fn wait_llm_retry_delay_interruptible(
    store: &AppStore,
    run_id: Option<&str>,
    delay_ms: u64,
) -> AppResult<bool> {
    let Some(run_id) = run_id else {
        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
        return Ok(true);
    };
    let started = Instant::now();
    let total = Duration::from_millis(delay_ms);
    loop {
        let state = store.agent_run(run_id)?.state;
        if matches!(state.as_str(), "completed" | "failed" | "aborted") {
            return Ok(false);
        }
        let elapsed = started.elapsed();
        if elapsed >= total {
            return Ok(true);
        }
        tokio::time::sleep((total - elapsed).min(Duration::from_millis(250))).await;
    }
}

pub(super) fn record_llm_usage(store: &AppStore, reply: &crate::llm::LlmReply) -> AppResult<()> {
    store.add_usage_detail(json!({
        "providerId": reply.provider_id.clone().unwrap_or_else(|| "unknown".into()),
        "providerType": reply.provider_type.clone().unwrap_or_default(),
        "model": reply.model.clone().unwrap_or_else(|| "unknown".into()),
        "baseUrl": reply.base_url.clone(),
        "promptTokens": reply.prompt_tokens,
        "completionTokens": reply.completion_tokens,
        "cacheReadTokens": reply.cache_read_tokens,
        "cacheWriteTokens": reply.cache_write_tokens,
        "reasoningTokens": reply.reasoning_tokens,
        "estimatedCostUsd": reply.estimated_cost_usd,
        "costStatus": reply.cost_status.clone(),
        "costSource": reply.cost_source.clone(),
        "rateLimitState": reply.rate_limit_state.clone(),
    }))
}

pub(super) fn recover_llm_failure_for_agent_run(
    store: &AppStore,
    run_id: &str,
    conversation_id: &str,
    history: &mut Vec<ChatMessage>,
    short_context: &mut ShortContextState,
    error: &AppError,
    attempted: &mut HashSet<String>,
    token_budget: usize,
) -> AppResult<Option<String>> {
    let kind = classify_llm_failure(error);
    if !attempted.insert(kind.to_string()) {
        return Ok(None);
    }

    let recovery = match kind {
        "context_overflow" | "payload_too_large" | "long_context_tier" => {
            recover_context_overflow_for_retry(
                store,
                run_id,
                conversation_id,
                history,
                short_context,
                token_budget,
                kind,
                error,
            )?
        }
        "image_too_large" | "multimodal_tool_content_unsupported" => {
            recover_image_payloads_for_retry_persisted(store, conversation_id, history, kind)?
        }
        "thinking_signature" => recover_reasoning_replay_text_for_retry(history, kind)?,
        "thinking_replay_missing" => recover_tool_replay_history_for_retry(history, kind)?,
        "invalid_encrypted_content" => recover_invalid_encrypted_content_for_retry(
            store,
            conversation_id,
            history,
            kind,
        )?,
        "tool_replay_orphan" => recover_tool_replay_history_for_retry(history, kind)?,
        "llama_cpp_grammar_pattern" => Some(
            "llama.cpp schema grammar recovery noted; SynthChat sends tools as prompt text in this path, so the request will be retried once with existing sanitized prompt text.".into(),
        ),
        _ => None,
    };

    if let Some(note) = recovery.as_ref() {
        let context_recovery =
            llm_context_error_recovery_detail(kind, &error.to_string(), token_budget);
        append_parent_phase_event(
            store,
            run_id,
            "llm_recovery",
            json!({
                "kind": kind,
                "message": error.to_string(),
                "note": note,
                "recoveryHints": llm_failure_recovery_hints(kind, &error.to_string()),
                "classifiedError": llm_classified_error_detail(kind, &error.to_string(), None, None),
                "contextRecovery": context_recovery,
            }),
        )?;
    }
    Ok(recovery)
}

fn recover_invalid_encrypted_content_for_retry(
    store: &AppStore,
    conversation_id: &str,
    history: &mut [ChatMessage],
    kind: &str,
) -> AppResult<Option<String>> {
    let in_memory = recover_reasoning_replay_text_for_retry(history, kind)?;
    let mut persisted = store.messages(conversation_id, None)?;
    let persisted_before = persisted.clone();
    let persisted_note = recover_reasoning_replay_text_for_retry(&mut persisted, kind)?;
    if persisted_note.is_some() {
        persist_recovered_messages_for_retry(
            store,
            conversation_id,
            &persisted_before,
            &persisted,
        )?;
    }
    Ok(combine_recovery_notes(in_memory, persisted_note))
}

fn recover_image_payloads_for_retry_persisted(
    store: &AppStore,
    conversation_id: &str,
    history: &mut [ChatMessage],
    kind: &str,
) -> AppResult<Option<String>> {
    let in_memory = recover_image_payloads_for_retry(history, kind)?;
    let mut persisted = store.messages(conversation_id, None)?;
    let persisted_before = persisted.clone();
    let persisted_note = recover_image_payloads_for_retry(&mut persisted, kind)?;
    if persisted_note.is_some() {
        persist_recovered_messages_for_retry(
            store,
            conversation_id,
            &persisted_before,
            &persisted,
        )?;
    }
    Ok(combine_recovery_notes(in_memory, persisted_note))
}

pub(super) fn persist_recovered_messages_for_retry(
    store: &AppStore,
    conversation_id: &str,
    before: &[ChatMessage],
    after: &[ChatMessage],
) -> AppResult<()> {
    let changed = changed_messages_for_retry_persist(before, after);
    if changed.is_empty() {
        return Ok(());
    }
    store.merge_conversation_messages_by_id(conversation_id, &changed)?;
    Ok(())
}

fn changed_messages_for_retry_persist(before: &[ChatMessage], after: &[ChatMessage]) -> Vec<ChatMessage> {
    let before_by_id = before
        .iter()
        .map(|message| (message.id.as_str(), message))
        .collect::<HashMap<_, _>>();
    after
        .iter()
        .filter(|message| {
            before_by_id
                .get(message.id.as_str())
                .is_some_and(|original| !chat_message_persisted_eq(original, message))
        })
        .cloned()
        .collect()
}

fn chat_message_persisted_eq(left: &ChatMessage, right: &ChatMessage) -> bool {
    left.id == right.id
        && left.conversation_id == right.conversation_id
        && left.role == right.role
        && left.content == right.content
        && left.created_at == right.created_at
        && left.source == right.source
        && left.account_id == right.account_id
        && left.provider_data == right.provider_data
}

fn combine_recovery_notes(left: Option<String>, right: Option<String>) -> Option<String> {
    match (left, right) {
        (Some(left), Some(right)) if left == right => Some(left),
        (Some(left), Some(right)) => Some(format!("{left} Persisted cleanup: {right}")),
        (Some(left), None) => Some(left),
        (None, Some(right)) => Some(format!("Persisted cleanup: {right}")),
        (None, None) => None,
    }
}

fn recover_llm_failure_for_provider_retry(
    store: &AppStore,
    run_id: Option<&str>,
    history: &mut [ChatMessage],
    error: &AppError,
    attempted: &mut HashSet<String>,
) -> AppResult<Option<String>> {
    let kind = classify_llm_failure(error);
    if !matches!(
        kind,
        "thinking_replay_missing" | "thinking_signature" | "tool_replay_orphan"
    ) {
        return Ok(None);
    }
    if !attempted.insert(kind.to_string()) {
        return Ok(None);
    }

    let recovery = match kind {
        "thinking_signature" => recover_reasoning_replay_text_for_retry(history, kind)?,
        "thinking_replay_missing" | "tool_replay_orphan" => {
            recover_tool_replay_history_for_retry(history, kind)?
        }
        _ => None,
    };

    if let (Some(run_id), Some(note)) = (run_id, recovery.as_ref()) {
        append_parent_phase_event(
            store,
            run_id,
            "llm_recovery",
            json!({
                "kind": kind,
                "message": error.to_string(),
                "note": note,
                "recoveryHints": llm_failure_recovery_hints(kind, &error.to_string()),
                "classifiedError": llm_classified_error_detail(kind, &error.to_string(), None, None),
            }),
        )?;
    }

    Ok(recovery)
}

pub(super) fn preflight_compact_context_for_agent_run(
    store: &AppStore,
    run_id: &str,
    conversation_id: &str,
    history: &mut Vec<ChatMessage>,
    short_context: &mut ShortContextState,
    chat_config: &ChatConfig,
) -> AppResult<Option<String>> {
    let mode = chat_config.short_context_mode.trim().to_ascii_lowercase();
    if matches!(mode.as_str(), "off" | "none" | "disabled" | "false") {
        return Ok(None);
    }
    let messages = store.messages(conversation_id, None)?;
    let keep_messages = (chat_config.max_context_rounds.max(1) * 2 + 1).clamp(3, 60);
    let rough_tokens = estimate_tokens(&format!(
        "{}\n{}",
        short_context.summary,
        render_messages_for_summary(history)
    ));
    if should_defer_preflight_to_real_usage(
        short_context,
        rough_tokens,
        chat_config.short_context_token_budget,
    ) {
        *short_context = store.save_short_context(short_context.clone())?;
        append_parent_phase_event(
            store,
            run_id,
            "llm_preflight_compaction_skipped",
            json!({
                "reason": "real_usage_defer",
                "roughTokens": rough_tokens,
                "thresholdTokens": chat_config.short_context_token_budget,
                "lastRealPromptTokens": short_context.last_real_prompt_tokens,
                "lastCompressionRoughTokens": short_context.last_compression_rough_tokens,
                "lastRoughTokensWhenRealPromptFit": short_context.last_rough_tokens_when_real_prompt_fit,
            }),
        )?;
        return Ok(Some(format!(
            "LLM preflight compaction skipped: rough estimate ~{} tokens, but the last provider usage fit at {} prompt tokens; deferring until rough growth exceeds tolerance.",
            rough_tokens, short_context.last_real_prompt_tokens
        )));
    }
    if messages.len() <= keep_messages + 2 {
        if rough_tokens <= chat_config.short_context_token_budget {
            return Ok(None);
        }
    }
    if let Some(engine_name) =
        selected_context_engine_name().filter(|engine| !engine.eq_ignore_ascii_case("compressor"))
    {
        match run_context_engine_should_compress(&engine_name, &messages, rough_tokens, true) {
            Ok(Some(false)) if rough_tokens <= chat_config.short_context_token_budget => {
                append_parent_phase_event(
                    store,
                    run_id,
                    "llm_preflight_compaction_skipped",
                    json!({
                        "reason": "context_engine_declined_preflight",
                        "contextEngine": engine_name,
                        "roughTokens": rough_tokens,
                        "thresholdTokens": chat_config.short_context_token_budget,
                    }),
                )?;
                return Ok(Some(format!(
                    "LLM preflight compaction skipped: contextEngine={engine_name} should_compress_preflight=false at ~{rough_tokens} tokens."
                )));
            }
            Ok(Some(false)) => {}
            Ok(Some(true)) | Ok(None) => {}
            Err(error) => {
                eprintln!(
                    "SynthChat context engine '{engine_name}' preflight should_compress failed: {error}"
                );
            }
        }
    }
    if let Some(note) = compression_anti_thrash_skip_note(short_context) {
        append_parent_phase_event(
            store,
            run_id,
            "llm_preflight_compaction_skipped",
            json!({
                "reason": "ineffective_compression_backoff",
                "ineffectiveCompressionCount": short_context.ineffective_compression_count,
                "lastCompressionSavingsPct": short_context.last_compression_savings_pct,
                "note": note,
            }),
        )?;
        return Ok(Some(format!("LLM preflight compaction skipped: {note}")));
    }

    let dynamic_note = compact_conversation_history_with_context_engine(
        store,
        Some(run_id),
        conversation_id,
        history,
        short_context,
        chat_config.short_context_token_budget,
        keep_messages,
        "llm_preflight_compacted",
        "Preflight context budget management before LLM request.",
    )?;
    let note = if let Some(note) = dynamic_note {
        Some(note)
    } else {
        compact_conversation_history_for_context(
            store,
            Some(run_id),
            conversation_id,
            history,
            short_context,
            chat_config.short_context_token_budget,
            keep_messages,
            "llm_preflight_compacted",
            "Preflight context budget management before LLM request.",
        )?
    };
    let Some(note) = note else {
        return Ok(None);
    };

    append_parent_phase_event(
        store,
        run_id,
        "llm_preflight_compaction",
        json!({
            "keepMessages": keep_messages,
            "summaryMessages": short_context.summary_messages,
            "summaryTokens": short_context.summary_tokens,
            "note": note,
        }),
    )?;
    Ok(Some(format!("LLM preflight compaction: {note}")))
}

pub(super) fn recover_context_overflow_for_retry(
    store: &AppStore,
    run_id: &str,
    conversation_id: &str,
    history: &mut Vec<ChatMessage>,
    short_context: &mut ShortContextState,
    token_budget: usize,
    kind: &str,
    error: &AppError,
) -> AppResult<Option<String>> {
    let detail = context_error_recovery_detail(&error.to_string(), token_budget);
    let recovery_budget = detail
        .provider_context_limit_tokens
        .filter(|limit| *limit < token_budget)
        .map(|limit| limit.saturating_mul(80) / 100)
        .map(|budget| budget.max(1_000))
        .unwrap_or(token_budget);
    let note = compact_conversation_history_for_context(
        store,
        Some(run_id),
        conversation_id,
        history,
        short_context,
        recovery_budget,
        8,
        "llm_context_recovered",
        &format!(
            "Automatic LLM recovery for {kind}: {error}.{}",
            detail.reason_suffix(recovery_budget)
        ),
    )?;
    Ok(note.map(|note| {
        format!(
            "Recovered {kind}: {note}{}",
            detail.note_suffix(recovery_budget)
        )
    }))
}

#[derive(Debug, Clone, Copy, Default)]
struct ContextErrorRecoveryDetail {
    provider_context_limit_tokens: Option<usize>,
    available_output_tokens: Option<usize>,
}

impl ContextErrorRecoveryDetail {
    fn reason_suffix(self, recovery_budget: usize) -> String {
        match self.provider_context_limit_tokens {
            Some(limit) => format!(
                " Provider reported context limit {limit} tokens; using recovery budget {recovery_budget} tokens."
            ),
            None => String::new(),
        }
    }

    fn note_suffix(self, recovery_budget: usize) -> String {
        let mut parts = Vec::new();
        if let Some(limit) = self.provider_context_limit_tokens {
            parts.push(format!(
                "providerContextLimitTokens={limit}, recoveryTokenBudget={recovery_budget}"
            ));
        }
        if let Some(tokens) = self.available_output_tokens {
            parts.push(format!("availableOutputTokens={tokens}"));
        }
        if parts.is_empty() {
            String::new()
        } else {
            format!(" ({})", parts.join(", "))
        }
    }
}

fn context_error_recovery_detail(
    message: &str,
    current_token_budget: usize,
) -> ContextErrorRecoveryDetail {
    let available_output_tokens = parse_available_output_tokens_from_error(message);
    let provider_context_limit_tokens = if available_output_tokens.is_some() {
        None
    } else {
        parse_context_limit_from_error(message).filter(|limit| *limit < current_token_budget)
    };
    ContextErrorRecoveryDetail {
        provider_context_limit_tokens,
        available_output_tokens,
    }
}

fn llm_context_error_recovery_detail(kind: &str, message: &str, token_budget: usize) -> Value {
    if !matches!(
        kind,
        "context_overflow" | "payload_too_large" | "long_context_tier"
    ) {
        return Value::Null;
    }
    let detail = context_error_recovery_detail(message, token_budget);
    if detail.provider_context_limit_tokens.is_none() && detail.available_output_tokens.is_none() {
        return Value::Null;
    }
    let recovery_budget = detail
        .provider_context_limit_tokens
        .map(|limit| limit.saturating_mul(80) / 100)
        .map(|budget| budget.max(1_000));
    json!({
        "providerContextLimitTokens": detail.provider_context_limit_tokens,
        "recoveryTokenBudget": recovery_budget,
        "availableOutputTokens": detail.available_output_tokens,
    })
}

pub(super) fn parse_context_limit_from_error(message: &str) -> Option<usize> {
    let lower = message.to_ascii_lowercase();
    for phrase in [
        "context_window",
        "context window",
        "context_length",
        "context length",
        "context size",
        "max_model_len",
        "maximum context",
        "max context",
        "context_length_exceeded",
    ] {
        if let Some(number) = first_reasonable_number_after(&lower, phrase, 80) {
            return Some(number);
        }
    }
    for (start, end, number) in digit_spans(&lower) {
        if !(1024..=10_000_000).contains(&number) {
            continue;
        }
        let window_start = start.saturating_sub(48);
        let window_end = (end + 48).min(lower.len());
        let window = &lower[window_start..window_end];
        if contains_any(window, &["context", "max_model_len", "limit", "maximum"]) {
            return Some(number);
        }
    }
    None
}

pub(super) fn parse_available_output_tokens_from_error(message: &str) -> Option<usize> {
    let lower = message.to_ascii_lowercase();
    if !lower.contains("max_tokens")
        || !contains_any(&lower, &["available_tokens", "available tokens"])
    {
        return None;
    }
    first_number_after(&lower, "available_tokens", 48)
        .or_else(|| first_number_after(&lower, "available tokens", 48))
        .or_else(|| {
            lower.rsplit_once('=').and_then(|(_, tail)| {
                digit_spans(tail)
                    .last()
                    .map(|(_, _, number)| number)
                    .copied()
            })
        })
        .filter(|tokens| *tokens >= 1)
}

fn first_number_after(text: &str, phrase: &str, max_chars: usize) -> Option<usize> {
    let start = text.find(phrase)? + phrase.len();
    let end = (start + max_chars).min(text.len());
    digit_spans(&text[start..end])
        .into_iter()
        .map(|(_, _, number)| number)
        .next()
}

fn first_reasonable_number_after(text: &str, phrase: &str, max_chars: usize) -> Option<usize> {
    let start = text.find(phrase)? + phrase.len();
    let end = (start + max_chars).min(text.len());
    digit_spans(&text[start..end])
        .into_iter()
        .map(|(_, _, number)| number)
        .find(|number| (1024..=10_000_000).contains(number))
}

fn digit_spans(text: &str) -> Vec<(usize, usize, usize)> {
    let mut spans = Vec::new();
    let mut start = None;
    for (index, ch) in text.char_indices() {
        if ch.is_ascii_digit() {
            start.get_or_insert(index);
            continue;
        }
        if let Some(begin) = start.take() {
            push_digit_span(text, begin, index, &mut spans);
        }
    }
    if let Some(begin) = start {
        push_digit_span(text, begin, text.len(), &mut spans);
    }
    spans
}

fn push_digit_span(text: &str, start: usize, end: usize, spans: &mut Vec<(usize, usize, usize)>) {
    if end.saturating_sub(start) < 1 {
        return;
    }
    if let Ok(number) = text[start..end].parse::<usize>() {
        spans.push((start, end, number));
    }
}

fn compact_conversation_history_with_context_engine(
    store: &AppStore,
    run_id: Option<&str>,
    conversation_id: &str,
    history: &mut Vec<ChatMessage>,
    short_context: &mut ShortContextState,
    token_budget: usize,
    keep_messages: usize,
    checkpoint_state: &str,
    reason: &str,
) -> AppResult<Option<String>> {
    let Some(engine_name) =
        selected_context_engine_name().filter(|engine| !engine.eq_ignore_ascii_case("compressor"))
    else {
        return Ok(None);
    };
    let messages = store.messages(conversation_id, None)?;
    if messages.len() < 4 {
        return Ok(None);
    }
    let keep_messages = keep_messages.max(1).min(messages.len());
    let older_count = tail_start_preserving_latest_user_and_token_budget(
        &messages,
        messages.len().saturating_sub(keep_messages),
        token_budget / 2,
    );
    if older_count < 2 {
        return Ok(None);
    }
    let boundary_message = &messages[older_count - 1];
    if short_context.boundary_id.as_deref() == Some(boundary_message.id.as_str()) {
        return Ok(None);
    }
    let start = short_context
        .boundary_id
        .as_deref()
        .and_then(|id| messages.iter().position(|message| message.id == id))
        .map(|idx| idx + 1)
        .unwrap_or(0);
    let start = align_compression_start_forward(&messages, start);
    if start >= older_count {
        return Ok(None);
    }

    let before_tokens = estimate_tokens(&format!(
        "{}\n{}",
        short_context.summary,
        render_messages_for_summary(&messages[start..])
    ));
    let compressed = match run_context_engine_compress(
        &engine_name,
        &messages[start..older_count],
        before_tokens,
        Some(reason),
    ) {
        Ok(compressed) => compressed,
        Err(error) => {
            eprintln!(
                "SynthChat context engine '{engine_name}' preflight compress failed: {error}"
            );
            return Ok(None);
        }
    };
    let summary = context_engine_messages_to_summary(
        short_context.summary.as_str(),
        &engine_name,
        &compressed,
        token_budget,
    );
    let after_tokens = estimate_tokens(&format!(
        "{}\n{}",
        summary,
        render_messages_for_summary(&messages[older_count..])
    ));
    short_context.boundary_id = Some(boundary_message.id.clone());
    short_context.summary_tokens = estimate_tokens(&summary);
    short_context.summary_messages = older_count;
    short_context.summary = summary;
    record_summary_success(short_context);
    record_compression_effectiveness(short_context, before_tokens, after_tokens);
    short_context.last_compression_rough_tokens = before_tokens;
    short_context.awaiting_real_usage_after_compression = true;
    *short_context = store.save_short_context(short_context.clone())?;
    *history = sanitize_retained_tool_pairs(messages[older_count..].to_vec());

    if let Some(run_id) = run_id {
        let mut run = store.agent_run(run_id)?;
        run.checkpoints.push(AgentCheckpointRecord {
            checkpoint_id: new_id("ckpt"),
            run_id: run_id.to_string(),
            iteration: 0,
            created_at: now_iso(),
            state: checkpoint_state.into(),
            completed_call_ids: Vec::new(),
            event_refs: Vec::new(),
            summary: format!(
                "{checkpoint_state}: compacted {} message(s) through context engine {engine_name}.",
                older_count.saturating_sub(start)
            ),
        });
        run.updated_at = now_iso();
        store.save_agent_run(run)?;
    }

    Ok(Some(format!(
        "compacted {} message(s), retained {} message(s), summaryTokens={}, contextEngine={}.",
        older_count.saturating_sub(start),
        history.len(),
        short_context.summary_tokens,
        engine_name
    )))
}

pub(super) fn compact_conversation_history_for_context(
    store: &AppStore,
    run_id: Option<&str>,
    conversation_id: &str,
    history: &mut Vec<ChatMessage>,
    short_context: &mut ShortContextState,
    token_budget: usize,
    keep_messages: usize,
    checkpoint_state: &str,
    reason: &str,
) -> AppResult<Option<String>> {
    let messages = store.messages(conversation_id, None)?;
    if messages.len() < 4 {
        return Ok(None);
    }
    let keep_messages = keep_messages.max(1).min(messages.len());
    let older_count = tail_start_preserving_latest_user_and_token_budget(
        &messages,
        messages.len().saturating_sub(keep_messages),
        token_budget / 2,
    );
    if older_count < 2 {
        return Ok(None);
    }
    let boundary_message = &messages[older_count - 1];
    if short_context.boundary_id.as_deref() == Some(boundary_message.id.as_str()) {
        return Ok(None);
    }
    let start = short_context
        .boundary_id
        .as_deref()
        .and_then(|id| messages.iter().position(|message| message.id == id))
        .map(|idx| idx + 1)
        .unwrap_or(0);
    let start = align_compression_start_forward(&messages, start);
    if start >= older_count {
        return Ok(None);
    }

    let before_tokens = estimate_tokens(&format!(
        "{}\n{}",
        short_context.summary,
        render_messages_for_summary(&messages[start..])
    ));
    let mut transcript = render_messages_for_summary(&messages[start..older_count]);
    let conversation = store.conversation(conversation_id)?;
    let persona = store
        .persona(conversation.persona_id.as_deref())
        .or_else(|_| store.persona(None))?;
    let memory_context = memory_pre_compress_context(store, &persona, &transcript)?;
    if !memory_context.trim().is_empty() {
        transcript = format!("{memory_context}\n{transcript}");
    }
    let summary = fallback_short_context_summary(
        short_context.summary.as_str(),
        &format!("{reason}\n\n{transcript}"),
        token_budget,
    );
    let after_tokens = estimate_tokens(&format!(
        "{}\n{}",
        summary,
        render_messages_for_summary(&messages[older_count..])
    ));
    short_context.boundary_id = Some(boundary_message.id.clone());
    short_context.summary_tokens = estimate_tokens(&summary);
    short_context.summary_messages = older_count;
    short_context.summary = summary;
    record_compression_effectiveness(short_context, before_tokens, after_tokens);
    short_context.last_compression_rough_tokens = before_tokens;
    short_context.awaiting_real_usage_after_compression = true;
    *short_context = store.save_short_context(short_context.clone())?;
    *history = sanitize_retained_tool_pairs(messages[older_count..].to_vec());

    if let Some(run_id) = run_id {
        let mut run = store.agent_run(run_id)?;
        run.checkpoints.push(AgentCheckpointRecord {
            checkpoint_id: new_id("ckpt"),
            run_id: run_id.to_string(),
            iteration: 0,
            created_at: now_iso(),
            state: checkpoint_state.into(),
            completed_call_ids: Vec::new(),
            event_refs: Vec::new(),
            summary: format!(
                "{checkpoint_state}: compacted {} message(s) into short context.",
                older_count.saturating_sub(start)
            ),
        });
        run.updated_at = now_iso();
        store.save_agent_run(run)?;
    }

    Ok(Some(format!(
        "compacted {} message(s), retained {} message(s), summaryTokens={}.",
        older_count.saturating_sub(start),
        history.len(),
        short_context.summary_tokens
    )))
}

pub(super) fn should_defer_preflight_to_real_usage(
    short_context: &mut ShortContextState,
    rough_tokens: usize,
    threshold_tokens: usize,
) -> bool {
    if rough_tokens < threshold_tokens {
        return false;
    }
    if short_context.last_real_prompt_tokens == 0 {
        return false;
    }
    if short_context.last_real_prompt_tokens >= threshold_tokens {
        return false;
    }
    let baseline = if short_context.last_rough_tokens_when_real_prompt_fit > 0 {
        short_context.last_rough_tokens_when_real_prompt_fit
    } else {
        short_context.last_compression_rough_tokens
    };
    if baseline == 0 {
        return false;
    }
    let growth = rough_tokens.saturating_sub(baseline);
    let tolerated_growth = 4096.max(threshold_tokens / 20);
    if growth > tolerated_growth {
        return false;
    }
    short_context.last_rough_tokens_when_real_prompt_fit = baseline.max(rough_tokens);
    true
}

pub(super) fn record_short_context_real_usage_for_run(
    store: &AppStore,
    run_id: &str,
    reply: &crate::llm::LlmReply,
    threshold_tokens: usize,
) -> AppResult<()> {
    let prompt_tokens = reply.prompt_tokens;
    if prompt_tokens == 0 {
        return Ok(());
    }
    let run = store.agent_run(run_id)?;
    let mut short_context = store.short_context(&run.conversation_id)?;
    update_short_context_real_usage(&mut short_context, prompt_tokens, threshold_tokens);
    store.save_short_context(short_context)?;
    notify_context_engine_update_from_response(store, run_id, reply, threshold_tokens)?;
    Ok(())
}

fn notify_context_engine_update_from_response(
    store: &AppStore,
    run_id: &str,
    reply: &crate::llm::LlmReply,
    threshold_tokens: usize,
) -> AppResult<()> {
    let Some(engine_name) =
        selected_context_engine_name().filter(|engine| !engine.eq_ignore_ascii_case("compressor"))
    else {
        return Ok(());
    };
    let total_tokens = reply
        .prompt_tokens
        .saturating_add(reply.completion_tokens)
        .saturating_add(reply.reasoning_tokens);
    let usage = json!({
        "prompt_tokens": reply.prompt_tokens,
        "completion_tokens": reply.completion_tokens,
        "total_tokens": total_tokens,
        "input_tokens": reply.prompt_tokens,
        "output_tokens": reply.completion_tokens,
        "cache_read_tokens": reply.cache_read_tokens,
        "cache_write_tokens": reply.cache_write_tokens,
        "reasoning_tokens": reply.reasoning_tokens,
        "provider_id": reply.provider_id.clone(),
        "provider_type": reply.provider_type.clone(),
        "model": reply.model.clone(),
        "base_url": reply.base_url.clone(),
    });
    let model_context = json!({
        "model": reply.model.clone().unwrap_or_default(),
        "context_length": threshold_tokens,
        "base_url": reply.base_url.clone().unwrap_or_default(),
        "api_key": "",
        "provider": reply.provider_id.clone().unwrap_or_default(),
        "api_mode": reply.provider_type.clone().unwrap_or_default(),
    });
    match run_context_engine_update_model(&engine_name, &model_context) {
        Ok(implemented) => {
            if implemented {
                append_parent_phase_event(
                    store,
                    run_id,
                    "context_engine_update_model",
                    json!({
                        "contextEngine": engine_name,
                        "model": reply.model.clone(),
                        "contextLength": threshold_tokens,
                        "provider": reply.provider_id.clone(),
                        "providerType": reply.provider_type.clone(),
                        "baseUrlConfigured": reply
                            .base_url
                            .as_ref()
                            .is_some_and(|value| !value.trim().is_empty()),
                        "apiKeyForwarded": false,
                    }),
                )?;
            }
        }
        Err(error) => {
            eprintln!("SynthChat context engine '{engine_name}' update_model failed: {error}");
        }
    }
    match run_context_engine_update_from_response(&engine_name, &usage) {
        Ok(implemented) => {
            if implemented {
                append_parent_phase_event(
                    store,
                    run_id,
                    "context_engine_update_from_response",
                    json!({
                        "contextEngine": engine_name,
                        "promptTokens": reply.prompt_tokens,
                        "completionTokens": reply.completion_tokens,
                        "totalTokens": total_tokens,
                    }),
                )?;
            }
        }
        Err(error) => {
            eprintln!(
                "SynthChat context engine '{engine_name}' update_from_response failed: {error}"
            );
        }
    }
    Ok(())
}

pub(super) fn maybe_post_turn_compress_with_context_engine(
    store: &AppStore,
    run_id: &str,
    conversation_id: &str,
    prompt_tokens: usize,
    token_budget: usize,
    keep_messages: usize,
) -> AppResult<Option<String>> {
    let Some(engine_name) =
        selected_context_engine_name().filter(|engine| !engine.eq_ignore_ascii_case("compressor"))
    else {
        return Ok(None);
    };
    let messages = store.messages(conversation_id, None)?;
    let should_compress = match run_context_engine_should_compress(
        &engine_name,
        &messages,
        prompt_tokens,
        false,
    ) {
        Ok(Some(decision)) => decision,
        Ok(None) => return Ok(None),
        Err(error) => {
            eprintln!(
                    "SynthChat context engine '{engine_name}' post-turn should_compress failed: {error}"
                );
            return Ok(None);
        }
    };
    if !should_compress {
        append_parent_phase_event(
            store,
            run_id,
            "context_engine_post_turn_compression_skipped",
            json!({
                "reason": "context_engine_declined_post_turn",
                "contextEngine": engine_name,
                "promptTokens": prompt_tokens,
            }),
        )?;
        return Ok(Some(format!(
            "Context engine post-turn compression skipped: contextEngine={engine_name} should_compress=false at {prompt_tokens} prompt tokens."
        )));
    }
    let mut history = messages.clone();
    let mut short_context = store.short_context(conversation_id)?;
    let note = compact_conversation_history_with_context_engine(
        store,
        Some(run_id),
        conversation_id,
        &mut history,
        &mut short_context,
        token_budget,
        keep_messages,
        "context_engine_post_turn_compacted",
        "Post-turn context engine compression after LLM response.",
    )?;
    if let Some(note) = note.as_ref() {
        append_parent_phase_event(
            store,
            run_id,
            "context_engine_post_turn_compression",
            json!({
                "contextEngine": engine_name,
                "promptTokens": prompt_tokens,
                "summaryMessages": short_context.summary_messages,
                "summaryTokens": short_context.summary_tokens,
                "note": note,
            }),
        )?;
    }
    Ok(note.map(|note| format!("Context engine post-turn compression: {note}")))
}

pub(super) fn update_short_context_real_usage(
    short_context: &mut ShortContextState,
    prompt_tokens: usize,
    threshold_tokens: usize,
) {
    if prompt_tokens == 0 {
        return;
    }
    short_context.last_real_prompt_tokens = prompt_tokens;
    if prompt_tokens < threshold_tokens {
        if short_context.awaiting_real_usage_after_compression
            && short_context.last_compression_rough_tokens > 0
        {
            short_context.last_rough_tokens_when_real_prompt_fit =
                short_context.last_compression_rough_tokens;
        }
    } else {
        short_context.last_rough_tokens_when_real_prompt_fit = 0;
    }
    short_context.awaiting_real_usage_after_compression = false;
}

pub(super) fn tail_start_preserving_latest_user(
    messages: &[ChatMessage],
    requested_tail_start: usize,
) -> usize {
    let tail_start = requested_tail_start.min(messages.len());
    let Some(last_user_idx) = messages.iter().rposition(|message| message.role == "user") else {
        return tail_start;
    };
    tail_start.min(last_user_idx)
}

pub(super) fn tail_start_preserving_latest_user_and_token_budget(
    messages: &[ChatMessage],
    requested_tail_start: usize,
    token_budget: usize,
) -> usize {
    if messages.is_empty() {
        return 0;
    }
    let message_tail_start = tail_start_preserving_latest_user(messages, requested_tail_start);
    let min_tail = messages.len().min(3);
    let soft_ceiling = token_budget.max(1000).saturating_mul(3) / 2;
    let mut accumulated = 0usize;
    let mut token_tail_start = messages.len();
    for idx in (0..messages.len()).rev() {
        let message_tokens = estimate_tokens(&strip_historical_media_payloads(
            messages[idx].content.as_str(),
        ))
        .saturating_add(10);
        let protected_count = messages.len().saturating_sub(idx);
        if accumulated.saturating_add(message_tokens) > soft_ceiling && protected_count >= min_tail
        {
            break;
        }
        accumulated = accumulated.saturating_add(message_tokens);
        token_tail_start = idx;
    }
    token_tail_start = token_tail_start.min(messages.len().saturating_sub(min_tail));
    if let Some(last_user_idx) = messages.iter().rposition(|message| message.role == "user") {
        token_tail_start = token_tail_start.min(last_user_idx);
        let tail_start = if token_tail_start > 0 && token_tail_start < message_tail_start {
            token_tail_start
        } else {
            message_tail_start.max(token_tail_start)
        }
        .min(last_user_idx);
        return align_tail_start_to_tool_group(messages, tail_start).min(last_user_idx);
    }
    align_tail_start_to_tool_group(messages, message_tail_start.max(token_tail_start))
        .min(messages.len())
}

pub(super) fn align_compression_start_forward(messages: &[ChatMessage], mut start: usize) -> usize {
    while start < messages.len() && messages[start].role == "tool" {
        start += 1;
    }
    start
}

pub(super) fn align_tail_start_to_tool_group(messages: &[ChatMessage], tail_start: usize) -> usize {
    if tail_start == 0 || tail_start >= messages.len() {
        return tail_start.min(messages.len());
    }
    let mut check = tail_start - 1;
    while messages
        .get(check)
        .map(|message| message.role == "tool")
        .unwrap_or(false)
    {
        if check == 0 {
            return tail_start;
        }
        check -= 1;
    }
    if messages
        .get(check)
        .map(assistant_message_has_tool_calls)
        .unwrap_or(false)
    {
        return check;
    }
    tail_start
}

fn assistant_message_has_tool_calls(message: &ChatMessage) -> bool {
    if message.role != "assistant" {
        return false;
    }
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(message.content.trim()) else {
        return false;
    };
    parsed
        .get("tool_calls")
        .or_else(|| parsed.get("toolCalls"))
        .and_then(serde_json::Value::as_array)
        .map(|calls| !calls.is_empty())
        .unwrap_or(false)
}

pub(super) fn sanitize_retained_tool_pairs(messages: Vec<ChatMessage>) -> Vec<ChatMessage> {
    let mut surviving_calls: HashMap<String, ToolCallStub> = HashMap::new();
    for (idx, message) in messages.iter().enumerate() {
        if message.role != "assistant" {
            continue;
        }
        for call in assistant_tool_calls(message) {
            if let Some(id) = tool_call_id(&call) {
                surviving_calls
                    .entry(id.clone())
                    .or_insert_with(|| ToolCallStub {
                        id,
                        name: tool_call_name(&call),
                        arguments: tool_call_arguments(&call)
                            .cloned()
                            .unwrap_or_else(|| json!({})),
                        assistant_index: idx,
                    });
            }
        }
    }
    if surviving_calls.is_empty() {
        return messages;
    }

    let surviving_ids = surviving_calls.keys().cloned().collect::<HashSet<_>>();
    let mut result_ids = HashSet::new();
    let mut direct_orphan_result_ids = HashSet::new();
    for message in messages.iter() {
        let Some(result) = tool_result_id(message) else {
            continue;
        };
        if surviving_ids.contains(&result.id) {
            result_ids.insert(result.id);
        } else if result.direct_provider_result {
            direct_orphan_result_ids.insert(result.id);
        }
    }
    let missing_ids = surviving_ids
        .difference(&result_ids)
        .cloned()
        .collect::<HashSet<_>>();
    if missing_ids.is_empty() && direct_orphan_result_ids.is_empty() {
        return messages;
    }

    let mut patched = Vec::with_capacity(messages.len() + missing_ids.len());
    for (idx, message) in messages.into_iter().enumerate() {
        if tool_result_id(&message)
            .filter(|result| {
                result.direct_provider_result && direct_orphan_result_ids.contains(&result.id)
            })
            .is_some()
        {
            continue;
        }
        patched.push(message);
        let stubs = surviving_calls
            .values()
            .filter(|stub| stub.assistant_index == idx && missing_ids.contains(&stub.id))
            .cloned()
            .collect::<Vec<_>>();
        for stub in stubs {
            patched.push(stub_tool_result_message(
                &patched.last().unwrap().conversation_id,
                &stub,
            ));
        }
    }
    patched
}

#[derive(Debug, Clone)]
struct ToolCallStub {
    id: String,
    name: String,
    arguments: Value,
    assistant_index: usize,
}

#[derive(Debug, Clone)]
struct ToolResultId {
    id: String,
    direct_provider_result: bool,
}

fn assistant_tool_calls(message: &ChatMessage) -> Vec<Value> {
    let Ok(parsed) = serde_json::from_str::<Value>(message.content.trim()) else {
        return Vec::new();
    };
    parsed
        .get("tool_calls")
        .or_else(|| parsed.get("toolCalls"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn tool_call_id(call: &Value) -> Option<String> {
    call.get("call_id")
        .or_else(|| call.get("callId"))
        .or_else(|| call.get("id"))
        .or_else(|| call.get("tool_call_id"))
        .or_else(|| call.get("toolCallId"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
}

fn tool_call_name(call: &Value) -> String {
    call.get("name")
        .or_else(|| call.get("tool"))
        .or_else(|| call.get("toolName"))
        .or_else(|| call.pointer("/function/name"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or("unknown")
        .to_string()
}

fn tool_call_arguments(call: &Value) -> Option<&Value> {
    call.get("arguments")
        .or_else(|| call.get("args"))
        .or_else(|| call.get("payload"))
        .or_else(|| call.get("input"))
        .or_else(|| call.pointer("/function/arguments"))
}

fn tool_result_id(message: &ChatMessage) -> Option<ToolResultId> {
    if message.role != "tool" {
        return None;
    }
    let value = serde_json::from_str::<Value>(&message.content).ok()?;
    if value.get("type").and_then(Value::as_str) == Some("toolEvent") {
        let event = value.get("event")?;
        return event
            .get("callId")
            .or_else(|| event.get("call_id"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(|id| ToolResultId {
                id: id.to_string(),
                direct_provider_result: false,
            });
    }
    value
        .get("tool_call_id")
        .or_else(|| value.get("toolCallId"))
        .or_else(|| value.get("call_id"))
        .or_else(|| value.get("callId"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(|id| ToolResultId {
            id: id.to_string(),
            direct_provider_result: true,
        })
}

fn stub_tool_result_message(conversation_id: &str, stub: &ToolCallStub) -> ChatMessage {
    let mut payload = match stub.arguments.clone() {
        Value::Object(_) => stub.arguments.clone(),
        value => json!({ "arguments": value }),
    };
    if let Some(object) = payload.as_object_mut() {
        object.insert(
            "__agentProviderToolCall".into(),
            json!({
                "id": stub.id,
                "call_id": stub.id,
            }),
        );
    }
    ChatMessage::new(
        conversation_id.to_string(),
        "tool",
        json!({
            "type": "toolEvent",
            "event": {
                "runId": "__context_compression__",
                "serverId": "__context__",
                "toolName": stub.name,
                "title": stub.name,
                "status": "completed",
                "ok": true,
                "callId": stub.id,
                "raw": { "payload": payload },
                "text": "[Result from earlier conversation - see context summary above]"
            }
        })
        .to_string(),
        "desktop-agent-context-compression",
    )
}

pub(super) fn recover_image_payloads_for_retry(
    history: &mut [ChatMessage],
    kind: &str,
) -> AppResult<Option<String>> {
    let mut replaced = 0usize;
    for message in history.iter_mut() {
        let (cleaned, count) = strip_data_image_payloads(&message.content);
        if count > 0 {
            message.content = cleaned;
            replaced += count;
        }
    }
    if replaced == 0 {
        return Ok(None);
    }
    Ok(Some(format!(
        "Recovered {kind} by replacing {replaced} inline image payload(s) with text placeholders."
    )))
}

pub(super) fn strip_historical_media_payloads(content: &str) -> String {
    strip_data_image_payloads_with_placeholder(
        content,
        "[inline image payload omitted from historical context]",
    )
    .0
}

pub(super) fn recover_reasoning_replay_text_for_retry(
    history: &mut [ChatMessage],
    kind: &str,
) -> AppResult<Option<String>> {
    let mut changed = 0usize;
    let mut provider_items_removed = 0usize;
    for message in history.iter_mut() {
        let cleaned = strip_reasoning_replay_markers(&message.content);
        if cleaned != message.content {
            message.content = cleaned;
            changed += 1;
        }
        provider_items_removed += strip_reasoning_provider_data(message);
    }
    if changed == 0 && provider_items_removed == 0 {
        return Ok(None);
    }
    Ok(Some(format!(
        "Recovered {kind} by stripping reasoning replay/signature markers from {changed} message(s) and removing {provider_items_removed} provider reasoning replay item(s)."
    )))
}

fn strip_reasoning_provider_data(message: &mut ChatMessage) -> usize {
    let Some(provider_data) = message.provider_data.as_mut() else {
        return 0;
    };
    let mut removed = 0usize;
    if let Some(responses) = provider_data
        .get_mut("responses")
        .and_then(Value::as_object_mut)
    {
        for key in [
            "reasoningItems",
            "reasoning_items",
            "codexReasoningItems",
            "codex_reasoning_items",
        ] {
            if let Some(value) = responses.remove(key) {
                removed += value.as_array().map(Vec::len).unwrap_or(1);
            }
        }
        if responses.is_empty() {
            provider_data.as_object_mut().map(|object| {
                object.remove("responses");
            });
        }
    }
    if let Some(openai) = provider_data
        .get_mut("openai")
        .and_then(Value::as_object_mut)
    {
        for key in ["reasoning_content", "reasoning", "reasoning_details"] {
            if openai.remove(key).is_some() {
                removed += 1;
            }
        }
        if openai.is_empty() {
            provider_data.as_object_mut().map(|object| {
                object.remove("openai");
            });
        }
    }
    if let Some(root) = provider_data.as_object_mut() {
        for key in ["reasoning_content", "reasoning", "reasoning_details"] {
            if root.remove(key).is_some() {
                removed += 1;
            }
        }
    }
    if provider_data.as_object().map(|object| object.is_empty()) == Some(true) {
        message.provider_data = None;
    }
    removed
}

pub(super) fn recover_tool_replay_history_for_retry(
    history: &mut [ChatMessage],
    kind: &str,
) -> AppResult<Option<String>> {
    let mut changed = 0usize;
    for message in history.iter_mut() {
        if message.role != "tool" {
            continue;
        }
        let Some(summary) = tool_event_text_fallback(&message.content) else {
            continue;
        };
        message.role = "user".into();
        message.source = "desktop-agent-tool-replay-fallback".into();
        message.content = summary;
        changed += 1;
    }
    if changed == 0 {
        return Ok(None);
    }
    Ok(Some(format!(
        "Recovered {kind} by downgrading {changed} historical tool result message(s) to plain text for provider retry."
    )))
}

fn tool_event_text_fallback(content: &str) -> Option<String> {
    let value = serde_json::from_str::<Value>(content).ok()?;
    if value.get("type").and_then(Value::as_str) != Some("toolEvent") {
        return None;
    }
    let event = value.get("event")?;
    let tool_name = event
        .get("toolName")
        .or_else(|| event.get("tool_name"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or("unknown");
    let call_id = event
        .get("callId")
        .or_else(|| event.get("call_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .unwrap_or("unknown");
    let ok = event.get("ok").and_then(Value::as_bool).unwrap_or(true);
    let text = event
        .get("text")
        .or_else(|| event.get("error"))
        .or_else(|| event.get("summary"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .unwrap_or(if ok {
            "Tool completed without textual output."
        } else {
            "Tool failed without textual output."
        });
    Some(format!(
        "Historical tool result for `{tool_name}` (call `{call_id}`, ok={ok}):\n{text}"
    ))
}

pub(super) fn strip_data_image_payloads(content: &str) -> (String, usize) {
    strip_data_image_payloads_with_placeholder(
        content,
        "[inline image payload omitted for provider retry]",
    )
}

pub(super) fn strip_data_image_payloads_with_placeholder(
    content: &str,
    placeholder: &str,
) -> (String, usize) {
    let mut output = String::with_capacity(content.len());
    let mut cursor = 0usize;
    let mut count = 0usize;
    while let Some(relative) = content[cursor..].find("data:image/") {
        let start = cursor + relative;
        output.push_str(&content[cursor..start]);
        let mut end = start;
        for (offset, ch) in content[start..].char_indices() {
            if ch.is_whitespace() || matches!(ch, '"' | '\'' | ')' | ']' | '}') {
                break;
            }
            end = start + offset + ch.len_utf8();
        }
        if end <= start {
            output.push_str("data:image/");
            cursor = start + "data:image/".len();
            continue;
        }
        output.push_str(placeholder);
        cursor = end;
        count += 1;
    }
    output.push_str(&content[cursor..]);
    (output, count)
}

pub(super) fn strip_reasoning_replay_markers(content: &str) -> String {
    content
        .lines()
        .filter(|line| {
            let lower = line.to_ascii_lowercase();
            !contains_any(
                &lower,
                &[
                    "codex_reasoning_items",
                    "reasoning_details",
                    "encrypted content",
                    "invalid_encrypted_content",
                    "thinking signature",
                    "thought_signature",
                ],
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn llm_api_request_hook_payload(
    run_id: Option<&str>,
    provider: &LlmProvider,
    attempt_number: usize,
    system_prompt: &str,
    history: &[ChatMessage],
    user_content: &str,
    native_tools: Option<&[ToolDefinition]>,
) -> Value {
    let history_chars: usize = history.iter().map(|message| message.content.len()).sum();
    json!({
        "session_id": run_id.unwrap_or_default(),
        "task_id": run_id.unwrap_or_default(),
        "platform": "desktop",
        "model": provider.model,
        "provider": provider.id,
        "provider_type": provider.provider_type,
        "base_url": provider.base_url,
        "api_call_count": attempt_number,
        "message_count": history.len() + 1,
        "tool_count": native_tools.map(|tools| tools.len()).unwrap_or(0),
        "request_char_count": system_prompt.len() + history_chars + user_content.len(),
        "system_prompt_chars": system_prompt.len(),
        "user_content_chars": user_content.len(),
    })
}

fn llm_api_response_hook_payload(
    mut payload: Value,
    elapsed_ms: u128,
    reply: Option<&crate::llm::LlmReply>,
    error: Option<(&str, &str)>,
) -> Value {
    if let Some(object) = payload.as_object_mut() {
        object.insert("api_duration".into(), json!(elapsed_ms as f64 / 1000.0));
        object.insert("elapsed_ms".into(), json!(elapsed_ms));
        if let Some(reply) = reply {
            object.insert("status".into(), json!("success"));
            object.insert(
                "response_model".into(),
                json!(reply.model.clone().unwrap_or_default()),
            );
            object.insert("finish_reason".into(), json!(reply.finish_reason.clone()));
            object.insert("assistant_content_chars".into(), json!(reply.content.len()));
            object.insert(
                "assistant_tool_call_count".into(),
                json!(reply
                    .provider_data
                    .as_ref()
                    .and_then(|value| value.get("toolCalls"))
                    .and_then(Value::as_array)
                    .map(|items| items.len())
                    .unwrap_or(0)),
            );
            object.insert(
                "usage".into(),
                json!({
                    "input_tokens": reply.prompt_tokens,
                    "output_tokens": reply.completion_tokens,
                    "cache_read_tokens": reply.cache_read_tokens,
                    "cache_write_tokens": reply.cache_write_tokens,
                    "reasoning_tokens": reply.reasoning_tokens,
                }),
            );
        }
        if let Some((kind, message)) = error {
            object.insert("status".into(), json!("error"));
            object.insert("error_kind".into(), json!(kind));
            object.insert(
                "error".into(),
                json!(truncate_for_prompt(&redact_sensitive_text(message), 800)),
            );
        }
    }
    payload
}

#[derive(Debug, Clone)]
struct ImageAttachmentPart {
    file_name: String,
    mime_type: String,
    path: PathBuf,
    data_url: String,
    base64_data: String,
}

fn history_with_native_image_attachments(
    store: &AppStore,
    provider: &LlmProvider,
    persona: &Persona,
    history: &[ChatMessage],
    user_content: &str,
) -> Vec<ChatMessage> {
    let prepared_history = history_with_current_user_content(history, user_content);
    let mut effective_provider = provider.clone();
    if !persona.llm_model.trim().is_empty() {
        effective_provider.model = persona.llm_model.trim().to_string();
    }
    let caps = model_catalog::provider_model_capabilities(&effective_provider);
    if !caps.supports_vision {
        return prepared_history;
    }
    let attachment_root = store.data_dir().join("attachments");
    prepared_history
        .into_iter()
        .map(|message| {
            if message.role != "user" {
                return message;
            }
            let attachments = image_attachments_from_message(&message, &attachment_root);
            if attachments.is_empty() {
                return message;
            }
            message_with_native_image_parts(&message, &attachments)
        })
        .collect()
}

fn history_with_current_user_content(history: &[ChatMessage], user_content: &str) -> Vec<ChatMessage> {
    let Some(last_user_index) = history.iter().rposition(|message| message.role == "user") else {
        if !user_content.trim().is_empty() {
            return vec![ChatMessage::new(
                "__current_turn__".into(),
                "user",
                user_content.to_string(),
                "desktop-agent-current-turn",
            )];
        }
        return history.to_vec();
    };
    if user_content.trim().is_empty() {
        return history.to_vec();
    }
    history
        .iter()
        .enumerate()
        .map(|(index, message)| {
            let mut message = message.clone();
            if index == last_user_index {
                message.content = user_content.to_string();
            }
            message
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_with_current_user_content_adds_missing_user_turn() {
        let history = history_with_current_user_content(&[], "search today's news");

        assert_eq!(history.len(), 1);
        assert_eq!(history[0].role, "user");
        assert_eq!(history[0].content, "search today's news");
    }

    #[test]
    fn history_with_current_user_content_replaces_latest_user_turn() {
        let earlier = ChatMessage::new(
            "conv".into(),
            "user",
            "old request".into(),
            "test",
        );
        let assistant = ChatMessage::new(
            "conv".into(),
            "assistant",
            "old answer".into(),
            "test",
        );
        let latest = ChatMessage::new(
            "conv".into(),
            "user",
            "placeholder".into(),
            "test",
        );

        let history =
            history_with_current_user_content(&[earlier, assistant, latest], "current request");

        assert_eq!(history.len(), 3);
        assert_eq!(history[0].content, "old request");
        assert_eq!(history[2].content, "current request");
    }
}

fn image_attachments_from_message(message: &ChatMessage, attachment_root: &PathBuf) -> Vec<ImageAttachmentPart> {
    let root = attachment_root
        .canonicalize()
        .unwrap_or_else(|_| attachment_root.to_path_buf());
    let mut attachments = Vec::new();
    let mut seen = HashSet::<String>::new();
    for line in message.content.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with('{') || !trimmed.contains("\"attachment\"") {
            continue;
        }
        if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
            push_image_attachment_part(&value, &root, &mut seen, &mut attachments);
        }
        if attachments.len() >= 6 {
            return attachments;
        }
    }
    if let Some(provider_data) = message.provider_data.as_ref() {
        for key in [
            "attachments",
            "attachmentContexts",
            "attachment_contexts",
            "mediaFiles",
            "media_files",
        ] {
            match provider_data.get(key) {
                Some(Value::Array(items)) => {
                    for item in items {
                        push_image_attachment_part(item, &root, &mut seen, &mut attachments);
                        if attachments.len() >= 6 {
                            return attachments;
                        }
                    }
                }
                Some(item) => {
                    push_image_attachment_part(item, &root, &mut seen, &mut attachments);
                    if attachments.len() >= 6 {
                        return attachments;
                    }
                }
                None => {}
            }
        }
    }
    attachments
}

fn push_image_attachment_part(
    value: &Value,
    attachment_root: &PathBuf,
    seen: &mut HashSet<String>,
    attachments: &mut Vec<ImageAttachmentPart>,
) {
    let Some(part) = image_attachment_part_from_value(value, attachment_root) else {
        return;
    };
    let key = format!("{}::{}", part.path.display(), part.mime_type);
    if seen.insert(key) {
        attachments.push(part);
    }
}

fn image_attachment_part_from_value(value: &Value, attachment_root: &PathBuf) -> Option<ImageAttachmentPart> {
    if value
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(|kind| !matches!(kind, "attachment" | "image" | "file"))
    {
        return None;
    }
    let mime_type = value
        .get("mimeType")
        .or_else(|| value.get("mime_type"))
        .or_else(|| value.get("contentType"))
        .or_else(|| value.get("content_type"))
        .and_then(Value::as_str)
        .unwrap_or("application/octet-stream")
        .trim();
    let path_text = value
        .get("path")
        .or_else(|| value.get("filePath"))
        .or_else(|| value.get("file_path"))
        .or_else(|| value.get("localPath"))
        .or_else(|| value.get("local_path"))
        .or_else(|| value.get("sourcePath"))
        .or_else(|| value.get("source_path"))
        .or_else(|| value.get("tempPath"))
        .or_else(|| value.get("temp_path"))
        .or_else(|| value.get("thumbPath"))
        .or_else(|| value.get("thumb_path"))
        .and_then(Value::as_str)?
        .trim();
    let path = PathBuf::from(path_text);
    let canonical = path.canonicalize().ok()?;
    if !canonical.starts_with(attachment_root) || !canonical.is_file() {
        return None;
    }
    let mime_type = normalized_image_mime(mime_type, &canonical)?;
    let metadata = fs::metadata(&canonical).ok()?;
    if metadata.len() == 0 || metadata.len() > 20 * 1024 * 1024 {
        return None;
    }
    let bytes = fs::read(&canonical).ok()?;
    use base64::Engine as _;
    let base64_data = base64::engine::general_purpose::STANDARD.encode(bytes);
    let data_url = format!("data:{mime_type};base64,{base64_data}");
    let file_name = value
        .get("fileName")
        .or_else(|| value.get("file_name"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| canonical.file_name().and_then(|name| name.to_str()).map(str::to_string))
        .unwrap_or_else(|| "image".into());
    Some(ImageAttachmentPart {
        file_name,
        mime_type,
        path: canonical,
        data_url,
        base64_data,
    })
}

fn normalized_image_mime(mime_type: &str, path: &PathBuf) -> Option<String> {
    let lower = mime_type.trim().to_ascii_lowercase();
    if lower.starts_with("image/") && lower != "image/jpg" {
        return Some(lower);
    }
    match path.extension().and_then(|ext| ext.to_str()).map(|ext| ext.to_ascii_lowercase()) {
        Some(ext) if ext == "jpg" || ext == "jpeg" => Some("image/jpeg".into()),
        Some(ext) if ext == "webp" => Some("image/webp".into()),
        Some(ext) if ext == "gif" => Some("image/gif".into()),
        Some(ext) if ext == "bmp" => Some("image/bmp".into()),
        Some(ext) if ext == "png" => Some("image/png".into()),
        _ => None,
    }
}

fn message_with_native_image_parts(message: &ChatMessage, attachments: &[ImageAttachmentPart]) -> ChatMessage {
    let mut next = message.clone();
    let cleaned_text = sanitize_native_image_text(&message.content);
    let text = cleaned_text.trim();
    let mut openai_content = Vec::new();
    let mut responses_content = Vec::new();
    let mut anthropic_content = Vec::new();
    let mut gemini_parts = Vec::new();
    if !text.is_empty() {
        openai_content.push(json!({"type": "text", "text": text}));
        responses_content.push(json!({"type": "input_text", "text": text}));
        anthropic_content.push(json!({"type": "text", "text": text}));
        gemini_parts.push(json!({"text": text}));
    }
    for attachment in attachments {
        openai_content.push(json!({
            "type": "image_url",
            "image_url": { "url": attachment.data_url }
        }));
        responses_content.push(json!({
            "type": "input_image",
            "image_url": attachment.data_url
        }));
        anthropic_content.push(json!({
            "type": "image",
            "source": {
                "type": "base64",
                "media_type": attachment.mime_type,
                "data": attachment.base64_data
            }
        }));
        gemini_parts.push(json!({
            "inlineData": {
                "mimeType": attachment.mime_type,
                "data": attachment.base64_data
            }
        }));
    }
    let mut root = next
        .provider_data
        .take()
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    let attachments_meta = attachments
        .iter()
        .map(|attachment| {
            json!({
                "fileName": attachment.file_name,
                "mimeType": attachment.mime_type,
                "bytesBase64": attachment.base64_data.len()
            })
        })
        .collect::<Vec<_>>();
    merge_provider_data_object(&mut root, "openai", json!({ "content": openai_content }));
    merge_provider_data_object(&mut root, "responses", json!({ "content": responses_content }));
    merge_provider_data_object(&mut root, "anthropic", json!({ "content": anthropic_content }));
    merge_provider_data_object(&mut root, "gemini", json!({ "parts": gemini_parts }));
    root.insert(
        "nativeImageAttachments".into(),
        json!({
            "count": attachments.len(),
            "attachments": attachments_meta
        }),
    );
    next.provider_data = Some(Value::Object(root));
    next
}

fn sanitize_native_image_text(content: &str) -> String {
    content
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            !(trimmed.starts_with('{') && trimmed.contains("\"attachment\""))
                && !trimmed.contains("[media attached:")
                && !trimmed
                    .get(..6)
                    .is_some_and(|prefix| prefix.eq_ignore_ascii_case("MEDIA:"))
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

fn merge_provider_data_object(root: &mut Map<String, Value>, key: &str, value: Value) {
    match root.get_mut(key).and_then(Value::as_object_mut) {
        Some(existing) => {
            if let Some(incoming) = value.as_object() {
                for (field, field_value) in incoming {
                    existing.insert(field.clone(), field_value.clone());
                }
            }
        }
        None => {
            root.insert(key.into(), value);
        }
    }
}

pub(super) async fn complete_chat_with_provider_failover(
    store: &AppStore,
    run_id: Option<&str>,
    providers: &[LlmProvider],
    persona: &Persona,
    system_prompt: String,
    history: Vec<ChatMessage>,
    user_content: &str,
    native_tools: Option<&[ToolDefinition]>,
    stream_delta_callback: Option<crate::llm::LlmDeltaCallback>,
) -> AppResult<crate::llm::LlmReply> {
    complete_chat_with_provider_failover_options(
        store,
        run_id,
        providers,
        persona,
        system_prompt,
        history,
        user_content,
        native_tools,
        stream_delta_callback,
        crate::llm::LlmCallOptions {
            fast_mode_enabled: true,
            ..crate::llm::LlmCallOptions::default()
        },
    )
    .await
}

pub(super) async fn complete_chat_with_provider_failover_options(
    store: &AppStore,
    run_id: Option<&str>,
    providers: &[LlmProvider],
    persona: &Persona,
    system_prompt: String,
    history: Vec<ChatMessage>,
    user_content: &str,
    native_tools: Option<&[ToolDefinition]>,
    stream_delta_callback: Option<crate::llm::LlmDeltaCallback>,
    base_options: crate::llm::LlmCallOptions,
) -> AppResult<crate::llm::LlmReply> {
    if providers.is_empty() {
        return Err(AppError::NotFound("llm provider".into()));
    }

    let chat_config = store.config()?.chat;
    let (mut request_history, repaired_history) = sanitize_history_for_llm_request(history);
    if repaired_history {
        if let Some(run_id) = run_id {
            append_parent_phase_event(
                store,
                run_id,
                "llm_history_repair",
                json!({
                    "kind": "tool_replay_sequence",
                    "note": "Repaired retained tool-call/tool-result history before provider request.",
                }),
            )?;
        }
    }
    let retry_count = chat_config.llm_retry_count.min(5);
    let retry_backoff_ms = chat_config.llm_retry_backoff_ms.min(60_000);
    let mut failed_providers = Vec::new();
    let mut attempts = Vec::new();
    let mut recovery_attempted = HashSet::new();
    for (index, provider) in providers.iter().enumerate() {
        let credential_binding = crate::llm::bind_runtime_credential_for_attempt(provider);
        let attempt_provider = credential_binding.provider;
        let credential_source = credential_binding.source;
        let mut attempt_index = 0usize;
        let mut max_tokens_override = None;
        let mut last_attempt_model = None::<String>;
        let result = loop {
            let attempt_number = attempt_index + 1;
            let attempt_started = Instant::now();
            let attempt_persona =
                persona_for_provider_attempt(persona, &attempt_provider, max_tokens_override);
            let attempt_model = if attempt_persona.llm_model.trim().is_empty() {
                attempt_provider.model.trim().to_string()
            } else {
                attempt_persona.llm_model.trim().to_string()
            };
            last_attempt_model = Some(attempt_model.clone());
            let mut attempt_event_provider = attempt_provider.clone();
            attempt_event_provider.model = attempt_model.clone();
            let attempt_history =
                history_with_native_image_attachments(
                    store,
                    &attempt_provider,
                    &attempt_persona,
                    &request_history,
                    user_content,
                );
            let api_hook_payload = llm_api_request_hook_payload(
                run_id,
                provider,
                attempt_number,
                &system_prompt,
                &attempt_history,
                user_content,
                native_tools,
            );
            run_pre_api_request_hooks(store, run_id.unwrap_or("llm-api"), &api_hook_payload).await;
            let result = complete_chat_attempt_with_run_interrupt(
                store,
                run_id,
                chat_config.agent_run_timeout_seconds,
                chat_config.agent_post_tool_quiet_timeout_seconds,
                attempt_started,
                crate::llm::complete_chat_with_options(
                    &attempt_provider,
                    &attempt_persona,
                    system_prompt.clone(),
                    attempt_history,
                    user_content,
                    native_tools,
                    &crate::llm::LlmCallOptions {
                        responses_reasoning_replay_enabled: chat_config
                            .responses_reasoning_replay_enabled
                            && base_options.responses_reasoning_replay_enabled,
                        fast_mode_enabled: chat_config.fast_mode_enabled
                            && base_options.fast_mode_enabled,
                        thinking_enabled: base_options.thinking_enabled,
                        stream_delta_callback: stream_delta_callback.clone(),
                    },
                ),
            )
            .await;
            let elapsed_ms = attempt_started.elapsed().as_millis();
            match result {
                Ok(reply) => {
                    run_post_api_request_hooks(
                        store,
                        run_id.unwrap_or("llm-api"),
                        &llm_api_response_hook_payload(
                            api_hook_payload.clone(),
                            elapsed_ms,
                            Some(&reply),
                            None,
                        ),
                    )
                    .await;
                    if let Some(run_id) = run_id {
                        append_llm_attempt_event(
                            store,
                            run_id,
                            &attempt_event_provider,
                            attempt_number,
                            retry_count,
                            elapsed_ms,
                            "success",
                            None,
                            None,
                            Some(&reply),
                        )?;
                    }
                    store.record_llm_credential_use(&provider.id)?;
                    break Ok(reply);
                }
                Err(error) => {
                    let kind = classify_llm_failure(&error);
                    let message = error.to_string();
                    run_post_api_request_hooks(
                        store,
                        run_id.unwrap_or("llm-api"),
                        &llm_api_response_hook_payload(
                            api_hook_payload.clone(),
                            elapsed_ms,
                            None,
                            Some((&kind, &message)),
                        ),
                    )
                    .await;
                    if let Some(run_id) = run_id {
                        append_llm_attempt_event(
                            store,
                            run_id,
                            &attempt_event_provider,
                            attempt_number,
                            retry_count,
                            elapsed_ms,
                            "error",
                            Some(kind),
                            Some(&message),
                            None,
                        )?;
                    }
                    attempts.push(crate::llm::LlmFailoverAttempt {
                        provider_id: provider.id.clone(),
                        model: attempt_model.clone(),
                        kind: kind.to_string(),
                        message: message.clone(),
                    });
                    let next_max_tokens_override =
                        next_max_tokens_override(max_tokens_override, persona.max_tokens, &message);
                    if let Some(recovery_note) = recover_llm_failure_for_provider_retry(
                        store,
                        run_id,
                        &mut request_history,
                        &error,
                        &mut recovery_attempted,
                    )? {
                        attempt_index += 1;
                        max_tokens_override = next_max_tokens_override;
                        if let Some(run_id) = run_id {
                            append_parent_phase_event(
                                store,
                                run_id,
                                "llm_retry",
                                json!({
                                    "providerId": provider.id.clone(),
                                    "providerType": provider.provider_type.clone(),
                                    "model": attempt_model.clone(),
                                    "kind": kind,
                                    "attempt": attempt_index,
                                    "maxRetries": retry_count,
                                    "delayMs": 0,
                                    "maxTokensOverride": max_tokens_override,
                                    "message": message,
                                    "recovery": recovery_note,
                                }),
                            )?;
                        }
                        continue;
                    }
                    let rotate_local_credential =
                        llm_credential_variant_should_skip_retry(provider, kind);
                    let rotate_hermes_credential = if rotate_local_credential {
                        false
                    } else if llm_failure_kind_should_rotate_credential(kind) {
                        if let Some(source) = credential_source.as_deref() {
                            mark_hermes_credential_pool_failure_for_source(
                                provider, source, kind, &message,
                            )?
                            .is_some()
                        } else {
                            mark_hermes_credential_pool_failure(provider, kind, &message)?.is_some()
                        }
                    } else {
                        false
                    };
                    let rotate_credential = rotate_local_credential || rotate_hermes_credential;
                    if rotate_credential {
                        if rotate_local_credential {
                            store.mark_llm_credential_cooldown(&provider.id, kind, &message)?;
                        }
                        if let Some(run_id) = run_id {
                            append_parent_phase_event(
                                store,
                                run_id,
                                "llm_credential_rotate",
                                json!({
                                    "providerId": provider.id.clone(),
                                    "providerType": provider.provider_type.clone(),
                                    "model": attempt_model.clone(),
                                    "kind": kind,
                                    "message": message,
                                }),
                            )?;
                        }
                    }
                    if rotate_credential
                        || attempt_index >= retry_count
                        || !llm_failure_is_retryable(kind, &message)
                    {
                        break Err(error);
                    }
                    attempt_index += 1;
                    max_tokens_override = next_max_tokens_override;
                    let delay_ms = llm_retry_delay_ms(retry_backoff_ms, attempt_index, kind);
                    if let Some(run_id) = run_id {
                        let rate_limit_guard = if kind == "rate_limit" {
                            store.token_usage().ok().map(|usage| {
                                genuine_rate_limit_guard_state(usage.get("lastRateLimit"))
                            })
                        } else {
                            None
                        };
                        append_parent_phase_event(
                            store,
                            run_id,
                            "llm_retry",
                            json!({
                                "providerId": provider.id.clone(),
                                "providerType": provider.provider_type.clone(),
                                "model": attempt_model.clone(),
                                "kind": kind,
                                "attempt": attempt_index,
                                "maxRetries": retry_count,
                                "delayMs": delay_ms,
                                "maxTokensOverride": max_tokens_override,
                                "rateLimitGuard": rate_limit_guard.unwrap_or(Value::Null),
                                "message": message,
                            }),
                        )?;
                    }
                    if !wait_llm_retry_delay_interruptible(store, run_id, delay_ms).await? {
                        if let Some(run_id) = run_id {
                            append_parent_phase_event(
                                store,
                                run_id,
                                "llm_retry_interrupted",
                                json!({
                                    "providerId": provider.id.clone(),
                                    "providerType": provider.provider_type.clone(),
                                    "model": attempt_model.clone(),
                                    "kind": kind,
                                    "attempt": attempt_index,
                                    "delayMs": delay_ms,
                                }),
                            )?;
                        }
                        break Err(AppError::Llm(
                            "Agent run interrupted during LLM retry wait.".into(),
                        ));
                    }
                }
            }
        };
        match result {
            Ok(mut reply) => {
                reply.failover_attempts = attempts;
                record_llm_usage(store, &reply)?;
                if let Some(run_id) = run_id {
                    record_short_context_real_usage_for_run(
                        store,
                        run_id,
                        &reply,
                        chat_config.short_context_token_budget,
                    )?;
                }
                if !failed_providers.is_empty() {
                    if let Some(run_id) = run_id {
                        append_parent_phase_event(
                            store,
                            run_id,
                            "llm_failover",
                            json!({
                                "finalProviderId": provider.id.clone(),
                                "finalProviderType": provider.provider_type.clone(),
                                "finalModel": reply.model.clone(),
                                "failedProviders": failed_providers,
                            }),
                        )?;
                    }
                }
                return Ok(reply);
            }
            Err(error) => {
                let kind = classify_llm_failure(&error);
                let message = error.to_string();
                failed_providers.push(json!({
                    "providerId": provider.id.clone(),
                    "providerType": provider.provider_type.clone(),
                    "model": last_attempt_model
                        .clone()
                        .unwrap_or_else(|| provider.model.clone()),
                    "kind": kind,
                    "message": message,
                }));
                if index + 1 >= providers.len() {
                    if let Some(run_id) = run_id {
                        append_parent_phase_event(
                            store,
                            run_id,
                            "llm_failover",
                            json!({
                                "finalProviderId": Value::Null,
                                "failedProviders": failed_providers,
                                "exhausted": true,
                            }),
                        )?;
                        if let Some(artifact_path) = save_llm_failure_diagnostic_artifact(
                            store,
                            run_id,
                            providers,
                            &attempts,
                            &failed_providers,
                            &error,
                        )? {
                            return Err(append_llm_diagnostic_artifact_to_error(
                                error,
                                &artifact_path,
                            ));
                        }
                    }
                    return Err(error);
                }
            }
        }
    }
    Err(AppError::Llm(
        "provider failover ended without a result".into(),
    ))
}

fn append_llm_diagnostic_artifact_to_error(error: AppError, path: &PathBuf) -> AppError {
    AppError::Llm(format!(
        "{}\nDiagnostic artifact: {}",
        error,
        path.to_string_lossy()
    ))
}

pub(super) fn save_llm_failure_diagnostic_artifact(
    store: &AppStore,
    run_id: &str,
    providers: &[LlmProvider],
    attempts: &[crate::llm::LlmFailoverAttempt],
    failed_providers: &[Value],
    error: &AppError,
) -> AppResult<Option<PathBuf>> {
    let run = store.agent_run(run_id)?;
    let error_text = error.to_string();
    let redacted_error_text = redact_sensitive_text(&error_text);
    let kind = classify_llm_failure(error);
    let provider_summaries = providers
        .iter()
        .map(|provider| {
            json!({
                "id": provider.id,
                "providerType": provider.provider_type,
                "preset": provider.preset,
                "baseUrl": provider.base_url,
                "appendChatPath": provider.append_chat_path,
                "model": provider.model,
                "enabled": provider.enabled,
                "timeoutSeconds": provider.timeout_seconds,
                "promptCacheMode": provider.prompt_cache_mode,
                "promptCacheTtl": provider.prompt_cache_ttl,
                "promptCacheLayout": provider.prompt_cache_layout
            })
        })
        .collect::<Vec<_>>();
    let attempt_summaries = attempts
        .iter()
        .map(|attempt| {
            json!({
                "providerId": attempt.provider_id,
                "model": attempt.model,
                "kind": attempt.kind,
                "message": truncate_for_prompt(&redact_sensitive_text(&attempt.message), 2000)
            })
        })
        .collect::<Vec<_>>();
    let failed_provider_summaries = failed_providers
        .iter()
        .cloned()
        .map(redact_json_value)
        .collect::<Vec<_>>();
    let diagnostic = json!({
        "kind": "llmFailureDiagnostic",
        "createdAt": now_iso(),
        "runId": run_id,
        "conversationId": run.conversation_id,
        "agentId": run.agent_id,
        "state": run.state,
        "errorKind": kind,
        "error": truncate_for_prompt(&redacted_error_text, 4000),
        "classifiedError": llm_classified_error_detail(kind, &redacted_error_text, None, None),
        "recoveryHints": llm_failure_recovery_hints(kind, &redacted_error_text),
        "providers": provider_summaries,
        "attempts": attempt_summaries,
        "failedProviders": failed_provider_summaries,
        "recentPhaseEvents": run.phase_events.iter().rev().take(24).cloned().collect::<Vec<_>>(),
        "recentToolEvents": run.tool_events.iter().rev().take(12).cloned().collect::<Vec<_>>(),
        "checkpoints": run.checkpoints.iter().rev().take(8).cloned().collect::<Vec<_>>()
    });
    let content = serde_json::to_string_pretty(&diagnostic)?;
    Ok(Some(store.save_tool_artifact(
        run_id,
        "llm_failure_diagnostic",
        &content,
    )?))
}

async fn complete_chat_attempt_with_run_interrupt<F>(
    store: &AppStore,
    run_id: Option<&str>,
    timeout_seconds: u64,
    post_tool_quiet_timeout_seconds: u64,
    attempt_started: Instant,
    future: F,
) -> AppResult<crate::llm::LlmReply>
where
    F: Future<Output = AppResult<crate::llm::LlmReply>>,
{
    let Some(run_id) = run_id else {
        return future.await;
    };

    tokio::pin!(future);
    let mut interval = tokio::time::interval(Duration::from_millis(250));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            result = &mut future => return result,
            _ = interval.tick() => {
                if let Some(error) = llm_attempt_interruption_error(
                    store,
                    run_id,
                    attempt_started,
                    timeout_seconds,
                    post_tool_quiet_timeout_seconds,
                )? {
                    return Err(error);
                }
            }
        }
    }
}

fn llm_attempt_interruption_error(
    store: &AppStore,
    run_id: &str,
    attempt_started: Instant,
    timeout_seconds: u64,
    post_tool_quiet_timeout_seconds: u64,
) -> AppResult<Option<AppError>> {
    let run = store.agent_run(run_id)?;
    if run.state == "aborted" {
        append_parent_phase_event(
            store,
            run_id,
            "llm_request_interrupted",
            json!({
                "reason": run.error.clone(),
                "state": run.state,
            }),
        )?;
        return Ok(Some(AppError::Llm(
            "agent run was aborted before the LLM provider completed".into(),
        )));
    }

    let effective_timeout = llm_attempt_effective_timeout_seconds(
        &run,
        timeout_seconds,
        post_tool_quiet_timeout_seconds,
    );
    if effective_timeout > 0
        && llm_attempt_idle_for_timeout(&run, attempt_started, effective_timeout)
    {
        let reason = llm_attempt_timeout_reason(&run, effective_timeout);
        let aborted = store.abort_agent_run(run_id, Some(reason.clone()))?;
        spawn_session_finished_hooks(
            store,
            aborted.clone(),
            json!({
                "source": "llm_attempt_timeout",
                "reason": reason.clone(),
                "timeout_seconds": effective_timeout,
            }),
        );
        append_parent_phase_event(
            store,
            run_id,
            "llm_request_interrupted",
            json!({
                "reason": reason,
                "state": aborted.state,
                "timeoutSeconds": effective_timeout,
            }),
        )?;
        store.append_message(ChatMessage::new(
            aborted.conversation_id,
            "assistant",
            format!("本轮 agent 已自动结束：{}", reason),
            "desktop-agent-error",
        ))?;
        return Ok(Some(AppError::Llm(
            "agent run timed out before the LLM provider completed".into(),
        )));
    }

    Ok(None)
}

fn llm_attempt_effective_timeout_seconds(
    run: &AgentRunRecord,
    timeout_seconds: u64,
    post_tool_quiet_timeout_seconds: u64,
) -> u64 {
    if post_tool_quiet_timeout_seconds > 0
        && run
            .last_activity_desc
            .as_deref()
            .map(str::trim)
            .is_some_and(|activity| {
                activity.starts_with("tool completed:")
                    || activity.starts_with("tool failed:")
                    || activity.starts_with("tool error:")
            })
    {
        if timeout_seconds > 0 {
            post_tool_quiet_timeout_seconds.min(timeout_seconds)
        } else {
            post_tool_quiet_timeout_seconds
        }
    } else {
        timeout_seconds
    }
}

fn llm_attempt_idle_for_timeout(
    run: &AgentRunRecord,
    attempt_started: Instant,
    timeout_seconds: u64,
) -> bool {
    if let Some(activity_at) = run
        .last_activity_at
        .as_deref()
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .map(|value| value.with_timezone(&Utc))
    {
        return Utc::now().signed_duration_since(activity_at).num_seconds()
            >= timeout_seconds as i64;
    }
    attempt_started.elapsed() >= Duration::from_secs(timeout_seconds)
}

fn llm_attempt_timeout_reason(run: &AgentRunRecord, timeout_seconds: u64) -> String {
    if let Some(activity) = run
        .last_activity_desc
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        return format!(
            "Agent run timed out after {timeout_seconds}s of inactivity; last activity: {activity}."
        );
    }
    format!("Agent run timed out after {timeout_seconds}s.")
}

pub(super) fn sanitize_history_for_llm_request(
    history: Vec<ChatMessage>,
) -> (Vec<ChatMessage>, bool) {
    let before = history_sequence_signature(&history);
    let repaired = sanitize_retained_tool_pairs(history);
    let changed = before != history_sequence_signature(&repaired);
    (repaired, changed)
}

fn history_sequence_signature(messages: &[ChatMessage]) -> Vec<(String, String)> {
    messages
        .iter()
        .map(|message| (message.role.clone(), message.content.clone()))
        .collect()
}

pub(super) fn append_llm_attempt_event(
    store: &AppStore,
    run_id: &str,
    provider: &LlmProvider,
    attempt: usize,
    max_retries: usize,
    elapsed_ms: u128,
    outcome: &str,
    kind: Option<&str>,
    message: Option<&str>,
    reply: Option<&crate::llm::LlmReply>,
) -> AppResult<()> {
    let mut detail = json!({
        "providerId": provider.id.clone(),
        "providerType": provider.provider_type.clone(),
        "model": provider.model.clone(),
        "baseUrl": provider.base_url.clone(),
        "attempt": attempt,
        "maxRetries": max_retries,
        "elapsedMs": elapsed_ms,
        "outcome": outcome,
    });
    if let Some(kind) = kind {
        detail["kind"] = json!(kind);
    }
    if let Some(message) = message {
        detail["message"] = json!(truncate_for_prompt(message, 500));
        if let Some(transport_diagnostics) = error_transport_diagnostics(message) {
            detail["transportDiagnostics"] = transport_diagnostics;
        }
        if let Some(kind) = kind {
            detail["recoveryHints"] = llm_failure_recovery_hints(kind, message);
            detail["classifiedError"] = llm_classified_error_detail(
                kind,
                message,
                Some(provider.provider_type.as_str()),
                Some(provider.model.as_str()),
            );
        }
    }
    if let Some(reply) = reply {
        detail["finishReason"] = json!(reply.finish_reason.clone());
        detail["promptTokens"] = json!(reply.prompt_tokens);
        detail["completionTokens"] = json!(reply.completion_tokens);
        detail["cacheReadTokens"] = json!(reply.cache_read_tokens);
        detail["cacheWriteTokens"] = json!(reply.cache_write_tokens);
        detail["reasoningTokens"] = json!(reply.reasoning_tokens);
        if let Some(cost) = reply.estimated_cost_usd {
            detail["estimatedCostUsd"] = json!(cost);
        }
        if let Some(rate_limit_state) = reply.rate_limit_state.clone() {
            detail["rateLimitState"] = rate_limit_state;
        }
        if let Some(transport_diagnostics) = reply.transport_diagnostics.clone() {
            detail["transportDiagnostics"] = transport_diagnostics;
        }
    }
    detail["runnerDiagnostics"] = llm_runner_diagnostics(
        provider,
        elapsed_ms,
        outcome,
        reply,
        detail.get("transportDiagnostics"),
    );
    append_parent_phase_event(store, run_id, "llm_attempt", detail)
}

fn llm_runner_diagnostics(
    provider: &LlmProvider,
    elapsed_ms: u128,
    outcome: &str,
    reply: Option<&crate::llm::LlmReply>,
    transport_diagnostics: Option<&Value>,
) -> Value {
    let status = transport_diagnostics
        .and_then(|value| value.get("status").or_else(|| value.get("httpStatus")))
        .and_then(Value::as_u64);
    let transport_elapsed_ms = transport_diagnostics
        .and_then(|value| value.get("elapsedMs"))
        .and_then(Value::as_u64)
        .unwrap_or_else(|| elapsed_ms.min(u64::MAX as u128) as u64);
    let headers = transport_diagnostics
        .and_then(|value| value.get("headers"))
        .cloned()
        .unwrap_or_else(|| json!({}));
    let endpoint = transport_diagnostics
        .and_then(|value| value.get("endpoint"))
        .cloned()
        .unwrap_or(Value::Null);
    let transport = transport_diagnostics
        .and_then(|value| value.get("transport"))
        .cloned()
        .unwrap_or(Value::Null);
    let response_bytes = reply
        .map(|reply| reply.content.as_bytes().len() as u64)
        .unwrap_or(0);

    json!({
        "mode": "non_stream",
        "streaming": false,
        "outcome": outcome,
        "chunks": 0,
        "bytes": 0,
        "responseBytes": response_bytes,
        "ttfbMs": Value::Null,
        "elapsedMs": transport_elapsed_ms,
        "staleTimeoutSeconds": provider.timeout_seconds,
        "httpStatus": status,
        "transport": transport,
        "endpoint": endpoint,
        "headers": headers
    })
}

fn error_transport_diagnostics(message: &str) -> Option<Value> {
    let status = extract_http_status_from_error(message);
    let preview = truncate_for_prompt(message, 500);
    status.map(|status| {
        json!({
            "capturedAt": now_iso(),
            "httpStatus": status,
            "errorPreview": preview
        })
    })
}

fn extract_http_status_from_error(message: &str) -> Option<u16> {
    let lower = message.to_ascii_lowercase();
    let status_markers = [
        "provider returned ",
        "invalid llm response (",
        "invalid responses llm response (",
        "invalid anthropic response (",
        "invalid gemini response (",
        "invalid bedrock response (",
    ];
    for marker in status_markers {
        let Some(index) = lower.find(marker) else {
            continue;
        };
        let start = index + marker.len();
        let digits = lower[start..]
            .chars()
            .skip_while(|ch| !ch.is_ascii_digit())
            .take_while(|ch| ch.is_ascii_digit())
            .collect::<String>();
        if digits.len() == 3 {
            if let Ok(status) = digits.parse::<u16>() {
                return Some(status);
            }
        }
    }
    None
}
