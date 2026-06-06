use serde_json::{json, Value};

use crate::{
    error::{AppError, AppResult},
    llm::{complete_chat, complete_text_prompt, estimate_tokens},
    models::{
        new_id, now_iso, AgentDefinition, AgentRunRecord, ChatMessage, Conversation, Persona,
        PlannerTraceRecord, SendChatRequest, SkillPromptBlock, ToolApprovalRequest, ToolDefinition,
        ToolEvent, ToolRouterTraceRecord, ToolTraceEntry,
    },
    mcp,
    skills,
    store::AppStore,
};

pub async fn run_chat_turn(store: &AppStore, request: SendChatRequest) -> AppResult<Vec<ChatMessage>> {
    let clean = request.content.trim().to_string();
    if clean.is_empty() {
        return Ok(vec![]);
    }

    let persona = store.persona(request.persona_id.as_deref())?;
    let conversation = match request.conversation_id.as_deref() {
        Some(id) if !id.trim().is_empty() => {
            let existing = store.conversation(id)?;
            if existing.persona_id.as_deref() == Some(persona.id.as_str()) {
                existing
            } else {
                store.create_conversation(None, Some(persona.id.clone()))?
            }
        }
        _ => store.create_conversation(None, Some(persona.id.clone()))?,
    };
    let agent = store.agent(Some(&conversation.agent_id))?;

    let user_message = ChatMessage::new(conversation.id.clone(), "user", clean.clone(), "desktop");
    store.append_message(user_message.clone())?;

    if let Some(summary) = parse_remember_command(&clean) {
        let memory = store.save_memory(crate::models::MemoryEntry {
            id: String::new(),
            persona_id: persona.id.clone(),
            summary: summary.to_string(),
            importance: 4,
            created_at: String::new(),
            updated_at: String::new(),
        })?;
        let assistant = ChatMessage::new(
            conversation.id,
            "assistant",
            format!("已写入 `{}` 的长期记忆：{}", persona.name, memory.summary),
            "desktop",
        );
        store.append_message(assistant.clone())?;
        return Ok(vec![user_message, assistant]);
    }

    let mut run = AgentRunRecord::new(conversation.id.clone(), persona.id.clone(), agent.id.clone());
    store.save_agent_run(run.clone())?;

    if let Some((server_id, tool_name, payload)) = parse_tool_command(&clean) {
        let result = mcp::call_tool(store, server_id.clone(), tool_name.clone(), payload, None).await?;
        let traces = store.tool_traces()?;
        let latest_event = traces
            .iter()
            .rev()
            .find(|trace| trace.server_id == server_id && trace.tool_name == tool_name)
            .map(|trace| trace.event.clone());
        if let Some(event) = latest_event {
            let tool_message = ChatMessage::new(
                conversation.id.clone(),
                "tool",
                json!({"type": "toolEvent", "event": event}).to_string(),
                "desktop",
            );
            store.append_message(tool_message.clone())?;
            run.tool_events.push(serde_json::to_value(&event).unwrap_or_else(|_| json!({})));
            run.state = if result.ok { "completed".into() } else { "failed".into() };
            run.error = result.error.clone();
            run.updated_at = now_iso();
            run.completed_at = Some(run.updated_at.clone());
            store.save_agent_run(run)?;
            let assistant = ChatMessage::new(
                conversation.id,
                "assistant",
                if result.ok {
                    format!("工具 `{server_id}.{tool_name}` 调用完成。")
                } else {
                    format!("工具 `{server_id}.{tool_name}` 调用失败：{}", result.error.unwrap_or_else(|| result.stderr))
                },
                "desktop",
            );
            store.append_message(assistant.clone())?;
            return Ok(vec![user_message, tool_message, assistant]);
        }
    }

    let result: Result<AgentTurn, AppError> = async {
        let config = store.config()?;
        let history = store.messages(&conversation.id, Some(config.chat.max_context_rounds * 2 + 1))?;
        let provider_id = if !persona.llm_provider.trim().is_empty() {
            Some(persona.llm_provider.as_str())
        } else if !agent.llm_provider.trim().is_empty() {
            Some(agent.llm_provider.as_str())
        } else {
            None
        };
        let provider = store.provider(provider_id)?;
        let memories = if persona
            .memory
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(true)
            && persona
                .memory
                .get("includeInPrompt")
                .and_then(Value::as_bool)
                .unwrap_or(true)
        {
            let max = persona.memory.get("maxMemories").and_then(Value::as_u64).unwrap_or(50) as usize;
            let mut items = store.memories(Some(&persona.id))?;
            items.truncate(max.max(1));
            items
        } else {
            vec![]
        };
        let skill_blocks = skills::prompt_blocks_for_request(store, &agent, &clean)?;
        let worldbook_blocks = matching_worldbook_blocks(store, &persona, &clean)?;
        let system_prompt = build_system_prompt(&conversation, &persona, &memories, &skill_blocks, &worldbook_blocks);
        if let Some(tool_turn) = maybe_run_auto_tool(
            store,
            &conversation,
            &persona,
            &agent,
            &mut run,
            &provider,
            &system_prompt,
            &clean,
            &history,
        ).await? {
            Ok(tool_turn)
        } else {
            let reply = complete_chat(&provider, &persona, system_prompt, history, &clean).await?;
            Ok(AgentTurn::Plain(reply))
        }
    }
    .await;

    match result {
        Ok(AgentTurn::Plain(reply)) => {
            store.add_usage(reply.prompt_tokens, reply.completion_tokens)?;
            let assistant = ChatMessage::new(conversation.id.clone(), "assistant", reply.content, "desktop-stream");
            store.append_message(assistant.clone())?;
            run.state = "completed".into();
            run.updated_at = now_iso();
            run.completed_at = Some(run.updated_at.clone());
            run.checkpoints.push(json!({
                "checkpointId": format!("ckpt-{}", run.run_id),
                "runId": run.run_id,
                "iteration": 1,
                "createdAt": run.updated_at,
                "state": "completed",
                "summary": "LLM reply generated",
                "completedCallIds": [],
                "eventRefs": []
            }));
            store.save_agent_run(run)?;
            Ok(vec![user_message, assistant])
        }
        Ok(AgentTurn::WithTools { tool_messages, assistant, prompt_tokens, completion_tokens }) => {
            store.add_usage(prompt_tokens, completion_tokens)?;
            run.state = "completed".into();
            run.updated_at = now_iso();
            run.completed_at = Some(run.updated_at.clone());
            run.checkpoints.push(json!({
                "checkpointId": format!("ckpt-{}", run.run_id),
                "runId": run.run_id,
                "iteration": 1,
                "createdAt": run.updated_at,
                "state": "completed",
                "summary": "Tool routed and final reply generated",
                "completedCallIds": [],
                "eventRefs": []
            }));
            store.save_agent_run(run)?;
            let mut messages = vec![user_message];
            messages.extend(tool_messages);
            messages.push(assistant);
            Ok(messages)
        }
        Err(error) => {
            let message = ChatMessage::new(
                conversation.id.clone(),
                "assistant",
                format!("对话链执行失败：{error}"),
                "desktop",
            );
            store.append_message(message.clone())?;
            run.state = "failed".into();
            run.error = Some(error.to_string());
            run.updated_at = now_iso();
            run.completed_at = Some(run.updated_at.clone());
            store.save_agent_run(run)?;
            Ok(vec![user_message, message])
        }
    }
}

