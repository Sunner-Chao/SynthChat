use serde_json::{json, Value};

use crate::{
    error::AppResult,
    models::{AgentDefinition, SendChatRequest},
    store::AppStore,
};

use super::{
    append_parent_phase_event,
    delegation_artifacts::{
        append_delegation_memory_observation, append_diagnostic_artifact_to_error,
        save_subagent_failure_diagnostic_artifact,
    },
    delegation_request::DelegateTaskRequest,
    delegation_run_state::{latest_run_for_conversation, mark_run_as_subagent},
    delegation_scope::delegation_child_toolsets,
    run_chat_turn_with_toolset_policy_and_iteration_limit,
    shell_hooks::run_subagent_stop_hooks,
};

pub(super) async fn execute_synthchat_delegate_task_request(
    store: &AppStore,
    agent: &AgentDefinition,
    parent_run_id: &str,
    parent_depth: u32,
    child_index: u32,
    request: &DelegateTaskRequest,
    provider_id_override: &str,
    model_override: &str,
    subagent_auto_approve: bool,
    inherit_mcp_toolsets: bool,
) -> AppResult<Value> {
    let parent_run = store.agent_run(parent_run_id)?;
    let child_can_delegate = request.can_delegate && parent_depth + 1 < agent.max_subagent_depth;
    let child_context = if child_can_delegate {
        super::ToolExecutionContext::SubagentOrchestrator
    } else {
        super::ToolExecutionContext::SubagentLeaf
    };
    let child_conversation = store.create_conversation(
        Some(format!("Subagent {} · {}", child_index, request.role)),
        Some(parent_run.persona_id.clone()),
    )?;
    let child_prompt = format!(
        "You are a focused SynthChat subagent.\nRole: {}\nToolsets: {}\nMax iterations budget: {}\n\nTask:\n{}\n\nReturn a concise result for the parent agent. Do not ask the user follow-up questions. You have no memory of the parent conversation beyond this task text. Use tools when evidence is needed, and return verifiable handles for file writes, URLs, or external side effects.",
        request.role,
        if request.toolsets.is_empty() {
            "default leaf scope".into()
        } else {
            request.toolsets.join(", ")
        },
        request.max_iterations,
        request.task
    );
    let enabled_toolsets = delegation_child_toolsets(agent, request, inherit_mcp_toolsets);
    let result = Box::pin(run_chat_turn_with_toolset_policy_and_iteration_limit(
        store,
        SendChatRequest {
            conversation_id: Some(child_conversation.id.clone()),
            persona_id: Some(parent_run.persona_id.clone()),
            agent_id: None,
            content: child_prompt,
            provider_data: None,
            queue_item_id: None,
        },
        child_context,
        enabled_toolsets,
        None,
        Some(request.max_iterations),
        Some(provider_id_override.to_string()),
        Some(model_override.to_string()),
        None,
        None,
        Some(subagent_auto_approve),
        None,
        None,
        None,
        None,
    ))
    .await;

    let child_run = latest_run_for_conversation(store, &child_conversation.id);
    match result {
        Ok(messages) => {
            let child_run = child_run?;
            let reply_content = messages
                .iter()
                .rev()
                .find(|message| message.role == "assistant")
                .map(|message| message.content.clone())
                .unwrap_or_default();
            let saved = mark_run_as_subagent(
                store,
                child_run,
                parent_run_id,
                parent_depth,
                child_index,
                request,
                child_can_delegate,
            )?;
            if saved.state != "completed" {
                let error = saved
                    .error
                    .clone()
                    .filter(|value| !value.trim().is_empty())
                    .unwrap_or_else(|| reply_content.clone());
                let diagnostic_artifact_path = save_subagent_failure_diagnostic_artifact(
                    store,
                    parent_run_id,
                    &child_conversation.id,
                    Some(&saved),
                    request,
                    &error,
                    "synthchat",
                )?;
                let error = append_diagnostic_artifact_to_error(error, &diagnostic_artifact_path);
                append_parent_phase_event(
                    store,
                    parent_run_id,
                    "subagent_failed",
                    json!({
                        "childRunId": saved.run_id,
                        "childConversationId": child_conversation.id,
                        "role": request.role,
                        "task": request.task,
                        "toolsets": request.toolsets,
                        "maxIterations": request.max_iterations,
                        "diagnosticArtifactPath": diagnostic_artifact_path.as_ref().map(|path| path.to_string_lossy().to_string()),
                        "error": error
                    }),
                )?;
                run_subagent_stop_hooks(
                    store,
                    parent_run_id,
                    &saved,
                    request,
                    "failed",
                    &error,
                    "synthchat",
                    json!({
                        "diagnosticArtifactPath": diagnostic_artifact_path.as_ref().map(|path| path.to_string_lossy().to_string()),
                    }),
                )
                .await;
                return Ok(json!({
                    "status": "failed",
                    "childRunId": saved.run_id,
                    "childConversationId": child_conversation.id,
                    "role": request.role,
                    "task": request.task,
                    "maxIterations": request.max_iterations,
                    "diagnosticArtifactPath": diagnostic_artifact_path.as_ref().map(|path| path.to_string_lossy().to_string()),
                    "error": error
                }));
            }
            append_parent_phase_event(
                store,
                parent_run_id,
                "subagent_completed",
                json!({
                    "childRunId": saved.run_id,
                    "childConversationId": child_conversation.id,
                    "role": request.role,
                    "task": request.task,
                    "toolsets": request.toolsets,
                    "maxIterations": request.max_iterations,
                    "summary": reply_content
                }),
            )?;
            append_delegation_memory_observation(
                store,
                parent_run_id,
                &saved.run_id,
                &child_conversation.id,
                request,
                &reply_content,
                "synthchat",
            )?;
            run_subagent_stop_hooks(
                store,
                parent_run_id,
                &saved,
                request,
                "completed",
                &reply_content,
                "synthchat",
                json!({}),
            )
            .await;
            Ok(json!({
                "status": "completed",
                "childRunId": saved.run_id,
                "childConversationId": child_conversation.id,
                "role": request.role,
                "task": request.task,
                "maxIterations": request.max_iterations,
                "result": reply_content
            }))
        }
        Err(error) => {
            let error_text = error.to_string();
            let child_run = child_run.ok();
            let saved = match child_run {
                Some(run) => Some(mark_run_as_subagent(
                    store,
                    run,
                    parent_run_id,
                    parent_depth,
                    child_index,
                    request,
                    child_can_delegate,
                )?),
                None => None,
            };
            let diagnostic_artifact_path = save_subagent_failure_diagnostic_artifact(
                store,
                parent_run_id,
                &child_conversation.id,
                saved.as_ref(),
                request,
                &error_text,
                "synthchat",
            )?;
            let error_text =
                append_diagnostic_artifact_to_error(error_text, &diagnostic_artifact_path);
            append_parent_phase_event(
                store,
                parent_run_id,
                "subagent_failed",
                json!({
                    "childRunId": saved.as_ref().map(|run| run.run_id.clone()),
                    "childConversationId": child_conversation.id,
                    "role": request.role,
                    "task": request.task,
                    "maxIterations": request.max_iterations,
                    "diagnosticArtifactPath": diagnostic_artifact_path.as_ref().map(|path| path.to_string_lossy().to_string()),
                    "error": error_text.clone()
                }),
            )?;
            if let Some(saved) = saved.as_ref() {
                run_subagent_stop_hooks(
                    store,
                    parent_run_id,
                    saved,
                    request,
                    "failed",
                    &error_text,
                    "synthchat",
                    json!({
                        "diagnosticArtifactPath": diagnostic_artifact_path.as_ref().map(|path| path.to_string_lossy().to_string()),
                    }),
                )
                .await;
            }
            Ok(json!({
                "status": "failed",
                "childRunId": saved.as_ref().map(|run| run.run_id.clone()),
                "childConversationId": child_conversation.id,
                "role": request.role,
                "task": request.task,
                "maxIterations": request.max_iterations,
                "diagnosticArtifactPath": diagnostic_artifact_path.as_ref().map(|path| path.to_string_lossy().to_string()),
                "error": error_text
            }))
        }
    }
}
