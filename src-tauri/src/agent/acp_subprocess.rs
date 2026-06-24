use std::{
    collections::VecDeque,
    path::Path,
    process::Stdio,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use serde_json::{json, Value};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{ChildStdin, ChildStdout, Command},
};

use crate::{
    error::{AppError, AppResult},
    process_utils::CommandWindowExt,
    store::AppStore,
};

use super::{
    acp_child_events::{
        acp_session_update_kind, acp_session_update_record, acp_session_update_text,
        append_acp_tool_event_record,
    },
    acp_client::{acp_prompt_result_error, acp_session_cancel_request, acp_session_start_request},
    acp_client_fs::{acp_read_text_file_response, acp_write_text_file_response},
    acp_permissions::{
        acp_permission_decision_with_context, acp_permission_response_with_context,
        AcpPermissionApprovalContext,
    },
    acp_session::acp_session_runtime_config_for_store,
    append_parent_phase_event,
};

pub(super) struct AcpPromptResult {
    pub(super) text: String,
    pub(super) reasoning: String,
    pub(super) session_updates: Vec<Value>,
    pub(super) permission_decisions: Vec<Value>,
}

pub(super) struct AcpRunObserver<'a> {
    pub(super) store: &'a AppStore,
    pub(super) parent_run_id: &'a str,
    pub(super) child_run_id: &'a str,
    pub(super) child_conversation_id: &'a str,
}

pub(super) fn acp_permission_approval_context(
    observer: Option<&AcpRunObserver<'_>>,
    message: &Value,
    cwd: &Path,
    subagent_auto_approve: bool,
) -> AcpPermissionApprovalContext {
    let edit_policy = observer
        .and_then(|observer| {
            let session_id = message
                .get("params")
                .and_then(|params| params.get("sessionId").or_else(|| params.get("session_id")))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())?;
            acp_session_runtime_config_for_store(observer.store, session_id)
                .ok()
                .flatten()
                .and_then(|runtime| runtime.mode)
        })
        .filter(|mode| !mode.trim().is_empty());
    AcpPermissionApprovalContext {
        auto_approve: subagent_auto_approve,
        edit_policy,
        cwd: Some(cwd.to_path_buf()),
    }
}

pub(super) fn acp_permission_decision_for_observer(
    observer: Option<&AcpRunObserver<'_>>,
    message: &Value,
    cwd: &Path,
    subagent_auto_approve: bool,
) -> Value {
    let approval_context =
        acp_permission_approval_context(observer, message, cwd, subagent_auto_approve);
    acp_permission_decision_with_context(message, &approval_context)
}

pub(super) fn acp_permission_response_for_observer(
    observer: Option<&AcpRunObserver<'_>>,
    message: &Value,
    cwd: &Path,
    subagent_auto_approve: bool,
) -> Value {
    let approval_context =
        acp_permission_approval_context(observer, message, cwd, subagent_auto_approve);
    acp_permission_response_with_context(message, &approval_context)
}

pub(super) fn append_acp_observer_tool_event(
    observer: &AcpRunObserver<'_>,
    record: &Value,
) -> AppResult<()> {
    append_acp_tool_event_record(observer.store, observer.child_run_id, record)
}

pub(super) fn append_acp_observer_phase(
    observer: &AcpRunObserver<'_>,
    phase: &str,
    detail: Value,
) -> AppResult<()> {
    let mut payload = json!({
        "childRunId": observer.child_run_id,
        "childConversationId": observer.child_conversation_id
    });
    if let (Some(target), Some(source)) = (payload.as_object_mut(), detail.as_object()) {
        for (key, value) in source {
            target.insert(key.clone(), value.clone());
        }
    }
    append_parent_phase_event(observer.store, observer.parent_run_id, phase, payload)
}

