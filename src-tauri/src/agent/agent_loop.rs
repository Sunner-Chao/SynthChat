use std::{
    collections::{HashMap, HashSet},
    future::Future,
    time::{Duration, Instant},
};

use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use tauri::AppHandle;

use crate::{
    error::AppResult,
    models::{
        AgentCheckpointRecord, AgentDefinition, AgentRunPhaseRecord, AgentRunRecord, ChatMessage,
        Conversation, Persona, SendChatRequest,
    },
    store::AppStore,
};

use super::*;
pub async fn run_chat_turn(
    store: &AppStore,
    request: SendChatRequest,
    app: Option<&AppHandle>,
) -> AppResult<Vec<ChatMessage>> {
    run_chat_turn_with_app(store, request, ToolExecutionContext::Interactive, app).await
}

pub(super) async fn run_chat_turn_in_context(
    store: &AppStore,
    request: SendChatRequest,
    tool_context: ToolExecutionContext,
) -> AppResult<Vec<ChatMessage>> {
    run_chat_turn_with_app(store, request, tool_context, None).await
}

pub(super) async fn run_chat_turn_with_app(
    store: &AppStore,
    request: SendChatRequest,
    tool_context: ToolExecutionContext,
    app: Option<&AppHandle>,
) -> AppResult<Vec<ChatMessage>> {
    run_chat_turn_with_toolset_policy(store, request, tool_context, None, None, app).await
}

pub(super) async fn run_chat_turn_with_toolset_policy(
    store: &AppStore,
    request: SendChatRequest,
    tool_context: ToolExecutionContext,
    enabled_toolsets: Option<Vec<String>>,
    disabled_toolsets: Option<Vec<String>>,
    app: Option<&AppHandle>,
) -> AppResult<Vec<ChatMessage>> {
    run_chat_turn_with_toolset_policy_and_iteration_limit(
        store,
        request,
        tool_context,
        enabled_toolsets,
        disabled_toolsets,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        app,
    )
    .await
}

