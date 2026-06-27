use std::{
    collections::{HashMap, HashSet},
    env,
    hash::{Hash, Hasher},
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{
    error::{AppError, AppResult},
    models::{AgentDefinition, ChatConfig, ChatMessage, Conversation, LlmProvider, Persona},
    store::AppStore,
};
use serde_json::Value;

use super::{
    align_compression_start_forward, complete_chat_with_provider_failover, effective_llm_persona,
    list_agent_auxiliary_task_assignments, memory_pre_compress_context,
    run_context_engine_compress, selected_provider_id, strip_historical_media_payloads,
    tail_start_preserving_latest_user_and_token_budget, ContextEngineCompressedMessage,
    LEGACY_SHORT_CONTEXT_SUMMARY_PREFIX, SHORT_CONTEXT_SUMMARY_PREFIX,
};

const SUMMARY_MIN_TARGET_TOKENS: usize = 2_000;
const SUMMARY_MAX_TARGET_TOKENS: usize = 12_000;
const SUMMARY_TARGET_RATIO_NUMERATOR: usize = 1;
const SUMMARY_TARGET_RATIO_DENOMINATOR: usize = 5;
const SUMMARY_FAILURE_COOLDOWN_MS: u64 = 600_000;

pub(super) async fn handle_compact_control_command(
    store: &AppStore,
    conversation: &Conversation,
    persona: &Persona,
    agent: &AgentDefinition,
    argument_raw: &str,
) -> AppResult<String> {
    let messages = store.messages(&conversation.id, None)?;
    if messages.is_empty() {
        return Ok("Nothing to compress - conversation is empty.".into());
    }
    if messages.len() < 4 {
        return Ok(format!(
            "Nothing to compress yet - conversation has only {} messages. 当前会话消息太少，暂不需要压缩。",
            messages.len()
        ));
    }
    let config = store.config()?;
    let default_keep = (config.chat.max_context_rounds.max(1) * 2 + 1).clamp(3, 60);
    let control_args = parse_compact_control_args(argument_raw, default_keep);
    let keep_messages = control_args.keep_messages;
    let focus = control_args.focus.as_str();
    let older_count = tail_start_preserving_latest_user_and_token_budget(
        &messages,
        messages.len().saturating_sub(keep_messages.max(1)),
        config.chat.short_context_token_budget / 2,
    );
    if older_count < 2 {
        return Ok(format!(
            "当前会话可压缩历史不足；目前消息 {} 条，保留 {} 条。",
            messages.len(),
            keep_messages
        ));
    }
    let boundary_message = &messages[older_count - 1];
    let mut state = store.short_context(&conversation.id)?;
    if state.boundary_id.as_deref() == Some(boundary_message.id.as_str()) {
        let before_count = compressed_request_message_count(&state.summary, messages.len());
        let before_tokens = estimate_compressed_request_tokens(&state.summary, &messages);
        let feedback =
            manual_compression_feedback(before_count, before_count, before_tokens, before_tokens);
        return Ok(format!(
            "当前会话已压缩到该边界：{}。summaryTokens={}\n{}",
            boundary_message.id, state.summary_tokens, feedback
        ));
    }
    let start = state
        .boundary_id
        .as_deref()
        .and_then(|id| messages.iter().position(|message| message.id == id))
        .map(|idx| idx + 1)
        .unwrap_or(0);
    let start = align_compression_start_forward(&messages, start);
    if start >= older_count {
        let before_count =
            compressed_request_message_count(&state.summary, messages.len().saturating_sub(start));
        let before_tokens = estimate_compressed_request_tokens(&state.summary, &messages[start..]);
        let feedback =
            manual_compression_feedback(before_count, before_count, before_tokens, before_tokens);
        return Ok(format!("当前会话没有新的旧历史需要压缩。\n{}", feedback));
    }

    let before_count =
        compressed_request_message_count(&state.summary, messages.len().saturating_sub(start));
    let before_tokens = estimate_compressed_request_tokens(&state.summary, &messages[start..]);
    let mut transcript = render_messages_for_summary(&messages[start..older_count]);
    let memory_context = memory_pre_compress_context(store, persona, &transcript)?;
    if !memory_context.trim().is_empty() {
        transcript = format!("{memory_context}\n{transcript}");
    }
    if !focus.trim().is_empty() {
        transcript = format!(
            "Manual compaction focus: {}\n\n{}",
            compact_text(focus.trim(), 1200),
            transcript
        );
    }
    let selected_context_engine = selected_context_engine_name();
    let dynamic_context_summary = selected_context_engine
        .as_deref()
        .filter(|engine| !engine.eq_ignore_ascii_case("compressor"))
        .and_then(|engine| {
            match run_context_engine_compress(
                engine,
                &messages[start..older_count],
                before_tokens,
                (!focus.trim().is_empty()).then_some(focus.trim()),
            ) {
                Ok(compressed) => Some(context_engine_messages_to_summary(
                    state.summary.as_str(),
                    engine,
                    &compressed,
                    config.chat.short_context_token_budget,
                )),
                Err(error) => {
                    eprintln!(
                        "SynthChat context engine '{engine}' manual compress failed: {error}"
                    );
                    None
                }
            }
        });
    let mut used_fallback = false;
    let abort_on_summary_failure = config.chat.short_context_abort_on_summary_failure;
    let summary_failure_cooldown = summary_failure_cooldown_remaining_seconds(&state);
    let mut used_dynamic_context_engine = false;
    let summary = if let Some(summary) = dynamic_context_summary {
        used_dynamic_context_engine = true;
        record_summary_success(&mut state);
        summary
    } else {
        let providers = store.provider_candidates(selected_provider_id(persona, agent))?;
        let provider = providers
            .first()
            .ok_or_else(|| AppError::NotFound("llm provider".into()))?;
        let effective_persona = effective_llm_persona(persona, agent);
        let summary_plan =
            build_summary_provider_plan(store, &config.chat, &providers, &effective_persona)?;
        if summary_plan.aux_label.is_none()
            && (provider.provider_type == "echo"
                || (provider.base_url.trim().is_empty()
                    && provider.provider_type.to_lowercase() != "gemini"))
        {
            fallback_short_context_summary(
                state.summary.as_str(),
                &transcript,
                config.chat.short_context_token_budget,
            )
        } else if let Some(remaining_seconds) =
            summary_failure_cooldown.filter(|_| !control_args.force)
        {
            if abort_on_summary_failure {
                state.last_compress_aborted = true;
                state.last_summary_fallback_used = false;
                state.last_summary_dropped_count = older_count.saturating_sub(start);
                let error = state
                    .last_summary_error
                    .clone()
                    .unwrap_or_else(|| "unknown summary error".into());
                store.save_short_context(state)?;
                return Ok(format!(
                    "压缩已中止：摘要模型仍在失败 cooldown 中（约 {remaining_seconds}s），未丢弃历史消息。上一错误：{}。请修复摘要模型或关闭 shortContextAbortOnSummaryFailure 后重试 /compact。",
                    compact_text(&error, 500)
                ));
            }
            used_fallback = true;
            state.last_summary_fallback_used = true;
            state.last_summary_dropped_count = older_count.saturating_sub(start);
            fallback_short_context_summary_after_note(
                state.summary.as_str(),
                &transcript,
                config.chat.short_context_token_budget,
                &format!(
                    "Context summarizer is in failure cooldown for about {remaining_seconds}s after the previous error: {}",
                    state
                        .last_summary_error
                        .as_deref()
                        .unwrap_or("unknown summary error")
                ),
            )
        } else {
            let previous_summary = state.summary.clone();
            match summarize_short_context_with_main_fallback(
                store,
                &summary_plan,
                &providers,
                &effective_persona,
                previous_summary.as_str(),
                &transcript,
                config.chat.short_context_token_budget,
                &mut state,
            )
            .await
            {
                Ok(summary) => {
                    record_summary_success(&mut state);
                    summary
                }
                Err(error) => {
                    if abort_on_summary_failure {
                        record_summary_abort(
                            &mut state,
                            &error.to_string(),
                            older_count.saturating_sub(start),
                        );
                        store.save_short_context(state)?;
                        return Ok(format!(
                            "压缩已中止：摘要模型失败，未丢弃历史消息。错误：{}。当前会话会冻结 agent 继续运行，直到手动 /compact 成功或关闭 shortContextAbortOnSummaryFailure。",
                            compact_text(&error.to_string(), 500)
                        ));
                    }
                    used_fallback = true;
                    record_summary_failure(
                        &mut state,
                        &error.to_string(),
                        older_count.saturating_sub(start),
                    );
                    fallback_short_context_summary_after_error(
                        state.summary.as_str(),
                        &transcript,
                        config.chat.short_context_token_budget,
                        &error,
                    )
                }
            }
        }
    };

    let after_tokens = estimate_compressed_request_tokens(&summary, &messages[older_count..]);
    state.boundary_id = Some(boundary_message.id.clone());
    state.summary_tokens = estimate_tokens(&summary);
    state.summary_messages = older_count;
    state.summary = summary;
    record_compression_effectiveness(&mut state, before_tokens, after_tokens);
    let saved = store.save_short_context(state)?;
    let retained_messages = messages.len().saturating_sub(older_count);
    let after_count = compressed_request_message_count(&saved.summary, retained_messages);
    let feedback =
        manual_compression_feedback(before_count, after_count, before_tokens, after_tokens);
    let hermes_feedback = format!(
        "Context compressed: {} -> {} messages\n~{} -> ~{} tokens",
        before_count,
        after_count,
        format_token_count(before_tokens),
        format_token_count(after_tokens)
    );
    let fallback_note = if used_fallback {
        " fallback=deterministic"
    } else {
        ""
    };
    let context_engine_note = if used_dynamic_context_engine {
        format!(
            " contextEngine={}",
            selected_context_engine.as_deref().unwrap_or("unknown")
        )
    } else {
        String::new()
    };
    Ok(format!(
        "{hermes_feedback}\n已手动压缩当前会话历史：compressedMessages={} retainedMessages={} summaryTokens={} boundary={}{}{}{}\n{}",
        older_count.saturating_sub(start),
        retained_messages,
        saved.summary_tokens,
        saved.boundary_id.as_deref().unwrap_or("-"),
        fallback_note,
        context_engine_note,
        if control_args.force { " force=true" } else { "" },
        feedback
    ))
}

struct SummaryProviderPlan {
    providers: Vec<LlmProvider>,
    persona: Persona,
    aux_label: Option<String>,
}

fn build_summary_provider_plan(
    store: &AppStore,
    chat_config: &ChatConfig,
    main_providers: &[LlmProvider],
    main_persona: &Persona,
) -> AppResult<SummaryProviderPlan> {
    let legacy_provider_id = chat_config.short_context_summary_provider_id.trim();
    let legacy_model = chat_config.short_context_summary_model.trim();
    let compression_assignment = if legacy_provider_id.is_empty() && legacy_model.is_empty() {
        list_agent_auxiliary_task_assignments(store)?
            .into_iter()
            .find(|assignment| assignment.key == "compression")
    } else {
        None
    };
    let assignment_provider = compression_assignment
        .as_ref()
        .map(|assignment| assignment.provider.trim())
        .unwrap_or("");
    let assignment_provider_id = if assignment_provider.eq_ignore_ascii_case("auto") {
        ""
    } else {
        assignment_provider
    };
    let provider_id = legacy_provider_id
        .to_string()
        .or_else_nonempty(|| Some(assignment_provider_id.to_string()))
        .unwrap_or_default();
    let model = legacy_model
        .to_string()
        .or_else_nonempty(|| {
            compression_assignment
                .as_ref()
                .map(|assignment| assignment.model.clone())
        })
        .unwrap_or_default();
    let custom_base_url = compression_assignment
        .as_ref()
        .map(|assignment| assignment.base_url.trim())
        .unwrap_or("");
    let custom_api_key = compression_assignment
        .as_ref()
        .map(|assignment| assignment.api_key.trim())
        .unwrap_or("");
    let custom_timeout = compression_assignment
        .as_ref()
        .map(|assignment| assignment.timeout)
        .unwrap_or(60);

    if provider_id.is_empty() && model.is_empty() && custom_base_url.is_empty() {
        return Ok(SummaryProviderPlan {
            providers: main_providers.to_vec(),
            persona: main_persona.clone(),
            aux_label: None,
        });
    }

    let mut providers = if !custom_base_url.is_empty() {
        vec![LlmProvider {
            id: "auxiliary-compression-custom".into(),
            name: "Compression auxiliary".into(),
            provider_type: "openai_compatible".into(),
            base_url: custom_base_url.into(),
            append_chat_path: true,
            api_key: (!custom_api_key.is_empty()).then(|| custom_api_key.to_string()),
            model: model
                .to_string()
                .or_else_nonempty(|| {
                    main_providers
                        .first()
                        .map(|provider| provider.model.clone())
                })
                .unwrap_or_default(),
            enabled: true,
            timeout_seconds: custom_timeout,
            ..LlmProvider::default()
        }]
    } else if provider_id.is_empty() {
        main_providers.to_vec()
    } else {
        let mut candidates = store.provider_candidates(Some(&provider_id))?;
        let credential_prefix = format!("{}:cred-", provider_id);
        candidates.retain(|provider| {
            provider.id == provider_id || provider.id.starts_with(&credential_prefix)
        });
        if candidates.is_empty() {
            return Err(AppError::NotFound(format!(
                "summary llm provider {provider_id}"
            )));
        }
        candidates
    };
    let mut persona = main_persona.clone();
    if !provider_id.is_empty() {
        persona.llm_provider = provider_id.to_string();
    }
    if model.is_empty() {
        persona.llm_model.clear();
    } else {
        persona.llm_model = model.to_string();
        for provider in &mut providers {
            provider.model = model.to_string();
        }
    }
    let provider_label = if !custom_base_url.is_empty() {
        "custom".to_string()
    } else {
        provider_id.to_string()
    }
    .or_else_nonempty(|| providers.first().map(|provider| provider.id.clone()))
    .unwrap_or_else(|| "main".into());
    let model_label = model
        .clone()
        .or_else_nonempty(|| providers.first().map(|provider| provider.model.clone()))
        .unwrap_or_else(|| "default".into());
    Ok(SummaryProviderPlan {
        providers,
        persona,
        aux_label: Some(format!("{provider_label}/{model_label}")),
    })
}

trait NonEmptyStringOption {
    fn or_else_nonempty<F>(self, fallback: F) -> Option<String>
    where
        F: FnOnce() -> Option<String>;
}

impl NonEmptyStringOption for String {
    fn or_else_nonempty<F>(self, fallback: F) -> Option<String>
    where
        F: FnOnce() -> Option<String>,
    {
        if self.trim().is_empty() {
            fallback().filter(|value| !value.trim().is_empty())
        } else {
            Some(self)
        }
    }
}

fn compressed_request_message_count(summary: &str, visible_messages: usize) -> usize {
    visible_messages + usize::from(!summary.trim().is_empty())
}

pub(super) fn selected_context_engine_name() -> Option<String> {
    env::var("SYNTHCHAT_CONTEXT_ENGINE")
        .or_else(|_| env::var("HERMES_CONTEXT_ENGINE"))
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub(super) fn context_engine_messages_to_summary(
    previous_summary: &str,
    engine_name: &str,
    messages: &[ContextEngineCompressedMessage],
    token_budget: usize,
) -> String {
    let mut sections = Vec::new();
    let previous = strip_short_context_summary_prefix(previous_summary)
        .trim()
        .to_string();
    if !previous.is_empty() {
        sections.push(format!("## Previous Summary\n{previous}"));
    }
    sections.push(format!(
        "## Dynamic Context Engine Output\nengine={engine_name}\n{}",
        messages
            .iter()
            .map(|message| format!(
                "[{}] {}",
                message.role,
                compact_text(&message.content, 2400)
            ))
            .collect::<Vec<_>>()
            .join("\n")
    ));
    sections.push(
        "## Critical Context\nThis summary was produced by a Hermes context-engine plugin through SynthChat's bounded Python bridge. Treat it as compressed historical context; the latest visible user message remains authoritative."
            .into(),
    );
    normalize_short_context_summary(&sections.join("\n\n"), token_budget.max(500) * 4)
}

fn estimate_compressed_request_tokens(summary: &str, visible_messages: &[ChatMessage]) -> usize {
    let summary_tokens = if summary.trim().is_empty() {
        0
    } else {
        estimate_tokens(summary)
    };
    summary_tokens + estimate_tokens(&render_messages_for_summary(visible_messages))
}

pub(super) fn compression_savings_pct(before_tokens: usize, after_tokens: usize) -> f64 {
    if before_tokens == 0 {
        return 0.0;
    }
    let saved = before_tokens.saturating_sub(after_tokens);
    (saved as f64 / before_tokens as f64) * 100.0
}

pub(super) fn record_compression_effectiveness(
    short_context: &mut crate::models::ShortContextState,
    before_tokens: usize,
    after_tokens: usize,
) {
    let savings_pct = compression_savings_pct(before_tokens, after_tokens);
    short_context.last_compression_savings_pct = savings_pct;
    if savings_pct < 10.0 {
        short_context.ineffective_compression_count = short_context
            .ineffective_compression_count
            .saturating_add(1);
    } else {
        short_context.ineffective_compression_count = 0;
    }
}

pub(super) fn compression_anti_thrash_skip_note(
    short_context: &crate::models::ShortContextState,
) -> Option<String> {
    if short_context.ineffective_compression_count < 2 {
        return None;
    }
    Some(format!(
        "Compression skipped: last {} compression(s) saved <10% each; latest savings {:.1}%. Use /compact here <focus> for manual focused compression.",
        short_context.ineffective_compression_count,
        short_context.last_compression_savings_pct
    ))
}

pub(super) fn manual_compression_feedback(
    before_count: usize,
    after_count: usize,
    before_tokens: usize,
    after_tokens: usize,
) -> String {
    let noop = before_count == after_count && before_tokens == after_tokens;
    let headline = if noop {
        format!("No changes from compression: {} messages", before_count)
    } else {
        format!("Compressed: {} -> {} messages", before_count, after_count)
    };
    let token_line = if noop && before_tokens == after_tokens {
        format!(
            "Approx request size: ~{} tokens (unchanged)",
            format_token_count(before_tokens)
        )
    } else {
        format!(
            "Approx request size: ~{} -> ~{} tokens",
            format_token_count(before_tokens),
            format_token_count(after_tokens)
        )
    };
    let note = if !noop && after_count < before_count && after_tokens > before_tokens {
        Some(
            "Note: fewer messages can still raise this estimate when compression rewrites the transcript into denser summaries.",
        )
    } else {
        None
    };
    [Some(headline), Some(token_line), note.map(str::to_string)]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_token_count(value: usize) -> String {
    let raw = value.to_string();
    let mut output = String::with_capacity(raw.len() + raw.len() / 3);
    for (idx, ch) in raw.chars().rev().enumerate() {
        if idx > 0 && idx % 3 == 0 {
            output.push(',');
        }
        output.push(ch);
    }
    output.chars().rev().collect()
}

pub(super) struct CompactControlArgs {
    pub(super) keep_messages: usize,
    pub(super) focus: String,
    pub(super) force: bool,
}

pub(super) fn parse_compact_control_args(
    argument_raw: &str,
    default_keep: usize,
) -> CompactControlArgs {
    let argument = argument_raw.trim();
    if argument.is_empty() {
        return CompactControlArgs {
            keep_messages: default_keep,
            focus: String::new(),
            force: false,
        };
    }
    let mut force = false;
    let parts = argument
        .split_whitespace()
        .filter(|part| {
            if part.eq_ignore_ascii_case("force") || part.eq_ignore_ascii_case("--force") {
                force = true;
                false
            } else {
                true
            }
        })
        .collect::<Vec<_>>();
    if parts.is_empty() {
        return CompactControlArgs {
            keep_messages: default_keep,
            focus: String::new(),
            force,
        };
    }
    if parts
        .first()
        .map(|part| part.eq_ignore_ascii_case("here"))
        .unwrap_or(false)
    {
        let keep_exchanges = parts
            .get(1)
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(3)
            .clamp(1, 30);
        let focus_start = if parts
            .get(1)
            .and_then(|value| value.parse::<usize>().ok())
            .is_some()
        {
            2
        } else {
            1
        };
        return CompactControlArgs {
            keep_messages: keep_exchanges * 2 + 1,
            focus: parts[focus_start..].join(" "),
            force,
        };
    }
    if parts
        .first()
        .map(|part| part.eq_ignore_ascii_case("--keep"))
        .unwrap_or(false)
    {
        if let Some(keep) = parts.get(1).and_then(|value| value.parse::<usize>().ok()) {
            return CompactControlArgs {
                keep_messages: keep.clamp(1, 80),
                focus: parts[2..].join(" "),
                force,
            };
        }
    }
    CompactControlArgs {
        keep_messages: default_keep,
        focus: parts.join(" "),
        force,
    }
}

pub(super) fn render_messages_for_summary(messages: &[ChatMessage]) -> String {
    let duplicate_tool_outputs = duplicate_old_tool_output_indexes(messages);
    messages
        .iter()
        .enumerate()
        .filter(|(_, message)| matches!(message.role.as_str(), "user" | "assistant" | "tool"))
        .map(|(idx, message)| {
            let stripped = strip_historical_media_payloads(&message.content);
            let content = if message.role == "tool" {
                prune_tool_output_for_summary(&stripped, duplicate_tool_outputs.contains(&idx))
            } else if message.role == "assistant" {
                assistant_content_for_summary(&stripped)
            } else {
                compact_text(&stripped, 2400)
            };
            format!("[{} at {}] {}", message.role, message.created_at, content)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn assistant_content_for_summary(content: &str) -> String {
    if let Some(tool_calls) = summarize_assistant_tool_calls(content) {
        return tool_calls;
    }
    compact_text(content, 2400)
}

fn summarize_assistant_tool_calls(content: &str) -> Option<String> {
    let parsed = serde_json::from_str::<Value>(content.trim()).ok()?;
    let calls = parsed
        .get("tool_calls")
        .or_else(|| parsed.get("toolCalls"))
        .and_then(Value::as_array)?;
    if calls.is_empty() {
        return None;
    }
    let mut lines = vec!["[Assistant tool calls summarized for context compression]".to_string()];
    for call in calls.iter().take(12) {
        let name = tool_call_name(call);
        let arguments = tool_call_arguments(call)
            .map(truncated_tool_arguments_for_summary)
            .unwrap_or_else(|| "{}".into());
        lines.push(format!("  {}({})", name, arguments));
    }
    if calls.len() > 12 {
        lines.push(format!("  ... {} more tool call(s)", calls.len() - 12));
    }
    Some(lines.join("\n"))
}

fn tool_call_name(call: &Value) -> String {
    call.get("name")
        .or_else(|| call.get("tool"))
        .or_else(|| call.get("toolName"))
        .or_else(|| call.pointer("/function/name"))
        .and_then(Value::as_str)
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

fn truncated_tool_arguments_for_summary(arguments: &Value) -> String {
    let mut value = if let Some(raw) = arguments.as_str() {
        serde_json::from_str::<Value>(raw).unwrap_or_else(|_| Value::String(raw.to_string()))
    } else {
        arguments.clone()
    };
    truncate_json_string_leaves(&mut value, 200);
    let rendered = if let Value::String(raw) = &value {
        raw.clone()
    } else {
        serde_json::to_string(&value).unwrap_or_else(|_| value.to_string())
    };
    compact_text(&rendered, 1500)
}

fn truncate_json_string_leaves(value: &mut Value, head_chars: usize) {
    match value {
        Value::String(text) => {
            let char_count = text.chars().count();
            if char_count > head_chars + 80 {
                let head = text.chars().take(head_chars).collect::<String>();
                *text = format!(
                    "{}...[truncated tool argument string: originalChars={}]",
                    head, char_count
                );
            }
        }
        Value::Array(items) => {
            for item in items {
                truncate_json_string_leaves(item, head_chars);
            }
        }
        Value::Object(object) => {
            for value in object.values_mut() {
                truncate_json_string_leaves(value, head_chars);
            }
        }
        _ => {}
    }
}

fn duplicate_old_tool_output_indexes(messages: &[ChatMessage]) -> HashSet<usize> {
    let mut seen: HashMap<u64, usize> = HashMap::new();
    let mut duplicates = HashSet::new();
    for (idx, message) in messages.iter().enumerate().rev() {
        if message.role != "tool" {
            continue;
        }
        let content = strip_historical_media_payloads(&message.content);
        if content.chars().count() < 200 {
            continue;
        }
        let hash = stable_text_hash(&content);
        if seen.insert(hash, idx).is_some() {
            duplicates.insert(idx);
        }
    }
    duplicates
}

fn stable_text_hash(value: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

fn prune_tool_output_for_summary(content: &str, duplicate: bool) -> String {
    if duplicate {
        return "[Duplicate tool output - same content as a more recent call]".into();
    }
    let char_count = content.chars().count();
    if char_count <= 1600 {
        return compact_text(content, 2400);
    }
    let head = content.chars().take(650).collect::<String>();
    let tail = content
        .chars()
        .rev()
        .take(650)
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>();
    format!(
        "[Old tool output summarized for context compression; originalChars={}]\nHead:\n{}\nTail:\n{}",
        char_count,
        head.trim(),
        tail.trim()
    )
}

async fn summarize_short_context(
    store: &AppStore,
    providers: &[LlmProvider],
    persona: &Persona,
    previous_summary: &str,
    transcript: &str,
    token_budget: usize,
) -> AppResult<String> {
    let target_tokens = compute_summary_token_budget(transcript, token_budget);
    let system_prompt = [
        "You are a summarization agent creating a context checkpoint for an agent runtime.",
        "Preserve durable facts, user goals, decisions, constraints, tool outcomes, unresolved blockers, and file/path references.",
        "Write in the same language the user used. Remove pleasantries and duplicated details. Do not invent facts.",
        "Never include API keys, tokens, passwords, secrets, credentials, or connection strings; replace values with [REDACTED].",
        "Return only a structured handoff using these headings when applicable: ## Active Task, ## Goal, ## Constraints & Preferences, ## Completed Actions, ## Active State, ## In Progress, ## Blocked, ## Key Decisions, ## Resolved Questions, ## Pending User Asks, ## Relevant Files, ## Remaining Work, ## Critical Context.",
    ]
    .join("\n");
    let template = summary_template(target_tokens);
    let user_prompt = if previous_summary.trim().is_empty() {
        format!(
            "Create a structured checkpoint summary for the conversation after earlier turns are compacted.\n\nTranscript to summarize:\n{}\n\nUse this exact structure:\n\n{}",
            transcript, template
        )
    } else {
        format!(
            "Update the rolling context compaction summary. Preserve existing relevant information, add new completed actions, move answered questions to Resolved Questions, and update Active Task to the most recent unfulfilled user ask.\n\nPrevious summary:\n{}\n\nNew transcript to incorporate:\n{}\n\nUse this exact structure:\n\n{}",
            previous_summary.trim(),
            transcript,
            template
        )
    };
    let message = ChatMessage::new(
        "__compact__".into(),
        "user",
        user_prompt.clone(),
        "internal",
    );
    let reply = complete_chat_with_provider_failover(
        store,
        None,
        providers,
        persona,
        system_prompt,
        vec![message],
        &user_prompt,
        None,
        None,
    )
    .await?;
    Ok(normalize_short_context_summary(
        &reply.content,
        target_tokens.max(500) * 5,
    ))
}

async fn summarize_short_context_with_main_fallback(
    store: &AppStore,
    summary_plan: &SummaryProviderPlan,
    main_providers: &[LlmProvider],
    main_persona: &Persona,
    previous_summary: &str,
    transcript: &str,
    token_budget: usize,
    short_context: &mut crate::models::ShortContextState,
) -> AppResult<String> {
    if summary_plan.aux_label.is_none() {
        clear_aux_summary_failure(short_context);
        return summarize_short_context(
            store,
            &summary_plan.providers,
            &summary_plan.persona,
            previous_summary,
            transcript,
            token_budget,
        )
        .await;
    }

    match summarize_short_context(
        store,
        &summary_plan.providers,
        &summary_plan.persona,
        previous_summary,
        transcript,
        token_budget,
    )
    .await
    {
        Ok(summary) => {
            clear_aux_summary_failure(short_context);
            Ok(summary)
        }
        Err(aux_error) => {
            record_aux_summary_failure(
                short_context,
                summary_plan.aux_label.as_deref().unwrap_or("auxiliary"),
                &aux_error.to_string(),
            );
            summarize_short_context(
                store,
                main_providers,
                main_persona,
                previous_summary,
                transcript,
                token_budget,
            )
            .await
        }
    }
}

pub(super) fn compute_summary_token_budget(transcript: &str, token_budget: usize) -> usize {
    let content_tokens = estimate_tokens(transcript);
    let scaled = content_tokens.saturating_mul(SUMMARY_TARGET_RATIO_NUMERATOR)
        / SUMMARY_TARGET_RATIO_DENOMINATOR;
    let max_for_context = (token_budget.max(4_000) / 2).min(SUMMARY_MAX_TARGET_TOKENS);
    scaled
        .max(SUMMARY_MIN_TARGET_TOKENS)
        .min(max_for_context.max(SUMMARY_MIN_TARGET_TOKENS))
}

fn summary_template(target_tokens: usize) -> String {
    format!(
        r#"## Active Task
[The user's most recent unfulfilled ask, question, decision request, or reverse signal. If none, write "None."]

## Goal
[What the user is trying to accomplish overall.]

## Constraints & Preferences
[User preferences, coding style, constraints, and important decisions.]

## Completed Actions
[Numbered concrete actions with target and outcome. Include tool/command names, file paths, line numbers, and test results when available.]

## Active State
[Current working state: modified files, test status, running processes, environment details that matter.]

## In Progress
[Work underway when compaction happened.]

## Blocked
[Unresolved blockers or exact errors. If none, write "None."]

## Key Decisions
[Important technical decisions and why.]

## Resolved Questions
[Questions already answered, with the answer, so they are not repeated.]

## Pending User Asks
[Questions or requests not yet answered or fulfilled. If none, write "None."]

## Relevant Files
[Files read, modified, or created, with a brief note.]

## Remaining Work
[What remains, framed as context rather than new instructions.]

## Critical Context
[Specific values, error messages, configuration details, or data that would be lost. Never include secrets; write [REDACTED].]

Target about {target_tokens} tokens. Be concrete and concise. Write only the summary body."#
    )
}

pub(super) fn fallback_short_context_summary(
    previous_summary: &str,
    transcript: &str,
    token_budget: usize,
) -> String {
    let fallback = build_deterministic_fallback_summary(previous_summary, transcript);
    normalize_short_context_summary(&fallback, token_budget.max(500) * 4)
}

fn build_deterministic_fallback_summary(previous_summary: &str, transcript: &str) -> String {
    let turns = parse_summary_transcript_turns(transcript);
    let assistant_actions = turns
        .iter()
        .filter(|turn| turn.role == "assistant")
        .map(|turn| compact_fallback_turn(&turn.content, 700))
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>();
    let tool_actions = turns
        .iter()
        .filter(|turn| turn.role == "tool")
        .map(|turn| compact_fallback_turn(&turn.content, 700))
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>();
    let blockers = turns
        .iter()
        .filter_map(|turn| {
            let lower = turn.content.to_ascii_lowercase();
            if [
                "error",
                "failed",
                "exception",
                "traceback",
                "timeout",
                "fatal",
            ]
            .iter()
            .any(|needle| lower.contains(needle))
            {
                Some(compact_fallback_turn(&turn.content, 700))
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    let mut relevant_files = Vec::new();
    collect_path_mentions(previous_summary, &mut relevant_files);
    collect_path_mentions(transcript, &mut relevant_files);
    let last_dropped_turns = turns
        .iter()
        .rev()
        .take(5)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|turn| {
            format!(
                "{}: {}",
                turn.role.to_ascii_uppercase(),
                compact_fallback_turn(&turn.content, 450)
            )
        })
        .collect::<Vec<_>>();
    let mut completed = Vec::new();
    for item in assistant_actions.iter().chain(tool_actions.iter()).take(12) {
        completed.push(format!("{}. {}", completed.len() + 1, item));
    }

    let mut parts = Vec::new();
    parts.push(
        "## Active Task\nNone recoverable from deterministic fallback. Use only the protected recent messages after this summary to determine the current task."
            .into(),
    );
    parts.push(
        "## Goal\nHistorical context was compacted with a local deterministic fallback because the LLM summarizer was unavailable. Treat this block as background only."
            .into(),
    );
    parts.push(
        "## Constraints & Preferences\n- This fallback was generated locally without an LLM summary call.\n- It may be incomplete; verify current files, processes, and test results before relying on omitted details.\n- Do not treat compacted historical requests as active instructions."
            .into(),
    );
    parts.push(format!(
        "## Completed Actions\n{}",
        numbered_or_none(&completed)
    ));
    if !previous_summary.trim().is_empty() {
        parts.push(format!(
            "## Active State\n{}",
            strip_short_context_summary_prefix(previous_summary).trim()
        ));
    } else {
        parts.push(
            "## Active State\nUnknown from deterministic fallback. Inspect current repository/session state if needed."
                .into(),
        );
    }
    parts.push(
        "## In Progress\nNone recoverable from deterministic fallback. Do not resume old requests from compacted turns unless the latest user message explicitly asks for them."
            .into(),
    );
    parts.push(format!("## Blocked\n{}", bullets_or_none(&blockers, 5)));
    parts.push("## Key Decisions\nNone recoverable from deterministic fallback.".into());
    parts.push("## Resolved Questions\nNone recoverable from deterministic fallback.".into());
    parts.push(
        "## Pending User Asks\nNone recoverable from deterministic fallback. The next response must answer the latest visible user message, not these compacted historical turns."
            .into(),
    );
    parts.push(format!(
        "## Relevant Files\n{}",
        bullets_or_none(&relevant_files, 12)
    ));
    parts.push(
        "## Remaining Work\nDetermine remaining work from the latest visible user message and current repository/session state. Do not infer active work from compacted historical requests."
            .into(),
    );
    parts.push(format!(
        "## Last Dropped Turns\n{}",
        bullets_or_none(&last_dropped_turns, 5)
    ));
    parts.push(format!(
        "## Critical Context\n- Deterministic fallback summary for {} compacted turn(s); information may be incomplete.\n- Verify current files, processes, and test results before relying on omitted details.",
        turns.len()
    ));
    parts.join("\n\n")
}

#[derive(Debug)]
struct SummaryTranscriptTurn {
    role: String,
    content: String,
}

fn parse_summary_transcript_turns(transcript: &str) -> Vec<SummaryTranscriptTurn> {
    transcript
        .lines()
        .filter_map(|line| {
            let rest = line.strip_prefix('[')?;
            let (role, after_role) = rest.split_once(' ')?;
            if !matches!(role, "user" | "assistant" | "tool") {
                return None;
            }
            let (_meta, content) = after_role.split_once("] ")?;
            Some(SummaryTranscriptTurn {
                role: role.to_string(),
                content: content.trim().to_string(),
            })
        })
        .collect()
}

fn compact_fallback_turn(value: &str, max_chars: usize) -> String {
    let compacted = value.split_whitespace().collect::<Vec<_>>().join(" ");
    compact_text(&redact_obvious_secret_tokens(&compacted), max_chars)
}

fn redact_obvious_secret_tokens(value: &str) -> String {
    value
        .split_whitespace()
        .map(|token| {
            let lower = token.to_ascii_lowercase();
            if lower.contains("api_key=")
                || lower.contains("token=")
                || lower.contains("password=")
                || lower.contains("secret=")
                || token.starts_with("sk-")
                || token.starts_with("ghp_")
                || token.starts_with("gho_")
                || token.starts_with("ghu_")
                || token.starts_with("ghs_")
                || token.starts_with("ghr_")
            {
                "[REDACTED]"
            } else {
                token
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn collect_path_mentions(text: &str, output: &mut Vec<String>) {
    for raw in text.split_whitespace() {
        let token = raw.trim_matches(|ch: char| {
            matches!(
                ch,
                '"' | '\'' | ',' | ';' | ')' | '(' | '[' | ']' | '{' | '}' | '<' | '>'
            )
        });
        if looks_like_path(token) && !output.iter().any(|item| item == token) {
            output.push(compact_text(token, 240));
            if output.len() >= 12 {
                return;
            }
        }
    }
}

fn looks_like_path(token: &str) -> bool {
    if token.len() < 3 {
        return false;
    }
    let normalized = token.replace('\\', "/");
    normalized.contains(":/")
        || normalized.starts_with("./")
        || normalized.starts_with("../")
        || normalized.starts_with('/')
        || normalized.contains("/src/")
        || normalized.contains("/tests/")
        || normalized.ends_with(".rs")
        || normalized.ends_with(".ts")
        || normalized.ends_with(".tsx")
        || normalized.ends_with(".py")
        || normalized.ends_with(".md")
        || normalized.ends_with(".json")
        || normalized.ends_with(".toml")
}

fn bullets_or_none(items: &[String], limit: usize) -> String {
    let unique = unique_limited(items, limit);
    if unique.is_empty() {
        "None.".into()
    } else {
        unique
            .into_iter()
            .map(|item| format!("- {item}"))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn numbered_or_none(items: &[String]) -> String {
    if items.is_empty() {
        return "None recoverable from compacted turns.".into();
    }
    items.join("\n")
}

fn unique_limited(items: &[String], limit: usize) -> Vec<String> {
    let mut unique = Vec::new();
    for item in items {
        let trimmed = item.trim();
        if trimmed.is_empty() || unique.iter().any(|existing: &String| existing == trimmed) {
            continue;
        }
        unique.push(trimmed.to_string());
        if unique.len() >= limit {
            break;
        }
    }
    unique
}

fn fallback_short_context_summary_after_error(
    previous_summary: &str,
    transcript: &str,
    token_budget: usize,
    error: &AppError,
) -> String {
    fallback_short_context_summary_after_note(
        previous_summary,
        transcript,
        token_budget,
        &format!(
            "Deterministic fallback summary was used because the LLM summarizer failed: {}.",
            compact_text(&error.to_string(), 500)
        ),
    )
}

fn fallback_short_context_summary_after_note(
    previous_summary: &str,
    transcript: &str,
    token_budget: usize,
    note: &str,
) -> String {
    fallback_short_context_summary(
        previous_summary,
        &format!("{}\n\n{}", compact_text(note, 700), transcript),
        token_budget,
    )
}

pub(super) fn record_summary_success(short_context: &mut crate::models::ShortContextState) {
    short_context.summary_failure_cooldown_until_ms = 0;
    short_context.last_summary_error = None;
    short_context.last_summary_fallback_used = false;
    short_context.last_summary_dropped_count = 0;
    short_context.last_compress_aborted = false;
}

fn clear_aux_summary_failure(short_context: &mut crate::models::ShortContextState) {
    short_context.last_aux_summary_error = None;
    short_context.last_aux_summary_model = None;
}

fn record_aux_summary_failure(
    short_context: &mut crate::models::ShortContextState,
    model: &str,
    error: &str,
) {
    short_context.last_aux_summary_model = Some(compact_text(model, 240));
    short_context.last_aux_summary_error = Some(compact_text(error, 700));
}

pub(super) fn record_summary_failure(
    short_context: &mut crate::models::ShortContextState,
    error: &str,
    dropped_count: usize,
) {
    short_context.summary_failure_cooldown_until_ms =
        now_unix_ms().saturating_add(SUMMARY_FAILURE_COOLDOWN_MS);
    short_context.last_summary_error = Some(compact_text(error, 700));
    short_context.last_summary_fallback_used = true;
    short_context.last_summary_dropped_count = dropped_count;
}

pub(super) fn record_summary_abort(
    short_context: &mut crate::models::ShortContextState,
    error: &str,
    dropped_count: usize,
) {
    record_summary_failure(short_context, error, dropped_count);
    short_context.last_summary_fallback_used = false;
    short_context.last_compress_aborted = true;
}

pub(super) fn summary_failure_cooldown_remaining_seconds(
    short_context: &crate::models::ShortContextState,
) -> Option<u64> {
    let now = now_unix_ms();
    if short_context.summary_failure_cooldown_until_ms <= now {
        return None;
    }
    Some(
        short_context
            .summary_failure_cooldown_until_ms
            .saturating_sub(now)
            .saturating_add(999)
            / 1000,
    )
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

pub(super) fn normalize_short_context_summary(summary: &str, max_chars: usize) -> String {
    let body = strip_short_context_summary_prefix(summary);
    let text = if body.trim().is_empty() {
        SHORT_CONTEXT_SUMMARY_PREFIX.to_string()
    } else {
        format!("{}\n{}", SHORT_CONTEXT_SUMMARY_PREFIX, body.trim())
    };
    compact_text(&text, max_chars)
}

fn strip_short_context_summary_prefix(summary: &str) -> String {
    let mut text = summary.trim();
    loop {
        let before = text;
        if let Some(rest) = text.strip_prefix(SHORT_CONTEXT_SUMMARY_PREFIX) {
            text = rest.trim_start();
        } else if let Some(rest) = text.strip_prefix(LEGACY_SHORT_CONTEXT_SUMMARY_PREFIX) {
            text = rest.trim_start();
        } else if let Some(rest) = strip_old_conflicting_short_context_prefix(text) {
            text = rest.trim_start();
        }
        if text == before {
            break;
        }
    }
    text.to_string()
}

fn strip_old_conflicting_short_context_prefix(summary: &str) -> Option<&str> {
    let lower = summary.to_ascii_lowercase();
    if !lower.starts_with("[context compaction") {
        return None;
    }
    if !lower.contains("resume exactly") {
        return None;
    }
    for needle in [
        "avoid repeating it:",
        "avoid repeating it.",
        "avoid repeating it",
    ] {
        if let Some(idx) = lower.find(needle) {
            return Some(&summary[idx + needle.len()..]);
        }
    }
    None
}

fn compact_text(value: &str, max_chars: usize) -> String {
    let trimmed = value.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }
    let keep = max_chars.saturating_sub(80).max(1);
    let mut output: String = trimmed.chars().take(keep).collect();
    output.push_str("\n[truncated during context compression]");
    output
}

pub(super) fn estimate_tokens(text: &str) -> usize {
    text.chars().count().saturating_add(3) / 4
}