enum AgentTurn {
    Plain(crate::llm::LlmReply),
    WithTools {
        tool_messages: Vec<ChatMessage>,
        assistant: ChatMessage,
        prompt_tokens: usize,
        completion_tokens: usize,
    },
}

fn build_system_prompt(
    conversation: &Conversation,
    persona: &Persona,
    memories: &[crate::models::MemoryEntry],
    skills: &[SkillPromptBlock],
    worldbooks: &[String],
) -> String {
    let mut parts = vec![
        persona.system_prompt.clone(),
        persona.system_instructions.clone(),
        "## Current Session Context".into(),
        "**Source:** SynthChat desktop (local machine)".into(),
        format!("**Conversation ID:** {}", conversation.id),
        format!("**Persona:** {}", persona.name),
        "Keep responses aligned with the persona and the user's latest request.".into(),
    ];

    if !persona.character_prompt.trim().is_empty() {
        parts.push("## Character".into());
        parts.push(persona.character_prompt.clone());
    }
    if !persona.output_examples.trim().is_empty() {
        parts.push("## Output Examples".into());
        parts.push(persona.output_examples.clone());
    }
    if !memories.is_empty() {
        parts.push("## Long-term Memory".into());
        for memory in memories.iter().take(8) {
            parts.push(format!("- {}", memory.summary));
        }
    }
    if !worldbooks.is_empty() {
        parts.push("## Worldbook Context".into());
        parts.extend(worldbooks.iter().take(12).cloned());
    }
    if !skills.is_empty() {
        parts.push("## Active Skills".into());
        parts.push(
            "Follow these skill instructions when they are relevant to the user's latest request. Skill instructions are local capability guides, not user content.".into(),
        );
        for skill in skills {
            parts.push(format!("### Skill: {} ({})\n{}", skill.name, skill.id, skill.content));
        }
    }
    parts.join("\n\n")
}

