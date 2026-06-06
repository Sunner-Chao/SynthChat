use serde_json::Value;

use crate::error::{AppError, AppResult};

use super::{normalize_toolset_name, payload_string_array};
pub(super) struct DelegateTaskRequest {
    pub(super) task: String,
    pub(super) role: String,
    pub(super) toolsets: Vec<String>,
    pub(super) can_delegate: bool,
    pub(super) max_iterations: u32,
    pub(super) acp_command: String,
    pub(super) acp_args: Vec<String>,
    pub(super) acp_session_id: String,
    pub(super) acp_session_mode: String,
}

pub(super) fn delegate_task_requests(payload: &Value) -> AppResult<Vec<DelegateTaskRequest>> {
    let top_role = payload
        .get("role")
        .and_then(Value::as_str)
        .map(normalize_delegate_role)
        .unwrap_or_else(|| "subagent".into());
    let top_can_delegate_explicit = payload
        .get("canDelegate")
        .or_else(|| payload.get("can_delegate"))
        .and_then(Value::as_bool);
    let top_can_delegate = delegate_role_can_delegate(&top_role, top_can_delegate_explicit);
    let top_toolsets = delegate_task_toolsets(
        payload_string_array(payload, "toolsets", "toolsets"),
        &top_role,
        top_can_delegate,
    );
    let top_max_iterations = delegate_task_max_iterations(payload, 50);
    let top_acp_command = delegate_task_acp_command(payload);
    let top_acp_args = payload_string_array(payload, "acpArgs", "acp_args");
    let top_acp_session_id = delegate_task_acp_session_id(payload);
    let top_acp_session_mode = delegate_task_acp_session_mode(payload);
    if let Some(tasks) = payload.get("tasks") {
        let tasks = tasks.as_array().ok_or_else(|| {
            AppError::BadRequest("delegate_task payload.tasks must be an array".into())
        })?;
        if tasks.is_empty() {
            return Err(AppError::BadRequest(
                "delegate_task payload.tasks must not be empty".into(),
            ));
        }
        return tasks
            .iter()
            .enumerate()
            .map(|(index, item)| {
                delegate_task_request_from_value(
                    item,
                    index,
                    &top_role,
                    &top_toolsets,
                    top_can_delegate,
                    top_max_iterations,
                    &top_acp_command,
                    &top_acp_args,
                    &top_acp_session_id,
                    &top_acp_session_mode,
                )
            })
            .collect();
    }
    let task = delegate_task_text(payload, 0)?;
    Ok(vec![DelegateTaskRequest {
        task,
        role: top_role,
        toolsets: top_toolsets,
        can_delegate: top_can_delegate,
        max_iterations: top_max_iterations,
        acp_command: top_acp_command,
        acp_args: top_acp_args,
        acp_session_id: top_acp_session_id,
        acp_session_mode: top_acp_session_mode,
    }])
}

fn delegate_task_request_from_value(
    item: &Value,
    index: usize,
    top_role: &str,
    top_toolsets: &[String],
    top_can_delegate: bool,
    top_max_iterations: u32,
    top_acp_command: &str,
    top_acp_args: &[String],
    top_acp_session_id: &str,
    top_acp_session_mode: &str,
) -> AppResult<DelegateTaskRequest> {
    let object = item.as_object().ok_or_else(|| {
        AppError::BadRequest(format!("delegate_task tasks[{index}] must be an object"))
    })?;
    let task = delegate_task_text(item, index)?;
    let role = object
        .get("role")
        .and_then(Value::as_str)
        .map(normalize_delegate_role)
        .unwrap_or_else(|| top_role.to_string());
    let explicit_can_delegate = object
        .get("canDelegate")
        .or_else(|| object.get("can_delegate"))
        .and_then(Value::as_bool);
    let can_delegate =
        explicit_can_delegate.unwrap_or_else(|| role == "orchestrator" || top_can_delegate);
    let toolsets = payload_string_array(item, "toolsets", "toolsets");
    let toolsets = if toolsets.is_empty() {
        top_toolsets.to_vec()
    } else {
        toolsets
    };
    let max_iterations = delegate_task_max_iterations(item, top_max_iterations);
    let acp_command = delegate_task_acp_command(item);
    let acp_args = payload_string_array(item, "acpArgs", "acp_args");
    let acp_session_id = delegate_task_acp_session_id(item);
    let acp_session_mode = delegate_task_acp_session_mode(item);
    Ok(DelegateTaskRequest {
        task,
        toolsets: delegate_task_toolsets(toolsets, &role, can_delegate),
        role,
        can_delegate,
        max_iterations,
        acp_command: if acp_command.is_empty() {
            top_acp_command.to_string()
        } else {
            acp_command
        },
        acp_args: if acp_args.is_empty() {
            top_acp_args.to_vec()
        } else {
            acp_args
        },
        acp_session_id: if acp_session_id.is_empty() {
            top_acp_session_id.to_string()
        } else {
            acp_session_id
        },
        acp_session_mode: if acp_session_mode.is_empty() {
            top_acp_session_mode.to_string()
        } else {
            acp_session_mode
        },
    })
}