pub(super) async fn run_chat_turn_with_toolset_policy_and_iteration_limit(
    store: &AppStore,
    request: SendChatRequest,
    tool_context: ToolExecutionContext,
    enabled_toolsets: Option<Vec<String>>,
    disabled_toolsets: Option<Vec<String>>,
    max_tool_iterations: Option<u32>,
    provider_id_override: Option<String>,
    model_override: Option<String>,
    base_url_override: Option<String>,
    timeout_seconds_override: Option<u64>,
    subagent_auto_approve: Option<bool>,
    workspace_dir_override: Option<String>,
    enabled_skills: Option<Vec<String>>,
    stream_delta_callback: Option<crate::llm::LlmDeltaCallback>,
    app: Option<&AppHandle>,
) -> AppResult<Vec<ChatMessage>> {
    let conversation = match request.conversation_id.as_deref() {
        Some(id) if !id.trim().is_empty() => store.conversation(id)?,
        _ => store.create_conversation(None, request.persona_id.clone())?,
    };
    let (persona, mut agent) = resolve_chat_turn_persona_and_agent(store, &conversation, &request)?;
    apply_acp_session_mcp_scope(store, &conversation, &mut agent)?;
    if let Some(toolsets) = enabled_toolsets {
        agent.enabled_toolsets = toolsets;
    }
    if let Some(toolsets) = disabled_toolsets {
        merge_disabled_toolset_overrides(&mut agent.disabled_toolsets, toolsets);
    }
    if let Some(limit) = max_tool_iterations {
        agent.max_tool_iterations = limit.max(1).min(90);
    }
    if let Some(provider_id) = provider_id_override.filter(|value| !value.trim().is_empty()) {
        agent.llm_provider = provider_id;
    }
    if let Some(model) = model_override.filter(|value| !value.trim().is_empty()) {
        agent.llm_model = model;
    }
    if let Some(workspace_dir) = workspace_dir_override.filter(|value| !value.trim().is_empty()) {
        agent.workspace_dir = workspace_dir;
    }
    if let Some(skills) = enabled_skills {
        agent.enabled_skills = skills;
    }
    if let Some(control) =
        handle_agent_control_command(store, &conversation, &persona, &request.content, app).await?
    {
        let user = store.append_message(ChatMessage::new(
            conversation.id.clone(),
            "user",
            request.content.clone(),
            "desktop-control",
        ))?;
        let assistant = store.append_message(control)?;
        return Ok(vec![user, assistant]);
    }
    if let Some(messages) =
        handle_busy_conversation_input(store, &conversation, &persona, &request.content, app)?
    {
        return Ok(messages);
    }
    let effective_request_content =
        clarification_response_context_for_turn(store, &conversation.id, &request.content)?
            .unwrap_or_else(|| request.content.clone());
    let chat_config = store.config()?.chat;
    let enriched_user_content = expand_context_references(
        &agent,
        &effective_request_content,
        chat_config.short_context_token_budget,
        Some(&store.data_dir().join("attachments")),
    )
    .await?;
    let mut user_message = ChatMessage::new(
        conversation.id.clone(),
        "user",
        request.content.clone(),
        "desktop",
    );
    user_message.provider_data = request.provider_data.clone();
    let user = store.append_message(user_message)?;

    let mut run = AgentRunRecord::new(
        conversation.id.clone(),
        persona.id.clone(),
        agent.id.clone(),
    );
    run.user_request = effective_request_content.clone();
    run.queue_item_id = request.queue_item_id.clone();
    run.state = "running".into();
    let saved_run = store.save_agent_run(run.clone())?;
    emit_agent_run_record(app, &saved_run, None);
    run_session_lifecycle_hooks(
        store,
        "on_session_start",
        &saved_run,
        json!({"source": "chat_turn"}),
    )
    .await;

    let mut history = store.messages(&conversation.id, Some(30))?;
    let effective_persona = effective_llm_persona(&persona, &agent);
    let mut providers = store.provider_candidates(selected_provider_id(&persona, &agent))?;
    if let Some(base_url) = base_url_override
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
    {
        let selected = selected_provider_id(&persona, &agent).map(str::to_string);
        let provider_index = selected
            .as_deref()
            .and_then(|id| providers.iter().position(|provider| provider.id == id))
            .unwrap_or(0);
        if let Some(provider) = providers.get_mut(provider_index) {
            provider.base_url = base_url;
        }
    }
    let mut observations = Vec::new();
    let mut assistant_text = String::new();
    let mut assistant_provider_data: Option<Value> = None;
    let mut assistant_model: Option<String> = None;
    let mut assistant_provider_id: Option<String> = None;
    let skill_blocks =
        crate::skills::prompt_blocks_for_request(store, &agent, &effective_request_content)?;
    let memory_blocks =
        memory_prompt_blocks_for_query(store, &persona, &effective_request_content)?;
    let mut short_context = store.short_context(&conversation.id)?;
    let mcp_tools = available_mcp_tool_definitions(store, &agent)?;
    let native_tools = visible_tool_definitions_for_agent(store, &agent, tool_context)?;
    on_memory_turn_start(
        store,
        &saved_run.run_id,
        &conversation.id,
        &persona,
        &effective_request_content,
        memory_blocks.len(),
        native_tools.len() + mcp_tools.len(),
    )?;
    let run_started_at = Instant::now();
    let run_timeout_seconds =
        timeout_seconds_override.unwrap_or(chat_config.agent_run_timeout_seconds);
    let post_tool_quiet_timeout_seconds = chat_config.agent_post_tool_quiet_timeout_seconds;
    let mut tool_guardrails = ToolLoopGuardrails::new(&chat_config);
    let mut failed_file_mutations: HashMap<String, String> = HashMap::new();
    let mut llm_recoveries_attempted: HashSet<String> = HashSet::new();
    let mut empty_llm_recovery_attempts: HashMap<String, u32> = HashMap::new();
    let mut iteration_budget = IterationBudget::new(agent.max_tool_iterations.max(1).min(90));
    if chat_config.short_context_abort_on_summary_failure && short_context.last_compress_aborted {
        append_parent_phase_event(
            store,
            &saved_run.run_id,
            "context_compression_frozen",
            json!({
                "lastSummaryError": short_context.last_summary_error,
                "lastSummaryDroppedCount": short_context.last_summary_dropped_count,
                "summaryFailureCooldownUntilMs": short_context.summary_failure_cooldown_until_ms,
            }),
        )?;
        run.state = "failed".into();
        run.error = Some("Context compression is frozen after summary failure.".into());
        run.updated_at = now_iso();
        run.completed_at = Some(run.updated_at.clone());
        let saved_failed_run = store.save_agent_run(run)?;
        run_session_finished_hooks(
            store,
            &saved_failed_run,
            json!({"source": "context_compression_frozen"}),
        )
        .await;
        let assistant = store.append_message(ChatMessage::new(
            conversation.id.clone(),
            "assistant",
            format!(
                "本轮对话已暂停：上一次上下文压缩摘要失败，并且已开启 shortContextAbortOnSummaryFailure。为避免丢失旧对话历史，agent 不会继续运行。\n\n请修复摘要模型后执行 /compact，或在设置中关闭该开关后重试。上一错误：{}",
                short_context
                    .last_summary_error
                    .as_deref()
                    .unwrap_or("unknown summary error")
            ),
            "desktop-agent-error",
        ))?;
        emit_agent_run_record(app, &saved_failed_run, Some(&assistant));
        return Ok(vec![user, assistant]);
    }
    if let Some(note) = preflight_compact_context_for_agent_run(
        store,
        &saved_run.run_id,
        &conversation.id,
        &mut history,
        &mut short_context,
        &chat_config,
    )? {
        observations.push(note);
    }
    for iteration in 0..iteration_budget.max_total() {
        if !iteration_budget.consume() {
            append_parent_phase_event(
                store,
                &saved_run.run_id,
                "iteration_budget_exhausted",
                json!({
                    "used": iteration_budget.used(),
                    "maxTotal": iteration_budget.max_total(),
                    "remaining": iteration_budget.remaining(),
                }),
            )?;
            break;
        }
        if check_agent_run_interrupted(
            store,
            &saved_run.run_id,
            run_started_at,
            run_timeout_seconds,
            post_tool_quiet_timeout_seconds,
            app,
        )? {
            return Ok(vec![user]);
        }
        drain_agent_steers_into_observations(store, &mut run, &mut observations)?;
        let prompt_observations = observations_for_prompt(store, &saved_run.run_id, &observations)?;
        let planner_prompt = agent_planner_prompt_for_agent_context_with_store(
            store,
            &prompt_observations,
            &skill_blocks,
            &memory_blocks,
            &short_context,
            &mcp_tools,
            tool_context,
            &agent,
        );
        let pre_llm_contexts =
            run_pre_llm_call_hooks(store, &saved_run.run_id, &enriched_user_content).await;
        let llm_user_content =
            inject_pre_llm_hook_context(&enriched_user_content, &pre_llm_contexts);
        let reply_result = await_agent_run_interruptible(
            store,
            &saved_run.run_id,
            run_started_at,
            run_timeout_seconds,
            post_tool_quiet_timeout_seconds,
            app,
            complete_chat_with_provider_failover(
                store,
                Some(&saved_run.run_id),
                &providers,
                &effective_persona,
                planner_prompt.clone(),
                history.clone(),
                &llm_user_content,
                Some(&native_tools),
                stream_delta_callback.clone(),
            ),
        )
        .await?;
        let Some(reply_result) = reply_result else {
            return Ok(vec![user]);
        };
        let reply = match reply_result {
            Ok(reply) => reply,
            Err(error) => {
                if check_agent_run_interrupted(
                    store,
                    &saved_run.run_id,
                    run_started_at,
                    run_timeout_seconds,
                    post_tool_quiet_timeout_seconds,
                    app,
                )? {
                    return Ok(vec![user]);
                }
                if let Some(recovery_note) = recover_llm_failure_for_agent_run(
                    store,
                    &saved_run.run_id,
                    &conversation.id,
                    &mut history,
                    &mut short_context,
                    &error,
                    &mut llm_recoveries_attempted,
                    chat_config.short_context_token_budget,
                )? {
                    observations.push(format!(
                        "Iteration {} LLM recovery: {}",
                        iteration + 1,
                        recovery_note
                    ));
                    continue;
                }
                let mut failed_run = store.agent_run(&saved_run.run_id)?;
                if failed_run.state != "aborted" {
                    failed_run.state = "failed".into();
                    failed_run.error = Some(error.to_string());
                    failed_run.updated_at = now_iso();
                    failed_run.completed_at = Some(failed_run.updated_at.clone());
                    let saved_failed_run = store.save_agent_run(failed_run)?;
                    run_session_finished_hooks(
                        store,
                        &saved_failed_run,
                        json!({"source": "llm_error"}),
                    )
                    .await;
                    emit_agent_run_record(app, &saved_failed_run, None);
                }
                let assistant = store.append_message(ChatMessage::new(
                    conversation.id.clone(),
                    "assistant",
                    format!("本轮对话没有返回，是因为模型请求失败：{error}"),
                    "desktop-agent-error",
                ))?;
                if let Ok(saved_failed_run) = store.agent_run(&saved_run.run_id) {
                    emit_agent_run_record(app, &saved_failed_run, Some(&assistant));
                }
                return Ok(vec![user, assistant]);
            }
        };
        if abort_agent_run_for_turn_aborted_marker(store, &saved_run.run_id, &reply.content, app)? {
            return Ok(vec![user]);
        }
        if check_agent_run_interrupted(
            store,
            &saved_run.run_id,
            run_started_at,
            run_timeout_seconds,
            post_tool_quiet_timeout_seconds,
            app,
        )? {
            return Ok(vec![user]);
        }
        if reply.finish_reason.as_deref() == Some("incomplete") {
            let recovery_key = "incomplete_response";
            if llm_recoveries_attempted.insert(recovery_key.into()) {
                let note = "Provider returned an incomplete Responses turn (reasoning/commentary without final answer, or unfinished output item). Continue from the current context and return a valid planner JSON object: either {\"action\":\"tool\",...} or {\"action\":\"final\",\"content\":\"...\"}.";
                observations.push(format!(
                    "Iteration {} LLM recovery: {}",
                    iteration + 1,
                    note
                ));
                append_parent_phase_event(
                    store,
                    &saved_run.run_id,
                    "llm_recovery",
                    json!({
                        "kind": recovery_key,
                        "note": note,
                    }),
                )?;
                continue;
            }
        }
        if reply.content.trim().is_empty() {
            if let Some(recovery) =
                next_empty_llm_response_recovery(&observations, &mut empty_llm_recovery_attempts)
            {
                observations.push(format!(
                    "Iteration {} LLM recovery: {}",
                    iteration + 1,
                    recovery.note
                ));
                append_parent_phase_event(
                    store,
                    &saved_run.run_id,
                    "llm_recovery",
                    json!({
                        "kind": recovery.kind,
                        "note": recovery.note,
                        "attempt": recovery.attempt,
                        "maxAttempts": recovery.max_attempts,
                        "afterTools": recovery.after_tools,
                        "finishReason": reply.finish_reason.clone(),
                        "providerId": reply.provider_id.clone(),
                        "model": reply.model.clone(),
                    }),
                )?;
                continue;
            } else {
                append_parent_phase_event(
                    store,
                    &saved_run.run_id,
                    "llm_recovery_exhausted",
                    json!({
                        "kind": if observations.is_empty() { "empty_response" } else { "empty_response_after_tools" },
                        "attempts": empty_llm_recovery_attempts.clone(),
                        "finishReason": reply.finish_reason.clone(),
                        "providerId": reply.provider_id.clone(),
                        "model": reply.model.clone(),
                    }),
                )?;
            }
        }
        let decision = parse_agent_decision(&reply.content);
        append_planner_trace(
            store,
            &saved_run.run_id,
            &conversation.id,
            &persona.id,
            &agent.id,
            iteration + 1,
            &planner_prompt,
            &reply.content,
            &decision,
        )?;
        match decision
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("final")
        {
            "tool" => {
                let requests = planned_tool_requests_from_decision(&decision);
                if requests.is_empty() {
                    observations.push(format!(
                        "Iteration {} tool error: planner requested tool action without a valid tool name",
                        iteration + 1
                    ));
                    continue;
                }
                let refund_iteration_for_execute_code_only =
                    tool_batch_is_execute_code_only(&requests);
                let should_parallelize = should_parallelize_tool_batch(
                    &requests,
                    &mcp_tools,
                    &agent,
                    &chat_config,
                    store,
                    tool_context,
                )?;
                if should_parallelize {
                    for (tool_name, payload) in &requests {
                        let guardrail_payload = payload.clone();
                        if let Some(outcome) =
                            tool_guardrails.before_call(tool_name, &guardrail_payload)
                        {
                            let guardrail_message = outcome.message.clone();
                            observations.push(format!(
                                "Iteration {} tool {} guardrail: {}",
                                iteration + 1,
                                tool_name,
                                guardrail_message
                            ));
                            if outcome.halt {
                                record_tool_failed_for_run(
                                    store,
                                    app,
                                    &conversation.id,
                                    &saved_run.run_id,
                                    tool_name,
                                    &mcp_tools,
                                    &guardrail_payload,
                                    &AppError::BadRequest(guardrail_message.clone()),
                                )?;
                                assistant_text = guardrail_message;
                                break;
                            }
                        }
                    }
                }
                if !assistant_text.trim().is_empty() {
                    break;
                }
                if should_parallelize && assistant_text.trim().is_empty() {
                    let parallel_results = await_agent_run_interruptible(
                        store,
                        &saved_run.run_id,
                        run_started_at,
                        run_timeout_seconds,
                        post_tool_quiet_timeout_seconds,
                        app,
                        execute_parallel_tool_batch(
                            store,
                            &agent,
                            &conversation.id,
                            &saved_run.run_id,
                            &requests,
                            &mcp_tools,
                            tool_context,
                            iteration + 1,
                            app,
                        ),
                    )
                    .await?;
                    let Some(parallel_results) = parallel_results else {
                        return Ok(vec![user]);
                    };
                    for (tool_name, payload, result) in parallel_results {
                        let guardrail_payload = payload.clone();
                        match result {
                            Ok((text, mut event)) => {
                                record_file_mutation_result(
                                    &mut failed_file_mutations,
                                    &tool_name,
                                    &payload,
                                    &text,
                                    false,
                                );
                                if check_agent_run_interrupted(
                                    store,
                                    &saved_run.run_id,
                                    run_started_at,
                                    run_timeout_seconds,
                                    post_tool_quiet_timeout_seconds,
                                    app,
                                )? {
                                    return Ok(vec![user]);
                                }
                                let context_text = persist_large_tool_result_for_context(
                                    store,
                                    &saved_run.run_id,
                                    &tool_name,
                                    &text,
                                    &mut event,
                                )?;
                                let observation_text = append_subdirectory_hints_to_tool_result(
                                    &agent,
                                    &tool_name,
                                    &payload,
                                    &context_text,
                                );
                                observations.push(tool_result_replay_observation(
                                    iteration + 1,
                                    &tool_name,
                                    &tool_name,
                                    &observation_text,
                                ));
                                if let Some(outcome) = tool_guardrails.after_call(
                                    &tool_name,
                                    &guardrail_payload,
                                    &context_text,
                                    false,
                                ) {
                                    observations.push(format!(
                                        "Iteration {} tool {} guardrail: {}",
                                        iteration + 1,
                                        tool_name,
                                        outcome.message
                                    ));
                                    if outcome.halt {
                                        assistant_text = outcome.message.clone();
                                        break;
                                    }
                                }
                                let _tool_message = store.append_message(ChatMessage::new(
                                    conversation.id.clone(),
                                    "tool",
                                    json!({"type": "toolEvent", "event": event.clone()})
                                        .to_string(),
                                    "desktop-agent-tool",
                                ))?;
                                history.push(_tool_message);
                                push_tool_event_record(&mut run, &event);
                                if let Some(assistant) = pause_run_for_clarify_tool(
                                    store,
                                    app,
                                    &mut run,
                                    &conversation.id,
                                    &text,
                                    &event,
                                )? {
                                    return Ok(vec![user, assistant]);
                                }
                                emit_agent_run_record(app, &run, None);
                            }
                            Err(error) => {
                                record_tool_failed_for_run(
                                    store,
                                    app,
                                    &conversation.id,
                                    &saved_run.run_id,
                                    &tool_name,
                                    &mcp_tools,
                                    &payload,
                                    &error,
                                )?;
                                record_file_mutation_result(
                                    &mut failed_file_mutations,
                                    &tool_name,
                                    &payload,
                                    &error.to_string(),
                                    true,
                                );
                                observations.push(format!(
                                    "Iteration {} tool {} error: {}",
                                    iteration + 1,
                                    tool_name,
                                    error
                                ));
                                if let Some(outcome) = tool_guardrails.after_call(
                                    &tool_name,
                                    &guardrail_payload,
                                    &error.to_string(),
                                    true,
                                ) {
                                    observations.push(format!(
                                        "Iteration {} tool {} guardrail: {}",
                                        iteration + 1,
                                        tool_name,
                                        outcome.message
                                    ));
                                    if outcome.halt {
                                        assistant_text = outcome.message.clone();
                                        break;
                                    }
                                }
                            }
                        }
                    }
                    if !assistant_text.trim().is_empty() {
                        break;
                    }
                    if refund_iteration_for_execute_code_only {
                        iteration_budget.refund();
                    }
                    continue;
                }
                for (tool_name, payload) in requests {
                    let guardrail_payload = payload.clone();
                    if let Some(outcome) =
                        tool_guardrails.before_call(&tool_name, &guardrail_payload)
                    {
                        let guardrail_message = outcome.message.clone();
                        observations.push(format!(
                            "Iteration {} tool {} guardrail: {}",
                            iteration + 1,
                            tool_name,
                            guardrail_message
                        ));
                        if outcome.halt {
                            record_tool_failed_for_run(
                                store,
                                app,
                                &conversation.id,
                                &saved_run.run_id,
                                &tool_name,
                                &mcp_tools,
                                &guardrail_payload,
                                &AppError::BadRequest(guardrail_message.clone()),
                            )?;
                            assistant_text = guardrail_message;
                            break;
                        }
                    }
                    if is_internal_tool(&tool_name) {
                        let approval_reason = match tool_approval_reason(
                            store,
                            "__internal",
                            &tool_name,
                            &payload,
                            is_risky_tool_call(&tool_name, &payload),
                        ) {
                            Ok(reason) => reason,
                            Err(error) => {
                                record_tool_failed_for_run(
                                    store,
                                    app,
                                    &conversation.id,
                                    &saved_run.run_id,
                                    &tool_name,
                                    &mcp_tools,
                                    &guardrail_payload,
                                    &error,
                                )?;
                                observations.push(format!(
                                    "Iteration {} tool {} approval error: {}",
                                    iteration + 1,
                                    tool_name,
                                    error
                                ));
                                continue;
                            }
                        };
                        let approval_reason = match apply_scheduled_approval_mode(
                            store,
                            tool_context,
                            approval_reason,
                            &tool_name,
                        ) {
                            Ok(reason) => reason,
                            Err(error) => {
                                record_tool_failed_for_run(
                                    store,
                                    app,
                                    &conversation.id,
                                    &saved_run.run_id,
                                    &tool_name,
                                    &mcp_tools,
                                    &guardrail_payload,
                                    &error,
                                )?;
                                observations.push(format!(
                                    "Iteration {} tool {} approval error: {}",
                                    iteration + 1,
                                    tool_name,
                                    error
                                ));
                                continue;
                            }
                        };
                        let approval_reason = match apply_smart_approval_mode(
                            store,
                            &saved_run.run_id,
                            &providers,
                            &effective_persona,
                            approval_reason,
                            &tool_name,
                            &payload,
                        )
                        .await
                        {
                            Ok(reason) => reason,
                            Err(error) => {
                                record_tool_failed_for_run(
                                    store,
                                    app,
                                    &conversation.id,
                                    &saved_run.run_id,
                                    &tool_name,
                                    &mcp_tools,
                                    &guardrail_payload,
                                    &error,
                                )?;
                                observations.push(format!(
                                    "Iteration {} tool {} approval error: {}",
                                    iteration + 1,
                                    tool_name,
                                    error
                                ));
                                continue;
                            }
                        };
                        let approval_reason = match apply_subagent_approval_override(
                            tool_context,
                            subagent_auto_approve,
                            approval_reason,
                            &tool_name,
                        ) {
                            Ok(reason) => reason,
                            Err(error) => {
                                record_tool_failed_for_run(
                                    store,
                                    app,
                                    &conversation.id,
                                    &saved_run.run_id,
                                    &tool_name,
                                    &mcp_tools,
                                    &guardrail_payload,
                                    &error,
                                )?;
                                observations.push(format!(
                                    "Iteration {} tool {} approval error: {}",
                                    iteration + 1,
                                    tool_name,
                                    error
                                ));
                                continue;
                            }
                        };
                        if let Some(reason) = approval_reason {
                            run_pre_approval_request_hooks(
                                store,
                                &saved_run.run_id,
                                "__internal",
                                &tool_name,
                                &payload,
                                &reason,
                            )
                            .await;
                            let approval = append_tool_approval_request(
                                store,
                                &conversation.id,
                                &persona.id,
                                &agent.id,
                                &saved_run.run_id,
                                "__internal",
                                &tool_name,
                                payload,
                                reason,
                            )?;
                            run.state = "pendingApproval".into();
                            run.updated_at = now_iso();
                            let saved_pending_run = store.save_agent_run(run)?;
                            emit_agent_run_record(app, &saved_pending_run, None);
                            let assistant = store.append_message(ChatMessage::new(
                                conversation.id,
                                "assistant",
                                format!(
                                    "工具调用正在等待审批：{} · {}",
                                    approval.server_id, approval.tool_name
                                ),
                                "desktop-agent",
                            ))?;
                            return Ok(vec![user, assistant]);
                        }
                        record_tool_started_for_run(
                            store,
                            app,
                            &saved_run.run_id,
                            "__internal",
                            &tool_name,
                            &payload,
                            iteration + 1,
                        )?;
                        run = store.agent_run(&saved_run.run_id)?;
                        emit_agent_run_record(app, &run, None);
                        let tool_result = await_agent_run_interruptible(
                            store,
                            &saved_run.run_id,
                            run_started_at,
                            run_timeout_seconds,
                            post_tool_quiet_timeout_seconds,
                            app,
                            execute_recovery_internal_tool(
                                store,
                                &agent,
                                &conversation.id,
                                &saved_run.run_id,
                                &tool_name,
                                payload,
                                tool_context,
                                app,
                            ),
                        )
                        .await?;
                        let Some(tool_result) = tool_result else {
                            return Ok(vec![user]);
                        };
                        match tool_result {
                            Ok((text, mut event)) => {
                                record_file_mutation_result(
                                    &mut failed_file_mutations,
                                    &tool_name,
                                    &guardrail_payload,
                                    &text,
                                    false,
                                );
                                if check_agent_run_interrupted(
                                    store,
                                    &saved_run.run_id,
                                    run_started_at,
                                    run_timeout_seconds,
                                    post_tool_quiet_timeout_seconds,
                                    app,
                                )? {
                                    return Ok(vec![user]);
                                }
                                let context_text = persist_large_tool_result_for_context(
                                    store,
                                    &saved_run.run_id,
                                    &tool_name,
                                    &text,
                                    &mut event,
                                )?;
                                let observation_text = append_subdirectory_hints_to_tool_result(
                                    &agent,
                                    &tool_name,
                                    &guardrail_payload,
                                    &context_text,
                                );
                                observations.push(tool_result_replay_observation(
                                    iteration + 1,
                                    &tool_name,
                                    &tool_name,
                                    &observation_text,
                                ));
                                if let Some(outcome) = tool_guardrails.after_call(
                                    &tool_name,
                                    &guardrail_payload,
                                    &context_text,
                                    false,
                                ) {
                                    observations.push(format!(
                                        "Iteration {} tool {} guardrail: {}",
                                        iteration + 1,
                                        tool_name,
                                        outcome.message
                                    ));
                                    if outcome.halt {
                                        assistant_text = outcome.message.clone();
                                        break;
                                    }
                                }
                                let _tool_message = store.append_message(ChatMessage::new(
                                    conversation.id.clone(),
                                    "tool",
                                    json!({"type": "toolEvent", "event": event.clone()})
                                        .to_string(),
                                    "desktop-agent-tool",
                                ))?;
                                history.push(_tool_message);
                                push_tool_event_record(&mut run, &event);
                                emit_agent_run_record(app, &run, None);
                            }
                            Err(error) => {
                                record_tool_failed_for_run(
                                    store,
                                    app,
                                    &conversation.id,
                                    &saved_run.run_id,
                                    &tool_name,
                                    &mcp_tools,
                                    &guardrail_payload,
                                    &error,
                                )?;
                                record_file_mutation_result(
                                    &mut failed_file_mutations,
                                    &tool_name,
                                    &guardrail_payload,
                                    &error.to_string(),
                                    true,
                                );
                                observations.push(format!(
                                    "Iteration {} tool {} error: {}",
                                    iteration + 1,
                                    tool_name,
                                    error
                                ));
                                if let Some(outcome) = tool_guardrails.after_call(
                                    &tool_name,
                                    &guardrail_payload,
                                    &error.to_string(),
                                    true,
                                ) {
                                    observations.push(format!(
                                        "Iteration {} tool {} guardrail: {}",
                                        iteration + 1,
                                        tool_name,
                                        outcome.message
                                    ));
                                    if outcome.halt {
                                        assistant_text = outcome.message.clone();
                                        break;
                                    }
                                }
                            }
                        }
                    } else if let Some(definition) = resolve_mcp_tool(&mcp_tools, &tool_name) {
                        let approval_reason = match tool_approval_reason(
                            store,
                            &definition.server_id,
                            &definition.tool_name,
                            &payload,
                            definition.requires_approval,
                        ) {
                            Ok(reason) => reason,
                            Err(error) => {
                                record_tool_failed_for_run(
                                    store,
                                    app,
                                    &conversation.id,
                                    &saved_run.run_id,
                                    &tool_name,
                                    &mcp_tools,
                                    &guardrail_payload,
                                    &error,
                                )?;
                                observations.push(format!(
                                    "Iteration {} tool {} approval error: {}",
                                    iteration + 1,
                                    tool_name,
                                    error
                                ));
                                continue;
                            }
                        };
                        let approval_reason = match apply_scheduled_approval_mode(
                            store,
                            tool_context,
                            approval_reason,
                            &tool_name,
                        ) {
                            Ok(reason) => reason,
                            Err(error) => {
                                record_tool_failed_for_run(
                                    store,
                                    app,
                                    &conversation.id,
                                    &saved_run.run_id,
                                    &tool_name,
                                    &mcp_tools,
                                    &guardrail_payload,
                                    &error,
                                )?;
                                observations.push(format!(
                                    "Iteration {} tool {} approval error: {}",
                                    iteration + 1,
                                    tool_name,
                                    error
                                ));
                                continue;
                            }
                        };
                        let approval_reason = match apply_smart_approval_mode(
                            store,
                            &saved_run.run_id,
                            &providers,
                            &effective_persona,
                            approval_reason,
                            &definition.tool_name,
                            &payload,
                        )
                        .await
                        {
                            Ok(reason) => reason,
                            Err(error) => {
                                record_tool_failed_for_run(
                                    store,
                                    app,
                                    &conversation.id,
                                    &saved_run.run_id,
                                    &tool_name,
                                    &mcp_tools,
                                    &guardrail_payload,
                                    &error,
                                )?;
                                observations.push(format!(
                                    "Iteration {} tool {} approval error: {}",
                                    iteration + 1,
                                    tool_name,
                                    error
                                ));
                                continue;
                            }
                        };
                        let approval_reason = match apply_subagent_approval_override(
                            tool_context,
                            subagent_auto_approve,
                            approval_reason,
                            &tool_name,
                        ) {
                            Ok(reason) => reason,
                            Err(error) => {
                                record_tool_failed_for_run(
                                    store,
                                    app,
                                    &conversation.id,
                                    &saved_run.run_id,
                                    &tool_name,
                                    &mcp_tools,
                                    &guardrail_payload,
                                    &error,
                                )?;
                                observations.push(format!(
                                    "Iteration {} tool {} approval error: {}",
                                    iteration + 1,
                                    tool_name,
                                    error
                                ));
                                continue;
                            }
                        };
                        if let Some(reason) = approval_reason {
                            run_pre_approval_request_hooks(
                                store,
                                &saved_run.run_id,
                                &definition.server_id,
                                &definition.tool_name,
                                &payload,
                                &reason,
                            )
                            .await;
                            let approval = append_tool_approval_request(
                                store,
                                &conversation.id,
                                &persona.id,
                                &agent.id,
                                &saved_run.run_id,
                                &definition.server_id,
                                &definition.tool_name,
                                payload,
                                reason,
                            )?;
                            run.state = "pendingApproval".into();
                            run.updated_at = now_iso();
                            let saved_pending_run = store.save_agent_run(run)?;
                            emit_agent_run_record(app, &saved_pending_run, None);
                            let assistant = store.append_message(ChatMessage::new(
                                conversation.id,
                                "assistant",
                                format!(
                                    "工具调用正在等待审批：{} · {}",
                                    approval.server_id, approval.tool_name
                                ),
                                "desktop-agent",
                            ))?;
                            return Ok(vec![user, assistant]);
                        }
                        record_tool_started_for_run(
                            store,
                            app,
                            &saved_run.run_id,
                            &definition.server_id,
                            &definition.tool_name,
                            &payload,
                            iteration + 1,
                        )?;
                        run = store.agent_run(&saved_run.run_id)?;
                        emit_agent_run_record(app, &run, None);
                        let tool_result = await_agent_run_interruptible(
                            store,
                            &saved_run.run_id,
                            run_started_at,
                            run_timeout_seconds,
                            post_tool_quiet_timeout_seconds,
                            app,
                            execute_recovery_mcp_tool(
                                store,
                                &saved_run.run_id,
                                &definition,
                                payload,
                            ),
                        )
                        .await?;
                        let Some(tool_result) = tool_result else {
                            return Ok(vec![user]);
                        };
                        match tool_result {
                            Ok((text, mut event)) => {
                                record_file_mutation_result(
                                    &mut failed_file_mutations,
                                    &tool_name,
                                    &guardrail_payload,
                                    &text,
                                    false,
                                );
                                if check_agent_run_interrupted(
                                    store,
                                    &saved_run.run_id,
                                    run_started_at,
                                    run_timeout_seconds,
                                    post_tool_quiet_timeout_seconds,
                                    app,
                                )? {
                                    return Ok(vec![user]);
                                }
                                let context_text = persist_large_tool_result_for_context(
                                    store,
                                    &saved_run.run_id,
                                    &tool_name,
                                    &text,
                                    &mut event,
                                )?;
                                let tool_source =
                                    format!("{}:{}", definition.server_id, definition.tool_name);
                                let observation_text = append_subdirectory_hints_to_tool_result(
                                    &agent,
                                    &tool_name,
                                    &guardrail_payload,
                                    &context_text,
                                );
                                observations.push(tool_result_replay_observation(
                                    iteration + 1,
                                    &tool_name,
                                    &tool_source,
                                    &observation_text,
                                ));
                                if let Some(outcome) = tool_guardrails.after_call(
                                    &tool_name,
                                    &guardrail_payload,
                                    &context_text,
                                    false,
                                ) {
                                    observations.push(format!(
                                        "Iteration {} tool {} guardrail: {}",
                                        iteration + 1,
                                        tool_name,
                                        outcome.message
                                    ));
                                    if outcome.halt {
                                        assistant_text = outcome.message.clone();
                                        break;
                                    }
                                }
                                let _tool_message = store.append_message(ChatMessage::new(
                                    conversation.id.clone(),
                                    "tool",
                                    json!({"type": "toolEvent", "event": event.clone()})
                                        .to_string(),
                                    "desktop-agent-tool",
                                ))?;
                                history.push(_tool_message);
                                push_tool_event_record(&mut run, &event);
                                emit_agent_run_record(app, &run, None);
                            }
                            Err(error) => {
                                record_tool_failed_for_run(
                                    store,
                                    app,
                                    &conversation.id,
                                    &saved_run.run_id,
                                    &tool_name,
                                    &mcp_tools,
                                    &guardrail_payload,
                                    &error,
                                )?;
                                record_file_mutation_result(
                                    &mut failed_file_mutations,
                                    &tool_name,
                                    &guardrail_payload,
                                    &error.to_string(),
                                    true,
                                );
                                observations.push(format!(
                                    "Iteration {} tool {} error: {}",
                                    iteration + 1,
                                    tool_name,
                                    error
                                ));
                                if let Some(outcome) = tool_guardrails.after_call(
                                    &tool_name,
                                    &guardrail_payload,
                                    &error.to_string(),
                                    true,
                                ) {
                                    observations.push(format!(
                                        "Iteration {} tool {} guardrail: {}",
                                        iteration + 1,
                                        tool_name,
                                        outcome.message
                                    ));
                                    if outcome.halt {
                                        assistant_text = outcome.message.clone();
                                        break;
                                    }
                                }
                            }
                        }
                    } else {
                        let error =
                            AppError::BadRequest(format!("tool is not available: {tool_name}"));
                        record_tool_failed_for_run(
                            store,
                            app,
                            &conversation.id,
                            &saved_run.run_id,
                            &tool_name,
                            &mcp_tools,
                            &guardrail_payload,
                            &error,
                        )?;
                        record_file_mutation_result(
                            &mut failed_file_mutations,
                            &tool_name,
                            &guardrail_payload,
                            &error.to_string(),
                            true,
                        );
                        observations.push(format!(
                            "Iteration {} tool {} error: {}",
                            iteration + 1,
                            tool_name,
                            error
                        ));
                        if let Some(outcome) = tool_guardrails.after_call(
                            &tool_name,
                            &guardrail_payload,
                            &error.to_string(),
                            true,
                        ) {
                            observations.push(format!(
                                "Iteration {} tool {} guardrail: {}",
                                iteration + 1,
                                tool_name,
                                outcome.message
                            ));
                            if outcome.halt {
                                assistant_text = outcome.message.clone();
                                break;
                            }
                        }
                    }
                    if !assistant_text.trim().is_empty() {
                        break;
                    }
                }
                if !assistant_text.trim().is_empty() {
                    break;
                }
                if refund_iteration_for_execute_code_only {
                    iteration_budget.refund();
                }
            }
            _ => {
                assistant_text = decision
                    .get("content")
                    .or_else(|| decision.get("answer"))
                    .and_then(Value::as_str)
                    .unwrap_or(reply.content.trim())
                    .to_string();
                assistant_provider_data = reply.provider_data.clone();
                assistant_model = reply.model.clone();
                assistant_provider_id = reply.provider_id.clone();
                break;
            }
        }
    }
    if assistant_text.trim().is_empty() {
        if iteration_budget.exhausted() {
            append_parent_phase_event(
                store,
                &saved_run.run_id,
                "iteration_budget_exhausted",
                json!({
                    "used": iteration_budget.used(),
                    "maxTotal": iteration_budget.max_total(),
                    "remaining": iteration_budget.remaining(),
                }),
            )?;
        }
        assistant_text = if observations.is_empty() {
            recovery_reply(&effective_request_content)
        } else if iteration_budget.exhausted() {
            format!(
                "已达到本轮 agent 迭代预算（{}/{}），当前没有得到最终回答。\n\n{}",
                iteration_budget.used(),
                iteration_budget.max_total(),
                observations.join("\n\n")
            )
        } else {
            format!(
                "已完成可用工具检查，但当前恢复版 agent loop 未得到最终回答。\n\n{}",
                observations.join("\n\n")
            )
        };
    }
    normalize_guardrail_halt_reply(&mut assistant_text, &observations);
    append_file_mutation_footer(&mut assistant_text, &failed_file_mutations);
    assistant_text = run_transform_llm_output_hooks(
        store,
        &saved_run.run_id,
        &effective_request_content,
        &assistant_text,
        assistant_model.as_deref(),
        assistant_provider_id.as_deref(),
    )
    .await;
    run_post_llm_call_hooks(
        store,
        &saved_run.run_id,
        &effective_request_content,
        &assistant_text,
        assistant_model.as_deref(),
        assistant_provider_id.as_deref(),
    )
    .await;
    if check_agent_run_interrupted(
        store,
        &saved_run.run_id,
        run_started_at,
        run_timeout_seconds,
        post_tool_quiet_timeout_seconds,
        app,
    )? {
        return Ok(vec![user]);
    }
    run.state = "completed".into();
    run.updated_at = now_iso();
    run.completed_at = Some(run.updated_at.clone());
    let saved_completed_run = store.save_agent_run(run)?;
    run_session_finished_hooks(store, &saved_completed_run, json!({"source": "chat_turn"})).await;

    let mut assistant_message = ChatMessage::new(
        conversation.id.clone(),
        "assistant",
        assistant_text.clone(),
        "desktop-agent",
    );
    assistant_message.provider_data = assistant_provider_data;
    let assistant = store.append_message(assistant_message)?;
    on_memory_turn_synced(
        store,
        &saved_completed_run.run_id,
        &conversation.id,
        &persona,
        &effective_request_content,
        &assistant_text,
    )?;
    let saved_completed_run = store.agent_run(&saved_completed_run.run_id)?;
    emit_agent_run_record(app, &saved_completed_run, Some(&assistant));
    Ok(vec![user, assistant])
}