fn matching_worldbook_blocks(store: &AppStore, persona: &Persona, user_request: &str) -> AppResult<Vec<String>> {
    let request = user_request.to_lowercase();
    let mut blocks = Vec::new();
    for book in store.static_list("worldbooks")? {
        let bound = book.get("boundPersonas").and_then(Value::as_array).cloned().unwrap_or_default();
        let applies_to_persona = bound.is_empty()
            || bound
                .iter()
                .filter_map(Value::as_str)
                .any(|id| id == persona.id);
        if !applies_to_persona {
            continue;
        }

        let book_name = book.get("name").and_then(Value::as_str).unwrap_or("Worldbook");
        let Some(sections) = book.get("sections").and_then(Value::as_array) else {
            continue;
        };
        for section in sections {
            if !section.get("enabled").and_then(Value::as_bool).unwrap_or(true) {
                continue;
            }
            let key = section.get("key").and_then(Value::as_str).unwrap_or("").trim();
            let content = section.get("content").and_then(Value::as_str).unwrap_or("").trim();
            if content.is_empty() {
                continue;
            }
            let key_matches = key.is_empty() || request.contains(&key.to_lowercase());
            if key_matches {
                blocks.push(format!("- [{} / {}] {}", book_name, if key.is_empty() { "general" } else { key }, content));
            }
        }
    }
    Ok(blocks)
}

#[allow(dead_code)]
fn context_token_estimate(messages: &[ChatMessage]) -> usize {
    messages.iter().map(|m| estimate_tokens(&m.content)).sum()
}

