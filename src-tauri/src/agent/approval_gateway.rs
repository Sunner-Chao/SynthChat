use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use serde_json::{json, Value};
use tauri::AppHandle;

use crate::{
    error::{AppError, AppResult},
    mcp,
    models::{ChatMessage, McpCallResult, ToolApprovalRequest, ToolDefinition},
    store::AppStore,
};

use super::decision_parser::PROVIDER_TOOL_CALL_META_KEY;
use super::*;

pub async fn call_mcp_tool_with_retry(
    store: &AppStore,
    server_id: String,
    tool_name: String,
    payload: Value,
    timeout_seconds: Option<u64>,
    run_id: Option<&str>,
    retry_count: usize,
    retry_backoff_ms: usize,
) -> AppResult<McpCallResult> {
    let mut last = None;
    for attempt in 0..=retry_count {
        match mcp::call_tool(
            store,
            server_id.clone(),
            tool_name.clone(),
            payload.clone(),
            timeout_seconds,
            run_id,
        )
        .await
        {
            Ok(result) if result.ok || attempt == retry_count => return Ok(result),
            Ok(result)
                if result
                    .error
                    .as_deref()
                    .is_some_and(mcp::mcp_error_needs_reauth) =>
            {
                return Ok(result);
            }
            Ok(result) => last = Some(result),
            Err(error) if attempt == retry_count => return Err(error),
            Err(error) => {
                if mcp::mcp_error_needs_reauth(&error.to_string()) {
                    return Ok(McpCallResult {
                        ok: false,
                        timed_out: false,
                        elapsed_ms: 0,
                        stdout: String::new(),
                        stderr: error.to_string(),
                        error: Some(error.to_string()),
                    });
                }
                last = Some(McpCallResult {
                    ok: false,
                    timed_out: false,
                    elapsed_ms: 0,
                    stdout: String::new(),
                    stderr: error.to_string(),
                    error: Some(error.to_string()),
                });
            }
        }
        tokio::time::sleep(Duration::from_millis(retry_backoff_ms as u64)).await;
    }
    Ok(last.unwrap_or(McpCallResult {
        ok: false,
        timed_out: false,
        elapsed_ms: 0,
        stdout: String::new(),
        stderr: String::new(),
        error: Some("tool call retry loop ended without a result".into()),
    }))
}