const EMPTY_RESPONSE_MAX_RECOVERY_ATTEMPTS: u32 = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct EmptyLlmResponseRecovery {
    pub kind: &'static str,
    pub note: String,
    pub attempt: u32,
    pub max_attempts: u32,
    pub after_tools: bool,
}

pub(super) fn next_empty_llm_response_recovery(
    observations: &[String],
    attempts: &mut HashMap<String, u32>,
) -> Option<EmptyLlmResponseRecovery> {
    let after_tools = !observations.is_empty();
    let kind = if after_tools {
        "empty_response_after_tools"
    } else {
        "empty_response"
    };
    let prior_attempts = attempts.get(kind).copied().unwrap_or(0);
    if prior_attempts >= EMPTY_RESPONSE_MAX_RECOVERY_ATTEMPTS {
        return None;
    }
    let attempt = prior_attempts + 1;
    attempts.insert(kind.to_string(), attempt);
    let max_attempts = EMPTY_RESPONSE_MAX_RECOVERY_ATTEMPTS;
    let note = if after_tools {
        format!(
            "Model returned an empty response after tool results (attempt {attempt}/{max_attempts}). You just received tool observations above; process them and return {{\"action\":\"final\",\"content\":\"...\"}}. Request another tool only if more evidence is required."
        )
    } else {
        format!(
            "Model returned an empty response (attempt {attempt}/{max_attempts}). Retry with a valid planner JSON object: either {{\"action\":\"tool\",...}} or {{\"action\":\"final\",\"content\":\"...\"}}."
        )
    };
    Some(EmptyLlmResponseRecovery {
        kind,
        note,
        attempt,
        max_attempts,
        after_tools,
    })
}