async fn maybe_run_auto_tool(
    store: &AppStore,
    conversation: &Conversation,
    persona: &Persona,
    agent: &AgentDefinition,
    run: &mut AgentRunRecord,
    provider: &crate::models::LlmProvider,
    system_prompt: &str,
    user_request: &str,
    history: &[ChatMessage],
) -> AppResult<Option<AgentTurn>> {
    if !agent.mcp_enabled || provider.provider_type == "echo" {
        return Ok(None);
    }

    let available_tools = available_tools_for_agent(store, agent)?;
    if available_tools.is_empty() {
        return Ok(None);
    }

    let max_iterations = agent.max_tool_iterations.max(1).min(24);
    let mut observations: Vec<String> = Vec::new();
    let mut tool_messages: Vec<ChatMessage> = Vec::new();
    let mut prompt_tokens = 0usize;
    let mut completion_tokens = 0usize;
    let mut seen_calls = std::collections::HashSet::<String>::new();
    let mut delegated_count = 0u32;

    for iteration in 0..max_iterations {
        let planner_prompt = build_tool_planner_prompt(user_request, &available_tools, &observations, iteration, max_iterations);
        let route_output = complete_text_prompt(
            provider,
            persona,
            "You are SynthChat's agent planner and tool router. Return compact JSON only. Do not explain.".into(),
            planner_prompt.clone(),
        )
        .await;

        let (output, decision, error, planner_usage) = match route_output {
            Ok(reply) => {
                let decision = extract_json_object(&reply.content);
                let usage = (reply.prompt_tokens, reply.completion_tokens);
                (reply.content, decision, None, usage)
            }
            Err(err) => (String::new(), None, Some(err.to_string()), (0, 0)),
        };
        prompt_tokens += planner_usage.0;
        completion_tokens += planner_usage.1;

        let route_status = if error.is_some() {
            "failed"
        } else if decision.is_some() {
            "completed"
        } else {
            "ignored"
        };

        store.append_tool_router_trace(ToolRouterTraceRecord {
            id: new_id("router"),
            created_at: now_iso(),
            conversation_id: conversation.id.clone(),
            persona_id: persona.id.clone(),
            agent_id: agent.id.clone(),
            semantic_intent: decision
                .as_ref()
                .and_then(|d| d.get("intent").and_then(Value::as_str))
                .unwrap_or("unknown")
                .to_string(),
            user_request: user_request.to_string(),
            prompt: planner_prompt,
            output: output.clone(),
            decision: decision.clone(),
            status: route_status.into(),
            error: error.clone(),
        })?;

        let Some(decision) = decision else {
            if observations.is_empty() {
                return Ok(None);
            }
            break;
        };

        let action = decision.get("action").and_then(Value::as_str).unwrap_or("").trim();
        let use_tool = decision.get("useTool").and_then(Value::as_bool).unwrap_or(false);
        if action.eq_ignore_ascii_case("final") || (!use_tool && action.is_empty()) {
            if observations.is_empty() {
                return Ok(None);
            }
            break;
        }
        if !use_tool && !action.eq_ignore_ascii_case("tool") {
            if observations.is_empty() {
                return Ok(None);
            }
            break;
        }

        let tool_name = decision.get("tool").and_then(Value::as_str).unwrap_or("").trim();
        let Some(tool) = resolve_tool(tool_name, &available_tools) else {
            store.append_planner_trace(PlannerTraceRecord {
                id: new_id("planner"),
                run_id: run.run_id.clone(),
                conversation_id: conversation.id.clone(),
                persona_id: persona.id.clone(),
                agent_id: agent.id.clone(),
                iteration,
                created_at: now_iso(),
                input: user_request.to_string(),
                output,
                parsed_step: "tool_not_found".into(),
                error: Some(format!("planner selected unavailable tool: {tool_name}")),
            })?;
            if observations.is_empty() {
                return Ok(None);
            }
            break;
        };

        let arguments = decision.get("arguments").cloned().unwrap_or_else(|| json!({}));
        let call_payload = tool_payload_for_definition(store, &tool, arguments.clone())?;
        let call_signature = format!("{}.{}:{}", tool.server_id, tool.tool_name, compact_json(&arguments));
        if !seen_calls.insert(call_signature.clone()) {
            store.append_planner_trace(PlannerTraceRecord {
                id: new_id("planner"),
                run_id: run.run_id.clone(),
                conversation_id: conversation.id.clone(),
                persona_id: persona.id.clone(),
                agent_id: agent.id.clone(),
                iteration,
                created_at: now_iso(),
                input: user_request.to_string(),
                output,
                parsed_step: "repeated_tool_call_guardrail".into(),
                error: Some(format!("repeated tool call blocked: {call_signature}")),
            })?;
            observations.push(format!("Iteration {}: repeated tool call blocked: {}", iteration + 1, call_signature));
            break;
        }

        if tool.requires_approval {
            let approval = store.append_tool_approval(ToolApprovalRequest {
                id: new_id("approval"),
                created_at: now_iso(),
                updated_at: now_iso(),
                status: "pending".into(),
                conversation_id: Some(conversation.id.clone()),
                persona_id: Some(persona.id.clone()),
                agent_id: Some(agent.id.clone()),
                run_id: Some(run.run_id.clone()),
                server_id: tool.server_id.clone(),
                tool_name: tool.tool_name.clone(),
                payload: call_payload.clone(),
                reason: decision
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or("Tool requires approval before execution.")
                    .to_string(),
                result: None,
                error: None,
            })?;
            store.append_planner_trace(PlannerTraceRecord {
                id: new_id("planner"),
                run_id: run.run_id.clone(),
                conversation_id: conversation.id.clone(),
                persona_id: persona.id.clone(),
                agent_id: agent.id.clone(),
                iteration,
                created_at: now_iso(),
                input: user_request.to_string(),
                output,
                parsed_step: format!("approval_required {}.{}", tool.server_id, tool.tool_name),
                error: None,
            })?;

            let event = ToolEvent {
                status: Some("pendingApproval".into()),
                reference_id: Some(approval.id.clone()),
                call_id: None,
                run_id: Some(run.run_id.clone()),
                checkpoint_id: None,
                event_type: "tool_approval".into(),
                server_id: tool.server_id.clone(),
                tool_name: tool.tool_name.clone(),
                ok: false,
                timed_out: false,
                elapsed_ms: 0,
                title: format!("等待审批 · {}.{}", tool.server_id, tool.tool_name),
                summary: format!("工具调用需要审批：{}", approval.reason),
                path: None,
                exists: None,
                mime_type: None,
                text: Some(approval.payload.to_string()),
                error: None,
                raw: Some(json!({"approvalId": approval.id, "status": "pending"})),
            };
            store.append_tool_trace(ToolTraceEntry {
                id: new_id("trace"),
                created_at: now_iso(),
                server_id: tool.server_id.clone(),
                tool_name: tool.tool_name.clone(),
                ok: false,
                timed_out: false,
                elapsed_ms: 0,
                payload: call_payload,
                event: event.clone(),
                error: None,
            })?;
            run.tool_events.push(serde_json::to_value(&event).unwrap_or_else(|_| json!({})));
            let tool_message = ChatMessage::new(
                conversation.id.clone(),
                "tool",
                json!({"type": "toolEvent", "event": event}).to_string(),
                "desktop",
            );
            store.append_message(tool_message.clone())?;
            tool_messages.push(tool_message);
            let assistant = ChatMessage::new(
                conversation.id.clone(),
                "assistant",
                "这个工具调用需要审批，已暂停执行。请在工具审批列表中批准或拒绝后再继续。".into(),
                "desktop",
            );
            store.append_message(assistant.clone())?;
            return Ok(Some(AgentTurn::WithTools {
                tool_messages,
                assistant,
                prompt_tokens,
                completion_tokens,
            }));
        }

        if tool.source == "internal" && tool.tool_name == "delegate_task" {
            if delegated_count >= agent.max_subagents {
                store.append_planner_trace(PlannerTraceRecord {
                    id: new_id("planner"),
                    run_id: run.run_id.clone(),
                    conversation_id: conversation.id.clone(),
                    persona_id: persona.id.clone(),
                    agent_id: agent.id.clone(),
                    iteration,
                    created_at: now_iso(),
                    input: user_request.to_string(),
                    output,
                    parsed_step: "subagent_limit_reached".into(),
                    error: Some(format!("maxSubagents reached: {}", agent.max_subagents)),
                })?;
                observations.push(format!("Iteration {}: subagent delegation blocked because maxSubagents was reached.", iteration + 1));
                break;
            }
            delegated_count += 1;
            store.append_planner_trace(PlannerTraceRecord {
                id: new_id("planner"),
                run_id: run.run_id.clone(),
                conversation_id: conversation.id.clone(),
                persona_id: persona.id.clone(),
                agent_id: agent.id.clone(),
                iteration,
                created_at: now_iso(),
                input: user_request.to_string(),
                output: output.clone(),
                parsed_step: "delegate_task".into(),
                error: None,
            })?;

            let delegate_reply = run_delegate_task(provider, persona, user_request, &call_payload).await?;
            prompt_tokens += delegate_reply.prompt_tokens;
            completion_tokens += delegate_reply.completion_tokens;
            let event = ToolEvent {
                status: Some("completed".into()),
                reference_id: Some(format!("subagent-{}", delegated_count)),
                call_id: Some(new_id("call")),
                run_id: Some(run.run_id.clone()),
                checkpoint_id: None,
                event_type: "subagent_delegate".into(),
                server_id: "__internal".into(),
                tool_name: "delegate_task".into(),
                ok: true,
                timed_out: false,
                elapsed_ms: 0,
                title: format!("Subagent {}", delegated_count),
                summary: delegate_reply.content.chars().take(180).collect(),
                path: None,
                exists: None,
                mime_type: None,
                text: Some(delegate_reply.content.clone()),
                error: None,
                raw: Some(json!({"delegateIndex": delegated_count, "payload": call_payload})),
            };
            store.append_tool_trace(ToolTraceEntry {
                id: new_id("trace"),
                created_at: now_iso(),
                server_id: "__internal".into(),
                tool_name: "delegate_task".into(),
                ok: true,
                timed_out: false,
                elapsed_ms: 0,
                payload: call_payload,
                event: event.clone(),
                error: None,
            })?;
            run.tool_events.push(serde_json::to_value(&event).unwrap_or_else(|_| json!({})));
            let tool_message = ChatMessage::new(
                conversation.id.clone(),
                "tool",
                json!({"type": "toolEvent", "event": event}).to_string(),
                "desktop",
            );
            store.append_message(tool_message.clone())?;
            tool_messages.push(tool_message);
            observations.push(format!(
                "Iteration {}: delegated subtask to subagent {}; result={}",
                iteration + 1,
                delegated_count,
                truncate_for_prompt(&delegate_reply.content, 4000),
            ));
            continue;
        }

        store.append_planner_trace(PlannerTraceRecord {
            id: new_id("planner"),
            run_id: run.run_id.clone(),
            conversation_id: conversation.id.clone(),
            persona_id: persona.id.clone(),
            agent_id: agent.id.clone(),
            iteration,
            created_at: now_iso(),
            input: user_request.to_string(),
            output: output.clone(),
            parsed_step: format!("call {}.{}", tool.server_id, tool.tool_name),
            error: None,
        })?;

        let call_result = mcp::call_tool(
            store,
            tool.server_id.clone(),
            tool.tool_name.clone(),
            call_payload,
            None,
        )
        .await?;
        let latest_event = store
            .tool_traces()?
            .iter()
            .rev()
            .find(|trace| trace.server_id == tool.server_id && trace.tool_name == tool.tool_name)
            .map(|trace| trace.event.clone());
        let Some(event) = latest_event else {
            break;
        };

        run.tool_events.push(serde_json::to_value(&event).unwrap_or_else(|_| json!({})));
        let tool_message = ChatMessage::new(
            conversation.id.clone(),
            "tool",
            json!({"type": "toolEvent", "event": event}).to_string(),
            "desktop",
        );
        store.append_message(tool_message.clone())?;
        tool_messages.push(tool_message);
        observations.push(format!(
            "Iteration {}: called {}.{}; ok={}; stdout/result={}; stderr/error={}",
            iteration + 1,
            tool.server_id,
            tool.tool_name,
            call_result.ok,
            truncate_for_prompt(&call_result.stdout, 4000),
            truncate_for_prompt(&call_result.error.clone().unwrap_or(call_result.stderr), 2000),
        ));
    }

    if observations.is_empty() {
        return Ok(None);
    }

    let final_prompt = format!(
        "用户请求：\n{}\n\n工具执行观察：\n{}\n\n请基于这些观察直接回答用户。如果仍有缺口，明确说明缺口和下一步建议。",
        user_request,
        observations.join("\n\n"),
    );
    let mut final_history = history.to_vec();
    final_history.push(ChatMessage::new(conversation.id.clone(), "user", final_prompt, "internal"));
    let final_reply = complete_chat(
        provider,
        persona,
        system_prompt.to_string(),
        final_history,
        user_request,
    )
    .await?;

    let assistant = ChatMessage::new(conversation.id.clone(), "assistant", final_reply.content, "desktop-stream");
    store.append_message(assistant.clone())?;
    prompt_tokens += final_reply.prompt_tokens;
    completion_tokens += final_reply.completion_tokens;

    Ok(Some(AgentTurn::WithTools {
        tool_messages,
        assistant,
        prompt_tokens,
        completion_tokens,
    }))
}