async fn continue_agent_run_after_approval(
    store: &AppStore,
    approval: &ToolApprovalRequest,
    app: Option<&AppHandle>,
) -> AppResult<()> {
    let run_id = approval
        .run_id
        .as_deref()
        .ok_or_else(|| AppError::BadRequest("approved tool call missing runId".into()))?;
    let mut run = store.agent_run(run_id)?;
    let conversation = store.conversation(&run.conversation_id)?;
    let persona = store.persona(Some(&run.persona_id))?;
    let mut agent = store.agent(Some(&run.agent_id))?;
    apply_persona_tool_policy_to_agent(&persona, &mut agent);
    let effective_persona = effective_llm_persona(&persona, &agent);
    let providers = store.provider_candidates(selected_provider_id(&persona, &agent))?;
    let mut history = store.messages(&run.conversation_id, Some(30))?;
    let skill_blocks = crate::skills::prompt_blocks_for_request(store, &agent, &run.user_request)?;
    let memory_blocks = memory_prompt_blocks(store, &persona)?;
    let short_context = store.short_context(&run.conversation_id)?;
    let mcp_tools = available_mcp_tool_definitions(store, &agent)?;
    let native_tools =
        visible_tool_definitions_for_agent(store, &agent, ToolExecutionContext::Interactive)?;
    let mut observations = vec![approved_tool_observation(approval)];
    let mut assistant_text = String::new();
    let mut assistant_provider_data: Option<Value> = None;
    let mut assistant_model: Option<String> = None;
    let mut assistant_provider_id: Option<String> = None;

    run.state = "running".into();
    run.completed_at = None;
    run.updated_at = now_iso();
    let saved_run = store.save_agent_run(run.clone())?;
    emit_agent_run_record(app, &saved_run, None);

    let run_started_at = Instant::now();
    let chat_config = store.config()?.chat;
    let run_timeout_seconds = chat_config.agent_run_timeout_seconds;
    let post_tool_quiet_timeout_seconds = chat_config.agent_post_tool_quiet_timeout_seconds;
    let mut tool_guardrails = ToolLoopGuardrails::new(&chat_config);
    let mut failed_file_mutations: HashMap<String, String> = HashMap::new();
    let mut empty_response_recoveries: HashMap<String, u32> = HashMap::new();
    let mut iteration_budget = IterationBudget::new(agent.max_tool_iterations.max(1).min(90));
    for iteration in 0..iteration_budget.max_total() {
        if !iteration_budget.consume() {
            append_parent_phase_event(
                store,
                &run.run_id,
                "iteration_budget_exhausted",
                json!({
                    "used": iteration_budget.used(),
                    "maxTotal": iteration_budget.max_total(),
                    "remaining": iteration_budget.remaining(),
                    "continuation": "approval",
                }),
            )?;
            break;
        }
        if check_agent_run_interrupted(
            store,
            &run.run_id,
            run_started_at,
            run_timeout_seconds,
            post_tool_quiet_timeout_seconds,
            app,
        )? {
            return Ok(());
        }
        let prompt_observations = observations_for_prompt(store, &run.run_id, &observations)?;
        let planner_prompt = agent_planner_prompt_for_agent_context_with_store(
            store,
            &prompt_observations,
            &skill_blocks,
            &memory_blocks,
            &short_context,
            &mcp_tools,
            ToolExecutionContext::Interactive,
            &agent,
            Some(&effective_persona),
        );
        let pre_llm_contexts = run_pre_llm_call_hooks(store, &run.run_id, &run.user_request).await;
        let llm_user_content = inject_pre_llm_hook_context(&run.user_request, &pre_llm_contexts);
        let reply = match complete_chat_with_provider_failover(
            store,
            Some(&run.run_id),
            &providers,
            &effective_persona,
            planner_prompt.clone(),
            history.clone(),
            &llm_user_content,
            Some(&native_tools),
            None,
        )
        .await
        {
            Ok(reply) => reply,
            Err(error) => {
                if check_agent_run_interrupted(
                    store,
                    &run.run_id,
                    run_started_at,
                    run_timeout_seconds,
                    post_tool_quiet_timeout_seconds,
                    app,
                )? {
                    return Ok(());
                }
                let mut failed_run = store.agent_run(&run.run_id)?;
                let mut saved_failed_run = None;
                if failed_run.state != "aborted" {
                    failed_run.state = "failed".into();
                    failed_run.error = Some(error.to_string());
                    failed_run.updated_at = now_iso();
                    failed_run.completed_at = Some(failed_run.updated_at.clone());
                    saved_failed_run = Some(store.save_agent_run(failed_run)?);
                }
                let assistant = store.append_message(ChatMessage::new(
                    run.conversation_id.clone(),
                    "assistant",
                    format!("审批后的继续执行没有返回，是因为模型请求失败：{error}"),
                    "desktop-agent-error",
                ))?;
                if let Some(saved_failed_run) =
                    saved_failed_run.or_else(|| store.agent_run(&run.run_id).ok())
                {
                    emit_agent_run_record(app, &saved_failed_run, Some(&assistant));
                }
                return Ok(());
            }
        };
        if abort_agent_run_for_turn_aborted_marker(store, &run.run_id, &reply.content, app)? {
            return Ok(());
        }
        if check_agent_run_interrupted(
            store,
            &run.run_id,
            run_started_at,
            run_timeout_seconds,
            post_tool_quiet_timeout_seconds,
            app,
        )? {
            return Ok(());
        }
        if reply.finish_reason.as_deref() == Some("incomplete") {
            let recovery_key = "incomplete_response";
            let attempt = {
                let count = empty_response_recoveries
                    .entry(recovery_key.into())
                    .and_modify(|count| *count += 1)
                    .or_insert(1);
                *count
            };
            if attempt == 1 {
                let note = "Provider returned an incomplete Responses continuation (reasoning/commentary without final answer, or unfinished output item). Continue from the approved tool observations and return a valid planner JSON object: either {\"action\":\"tool\",...} or {\"action\":\"final\",\"content\":\"...\"}.";
                observations.push(format!(
                    "Continuation iteration {} LLM recovery: {}",
                    iteration + 1,
                    note
                ));
                append_parent_phase_event(
                    store,
                    &run.run_id,
                    "llm_recovery",
                    json!({
                        "kind": recovery_key,
                        "note": note,
                        "attempt": attempt,
                        "maxAttempts": 1,
                        "continuation": "approval",
                        "finishReason": reply.finish_reason.clone(),
                        "providerId": reply.provider_id.clone(),
                        "model": reply.model.clone(),
                    }),
                )?;
                continue;
            } else {
                append_parent_phase_event(
                    store,
                    &run.run_id,
                    "llm_recovery_exhausted",
                    json!({
                        "kind": recovery_key,
                        "attempts": empty_response_recoveries.clone(),
                        "continuation": "approval",
                        "finishReason": reply.finish_reason.clone(),
                        "providerId": reply.provider_id.clone(),
                        "model": reply.model.clone(),
                    }),
                )?;
            }
        }
        if reply.content.trim().is_empty() {
            if let Some(recovery) =
                next_empty_llm_response_recovery(&observations, &mut empty_response_recoveries)
            {
                observations.push(format!(
                    "Continuation iteration {} LLM recovery: {}",
                    iteration + 1,
                    recovery.note
                ));
                append_parent_phase_event(
                    store,
                    &run.run_id,
                    "llm_recovery",
                    json!({
                        "kind": recovery.kind,
                        "note": recovery.note,
                        "attempt": recovery.attempt,
                        "maxAttempts": recovery.max_attempts,
                        "afterTools": recovery.after_tools,
                        "continuation": "approval",
                        "finishReason": reply.finish_reason.clone(),
                        "providerId": reply.provider_id.clone(),
                        "model": reply.model.clone(),
                    }),
                )?;
                continue;
            } else {
                append_parent_phase_event(
                    store,
                    &run.run_id,
                    "llm_recovery_exhausted",
                    json!({
                        "kind": if observations.is_empty() { "empty_response" } else { "empty_response_after_tools" },
                        "attempts": empty_response_recoveries.clone(),
                        "continuation": "approval",
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
            &run.run_id,
            &run.conversation_id,
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
                        "Continuation iteration {} tool error: planner requested tool action without a valid tool name",
                        iteration + 1
                    ));
                    continue;
                }
                let refund_iteration_for_execute_code_only =
                    tool_batch_is_execute_code_only(&requests);
                for (tool_name, payload) in requests {
                    let guardrail_payload = payload.clone();
                    if let Some(outcome) =
                        tool_guardrails.before_call(&tool_name, &guardrail_payload)
                    {
                        let guardrail_message = outcome.message.clone();
                        observations.push(format!(
                            "Continuation iteration {} tool {} guardrail: {}",
                            iteration + 1,
                            tool_name,
                            guardrail_message
                        ));
                        if outcome.halt {
                            record_tool_failed_for_run(
                                store,
                                app,
                                &run.conversation_id,
                                &run.run_id,
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
                                    &run.conversation_id,
                                    &run.run_id,
                                    &tool_name,
                                    &mcp_tools,
                                    &guardrail_payload,
                                    &error,
                                )?;
                                observations.push(format!(
                                    "Continuation iteration {} tool {} approval error: {}",
                                    iteration + 1,
                                    tool_name,
                                    error
                                ));
                                continue;
                            }
                        };
                        let approval_reason = match apply_smart_approval_mode(
                            store,
                            &run.run_id,
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
                                    &run.conversation_id,
                                    &run.run_id,
                                    &tool_name,
                                    &mcp_tools,
                                    &guardrail_payload,
                                    &error,
                                )?;
                                observations.push(format!(
                                    "Continuation iteration {} tool {} approval error: {}",
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
                                &run.run_id,
                                "__internal",
                                &tool_name,
                                &payload,
                                &reason,
                            )
                            .await;
                            append_tool_approval_request(
                                store,
                                &run.conversation_id,
                                &persona.id,
                                &agent.id,
                                &run.run_id,
                                "__internal",
                                &tool_name,
                                payload,
                                reason,
                                ToolExecutionContext::Interactive,
                            )?;
                            mark_run_pending_approval(store, app, &run.run_id)?;
                            append_waiting_for_approval_message(
                                store,
                                &run.conversation_id,
                                "__internal",
                                &tool_name,
                            )?;
                            return Ok(());
                        }
                        record_tool_started_for_run(
                            store,
                            app,
                            &run.run_id,
                            "__internal",
                            &tool_name,
                            &payload,
                            iteration + 1,
                        )?;
                        run = store.agent_run(&run.run_id)?;
                        match execute_recovery_internal_tool(
                            store,
                            &agent,
                            &run.conversation_id,
                            &run.run_id,
                            &tool_name,
                            payload,
                            ToolExecutionContext::Interactive,
                            app,
                        )
                        .await
                        {
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
                                    &run.run_id,
                                    run_started_at,
                                    run_timeout_seconds,
                                    post_tool_quiet_timeout_seconds,
                                    app,
                                )? {
                                    return Ok(());
                                }
                                let context_text = persist_large_tool_result_for_context(
                                    store,
                                    &run.run_id,
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
                                        "Continuation iteration {} tool {} guardrail: {}",
                                        iteration + 1,
                                        tool_name,
                                        outcome.message
                                    ));
                                    if outcome.halt {
                                        assistant_text = outcome.message.clone();
                                        break;
                                    }
                                }
                                record_tool_event_for_run(
                                    store,
                                    app,
                                    &run.conversation_id,
                                    &run.run_id,
                                    event.clone(),
                                )?;
                                let mut tool_message = ChatMessage::new(
                                    run.conversation_id.clone(),
                                    "tool",
                                    json!({"type": "toolEvent", "event": event.clone()})
                                        .to_string(),
                                    "desktop-agent-tool",
                                );
                                tool_message.provider_data = reply.provider_data.clone();
                                let tool_message = store.append_message(tool_message)?;
                                history.push(tool_message);
                                run = store.agent_run(&run.run_id)?;
                                if pause_run_for_clarify_tool(
                                    store,
                                    app,
                                    &mut run,
                                    &conversation.id,
                                    &text,
                                    &event,
                                )?
                                .is_some()
                                {
                                    return Ok(());
                                }
                            }
                            Err(error) => {
                                record_tool_failed_for_run(
                                    store,
                                    app,
                                    &run.conversation_id,
                                    &run.run_id,
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
                                    "Continuation iteration {} tool {} error: {}",
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
                                        "Continuation iteration {} tool {} guardrail: {}",
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
                                    &run.conversation_id,
                                    &run.run_id,
                                    &tool_name,
                                    &mcp_tools,
                                    &guardrail_payload,
                                    &error,
                                )?;
                                observations.push(format!(
                                    "Continuation iteration {} tool {} approval error: {}",
                                    iteration + 1,
                                    tool_name,
                                    error
                                ));
                                continue;
                            }
                        };
                        let approval_reason = match apply_smart_approval_mode(
                            store,
                            &run.run_id,
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
                                    &run.conversation_id,
                                    &run.run_id,
                                    &tool_name,
                                    &mcp_tools,
                                    &guardrail_payload,
                                    &error,
                                )?;
                                observations.push(format!(
                                    "Continuation iteration {} tool {} approval error: {}",
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
                                &run.run_id,
                                &definition.server_id,
                                &definition.tool_name,
                                &payload,
                                &reason,
                            )
                            .await;
                            append_tool_approval_request(
                                store,
                                &run.conversation_id,
                                &persona.id,
                                &agent.id,
                                &run.run_id,
                                &definition.server_id,
                                &definition.tool_name,
                                payload,
                                reason,
                                ToolExecutionContext::Interactive,
                            )?;
                            mark_run_pending_approval(store, app, &run.run_id)?;
                            append_waiting_for_approval_message(
                                store,
                                &run.conversation_id,
                                &definition.server_id,
                                &definition.tool_name,
                            )?;
                            return Ok(());
                        }
                        record_tool_started_for_run(
                            store,
                            app,
                            &run.run_id,
                            &definition.server_id,
                            &definition.tool_name,
                            &payload,
                            iteration + 1,
                        )?;
                        run = store.agent_run(&run.run_id)?;
                        match execute_recovery_mcp_tool(
                            store,
                            &run.run_id,
                            &definition,
                            payload,
                            None,
                        )
                        .await
                        {
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
                                    &run.run_id,
                                    run_started_at,
                                    run_timeout_seconds,
                                    post_tool_quiet_timeout_seconds,
                                    app,
                                )? {
                                    return Ok(());
                                }
                                let context_text = persist_large_tool_result_for_context(
                                    store,
                                    &run.run_id,
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
                                        "Continuation iteration {} tool {} guardrail: {}",
                                        iteration + 1,
                                        tool_name,
                                        outcome.message
                                    ));
                                    if outcome.halt {
                                        assistant_text = outcome.message.clone();
                                        break;
                                    }
                                }
                                record_tool_event_for_run(
                                    store,
                                    app,
                                    &run.conversation_id,
                                    &run.run_id,
                                    event.clone(),
                                )?;
                                let mut tool_message = ChatMessage::new(
                                    run.conversation_id.clone(),
                                    "tool",
                                    json!({"type": "toolEvent", "event": event}).to_string(),
                                    "desktop-agent-tool",
                                );
                                tool_message.provider_data = reply.provider_data.clone();
                                let tool_message = store.append_message(tool_message)?;
                                history.push(tool_message);
                                run = store.agent_run(&run.run_id)?;
                            }
                            Err(error) => {
                                record_tool_failed_for_run(
                                    store,
                                    app,
                                    &run.conversation_id,
                                    &run.run_id,
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
                                    "Continuation iteration {} tool {} error: {}",
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
                                        "Continuation iteration {} tool {} guardrail: {}",
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
                            &run.conversation_id,
                            &run.run_id,
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
                            "Continuation iteration {} tool {} error: {}",
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
                                "Continuation iteration {} tool {} guardrail: {}",
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
                &run.run_id,
                "iteration_budget_exhausted",
                json!({
                    "used": iteration_budget.used(),
                    "maxTotal": iteration_budget.max_total(),
                    "remaining": iteration_budget.remaining(),
                    "continuation": "approval",
                }),
            )?;
            assistant_text = format!(
                "审批后的继续执行已达到 agent 迭代预算（{}/{}），当前没有得到最终回答。\n\n{}",
                iteration_budget.used(),
                iteration_budget.max_total(),
                observations.join("\n\n")
            );
        } else {
            assistant_text = format!(
                "审批后的工具调用已执行，但当前恢复版 agent loop 未得到最终回答。\n\n{}",
                observations.join("\n\n")
            );
        }
    }
    normalize_guardrail_halt_reply(&mut assistant_text, &observations);
    append_file_mutation_footer(&mut assistant_text, &failed_file_mutations);
    assistant_text = sanitize_visible_assistant_reply(&assistant_text);
    assistant_text = run_transform_llm_output_hooks(
        store,
        &run.run_id,
        &run.user_request,
        &assistant_text,
        assistant_model.as_deref(),
        assistant_provider_id.as_deref(),
    )
    .await;
    run_post_llm_call_hooks(
        store,
        &run.run_id,
        &run.user_request,
        &assistant_text,
        assistant_model.as_deref(),
        assistant_provider_id.as_deref(),
    )
    .await;
    if check_agent_run_interrupted(
        store,
        &run.run_id,
        run_started_at,
        run_timeout_seconds,
        post_tool_quiet_timeout_seconds,
        app,
    )? {
        return Ok(());
    }
    let mut assistant_message = ChatMessage::new(
        conversation.id,
        "assistant",
        assistant_text,
        "desktop-agent",
    );
    assistant_message.provider_data = assistant_provider_data;
    let assistant = store.append_message(assistant_message)?;
    let mut final_run = store.agent_run(&run.run_id)?;
    final_run.state = "completed".into();
    final_run.updated_at = now_iso();
    final_run.completed_at = Some(final_run.updated_at.clone());
    let saved_final_run = store.save_agent_run(final_run)?;
    run_session_finished_hooks(
        store,
        &saved_final_run,
        json!({"source": "approval_continuation"}),
    )
    .await;
    emit_agent_run_record(app, &saved_final_run, Some(&assistant));
    Ok(())
}

