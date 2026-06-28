use std::time::Duration;

use serde_json::{json, Value};

use crate::{
    error::AppResult,
    models::{now_iso, AgentDefinition, AgentRunRecord},
    store::AppStore,
};

use super::{
    acp_subprocess::{
        acp_delegate_error_implies_aborted, acp_delegate_was_aborted, run_acp_prompt,
        AcpRunObserver,
    },
    append_parent_phase_event,
    delegation_artifacts::{
        append_delegation_memory_observation, append_diagnostic_artifact_to_error,
        save_subagent_failure_diagnostic_artifact,
    },
    delegation_request::DelegateTaskRequest,
    delegation_run_state::mark_run_as_subagent,
    delegation_scope::acp_mcp_servers_for_agent,
    shell_hooks::run_subagent_stop_hooks,
    workspace::workspace_root,
};

pub(super) async fn execute_acp_delegate_task_request(
    store: &AppStore,
    agent: &AgentDefinition,
    parent_run_id: &str,
    parent_depth: u32,
    child_index: u32,
    request: &DelegateTaskRequest,
    subagent_auto_approve: bool,
    inherit_mcp_toolsets: bool,
) -> AppResult<Value> {
    let parent_run = store.agent_run(parent_run_id)?;
    let child_conversation = store.create_internal_subagent_conversation(
        Some(format!("ACP subagent {} · {}", child_index, request.role)),
        Some(parent_run.persona_id.clone()),
        parent_run_id,
        child_index,
        "acp",
    )?;
    let mut child_run = AgentRunRecord::new(
        child_conversation.id.clone(),
        parent_run.persona_id.clone(),
        parent_run.agent_id.clone(),
    );
    child_run.state = "running".into();
    child_run.touch_activity(format!("ACP subagent starting: {}", request.acp_command));
    let child_run = mark_run_as_subagent(
        store,
        child_run,
        parent_run_id,
        parent_depth,
        child_index,
        request,
        request.can_delegate,
    )?;

    let cwd = workspace_root(agent)?;
    let mcp_servers = acp_mcp_servers_for_agent(store, agent, request, inherit_mcp_toolsets)?;
    append_parent_phase_event(
        store,
        parent_run_id,
        "subagent_started",
        json!({
            "childRunId": child_run.run_id,
            "childConversationId": child_conversation.id,
            "role": request.role,
            "task": request.task,
            "toolsets": request.toolsets,
            "maxIterations": request.max_iterations,
            "transport": "acp",
            "acpCommand": request.acp_command,
            "acpArgs": request.acp_args,
            "acpSessionId": request.acp_session_id,
            "acpSessionMode": request.acp_session_mode,
            "mcpServerCount": mcp_servers.len(),
            "cwd": cwd.to_string_lossy()
        }),
    )?;

    let prompt = format!(
        "You are a focused ACP subagent.\nRole: {}\nToolsets requested by parent: {}\nMax iterations budget: {}\n\nTask:\n{}\n\nReturn a concise result for the parent agent. Do not ask the user follow-up questions. Include file paths, commands, URLs, or other verifiable handles for any side effects.",
        request.role,
        if request.toolsets.is_empty() {
            "external ACP default scope".into()
        } else {
            request.toolsets.join(", ")
        },
        request.max_iterations,
        request.task
    );
    let acp_result = run_acp_prompt(
        &request.acp_command,
        &request.acp_args,
        &cwd,
        &prompt,
        request.acp_session_id.as_str(),
        request.acp_session_mode.as_str(),
        Some(AcpRunObserver {
            store,
            parent_run_id,
            child_run_id: &child_run.run_id,
            child_conversation_id: &child_conversation.id,
        }),
        mcp_servers,
        subagent_auto_approve,
        Duration::from_secs(
            (request.max_iterations as u64)
                .saturating_mul(60)
                .clamp(60, 1800),
        ),
    )
    .await;

    match acp_result {
        Ok(result) => {
            let mut saved = store.agent_run(&child_run.run_id)?;
            saved.state = "completed".into();
            saved.completed_at = Some(now_iso());
            saved.touch_activity("ACP subagent completed");
            saved = store.save_agent_run(saved)?;
            let output = if result.reasoning.trim().is_empty() {
                result.text
            } else {
                format!(
                    "{}\n\nReasoning summary:\n{}",
                    result.text, result.reasoning
                )
            };
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
                    "transport": "acp",
                    "acpSessionUpdates": result.session_updates.clone(),
                    "permissionDecisions": result.permission_decisions.clone(),
                    "summary": output
                }),
            )?;
            append_delegation_memory_observation(
                store,
                parent_run_id,
                &saved.run_id,
                &child_conversation.id,
                request,
                &output,
                "acp",
            )?;
            run_subagent_stop_hooks(
                store,
                parent_run_id,
                &saved,
                request,
                "completed",
                &output,
                "acp",
                json!({
                    "acpSessionUpdates": result.session_updates.clone(),
                    "permissionDecisions": result.permission_decisions.clone(),
                }),
            )
            .await;
            Ok(json!({
                "status": "completed",
                "childRunId": saved.run_id,
                "childConversationId": child_conversation.id,
                "role": request.role,
                "task": request.task,
                "maxIterations": request.max_iterations,
                "transport": "acp",
                "acpSessionUpdates": result.session_updates,
                "permissionDecisions": result.permission_decisions,
                "result": output
            }))
        }
        Err(error) => {
            let error_text = error.to_string();
            let aborted = acp_delegate_was_aborted(store, parent_run_id, &child_run.run_id)
                || acp_delegate_error_implies_aborted(&error_text);
            let mut saved = store.agent_run(&child_run.run_id)?;
            saved.state = if aborted {
                "aborted".into()
            } else {
                "failed".into()
            };
            saved.error = Some(error_text.clone());
            saved.completed_at = Some(now_iso());
            saved.touch_activity(if aborted {
                "ACP subagent aborted"
            } else {
                "ACP subagent failed"
            });
            saved = store.save_agent_run(saved)?;
            let diagnostic_artifact_path = save_subagent_failure_diagnostic_artifact(
                store,
                parent_run_id,
                &child_conversation.id,
                Some(&saved),
                request,
                &error_text,
                "acp",
            )?;
            let error_text =
                append_diagnostic_artifact_to_error(error_text, &diagnostic_artifact_path);
            append_parent_phase_event(
                store,
                parent_run_id,
                if aborted {
                    "subagent_aborted"
                } else {
                    "subagent_failed"
                },
                json!({
                    "childRunId": saved.run_id,
                    "childConversationId": child_conversation.id,
                    "role": request.role,
                    "task": request.task,
                    "toolsets": request.toolsets,
                    "maxIterations": request.max_iterations,
                    "transport": "acp",
                    "acpSessionUpdates": [],
                    "permissionDecisions": [],
                    "diagnosticArtifactPath": diagnostic_artifact_path.as_ref().map(|path| path.to_string_lossy().to_string()),
                    "error": error_text.clone()
                }),
            )?;
            run_subagent_stop_hooks(
                store,
                parent_run_id,
                &saved,
                request,
                if aborted { "aborted" } else { "failed" },
                &error_text,
                "acp",
                json!({
                    "diagnosticArtifactPath": diagnostic_artifact_path.as_ref().map(|path| path.to_string_lossy().to_string()),
                    "acpSessionUpdates": [],
                    "permissionDecisions": [],
                }),
            )
            .await;
            Ok(json!({
                "status": if aborted { "aborted" } else { "failed" },
                "childRunId": saved.run_id,
                "childConversationId": child_conversation.id,
                "role": request.role,
                "task": request.task,
                "maxIterations": request.max_iterations,
                "transport": "acp",
                "acpSessionUpdates": [],
                "permissionDecisions": [],
                "diagnosticArtifactPath": diagnostic_artifact_path.as_ref().map(|path| path.to_string_lossy().to_string()),
                "error": error_text
            }))
        }
    }
}