fn available_tools_for_agent(store: &AppStore, agent: &AgentDefinition) -> AppResult<Vec<ToolDefinition>> {
    let enabled_servers = store
        .static_list("mcpServers")?
        .into_iter()
        .filter_map(|value| serde_json::from_value::<crate::models::McpServer>(value).ok())
        .filter(|server| server.enabled)
        .map(|server| server.id)
        .collect::<std::collections::HashSet<_>>();
    let configured_servers = agent.enabled_mcp_servers.iter().cloned().collect::<std::collections::HashSet<_>>();
    let mut tools = store
        .tool_definitions()?
        .into_iter()
        .filter(|tool| {
            if tool.source == "internal" {
                return true;
            }
            if !configured_servers.is_empty() {
                configured_servers.contains(&tool.server_id)
            } else {
                enabled_servers.contains(&tool.server_id)
            }
        })
        .collect::<Vec<_>>();
    if agent.max_subagents > 0 {
        tools.push(delegate_tool_definition());
    }
    Ok(tools)
}

fn delegate_tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: "delegate_task".into(),
        display_name: "Delegate Task".into(),
        description: "Delegate a focused subtask to an internal subagent and return a concise result for the parent planner.".into(),
        source: "internal".into(),
        server_id: "__internal".into(),
        tool_name: "delegate_task".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "task": {"type": "string", "description": "Focused subtask for the subagent."},
                "context": {"type": "string", "description": "Relevant context the subagent should use."},
                "expectedOutput": {"type": "string", "description": "Desired format or acceptance criteria."}
            },
            "required": ["task"]
        }),
        requires_approval: false,
    }
}