pub(super) fn acp_observer_aborted(observer: Option<&AcpRunObserver<'_>>) -> AppResult<bool> {
    let Some(observer) = observer else {
        return Ok(false);
    };
    for run_id in [observer.parent_run_id, observer.child_run_id] {
        match observer.store.agent_run(run_id) {
            Ok(run) if run.state == "aborted" => return Ok(true),
            Ok(_) => {}
            Err(AppError::NotFound(_)) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(false)
}

pub(super) async fn acp_handle_server_message(
    stdin: &mut ChildStdin,
    message: &Value,
    cwd: &Path,
    text_parts: Option<&mut Vec<String>>,
    reasoning_parts: Option<&mut Vec<String>>,
    session_updates: Option<&mut Vec<Value>>,
    permission_decisions: Option<&mut Vec<Value>>,
    observer: Option<&AcpRunObserver<'_>>,
    subagent_auto_approve: bool,
) -> AppResult<bool> {
    let Some(method) = message.get("method").and_then(Value::as_str) else {
        return Ok(false);
    };
    if method == "session/update" {
        if let Some(updates) = session_updates {
            if let Some(record) = acp_session_update_record(message) {
                if let Some(observer) = observer {
                    append_acp_observer_phase(
                        observer,
                        "acp_session_update",
                        json!({
                            "update": record.clone()
                        }),
                    )?;
                    append_acp_observer_tool_event(observer, &record)?;
                }
                updates.push(record);
            }
        }
        let update = message
            .get("params")
            .and_then(|params| params.get("update"))
            .unwrap_or(&Value::Null);
        let kind = acp_session_update_kind(update);
        let text = acp_session_update_text(update);
        if kind == "agent_message_chunk" {
            if let Some(parts) = text_parts {
                parts.push(text);
            }
        } else if kind == "agent_thought_chunk" {
            if let Some(parts) = reasoning_parts {
                parts.push(text);
            }
        }
        return Ok(true);
    }
    let response = match method {
        "session/request_permission" => {
            let decision =
                acp_permission_decision_for_observer(observer, message, cwd, subagent_auto_approve);
            if let Some(observer) = observer {
                append_acp_observer_phase(
                    observer,
                    "acp_permission_decision",
                    json!({
                        "decision": decision.clone()
                    }),
                )?;
            }
            if let Some(decisions) = permission_decisions {
                decisions.push(decision);
            }
            acp_permission_response_for_observer(observer, message, cwd, subagent_auto_approve)
        }
        "fs/read_text_file" => acp_read_text_file_response(message, cwd).await,
        "fs/write_text_file" => acp_write_text_file_response(message, cwd).await,
        _ => json!({
            "jsonrpc": "2.0",
            "id": message.get("id").cloned().unwrap_or(Value::Null),
            "error": {"code": -32601, "message": format!("ACP client method '{method}' is not supported by SynthChat yet.")}
        }),
    };
    stdin.write_all(response.to_string().as_bytes()).await?;
    stdin.write_all(b"\n").await?;
    stdin.flush().await?;
    Ok(true)
}

pub(super) async fn acp_rpc_request(
    stdin: &mut ChildStdin,
    lines: &mut tokio::io::Lines<BufReader<ChildStdout>>,
    id: u64,
    method: &str,
    params: Value,
    cwd: &Path,
    mut text_parts: Option<&mut Vec<String>>,
    mut reasoning_parts: Option<&mut Vec<String>>,
    mut session_updates: Option<&mut Vec<Value>>,
    mut permission_decisions: Option<&mut Vec<Value>>,
    observer: Option<&AcpRunObserver<'_>>,
    subagent_auto_approve: bool,
    stderr_tail: &Arc<Mutex<VecDeque<String>>>,
    timeout_duration: Duration,
) -> AppResult<Value> {
    let request = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
    stdin.write_all(request.to_string().as_bytes()).await?;
    stdin.write_all(b"\n").await?;
    stdin.flush().await?;
    let started_at = Instant::now();
    loop {
        if acp_observer_aborted(observer)? {
            return Err(AppError::BadRequest(format!(
                "ACP {} aborted because the agent run was stopped",
                method
            )));
        }
        let elapsed = started_at.elapsed();
        if elapsed >= timeout_duration {
            return Err(acp_timeout_error(method, stderr_tail));
        }
        let remaining = timeout_duration.saturating_sub(elapsed);
        let poll_window = if remaining > Duration::from_millis(500) {
            Duration::from_millis(500)
        } else {
            remaining
        };
        let line = match tokio::time::timeout(poll_window, lines.next_line()).await {
            Ok(result) => result
                .map_err(AppError::from)?
                .ok_or_else(|| acp_closed_error(method, stderr_tail))?,
            Err(_) => continue,
        };
        let message: Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            Err(_) => continue,
        };
        if acp_handle_server_message(
            stdin,
            &message,
            cwd,
            text_parts.as_deref_mut(),
            reasoning_parts.as_deref_mut(),
            session_updates.as_deref_mut(),
            permission_decisions.as_deref_mut(),
            observer,
            subagent_auto_approve,
        )
        .await?
        {
            continue;
        }
        if message.get("id").and_then(Value::as_u64) != Some(id) {
            continue;
        }
        if let Some(error) = message.get("error") {
            return Err(AppError::BadRequest(format!(
                "ACP {} failed: {}",
                method,
                error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or_else(|| error.as_str().unwrap_or("unknown error"))
            )));
        }
        return Ok(message.get("result").cloned().unwrap_or(Value::Null));
    }
}

pub(super) async fn run_acp_prompt(
    command: &str,
    args: &[String],
    cwd: &Path,
    prompt: &str,
    requested_session_id: &str,
    requested_session_mode: &str,
    observer: Option<AcpRunObserver<'_>>,
    mcp_servers: Vec<Value>,
    subagent_auto_approve: bool,
    timeout_duration: Duration,
) -> AppResult<AcpPromptResult> {
    let args = if args.is_empty() {
        vec!["--acp".to_string(), "--stdio".to_string()]
    } else {
        args.to_vec()
    };
    let mut child = Command::new(command)
        .hide_window()
        .args(&args)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            AppError::BadRequest(format!(
                "Could not start ACP command '{}': {}",
                command, error
            ))
        })?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| AppError::BadRequest("ACP process did not expose stdin".into()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| AppError::BadRequest("ACP process did not expose stdout".into()))?;
    let stderr = child.stderr.take();
    let stderr_tail: Arc<Mutex<VecDeque<String>>> = Arc::new(Mutex::new(VecDeque::new()));
    if let Some(stderr) = stderr {
        let tail = Arc::clone(&stderr_tail);
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if let Ok(mut guard) = tail.lock() {
                    guard.push_back(line);
                    while guard.len() > 40 {
                        guard.pop_front();
                    }
                }
            }
        });
    }

    let mut lines = BufReader::new(stdout).lines();
    let mut next_id = 1_u64;
    let mut active_session_id = String::new();
    let result = async {
        acp_rpc_request(
            &mut stdin,
            &mut lines,
            next_id,
            "initialize",
            json!({
                "protocolVersion": 1,
                "clientCapabilities": {
                    "fs": {
                        "readTextFile": true,
                        "writeTextFile": true
                    }
                },
                "clientInfo": {
                    "name": "synthchat",
                    "title": "SynthChat",
                    "version": "1.0.0"
                }
            }),
            cwd,
            None,
            None,
            None,
            None,
            observer.as_ref(),
            subagent_auto_approve,
            &stderr_tail,
            timeout_duration,
        )
        .await?;
        next_id += 1;
        let (session_method, session_params) = acp_session_start_request(
            cwd,
            requested_session_id,
            requested_session_mode,
            mcp_servers,
        );
        let session = acp_rpc_request(
            &mut stdin,
            &mut lines,
            next_id,
            session_method.as_str(),
            session_params,
            cwd,
            None,
            None,
            None,
            None,
            observer.as_ref(),
            subagent_auto_approve,
            &stderr_tail,
            timeout_duration,
        )
        .await?;
        next_id += 1;
        let session_id = session
            .get("sessionId")
            .or_else(|| session.get("session_id"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                AppError::BadRequest(format!("ACP {session_method} did not return sessionId"))
            })?;
        active_session_id = session_id.to_string();
        let mut text_parts = Vec::<String>::new();
        let mut reasoning_parts = Vec::<String>::new();
        let mut session_updates = Vec::<Value>::new();
        let mut permission_decisions = Vec::<Value>::new();
        let prompt_request_id = next_id;
        next_id += 1;
        let prompt_result = acp_rpc_request(
            &mut stdin,
            &mut lines,
            prompt_request_id,
            "session/prompt",
            json!({
                "sessionId": session_id,
                "prompt": [{"type": "text", "text": prompt}]
            }),
            cwd,
            Some(&mut text_parts),
            Some(&mut reasoning_parts),
            Some(&mut session_updates),
            Some(&mut permission_decisions),
            observer.as_ref(),
            subagent_auto_approve,
            &stderr_tail,
            timeout_duration,
        )
        .await?;
        if let Some(error) = acp_prompt_result_error(&prompt_result) {
            return Err(error);
        }
        Ok(AcpPromptResult {
            text: text_parts.join(""),
            reasoning: reasoning_parts.join(""),
            session_updates,
            permission_decisions,
        })
    }
    .await;
    if result.is_err() && !active_session_id.trim().is_empty() {
        let aborted = acp_observer_aborted(observer.as_ref()).unwrap_or(false);
        let reason = if aborted {
            "parent_or_child_run_aborted"
        } else {
            "prompt_error_or_timeout"
        };
        let cancel_id = next_id;
        let _ = acp_rpc_request(
            &mut stdin,
            &mut lines,
            cancel_id,
            "session/cancel",
            acp_session_cancel_request(&active_session_id),
            cwd,
            None,
            None,
            None,
            None,
            None,
            subagent_auto_approve,
            &stderr_tail,
            Duration::from_secs(2),
        )
        .await;
        if let Some(observer) = observer.as_ref() {
            let _ = append_acp_observer_phase(
                observer,
                "acp_session_cancel",
                json!({
                    "sessionId": active_session_id,
                    "reason": reason
                }),
            );
        }
    }
    let _ = stdin.shutdown().await;
    let _ = child.kill().await;
    result
}