pub(super) fn approved_tool_observation(approval: &ToolApprovalRequest) -> String {
    let result = approval.result.as_ref();
    let stdout = result
        .and_then(|value| value.get("stdout"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let stderr = result
        .and_then(|value| value.get("stderr"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let error = result
        .and_then(|value| value.get("error"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let text = if !stdout.trim().is_empty() {
        stdout
    } else if !stderr.trim().is_empty() {
        stderr
    } else {
        error
    };
    format!(
        "Approved tool {}.{} result:\n{}",
        approval.server_id, approval.tool_name, text
    )
}

fn mark_run_pending_approval(
    store: &AppStore,
    app: Option<&AppHandle>,
    run_id: &str,
) -> AppResult<()> {
    let mut run = store.agent_run(run_id)?;
    run.state = "pendingApproval".into();
    run.updated_at = now_iso();
    let saved_run = store.save_agent_run(run)?;
    emit_agent_run_record(app, &saved_run, None);
    Ok(())
}

fn append_waiting_for_approval_message(
    store: &AppStore,
    conversation_id: &str,
    server_id: &str,
    tool_name: &str,
) -> AppResult<ChatMessage> {
    store.append_message(ChatMessage::new(
        conversation_id.to_string(),
        "assistant",
        format!("下一步工具调用正在等待审批：{} · {}", server_id, tool_name),
        "desktop-agent",
    ))
}

pub async fn approve_tool_call_and_resume(
    store: &AppStore,
    approval_id: String,
    timeout_seconds: Option<u64>,
    app: Option<&AppHandle>,
) -> AppResult<ToolApprovalRequest> {
    let approval = approve_tool_call_common(store, approval_id, timeout_seconds, app).await?;
    if approval.status == "approved" {
        continue_agent_run_after_approval(store, &approval, app).await?;
    }
    Ok(approval)
}

pub async fn approve_tool_call_always_and_resume(
    store: &AppStore,
    approval_id: String,
    timeout_seconds: Option<u64>,
    app: Option<&AppHandle>,
) -> AppResult<ToolApprovalRequest> {
    let approval = store.tool_approval(&approval_id)?;
    store.trust_tool_pattern(format!("{}.{}", approval.server_id, approval.tool_name))?;
    approve_tool_call_and_resume(store, approval_id, timeout_seconds, app).await
}

pub async fn approve_tool_call_server_and_resume(
    store: &AppStore,
    approval_id: String,
    timeout_seconds: Option<u64>,
    app: Option<&AppHandle>,
) -> AppResult<ToolApprovalRequest> {
    let approval = store.tool_approval(&approval_id)?;
    store.trust_tool_pattern(format!("{}.*", approval.server_id))?;
    approve_tool_call_and_resume(store, approval_id, timeout_seconds, app).await
}

pub(super) async fn approve_tool_call_common(
    store: &AppStore,
    approval_id: String,
    timeout_seconds: Option<u64>,
    app: Option<&AppHandle>,
) -> AppResult<ToolApprovalRequest> {
    let approval = store.tool_approval(&approval_id)?;
    if approval.status != "pending" {
        return Ok(approval);
    }
    let run_id_for_interrupt = approval.run_id.clone();
    let run_started_at = Instant::now();
    let chat_config = store.config()?.chat;
    let run_timeout_seconds = chat_config.agent_run_timeout_seconds;
    let post_tool_quiet_timeout_seconds = chat_config.agent_post_tool_quiet_timeout_seconds;
    let result = if approval.server_id == "__internal" {
        let agent_id = approval
            .agent_id
            .as_deref()
            .ok_or_else(|| AppError::BadRequest("internal approval missing agentId".into()))?;
        let conversation_id = approval.conversation_id.as_deref().ok_or_else(|| {
            AppError::BadRequest("internal approval missing conversationId".into())
        })?;
        let run_id = approval
            .run_id
            .as_deref()
            .ok_or_else(|| AppError::BadRequest("internal approval missing runId".into()))?;
        let agent = store.agent(Some(agent_id))?;
        let future = execute_recovery_internal_tool(
            store,
            &agent,
            conversation_id,
            run_id,
            &approval.tool_name,
            approval.payload.clone(),
            ToolExecutionContext::Interactive,
            app,
        );
        match await_agent_run_interruptible(
            store,
            run_id,
            run_started_at,
            run_timeout_seconds,
            post_tool_quiet_timeout_seconds,
            app,
            future,
        )
        .await?
        {
            Some(Ok((stdout, event))) => {
                record_tool_event_for_run(store, app, conversation_id, run_id, event.clone())?;
                McpCallResult {
                    ok: true,
                    timed_out: false,
                    elapsed_ms: 0,
                    stdout,
                    stderr: String::new(),
                    error: None,
                }
            }
            Some(Err(error)) => McpCallResult {
                ok: false,
                timed_out: false,
                elapsed_ms: 0,
                stdout: String::new(),
                stderr: error.to_string(),
                error: Some(error.to_string()),
            },
            None => McpCallResult {
                ok: false,
                timed_out: false,
                elapsed_ms: 0,
                stdout: String::new(),
                stderr: "Agent run was aborted before the approved tool completed.".into(),
                error: Some("Agent run was aborted before the approved tool completed.".into()),
            },
        }
    } else {
        let replay_payload = approval.payload.clone();
        let execution_payload = strip_provider_tool_call_metadata(approval.payload.clone());
        let future = call_mcp_tool_with_retry(
            store,
            approval.server_id.clone(),
            approval.tool_name.clone(),
            execution_payload,
            timeout_seconds,
            approval.run_id.as_deref(),
            0,
            0,
        );
        let result = if let Some(run_id) = run_id_for_interrupt.as_deref() {
            await_agent_run_interruptible(
                store,
                run_id,
                run_started_at,
                run_timeout_seconds,
                post_tool_quiet_timeout_seconds,
                app,
                future,
            )
            .await?
            .transpose()?
            .unwrap_or_else(|| McpCallResult {
                ok: false,
                timed_out: false,
                elapsed_ms: 0,
                stdout: String::new(),
                stderr: "Agent run was aborted before the approved MCP tool completed.".into(),
                error: Some("Agent run was aborted before the approved MCP tool completed.".into()),
            })
        } else {
            future.await?
        };
        if let (Some(conversation_id), Some(run_id)) = (
            approval.conversation_id.as_deref(),
            approval.run_id.as_deref(),
        ) {
            let definition = ToolDefinition {
                name: format!("{}.{}", approval.server_id, approval.tool_name),
                display_name: approval.tool_name.clone(),
                description: String::new(),
                source: "mcp".into(),
                server_id: approval.server_id.clone(),
                tool_name: approval.tool_name.clone(),
                input_schema: json!({"type": "object"}),
                requires_approval: false,
            };
            let mut event = mcp_result_to_tool_event(run_id, &definition, &result);
            event.raw = Some(redact_json_value(
                json!({"payload": replay_payload, "result": result.clone()}),
            ));
            record_tool_event_for_run(store, app, conversation_id, run_id, event.clone())?;
        }
        result
    };
    let updated = store.update_tool_approval(
        &approval_id,
        if result.ok { "approved" } else { "failed" },
        Some(json!(result)),
        result.error.clone(),
    )?;
    run_post_approval_response_hooks(store, &updated).await;
    Ok(updated)
}

fn strip_provider_tool_call_metadata(mut payload: Value) -> Value {
    if let Some(object) = payload.as_object_mut() {
        object.remove(PROVIDER_TOOL_CALL_META_KEY);
    }
    payload
}

pub fn deny_tool_call_and_update_run(
    store: &AppStore,
    approval_id: String,
    reason: Option<String>,
    app: Option<&AppHandle>,
) -> AppResult<ToolApprovalRequest> {
    let approval = store.tool_approval(&approval_id)?;
    let denial_reason = reason
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("Tool approval was denied by the user.")
        .to_string();
    let updated =
        store.update_tool_approval(&approval_id, "denied", None, Some(denial_reason.clone()))?;
    spawn_post_approval_response_hooks(store, updated.clone());
    if let Some(run_id) = approval.run_id.as_deref() {
        if let Ok(aborted) = store.abort_agent_run(
            run_id,
            Some(format!(
                "Tool approval denied for {}.{}: {}",
                approval.server_id, approval.tool_name, denial_reason
            )),
        ) {
            spawn_session_finished_hooks(
                store,
                aborted.clone(),
                json!({
                    "source": "approval_denied",
                    "server_id": approval.server_id,
                    "tool_name": approval.tool_name,
                    "reason": denial_reason,
                }),
            );
            if let Some(conversation_id) = approval.conversation_id.as_deref() {
                let assistant = store.append_message(ChatMessage::new(
                    conversation_id.to_string(),
                    "assistant",
                    format!(
                        "工具调用已拒绝，当前 agent run 已结束：{} · {}。\n\n原因：{}",
                        approval.server_id, approval.tool_name, denial_reason
                    ),
                    "desktop-agent-error",
                ))?;
                emit_agent_run_record(app, &aborted, Some(&assistant));
            } else {
                emit_agent_run_record(app, &aborted, None);
            }
        }
    }
    Ok(updated)
}