pub(super) fn merge_disabled_toolset_overrides(
    existing: &mut Vec<String>,
    additional: Vec<String>,
) {
    let mut seen = existing
        .iter()
        .map(|name| normalize_toolset_name(name))
        .filter(|name| !name.is_empty())
        .collect::<HashSet<_>>();
    for name in additional {
        let normalized = normalize_toolset_name(&name);
        if normalized.is_empty() || !seen.insert(normalized) {
            continue;
        }
        existing.push(name);
    }
}

pub(super) fn resolve_chat_turn_persona_and_agent(
    store: &AppStore,
    conversation: &Conversation,
    request: &SendChatRequest,
) -> AppResult<(Persona, AgentDefinition)> {
    let persona = store.persona(
        conversation
            .persona_id
            .as_deref()
            .or(request.persona_id.as_deref()),
    )?;
    let agent = store.agent(
        request
            .agent_id
            .as_deref()
            .filter(|id| !id.trim().is_empty())
            .or(Some(conversation.agent_id.as_str())),
    )?;
    Ok((persona, agent))
}

pub(super) fn tool_batch_is_execute_code_only(requests: &[(String, Value)]) -> bool {
    !requests.is_empty()
        && requests
            .iter()
            .all(|(tool_name, _)| tool_name == "execute_code")
}

