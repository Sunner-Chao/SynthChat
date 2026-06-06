use std::path::Path;

use serde_json::{json, Value};

use crate::error::AppError;

use super::acp_tool_output::acp_string_text;

pub(super) fn acp_session_start_request(
    cwd: &Path,
    requested_session_id: &str,
    requested_session_mode: &str,
    mcp_servers: Vec<Value>,
) -> (String, Value) {
    let session_id = requested_session_id.trim();
    let mode = requested_session_mode.trim().to_ascii_lowercase();
    if session_id.is_empty() || mode == "new" {
        return (
            "session/new".into(),
            json!({
                "cwd": cwd.to_string_lossy(),
                "mcpServers": mcp_servers
            }),
        );
    }
    let method = if mode == "load" {
        "session/load"
    } else {
        "session/resume"
    };
    (
        method.into(),
        json!({
            "cwd": cwd.to_string_lossy(),
            "sessionId": session_id,
            "mcpServers": mcp_servers
        }),
    )
}

pub(super) fn acp_session_cancel_request(session_id: &str) -> Value {
    json!({
        "sessionId": session_id.trim()
    })
}

pub(super) fn acp_prompt_result_stop_reason(result: &Value) -> String {
    acp_string_text(result, &["stopReason", "stop_reason"]).to_ascii_lowercase()
}

pub(super) fn acp_prompt_result_is_cancelled(result: &Value) -> bool {
    matches!(
        acp_prompt_result_stop_reason(result).as_str(),
        "cancelled" | "canceled"
    )
}

pub(super) fn acp_prompt_result_error(result: &Value) -> Option<AppError> {
    let stop_reason = acp_prompt_result_stop_reason(result);
    if stop_reason.is_empty() || stop_reason == "end_turn" {
        return None;
    }
    let message = if acp_prompt_result_is_cancelled(result) {
        "ACP session/prompt returned stopReason=cancelled".into()
    } else {
        format!("ACP session/prompt returned stopReason={stop_reason}")
    };
    Some(AppError::BadRequest(message))
}