fn build_tool_planner_prompt(
    user_request: &str,
    tools: &[ToolDefinition],
    observations: &[String],
    iteration: u32,
    max_iterations: u32,
) -> String {
    let tool_lines = tools
        .iter()
        .take(32)
        .map(|tool| {
            format!(
                "- name: {}; serverId: {}; toolName: {}; description: {}; inputSchema: {}",
                tool.name,
                tool.server_id,
                tool.tool_name,
                tool.description,
                compact_json(&tool.input_schema)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let observation_text = if observations.is_empty() {
        "None yet.".to_string()
    } else {
        observations.join("\n\n")
    };
    format!(
        "Available tools:\n{}\n\nUser request:\n{}\n\nPrevious observations:\n{}\n\nIteration: {}/{}\n\nReturn JSON only with one of these shapes:\n{{\"action\":\"tool\",\"useTool\":true,\"tool\":\"serverId.toolName or registry name\",\"intent\":\"short label\",\"arguments\":{{}},\"reason\":\"short reason\"}}\n{{\"action\":\"final\",\"useTool\":false,\"tool\":\"\",\"intent\":\"answer\",\"arguments\":{{}},\"reason\":\"enough information\"}}\nCall another tool only when it is clearly useful, not repetitive, and arguments can be inferred. If the observations are enough to answer, choose final.",
        tool_lines,
        user_request,
        observation_text,
        iteration + 1,
        max_iterations,
    )
}

fn resolve_tool(name: &str, tools: &[ToolDefinition]) -> Option<ToolDefinition> {
    tools
        .iter()
        .find(|tool| tool.name == name || format!("{}.{}", tool.server_id, tool.tool_name) == name || tool.tool_name == name)
        .cloned()
}

fn tool_payload_for_definition(store: &AppStore, tool: &ToolDefinition, arguments: Value) -> AppResult<Value> {
    if tool.source != "capability" {
        return Ok(arguments);
    }
    let Some(adapter) = store
        .capability_adapters()?
        .into_iter()
        .find(|adapter| adapter.enabled && adapter.name == tool.name)
    else {
        return Ok(arguments);
    };

    let mut payload = match adapter.parameters {
        Value::Object(map) => map,
        _ => serde_json::Map::new(),
    };
    let arg_obj = arguments.as_object().cloned().unwrap_or_default();
    if adapter.param_mapping.is_empty() {
        for (key, value) in arg_obj {
            payload.insert(key, value);
        }
    } else {
        for (from, to) in adapter.param_mapping {
            if let Some(value) = arg_obj.get(&from) {
                payload.insert(to, value.clone());
            }
        }
    }
    for (key, value) in adapter.inject_fields {
        payload.insert(key, Value::String(value));
    }
    Ok(Value::Object(payload))
}

async fn run_delegate_task(
    provider: &crate::models::LlmProvider,
    persona: &Persona,
    parent_request: &str,
    payload: &Value,
) -> AppResult<crate::llm::LlmReply> {
    let task = payload.get("task").and_then(Value::as_str).unwrap_or("").trim();
    let context = payload.get("context").and_then(Value::as_str).unwrap_or("").trim();
    let expected = payload
        .get("expectedOutput")
        .or_else(|| payload.get("expected_output"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if task.is_empty() {
        return Err(AppError::BadRequest("delegate_task requires arguments.task".into()));
    }
    let user_prompt = format!(
        "Parent request:\n{}\n\nSubtask:\n{}\n\nContext:\n{}\n\nExpected output:\n{}\n\nReturn only the useful result for the parent agent. Be concise, cite uncertainty, and do not ask the user questions.",
        parent_request,
        task,
        if context.is_empty() { "(none)" } else { context },
        if expected.is_empty() { "Concise findings and next-step-ready answer." } else { expected },
    );
    complete_text_prompt(
        provider,
        persona,
        "You are a focused SynthChat subagent. Solve only the delegated subtask. Do not use tools. Do not roleplay beyond the task.".into(),
        user_prompt,
    )
    .await
}

fn extract_json_object(text: &str) -> Option<Value> {
    if let Ok(value) = serde_json::from_str::<Value>(text.trim()) {
        if value.is_object() {
            return Some(value);
        }
    }
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    if end <= start {
        return None;
    }
    serde_json::from_str::<Value>(&text[start..=end]).ok().filter(Value::is_object)
}

fn compact_json(value: &Value) -> String {
    let text = value.to_string();
    if text.len() > 500 {
        format!("{}...", &text[..500])
    } else {
        text
    }
}

fn truncate_for_prompt(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut out = text.chars().take(max_chars).collect::<String>();
    out.push_str("\n[truncated]");
    out
}

fn parse_tool_command(input: &str) -> Option<(String, String, serde_json::Value)> {
    let rest = input.trim().strip_prefix("/tool ")?;
    let (target, payload_text) = rest.split_once(' ').unwrap_or((rest, "{}"));
    let (server_id, tool_name) = target.split_once('.')?;
    if server_id.trim().is_empty() || tool_name.trim().is_empty() {
        return None;
    }
    let payload = serde_json::from_str(payload_text.trim()).unwrap_or_else(|_| json!({"text": payload_text.trim()}));
    Some((server_id.trim().to_string(), tool_name.trim().to_string(), payload))
}

fn parse_remember_command(input: &str) -> Option<&str> {
    let trimmed = input.trim();
    let summary = trimmed
        .strip_prefix("/remember ")
        .or_else(|| trimmed.strip_prefix("/记住 "))
        .or_else(|| trimmed.strip_prefix("记住："))
        .or_else(|| trimmed.strip_prefix("记住:"))?
        .trim();
    if summary.is_empty() { None } else { Some(summary) }
}