pub(super) fn apply_subagent_approval_override(
    context: ToolExecutionContext,
    subagent_auto_approve: Option<bool>,
    approval_reason: Option<String>,
    tool_name: &str,
) -> AppResult<Option<String>> {
    let Some(reason) = approval_reason else {
        return Ok(None);
    };
    if !matches!(
        context,
        ToolExecutionContext::SubagentLeaf | ToolExecutionContext::SubagentOrchestrator
    ) {
        return Ok(Some(reason));
    }
    match subagent_auto_approve {
        Some(true) => Ok(None),
        Some(false) => Err(AppError::BadRequest(format!(
            "Subagent auto-denied tool approval for {tool_name}: {reason}. Set delegationSubagentAutoApprove=true to allow unattended approval."
        ))),
        None => Ok(Some(reason)),
    }
}

pub(super) fn handle_busy_conversation_input(
    store: &AppStore,
    conversation: &Conversation,
    persona: &Persona,
    content: &str,
    app: Option<&AppHandle>,
) -> AppResult<Option<Vec<ChatMessage>>> {
    let Some(active) = store.active_agent_run_for_conversation(&conversation.id)? else {
        return Ok(None);
    };
    match normalize_busy_input_mode(&store.config()?.chat.busy_input_mode).as_str() {
        "interrupt" => {
            abort_agent_run(
                store,
                active.run_id,
                Some("Agent run interrupted by a new user request.".into()),
                app,
            )?;
            Ok(None)
        }
        "steer" => {
            let user = store.append_message(ChatMessage::new(
                conversation.id.clone(),
                "user",
                content.to_string(),
                "desktop-steer",
            ))?;
            store.append_agent_run_steer(&active.run_id, content.to_string())?;
            let assistant = store.append_message(control_message(
                conversation,
                format!(
                    "已将新输入注入当前 agent run：{}。它会在下一轮规划前读取。",
                    active.run_id
                ),
            ))?;
            Ok(Some(vec![user, assistant]))
        }
        _ => {
            let (user, queued) =
                enqueue_prompt_for_conversation(store, conversation, persona, content)?;
            emit_agent_queue_event(app, "queued", Some(&queued), Some(&conversation.id));
            let assistant = store.append_message(control_message(
                conversation,
                format!(
                    "当前已有运行中的 agent run：{}。新输入已加入队列：{}。",
                    active.run_id, queued.id
                ),
            ))?;
            Ok(Some(vec![user, assistant]))
        }
    }
}

