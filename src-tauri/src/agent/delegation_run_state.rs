use crate::{
    error::{AppError, AppResult},
    models::AgentRunRecord,
    store::AppStore,
};

use super::delegation_request::DelegateTaskRequest;

pub(super) fn latest_run_for_conversation(
    store: &AppStore,
    conversation_id: &str,
) -> AppResult<AgentRunRecord> {
    store
        .agent_runs()?
        .into_iter()
        .filter(|run| run.conversation_id == conversation_id)
        .max_by(|left, right| {
            left.updated_at
                .cmp(&right.updated_at)
                .then_with(|| left.run_id.cmp(&right.run_id))
        })
        .ok_or_else(|| {
            AppError::NotFound(format!("subagent run for conversation {conversation_id}"))
        })
}

pub(super) fn mark_run_as_subagent(
    store: &AppStore,
    mut run: AgentRunRecord,
    parent_run_id: &str,
    parent_depth: u32,
    child_index: u32,
    request: &DelegateTaskRequest,
    child_can_delegate: bool,
) -> AppResult<AgentRunRecord> {
    run.parent_run_id = Some(parent_run_id.to_string());
    run.subagent_index = Some(child_index);
    run.subagent_depth = Some(parent_depth + 1);
    run.subagent_can_delegate = Some(child_can_delegate);
    run.subagent_role = Some(request.role.clone());
    run.subagent_task = Some(request.task.clone());
    run.subagent_toolsets = request.toolsets.clone();
    run.subagent_max_iterations = Some(request.max_iterations);
    run.user_request = request.task.clone();
    store.save_agent_run(run)
}