pub(super) fn acp_delegate_error_implies_aborted(error_text: &str) -> bool {
    let lower_error = error_text.to_ascii_lowercase();
    lower_error.contains("agent run was stopped")
        || lower_error.contains("cancelled")
        || lower_error.contains("canceled")
}

pub(super) fn acp_delegate_was_aborted(
    store: &AppStore,
    parent_run_id: &str,
    child_run_id: &str,
) -> bool {
    [parent_run_id, child_run_id].iter().any(|run_id| {
        store
            .agent_run(run_id)
            .map(|run| run.state == "aborted")
            .unwrap_or(false)
    })
}

pub(super) fn acp_timeout_error(
    method: &str,
    stderr_tail: &Arc<Mutex<VecDeque<String>>>,
) -> AppError {
    let stderr = acp_stderr_tail(stderr_tail);
    AppError::BadRequest(format!(
        "Timed out waiting for ACP response to {}{}",
        method,
        if stderr.is_empty() {
            String::new()
        } else {
            format!("; stderr: {stderr}")
        }
    ))
}

pub(super) fn acp_closed_error(
    method: &str,
    stderr_tail: &Arc<Mutex<VecDeque<String>>>,
) -> AppError {
    let stderr = acp_stderr_tail(stderr_tail);
    AppError::BadRequest(format!(
        "ACP process closed stdout before response to {}{}",
        method,
        if stderr.is_empty() {
            String::new()
        } else {
            format!("; stderr: {stderr}")
        }
    ))
}

fn acp_stderr_tail(stderr_tail: &Arc<Mutex<VecDeque<String>>>) -> String {
    stderr_tail
        .lock()
        .map(|tail| tail.iter().cloned().collect::<Vec<_>>().join("\n"))
        .unwrap_or_default()
}