pub(super) fn clarification_response_context_for_turn(
    store: &AppStore,
    conversation_id: &str,
    response: &str,
) -> AppResult<Option<String>> {
    let response = response.trim();
    if response.is_empty() {
        return Ok(None);
    }
    let Some(mut run) = latest_needs_clarification_run(store, conversation_id)? else {
        return Ok(None);
    };
    let question = run
        .checkpoints
        .iter()
        .rev()
        .find(|checkpoint| checkpoint.state == "needs_clarification")
        .map(|checkpoint| checkpoint.summary.clone())
        .unwrap_or_else(|| "Clarification requested.".into());
    let now = now_iso();
    run.checkpoints.push(AgentCheckpointRecord {
        checkpoint_id: new_id("ckpt"),
        run_id: run.run_id.clone(),
        iteration: run.checkpoints.len() as u32 + 1,
        created_at: now.clone(),
        state: "clarification_response".into(),
        completed_call_ids: Vec::new(),
        event_refs: Vec::new(),
        summary: format!(
            "User clarification response: {}",
            truncate_for_prompt(response, 500)
        ),
    });
    run.state = "completed".into();
    run.error = None;
    run.completed_at = Some(now.clone());
    run.updated_at = now;
    store.save_agent_run(run.clone())?;
    Ok(Some(format!(
        "Continue the user's original task after a clarification exchange.\n\nOriginal request:\n{}\n\nClarification question:\n{}\n\nUser clarification response:\n{}\n\nUse this response to continue the original task. Do not ask the same clarification again unless the response is still insufficient.",
        run.user_request,
        question,
        response
    )))
}