fn delegate_task_acp_command(payload: &Value) -> String {
    payload
        .get("acpCommand")
        .or_else(|| payload.get("acp_command"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("")
        .to_string()
}

fn delegate_task_acp_session_id(payload: &Value) -> String {
    payload
        .get("acpSessionId")
        .or_else(|| payload.get("acp_session_id"))
        .or_else(|| payload.get("sessionId"))
        .or_else(|| payload.get("session_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("")
        .to_string()
}

fn delegate_task_acp_session_mode(payload: &Value) -> String {
    payload
        .get("acpSessionMode")
        .or_else(|| payload.get("acp_session_mode"))
        .or_else(|| payload.get("sessionMode"))
        .or_else(|| payload.get("session_mode"))
        .and_then(Value::as_str)
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| matches!(value.as_str(), "new" | "load" | "resume"))
        .unwrap_or_default()
}

fn delegate_task_max_iterations(payload: &Value, default_value: u32) -> u32 {
    payload
        .get("maxIterations")
        .or_else(|| payload.get("max_iterations"))
        .and_then(Value::as_u64)
        .map(|value| value as u32)
        .unwrap_or(default_value)
        .max(1)
        .min(90)
}

fn normalize_delegate_role(value: &str) -> String {
    match value.trim().to_ascii_lowercase().replace('-', "_").as_str() {
        "leaf" => "subagent".into(),
        "subagent" | "researcher" | "planner" | "coder" | "orchestrator" => {
            value.trim().to_ascii_lowercase().replace('-', "_")
        }
        _ => "subagent".into(),
    }
}

fn delegate_role_can_delegate(role: &str, explicit: Option<bool>) -> bool {
    explicit.unwrap_or_else(|| role == "orchestrator")
}

fn delegate_task_toolsets(
    mut toolsets: Vec<String>,
    role: &str,
    can_delegate: bool,
) -> Vec<String> {
    if role == "orchestrator"
        && can_delegate
        && !toolsets
            .iter()
            .any(|toolset| normalize_toolset_name(toolset) == "delegation")
    {
        toolsets.push("delegation".into());
    }
    toolsets
}

pub(super) fn apply_delegation_runtime_config(
    requests: &mut [DelegateTaskRequest],
    orchestrator_enabled: bool,
) {
    if orchestrator_enabled {
        return;
    }
    for request in requests {
        if request.role == "orchestrator" {
            request.role = "subagent".into();
        }
        request.can_delegate = false;
        request
            .toolsets
            .retain(|toolset| normalize_toolset_name(toolset) != "delegation");
    }
}

fn delegate_task_text(payload: &Value, index: usize) -> AppResult<String> {
    let task = payload
        .get("task")
        .or_else(|| payload.get("goal"))
        .or_else(|| payload.get("content"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            AppError::BadRequest(format!(
                "delegate_task requires task/goal/content for item {index}"
            ))
        })?;
    let context = payload
        .get("context")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    Ok(match context {
        Some(context) => format!("Goal:\n{task}\n\nContext:\n{context}"),
        None => task.to_string(),
    })
}