fn latest_needs_clarification_run(
    store: &AppStore,
    conversation_id: &str,
) -> AppResult<Option<AgentRunRecord>> {
    let mut runs = store
        .agent_runs()?
        .into_iter()
        .filter(|run| {
            run.conversation_id == conversation_id
                && run.parent_run_id.is_none()
                && run.state == "needsClarification"
        })
        .collect::<Vec<_>>();
    runs.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
    Ok(runs.into_iter().next())
}

pub(super) fn pause_run_for_clarify_tool(
    store: &AppStore,
    app: Option<&AppHandle>,
    run: &mut AgentRunRecord,
    conversation_id: &str,
    tool_text: &str,
    event: &crate::models::ToolEvent,
) -> AppResult<Option<ChatMessage>> {
    if event.tool_name != "clarify" {
        return Ok(None);
    }
    let Ok(value) = serde_json::from_str::<Value>(tool_text) else {
        return Ok(None);
    };
    if value
        .get("requiresUserInput")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        != true
    {
        return Ok(None);
    }
    let question = value
        .get("question")
        .and_then(Value::as_str)
        .unwrap_or("Clarification required");
    let text = value
        .get("text")
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("Clarification required: {question}"));
    run.state = "needsClarification".into();
    run.error = None;
    run.updated_at = now_iso();
    run.checkpoints.push(AgentCheckpointRecord {
        checkpoint_id: new_id("ckpt"),
        run_id: run.run_id.clone(),
        iteration: run.checkpoints.len() as u32 + 1,
        created_at: run.updated_at.clone(),
        state: "needs_clarification".into(),
        completed_call_ids: event.call_id.clone().into_iter().collect(),
        event_refs: event
            .call_id
            .clone()
            .map(|call_id| vec![call_id])
            .unwrap_or_default(),
        summary: truncate_for_prompt(&text.replace('\n', " "), 500),
    });
    let saved_run = store.save_agent_run(run.clone())?;
    let assistant = store.append_message(ChatMessage::new(
        conversation_id.to_string(),
        "assistant",
        text,
        "desktop-agent-clarify",
    ))?;
    emit_agent_run_record(app, &saved_run, Some(&assistant));
    Ok(Some(assistant))
}

pub(super) fn normalize_busy_input_mode(mode: &str) -> String {
    match mode.trim().to_lowercase().as_str() {
        "steer" | "inject" | "plan" => "steer".into(),
        "interrupt" | "abort" | "replace" => "interrupt".into(),
        _ => "queue".into(),
    }
}

pub(super) fn check_agent_run_interrupted(
    store: &AppStore,
    run_id: &str,
    started_at: Instant,
    timeout_seconds: u64,
    post_tool_quiet_timeout_seconds: u64,
    app: Option<&AppHandle>,
) -> AppResult<bool> {
    let latest = store.agent_run(run_id)?;
    if latest.state == "aborted" {
        emit_agent_run_record(app, &latest, None);
        return Ok(true);
    }
    let effective_timeout_seconds = agent_run_effective_timeout_seconds(
        &latest,
        timeout_seconds,
        post_tool_quiet_timeout_seconds,
    );
    if effective_timeout_seconds > 0
        && agent_run_idle_for_timeout(&latest, started_at, effective_timeout_seconds)
    {
        let reason = agent_run_timeout_reason(&latest, effective_timeout_seconds);
        let aborted = store.abort_agent_run(run_id, Some(reason.clone()))?;
        spawn_session_finished_hooks(
            store,
            aborted.clone(),
            json!({
                "source": "agent_run_timeout",
                "reason": reason,
            }),
        );
        let assistant = store.append_message(ChatMessage::new(
            aborted.conversation_id.clone(),
            "assistant",
            format!("本轮 agent 已自动结束：{}", reason),
            "desktop-agent-error",
        ))?;
        emit_agent_run_record(app, &aborted, Some(&assistant));
        return Ok(true);
    }
    Ok(false)
}

pub(super) async fn await_agent_run_interruptible<F, T>(
    store: &AppStore,
    run_id: &str,
    started_at: Instant,
    timeout_seconds: u64,
    post_tool_quiet_timeout_seconds: u64,
    app: Option<&AppHandle>,
    future: F,
) -> AppResult<Option<T>>
where
    F: Future<Output = T>,
{
    tokio::pin!(future);
    loop {
        tokio::select! {
            output = &mut future => return Ok(Some(output)),
            _ = tokio::time::sleep(Duration::from_millis(500)) => {
                if check_agent_run_interrupted(
                    store,
                    run_id,
                    started_at,
                    timeout_seconds,
                    post_tool_quiet_timeout_seconds,
                    app,
                )? {
                    return Ok(None);
                }
            }
        }
    }
}

pub(super) fn apply_acp_session_mcp_scope(
    store: &AppStore,
    conversation: &Conversation,
    agent: &mut AgentDefinition,
) -> AppResult<()> {
    let has_session_mcp = conversation
        .metadata
        .pointer("/acpRuntimeConfig/mcpServers")
        .and_then(Value::as_array)
        .is_some_and(|items| !items.is_empty());
    if !has_session_mcp {
        return Ok(());
    }
    let prefix = format!(
        "acp_{}_",
        conversation
            .id
            .chars()
            .map(|ch| if ch.is_ascii_alphanumeric() || ch == '_' {
                ch
            } else {
                '_'
            })
            .collect::<String>()
    );
    let session_server_ids = store
        .static_list("mcpServers")?
        .into_iter()
        .filter_map(|server| {
            let id = server.get("id").and_then(Value::as_str)?.to_string();
            id.starts_with(&prefix).then_some(id)
        })
        .collect::<Vec<_>>();
    if !session_server_ids.is_empty() {
        agent.enabled_mcp_servers = session_server_ids;
    }
    Ok(())
}

pub(super) fn abort_agent_run_for_turn_aborted_marker(
    store: &AppStore,
    run_id: &str,
    text: &str,
    app: Option<&AppHandle>,
) -> AppResult<bool> {
    if !has_turn_aborted_marker(text) {
        return Ok(false);
    }
    let reason = "Provider reported turn_aborted before completing the turn.".to_string();
    let aborted = store.abort_agent_run(run_id, Some(reason.clone()))?;
    spawn_session_finished_hooks(
        store,
        aborted.clone(),
        json!({
            "source": "turn_aborted_marker",
            "reason": reason,
        }),
    );
    let assistant = store.append_message(ChatMessage::new(
        aborted.conversation_id.clone(),
        "assistant",
        format!("本轮 agent 已中止：{reason}"),
        "desktop-agent-error",
    ))?;
    emit_agent_run_record(app, &aborted, Some(&assistant));
    Ok(true)
}

pub(super) fn has_turn_aborted_marker(text: &str) -> bool {
    const TURN_ABORTED_MARKERS: [&str; 2] = ["<turn_aborted>", "<turn_aborted/>"];
    TURN_ABORTED_MARKERS
        .iter()
        .any(|marker| text.contains(marker))
}

fn agent_run_effective_timeout_seconds(
    run: &AgentRunRecord,
    timeout_seconds: u64,
    post_tool_quiet_timeout_seconds: u64,
) -> u64 {
    if post_tool_quiet_timeout_seconds > 0 && agent_run_last_activity_is_tool_result(run) {
        if timeout_seconds > 0 {
            post_tool_quiet_timeout_seconds.min(timeout_seconds)
        } else {
            post_tool_quiet_timeout_seconds
        }
    } else {
        timeout_seconds
    }
}

fn agent_run_last_activity_is_tool_result(run: &AgentRunRecord) -> bool {
    run.last_activity_desc
        .as_deref()
        .map(str::trim)
        .is_some_and(|activity| {
            activity.starts_with("tool completed:")
                || activity.starts_with("tool failed:")
                || activity.starts_with("tool error:")
        })
}

fn agent_run_idle_for_timeout(
    run: &AgentRunRecord,
    started_at: Instant,
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
    started_at.elapsed() >= Duration::from_secs(timeout_seconds)
}

fn agent_run_timeout_reason(run: &AgentRunRecord, timeout_seconds: u64) -> String {
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

pub(super) fn drain_agent_steers_into_observations(
    store: &AppStore,
    run: &mut AgentRunRecord,
    observations: &mut Vec<String>,
) -> AppResult<()> {
    let pending = store.drain_agent_run_steers(&run.run_id)?;
    if pending.is_empty() {
        return Ok(());
    }
    run.pending_steers.clear();
    for steer in &pending {
        observations.push(format!(
            "User steer injected before this planner step: {}",
            truncate_for_prompt(steer, 4000)
        ));
    }
    run.phase_events.push(AgentRunPhaseRecord {
        phase: "steer_injected".into(),
        detail: json!({
            "count": pending.len(),
            "previews": pending
                .iter()
                .map(|steer| truncate_for_prompt(steer, 180))
                .collect::<Vec<_>>()
        }),
        updated_at: now_iso(),
    });
    run.updated_at = now_iso();
    store.save_agent_run(run.clone())?;
    Ok(())
}

pub(super) fn recovery_reply(user_content: &str) -> String {
    let trimmed = user_content.trim();
    if trimmed.is_empty() {
        "Agent runtime recovery baseline is active. The previous full agent module must be restored before advanced tool orchestration is available.".into()
    } else {
        format!(
            "Agent runtime recovery baseline is active. I received: {trimmed}\n\nAdvanced Hermes-style tool orchestration is temporarily unavailable until the full agent module is restored."
        )
    }
}
