use std::{
    collections::HashMap,
    process::Stdio,
    sync::{Mutex, OnceLock},
    time::Duration,
};

use serde_json::{json, Value};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{ChildStdin, Command},
    time::timeout,
};

use crate::{
    error::{AppError, AppResult},
    models::now_iso,
    process_utils::CommandWindowExt,
    store::AppStore,
};

use super::{decode_base64_image, required_string_arg, string_arg, truncate_output};
static COMPUTER_USE_ELEMENTS: OnceLock<Mutex<HashMap<String, Vec<Value>>>> = OnceLock::new();
static CUA_MCP_LIFECYCLE: OnceLock<Mutex<CuaMcpLifecycle>> = OnceLock::new();
#[cfg(target_os = "macos")]
static CUA_MCP_SESSION: OnceLock<tokio::sync::Mutex<Option<CuaMcpPersistentSession>>> =
    OnceLock::new();
const COMPUTER_USE_DEFAULT_MAX_ELEMENTS: u64 = 100;
const COMPUTER_USE_MAX_ALLOWED_ELEMENTS: u64 = 1000;

#[derive(Debug, Clone, Default)]
struct CuaMcpLifecycle {
    one_shot_session_starts: u64,
    persistent_session_starts: u64,
    tool_calls: u64,
    probe_calls: u64,
    successes: u64,
    errors: u64,
    last_started_at: Option<String>,
    last_finished_at: Option<String>,
    last_tool: Option<String>,
    last_error: Option<String>,
}

#[cfg(target_os = "macos")]
struct CuaMcpPersistentSession {
    command: String,
    child: tokio::process::Child,
    stdin: ChildStdin,
    lines: tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    next_id: u64,
    started_at: String,
    calls: u64,
}

pub(super) async fn computer_use_tool(
    store: &AppStore,
    run_id: &str,
    payload: &Value,
) -> AppResult<String> {
    let action = computer_use_action(payload)?;
    let output = match action.as_str() {
        "status" | "capabilities" | "capability" | "backend_status" => {
            computer_use_status(payload)?
        }
        "requirements" => computer_use_requirements()?,
        "setup_schema" => computer_use_setup_schema()?,
        "reset_backend" => computer_use_reset_backend()?,
        "session_status" | "mcp_session_status" => computer_use_mcp_session_status()?,
        "mcp_probe" => computer_use_cua_mcp_probe(payload).await?,
        "wait" => computer_use_wait(store, run_id, payload).await?,
        "capture" => computer_use_capture(store, run_id, payload).await?,
        "list_apps" => computer_use_list_apps(payload).await?,
        "click" | "double_click" | "right_click" | "middle_click" | "drag" | "scroll" | "type"
        | "key" | "focus_app" | "set_value" => {
            computer_use_windows_action(store, run_id, payload).await?
        }
        other => {
            return Err(AppError::BadRequest(format!(
                "unsupported computer_use action: {other}"
            )))
        }
    };
    Ok(serde_json::to_string_pretty(&output)?)
}

pub(super) fn computer_use_action(payload: &Value) -> AppResult<String> {
    let action = required_string_arg(payload, &["action"], "computer_use")?
        .trim()
        .to_lowercase();
    let supported = [
        "capture",
        "click",
        "double_click",
        "right_click",
        "middle_click",
        "drag",
        "scroll",
        "type",
        "key",
        "set_value",
        "wait",
        "list_apps",
        "focus_app",
        "status",
        "capabilities",
        "capability",
        "requirements",
        "setup_schema",
        "backend_status",
        "reset_backend",
        "session_status",
        "mcp_session_status",
        "mcp_probe",
    ];
    if supported.contains(&action.as_str()) {
        Ok(action)
    } else {
        Err(AppError::BadRequest(format!(
            "unsupported computer_use action: {action}"
        )))
    }
}

fn computer_use_mcp_session_status() -> AppResult<Value> {
    let backend = computer_use_backend_status();
    Ok(json!({
        "action": "mcp_session_status",
        "ok": true,
        "backend": backend.get("name").cloned().unwrap_or(Value::Null),
        "platform": std::env::consts::OS,
        "persistentSessionImplemented": cfg!(target_os = "macos"),
        "activePersistentSession": computer_use_cua_mcp_persistent_session_snapshot(),
        "active_persistent_session": computer_use_cua_mcp_persistent_session_snapshot(),
        "lifecycleDiagnostics": computer_use_cua_mcp_lifecycle_snapshot(),
        "lifecycle_diagnostics": computer_use_cua_mcp_lifecycle_snapshot(),
        "mcpCommand": if backend.get("name").and_then(Value::as_str) == Some("cua-driver") {
            json!({
                "command": computer_use_cua_driver_command(),
                "args": ["mcp"]
            })
        } else {
            Value::Null
        },
        "hermesReference": {
            "backend": "cua-driver",
            "transport": "MCP stdio",
            "persistentSession": true,
            "lazyStart": true,
            "resetAction": "reset_backend"
        }
    }))
}

fn computer_use_status(_payload: &Value) -> AppResult<Value> {
    let backend = computer_use_backend_status();
    Ok(json!({
        "action": "status",
        "ok": true,
        "platform": std::env::consts::OS,
        "backend": backend,
        "safeActions": ["status", "capabilities", "capture", "list_apps", "wait"],
        "mutatingActions": [
            "click",
            "double_click",
            "right_click",
            "middle_click",
            "drag",
            "scroll",
            "type",
            "key",
            "set_value",
            "focus_app"
        ],
        "captureModes": ["som", "vision", "ax"],
        "maxElements": {
            "default": COMPUTER_USE_DEFAULT_MAX_ELEMENTS,
            "maximum": COMPUTER_USE_MAX_ALLOWED_ELEMENTS
        },
        "hermesParity": {
            "referenceBackend": "cua-driver MCP",
            "referencePlatform": "macOS",
            "referenceEnv": {
                "HERMES_COMPUTER_USE_BACKEND": "cua",
                "HERMES_CUA_DRIVER_CMD": "cua-driver",
                "HERMES_CUA_DRIVER_VERSION": "0.5.0"
            },
            "synthchatBackend": backend.get("name").cloned().unwrap_or(Value::Null),
            "backgroundInput": backend.get("backgroundInput").cloned().unwrap_or(Value::Bool(false)),
            "backgroundInputContract": computer_use_background_input_contract(&backend),
            "gap": backend.get("gap").cloned().unwrap_or(Value::Null)
        },
        "lifecycle": computer_use_lifecycle_status(&backend)
    }))
}

fn computer_use_requirements() -> AppResult<Value> {
    let backend = computer_use_backend_status();
    Ok(json!({
        "action": "requirements",
        "ok": true,
        "platform": std::env::consts::OS,
        "backend": backend,
        "requirements": {
            "hermesReference": {
                "platform": "macOS",
                "backend": "cua-driver",
                "transport": "MCP stdio",
                "command": computer_use_cua_driver_command(),
                "args": ["mcp"],
                "probeAction": {"action": "mcp_probe"},
                "env": {
                    "HERMES_COMPUTER_USE_BACKEND": "cua",
                    "HERMES_CUA_DRIVER_CMD": "cua-driver"
                },
                "install": [
                    "/bin/bash -c \"$(curl -fsSL https://raw.githubusercontent.com/trycua/cua/main/libs/cua-driver/scripts/install.sh)\"",
                    "brew install trycua/tap/cua-driver"
                ]
            },
            "synthchat": {
                "backend": backend.get("name").cloned().unwrap_or(Value::Null),
                "available": backend.get("available").cloned().unwrap_or(Value::Bool(false)),
                "transport": backend.get("transport").cloned().unwrap_or(Value::Null),
                "installHint": backend.get("installHint").cloned().unwrap_or(Value::Null),
                "backgroundInputContract": computer_use_background_input_contract(&backend),
                "lifecycle": computer_use_lifecycle_status(&backend)
            }
        }
    }))
}

fn computer_use_setup_schema() -> AppResult<Value> {
    Ok(json!({
        "action": "setup_schema",
        "ok": true,
        "backendOptions": [
            {
                "id": "windows-uia-compat",
                "platform": "windows",
                "transport": "PowerShell + UIAutomationClient + user32",
                "env": {},
                "implemented": cfg!(windows),
                "backgroundInput": false
            },
            {
                "id": "cua-driver",
                "platform": "macos",
                "transport": "MCP stdio",
                "env": {
                    "HERMES_COMPUTER_USE_BACKEND": "cua",
                    "HERMES_CUA_DRIVER_CMD": "cua-driver"
                },
                "command": computer_use_cua_driver_command(),
                "args": ["mcp"],
                "probeAction": {"action": "mcp_probe"},
                "implemented": cfg!(target_os = "macos"),
                "oneShotMcpClientImplemented": cfg!(target_os = "macos"),
                "persistentSessionImplemented": cfg!(target_os = "macos"),
                "backgroundInput": true,
                "backgroundInputContract": computer_use_cua_reference_background_input_contract(),
                "install": [
                    "/bin/bash -c \"$(curl -fsSL https://raw.githubusercontent.com/trycua/cua/main/libs/cua-driver/scripts/install.sh)\"",
                    "brew install trycua/tap/cua-driver"
                ]
            }
        ],
        "backgroundInputContract": computer_use_cua_reference_background_input_contract(),
        "nextImplementationStep": "Run a live macOS cua-driver smoke when that host/backend is available; desktop status now exposes the Hermes background-input contract and platform boundary explicitly."
    }))
}

fn computer_use_reset_backend() -> AppResult<Value> {
    let backend = computer_use_backend_status();
    let previous_lifecycle = computer_use_cua_mcp_lifecycle_snapshot();
    let stopped_persistent_session = computer_use_cua_mcp_reset_persistent_session();
    computer_use_cua_mcp_lifecycle_reset();
    Ok(json!({
        "action": "reset_backend",
        "ok": true,
        "backend": backend.get("name").cloned().unwrap_or(Value::Null),
        "reset": {
            "performed": true,
            "persistentSessionStopped": stopped_persistent_session,
            "clearedOneShotLifecycleStats": true,
            "previousLifecycle": previous_lifecycle,
            "reason": "Reset stops the macOS persistent cua-driver MCP session when present and clears SynthChat CUA MCP lifecycle diagnostics."
        },
        "lifecycle": computer_use_lifecycle_status(&backend)
    }))
}

fn computer_use_cua_mcp_lifecycle() -> &'static Mutex<CuaMcpLifecycle> {
    CUA_MCP_LIFECYCLE.get_or_init(|| Mutex::new(CuaMcpLifecycle::default()))
}

fn computer_use_cua_mcp_lifecycle_reset() {
    if let Ok(mut lifecycle) = computer_use_cua_mcp_lifecycle().lock() {
        *lifecycle = CuaMcpLifecycle::default();
    }
}

#[cfg(target_os = "macos")]
fn computer_use_cua_mcp_session() -> &'static tokio::sync::Mutex<Option<CuaMcpPersistentSession>> {
    CUA_MCP_SESSION.get_or_init(|| tokio::sync::Mutex::new(None))
}

#[cfg(target_os = "macos")]
fn computer_use_cua_mcp_reset_persistent_session() -> bool {
    let Some(session) = CUA_MCP_SESSION.get() else {
        return false;
    };
    if let Ok(mut guard) = session.try_lock() {
        if let Some(mut session) = guard.take() {
            let _ = session.child.start_kill();
            return true;
        }
    }
    false
}

#[cfg(not(target_os = "macos"))]
fn computer_use_cua_mcp_reset_persistent_session() -> bool {
    false
}

#[cfg(target_os = "macos")]
fn computer_use_cua_mcp_persistent_session_snapshot() -> Value {
    let Some(session) = CUA_MCP_SESSION.get() else {
        return Value::Null;
    };
    let Ok(guard) = session.try_lock() else {
        return json!({"active": true, "locked": true});
    };
    if let Some(session) = guard.as_ref() {
        json!({
            "active": true,
            "locked": false,
            "command": session.command,
            "startedAt": session.started_at,
            "started_at": session.started_at,
            "calls": session.calls,
            "nextId": session.next_id,
            "next_id": session.next_id
        })
    } else {
        Value::Null
    }
}

#[cfg(not(target_os = "macos"))]
fn computer_use_cua_mcp_persistent_session_snapshot() -> Value {
    Value::Null
}

fn computer_use_cua_mcp_record_start(tool: &str, probe: bool) {
    if let Ok(mut lifecycle) = computer_use_cua_mcp_lifecycle().lock() {
        if probe {
            lifecycle.one_shot_session_starts += 1;
            lifecycle.probe_calls += 1;
        } else {
            lifecycle.tool_calls += 1;
        }
        lifecycle.last_started_at = Some(now_iso());
        lifecycle.last_tool = Some(tool.to_string());
        lifecycle.last_error = None;
    }
}

#[cfg(target_os = "macos")]
fn computer_use_cua_mcp_record_persistent_session_start() {
    if let Ok(mut lifecycle) = computer_use_cua_mcp_lifecycle().lock() {
        lifecycle.persistent_session_starts += 1;
    }
}

fn computer_use_cua_mcp_record_finish(error: Option<String>) {
    if let Ok(mut lifecycle) = computer_use_cua_mcp_lifecycle().lock() {
        lifecycle.last_finished_at = Some(now_iso());
        if let Some(error) = error {
            lifecycle.errors += 1;
            lifecycle.last_error = Some(truncate_output(&error, 1000));
        } else {
            lifecycle.successes += 1;
            lifecycle.last_error = None;
        }
    }
}

fn computer_use_cua_mcp_lifecycle_snapshot() -> Value {
    let lifecycle = computer_use_cua_mcp_lifecycle()
        .lock()
        .map(|value| value.clone())
        .unwrap_or_default();
    json!({
        "persistentSession": cfg!(target_os = "macos"),
        "oneShotSessionStarts": lifecycle.one_shot_session_starts,
        "one_shot_session_starts": lifecycle.one_shot_session_starts,
        "persistentSessionStarts": lifecycle.persistent_session_starts,
        "persistent_session_starts": lifecycle.persistent_session_starts,
        "toolCalls": lifecycle.tool_calls,
        "tool_calls": lifecycle.tool_calls,
        "probeCalls": lifecycle.probe_calls,
        "probe_calls": lifecycle.probe_calls,
        "successes": lifecycle.successes,
        "errors": lifecycle.errors,
        "lastStartedAt": lifecycle.last_started_at.clone(),
        "last_started_at": lifecycle.last_started_at.clone(),
        "lastFinishedAt": lifecycle.last_finished_at.clone(),
        "last_finished_at": lifecycle.last_finished_at.clone(),
        "lastTool": lifecycle.last_tool.clone(),
        "last_tool": lifecycle.last_tool.clone(),
        "lastError": lifecycle.last_error.clone(),
        "last_error": lifecycle.last_error.clone()
    })
}

async fn computer_use_cua_mcp_probe(payload: &Value) -> AppResult<Value> {
    let command = computer_use_cua_driver_command();
    let timeout_seconds = computer_use_timeout_seconds(payload, 10).min(60);
    let Some(command_path) = find_computer_use_executable_on_path(&command) else {
        return Ok(json!({
            "action": "mcp_probe",
            "ok": false,
            "available": false,
            "command": command,
            "args": ["mcp"],
            "error": "cua-driver command was not found",
            "installHint": "Install cua-driver with: /bin/bash -c \"$(curl -fsSL https://raw.githubusercontent.com/trycua/cua/main/libs/cua-driver/scripts/install.sh)\""
        }));
    };
    let command_display = command_path.to_string_lossy().to_string();
    let probe = timeout(
        Duration::from_secs(timeout_seconds),
        computer_use_cua_mcp_tools_list(&command_display),
    )
    .await;
    match probe {
        Ok(Ok(raw)) => {
            let tools = raw
                .get("result")
                .and_then(|result| result.get("tools"))
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| item.get("name").and_then(Value::as_str))
                        .map(str::to_string)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            Ok(json!({
                "action": "mcp_probe",
                "ok": true,
                "available": true,
                "command": command_display,
                "args": ["mcp"],
                "tools": tools,
                "raw": raw
            }))
        }
        Ok(Err(error)) => Ok(json!({
            "action": "mcp_probe",
            "ok": false,
            "available": true,
            "command": command_display,
            "args": ["mcp"],
            "error": error.to_string()
        })),
        Err(_) => Ok(json!({
            "action": "mcp_probe",
            "ok": false,
            "available": true,
            "command": command_display,
            "args": ["mcp"],
            "timedOut": true,
            "timeoutSeconds": timeout_seconds,
            "error": format!("cua-driver MCP probe timed out after {timeout_seconds}s")
        })),
    }
}

async fn computer_use_cua_mcp_tools_list(command: &str) -> AppResult<Value> {
    computer_use_cua_mcp_record_start("tools/list", true);
    let mut child = Command::new(command)
        .hide_window()
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| {
            let error = format!("failed to start {command} mcp: {error}");
            computer_use_cua_mcp_record_finish(Some(error.clone()));
            AppError::BadRequest(error)
        })?;
    let mut stdin = child.stdin.take().ok_or_else(|| {
        let error = "missing cua-driver MCP stdin".to_string();
        computer_use_cua_mcp_record_finish(Some(error.clone()));
        AppError::BadRequest(error)
    })?;
    let stdout = child.stdout.take().ok_or_else(|| {
        let error = "missing cua-driver MCP stdout".to_string();
        computer_use_cua_mcp_record_finish(Some(error.clone()));
        AppError::BadRequest(error)
    })?;
    let mut lines = BufReader::new(stdout).lines();

    if let Err(error) = computer_use_write_rpc(
        &mut stdin,
        1,
        "initialize",
        json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "SynthChat", "version": "1.0.0"}
        }),
    )
    .await
    {
        computer_use_cua_mcp_record_finish(Some(error.to_string()));
        return Err(error);
    }
    if let Err(error) = computer_use_read_rpc_response(&mut lines, 1).await {
        computer_use_cua_mcp_record_finish(Some(error.to_string()));
        return Err(error);
    }
    if let Err(error) = stdin
        .write_all(
            json!({"jsonrpc":"2.0","method":"notifications/initialized","params":{}})
                .to_string()
                .as_bytes(),
        )
        .await
    {
        computer_use_cua_mcp_record_finish(Some(error.to_string()));
        return Err(error.into());
    }
    if let Err(error) = stdin.write_all(b"\n").await {
        computer_use_cua_mcp_record_finish(Some(error.to_string()));
        return Err(error.into());
    }
    if let Err(error) = computer_use_write_rpc(&mut stdin, 2, "tools/list", json!({})).await {
        computer_use_cua_mcp_record_finish(Some(error.to_string()));
        return Err(error);
    }
    let response = computer_use_read_rpc_response(&mut lines, 2).await;
    let _ = child.kill().await;
    match &response {
        Ok(_) => computer_use_cua_mcp_record_finish(None),
        Err(error) => computer_use_cua_mcp_record_finish(Some(error.to_string())),
    }
    response
}

#[cfg(target_os = "macos")]
async fn computer_use_cua_mcp_call_tool(
    tool_name: &str,
    arguments: Value,
    timeout_seconds: u64,
) -> AppResult<Value> {
    let command = computer_use_cua_driver_command();
    let command_path = find_computer_use_executable_on_path(&command).ok_or_else(|| {
        AppError::BadRequest(format!(
            "cua-driver command not found: {command}; run computer_use action=requirements"
        ))
    })?;
    let command_display = command_path.to_string_lossy().to_string();
    match timeout(
        Duration::from_secs(timeout_seconds.max(1)),
        computer_use_cua_mcp_call_tool_inner(&command_display, tool_name, arguments),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => {
            let error =
                format!("cua-driver MCP tool {tool_name} timed out after {timeout_seconds}s");
            computer_use_cua_mcp_record_finish(Some(error.clone()));
            let _ = computer_use_cua_mcp_reset_persistent_session();
            Err(AppError::BadRequest(error))
        }
    }
}

#[cfg(target_os = "macos")]
async fn computer_use_cua_mcp_call_tool_inner(
    command: &str,
    tool_name: &str,
    arguments: Value,
) -> AppResult<Value> {
    computer_use_cua_mcp_persistent_call_tool(command, tool_name, arguments).await
}

#[cfg(target_os = "macos")]
async fn computer_use_cua_mcp_start_persistent_session(
    command: &str,
) -> AppResult<CuaMcpPersistentSession> {
    let mut child = Command::new(command)
        .hide_window()
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| {
            let error = format!("failed to start {command} mcp: {error}");
            computer_use_cua_mcp_record_finish(Some(error.clone()));
            AppError::BadRequest(error)
        })?;
    let mut stdin = child.stdin.take().ok_or_else(|| {
        let error = "missing cua-driver MCP stdin".to_string();
        computer_use_cua_mcp_record_finish(Some(error.clone()));
        AppError::BadRequest(error)
    })?;
    let stdout = child.stdout.take().ok_or_else(|| {
        let error = "missing cua-driver MCP stdout".to_string();
        computer_use_cua_mcp_record_finish(Some(error.clone()));
        AppError::BadRequest(error)
    })?;
    let mut lines = BufReader::new(stdout).lines();

    if let Err(error) = computer_use_write_rpc(
        &mut stdin,
        1,
        "initialize",
        json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "SynthChat", "version": "1.0.0"}
        }),
    )
    .await
    {
        computer_use_cua_mcp_record_finish(Some(error.to_string()));
        return Err(error);
    }
    if let Err(error) = computer_use_read_rpc_response(&mut lines, 1).await {
        computer_use_cua_mcp_record_finish(Some(error.to_string()));
        return Err(error);
    }
    if let Err(error) = stdin
        .write_all(
            json!({"jsonrpc":"2.0","method":"notifications/initialized","params":{}})
                .to_string()
                .as_bytes(),
        )
        .await
    {
        computer_use_cua_mcp_record_finish(Some(error.to_string()));
        return Err(error.into());
    }
    if let Err(error) = stdin.write_all(b"\n").await {
        computer_use_cua_mcp_record_finish(Some(error.to_string()));
        return Err(error.into());
    }
    computer_use_cua_mcp_record_persistent_session_start();
    Ok(CuaMcpPersistentSession {
        command: command.to_string(),
        child,
        stdin,
        lines,
        next_id: 1,
        started_at: now_iso(),
        calls: 0,
    })
}

#[cfg(target_os = "macos")]
async fn computer_use_cua_mcp_persistent_call_tool(
    command: &str,
    tool_name: &str,
    arguments: Value,
) -> AppResult<Value> {
    computer_use_cua_mcp_record_start(tool_name, false);
    let session_lock = computer_use_cua_mcp_session();
    let mut guard = session_lock.lock().await;
    let needs_start = guard
        .as_ref()
        .map(|session| session.command != command)
        .unwrap_or(true);
    if needs_start {
        if let Some(mut previous) = guard.take() {
            let _ = previous.child.start_kill();
        }
        match computer_use_cua_mcp_start_persistent_session(command).await {
            Ok(session) => {
                *guard = Some(session);
            }
            Err(error) => {
                return Err(error);
            }
        }
    }
    let Some(session) = guard.as_mut() else {
        let error = "cua-driver persistent MCP session was not initialized".to_string();
        computer_use_cua_mcp_record_finish(Some(error.clone()));
        return Err(AppError::BadRequest(error));
    };
    session.calls += 1;
    session.next_id += 1;
    let id = session.next_id;
    if let Err(error) = computer_use_write_rpc(
        &mut session.stdin,
        id,
        "tools/call",
        json!({"name": tool_name, "arguments": arguments}),
    )
    .await
    {
        computer_use_cua_mcp_record_finish(Some(error.to_string()));
        if let Some(mut failed) = guard.take() {
            let _ = failed.child.start_kill();
        }
        return Err(error);
    }
    let response = computer_use_read_rpc_response(&mut session.lines, id).await;
    match &response {
        Ok(_) => computer_use_cua_mcp_record_finish(None),
        Err(error) => {
            computer_use_cua_mcp_record_finish(Some(error.to_string()));
            if let Some(mut failed) = guard.take() {
                let _ = failed.child.start_kill();
            }
        }
    }
    response
}

async fn computer_use_write_rpc(
    stdin: &mut ChildStdin,
    id: u64,
    method: &str,
    params: Value,
) -> AppResult<()> {
    stdin
        .write_all(
            json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})
                .to_string()
                .as_bytes(),
        )
        .await?;
    stdin.write_all(b"\n").await?;
    Ok(())
}

async fn computer_use_read_rpc_response(
    lines: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    id: u64,
) -> AppResult<Value> {
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(&line).map_err(|error| {
            AppError::BadRequest(format!("invalid cua-driver MCP JSON line: {error}: {line}"))
        })?;
        if value.get("id").and_then(Value::as_u64) != Some(id) {
            continue;
        }
        if let Some(error) = value.get("error") {
            return Err(AppError::BadRequest(format!(
                "cua-driver MCP error: {error}"
            )));
        }
        return Ok(value);
    }
    Err(AppError::BadRequest(
        "cua-driver MCP closed stdout before response".into(),
    ))
}

fn computer_use_lifecycle_status(backend: &Value) -> Value {
    let name = backend.get("name").and_then(Value::as_str).unwrap_or("");
    json!({
        "persistentSession": cfg!(target_os = "macos"),
        "lazyStart": name == "cua-driver",
        "resetSupported": true,
        "stdioMcpClientImplemented": cfg!(target_os = "macos"),
        "oneShotMcpClientImplemented": cfg!(target_os = "macos"),
        "stdioMcpProbeImplemented": true,
        "lifecycleDiagnostics": computer_use_cua_mcp_lifecycle_snapshot(),
        "activePersistentSession": computer_use_cua_mcp_persistent_session_snapshot(),
        "active_persistent_session": computer_use_cua_mcp_persistent_session_snapshot(),
        "mcpCommand": if name == "cua-driver" {
            json!({
                "command": computer_use_cua_driver_command(),
                "args": ["mcp"]
            })
        } else {
            Value::Null
        },
        "hermesReference": {
            "persistentSession": true,
            "lazyStart": true,
            "resetSupported": true,
            "stdioMcpClientImplemented": true,
            "backgroundInputContract": computer_use_cua_reference_background_input_contract()
        },
        "backgroundInputContract": computer_use_background_input_contract(backend),
        "gap": if cfg!(target_os = "macos") {
            "SynthChat uses a lazy shared cua-driver MCP session for capture and actions; live end-to-end validation depends on a configured macOS cua-driver host."
        } else {
            "SynthChat CUA persistent cua-driver MCP session is compiled for macOS; this platform uses a compatibility or unsupported backend."
        }
    })
}

fn computer_use_cua_reference_background_input_contract() -> Value {
    json!({
        "schema": "hermes_computer_use_background_input_contract_v1",
        "referenceBackend": "cua-driver",
        "transport": "MCP stdio",
        "doesNotStealCursor": true,
        "doesNotStealKeyboardFocus": true,
        "doesNotStealSpace": true,
        "hiddenOrBehindWindows": true,
        "elementTargeting": true,
        "pixelCoordinates": true,
        "nativeValueMutation": true,
        "focusWithoutRaiseDefault": true,
        "captureModes": ["som", "vision", "ax"],
        "readOnlyActions": ["capture", "wait", "list_apps"],
        "mutatingActions": [
            "click",
            "double_click",
            "right_click",
            "middle_click",
            "drag",
            "scroll",
            "type",
            "key",
            "set_value",
            "focus_app"
        ],
        "approvalRequiredForMutatingActions": true,
        "hardBlockedSafetyRules": [
            "destructive_system_shortcuts",
            "dangerous_shell_text_patterns"
        ],
        "defaultRaiseWindow": false
    })
}

fn computer_use_background_input_contract(backend: &Value) -> Value {
    let reference = computer_use_cua_reference_background_input_contract();
    let background_input = backend
        .get("backgroundInput")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let backend_name = backend
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    json!({
        "schema": "synthchat_computer_use_background_input_contract_v1",
        "matchesHermesReference": cfg!(target_os = "macos") && backend_name == "cua-driver",
        "reference": reference,
        "backend": backend_name,
        "transport": backend.get("transport").cloned().unwrap_or(Value::Null),
        "backgroundInput": background_input,
        "persistentMcpSession": cfg!(target_os = "macos") && backend_name == "cua-driver",
        "lazyStart": backend_name == "cua-driver",
        "captureModes": ["som", "vision", "ax"],
        "supportedTargeting": {
            "element": true,
            "coordinate": true,
            "app": true,
            "window": cfg!(target_os = "macos")
        },
        "approvalRequiredForMutatingActions": true,
        "hardBlockedSafetyRules": [
            "destructive_system_shortcuts",
            "dangerous_shell_text_patterns"
        ],
        "platformBoundary": if background_input {
            "Uses Hermes cua-driver MCP background input semantics when the macOS backend is available."
        } else {
            "This platform uses a foreground/compatibility backend or no backend, so Hermes background-input behavior is reported but not claimed."
        }
    })
}

fn computer_use_cua_driver_command() -> String {
    std::env::var("HERMES_CUA_DRIVER_CMD").unwrap_or_else(|_| "cua-driver".into())
}

fn computer_use_timeout_seconds(payload: &Value, default_seconds: u64) -> u64 {
    payload
        .get("timeoutSeconds")
        .or_else(|| payload.get("timeout_seconds"))
        .and_then(Value::as_u64)
        .unwrap_or(default_seconds)
        .clamp(1, 120)
}

#[cfg(target_os = "macos")]
fn computer_use_cua_windows_from_result(raw: &Value) -> Vec<Value> {
    if let Some(windows) = raw
        .get("result")
        .and_then(|result| result.get("structuredContent"))
        .and_then(|structured| structured.get("windows"))
        .and_then(Value::as_array)
    {
        return windows.clone();
    }
    computer_use_cua_parse_windows_from_text(&computer_use_cua_text_from_result(raw))
}

#[cfg(target_os = "macos")]
fn computer_use_cua_text_from_result(raw: &Value) -> String {
    raw.get("result")
        .and_then(|result| result.get("content"))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter(|item| item.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|item| item.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

#[cfg(target_os = "macos")]
fn computer_use_cua_images_from_result(raw: &Value) -> Vec<String> {
    raw.get("result")
        .and_then(|result| result.get("content"))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter(|item| item.get("type").and_then(Value::as_str) == Some("image"))
                .filter_map(|item| item.get("data").and_then(Value::as_str))
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

#[cfg(target_os = "macos")]
fn computer_use_cua_parse_windows_from_text(text: &str) -> Vec<Value> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            let rest = line.strip_prefix("- ")?;
            let (app_name, after_app) = rest.split_once(" (pid ")?;
            let (pid_text, after_pid) = after_app.split_once(')')?;
            let marker = "[window_id:";
            let window_start = after_pid.find(marker)? + marker.len();
            let window_rest = &after_pid[window_start..];
            let window_id_text = window_rest.split(']').next()?.trim();
            let title = after_pid
                .split('"')
                .nth(1)
                .map(str::to_string)
                .unwrap_or_default();
            Some(json!({
                "app_name": app_name.trim(),
                "pid": pid_text.trim().parse::<i64>().ok()?,
                "window_id": window_id_text.parse::<i64>().ok()?,
                "title": title,
                "off_screen": line.contains("[off-screen]")
            }))
        })
        .collect()
}

#[cfg(target_os = "macos")]
fn computer_use_cua_elements_from_tree(text: &str) -> Vec<Value> {
    text.lines()
        .filter_map(|line| {
            let trimmed = line.trim_start().strip_prefix("- ").unwrap_or(line.trim());
            let rest = trimmed.strip_prefix('[')?;
            let (index_text, after_index) = rest.split_once(']')?;
            let mut parts = after_index.trim().splitn(2, char::is_whitespace);
            let role = parts.next().unwrap_or_default();
            if role.is_empty() {
                return None;
            }
            let label = parts
                .next()
                .and_then(|tail| tail.split('"').nth(1).or_else(|| tail.strip_prefix("id=")))
                .unwrap_or_default();
            Some(json!({
                "index": index_text.trim().parse::<u64>().ok()?,
                "role": role,
                "label": label
            }))
        })
        .collect()
}

#[cfg(target_os = "macos")]
fn computer_use_image_extension(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        "jpg"
    } else if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        "png"
    } else {
        "bin"
    }
}

#[cfg(target_os = "macos")]
#[derive(Debug, Clone)]
struct CuaTargetWindow {
    app_name: String,
    title: String,
    pid: i64,
    window_id: i64,
}

#[cfg(target_os = "macos")]
impl CuaTargetWindow {
    fn as_json(&self) -> Value {
        json!({
            "appName": self.app_name,
            "title": self.title,
            "pid": self.pid,
            "windowId": self.window_id
        })
    }
}

#[cfg(target_os = "macos")]
async fn computer_use_cua_target_window(
    payload: &Value,
    timeout_seconds: u64,
) -> AppResult<CuaTargetWindow> {
    let app_filter = string_arg(payload, &["app"]).unwrap_or_default();
    let list_raw = computer_use_cua_mcp_call_tool(
        "list_windows",
        json!({"on_screen_only": true}),
        timeout_seconds,
    )
    .await?;
    let mut windows = computer_use_cua_windows_from_result(&list_raw);
    windows.sort_by_key(|window| {
        window
            .get("z_index")
            .and_then(Value::as_i64)
            .unwrap_or(i64::MAX)
    });
    if !app_filter.trim().is_empty() {
        let needle = app_filter.to_lowercase();
        windows.retain(|window| {
            ["app_name", "appName", "processName", "title"]
                .iter()
                .filter_map(|key| window.get(*key).and_then(Value::as_str))
                .any(|value| value.to_lowercase().contains(&needle))
        });
    }
    let Some(window) = windows.first() else {
        return Err(AppError::BadRequest(format!(
            "No on-screen window matched app filter '{}'; call computer_use list_apps",
            app_filter
        )));
    };
    Ok(CuaTargetWindow {
        app_name: window
            .get("app_name")
            .or_else(|| window.get("appName"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        title: window
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        pid: window
            .get("pid")
            .and_then(Value::as_i64)
            .ok_or_else(|| AppError::BadRequest("cua-driver target window missing pid".into()))?,
        window_id: window
            .get("window_id")
            .or_else(|| window.get("windowId"))
            .and_then(Value::as_i64)
            .ok_or_else(|| {
                AppError::BadRequest("cua-driver target window missing window_id".into())
            })?,
    })
}

#[cfg(target_os = "macos")]
fn computer_use_cua_action_call(
    action: &str,
    payload: &Value,
    target: &CuaTargetWindow,
) -> AppResult<(String, Value)> {
    let mut args = json!({"pid": target.pid});
    match action {
        "click" | "double_click" | "right_click" | "middle_click" => {
            let tool = match action {
                "double_click" => "double_click",
                "right_click" => "right_click",
                "middle_click" => "middle_click",
                _ => payload
                    .get("button")
                    .and_then(Value::as_str)
                    .map_or("click", |button| {
                        if button.eq_ignore_ascii_case("right") {
                            "right_click"
                        } else if button.eq_ignore_ascii_case("middle") {
                            "middle_click"
                        } else {
                            "click"
                        }
                    }),
            };
            if let Some(element) = payload.get("element").and_then(Value::as_u64) {
                args["window_id"] = json!(target.window_id);
                args["element_index"] = json!(element);
            } else {
                let (x, y) = computer_use_coordinate(payload, "coordinate")?;
                args["x"] = json!(x);
                args["y"] = json!(y);
            }
            Ok((tool.into(), args))
        }
        "drag" => {
            if let (Some(from_element), Some(to_element)) = (
                payload.get("from_element").and_then(Value::as_u64),
                payload.get("to_element").and_then(Value::as_u64),
            ) {
                args["window_id"] = json!(target.window_id);
                args["from_element"] = json!(from_element);
                args["to_element"] = json!(to_element);
            } else {
                let (from_x, from_y) = computer_use_coordinate(payload, "from_coordinate")?;
                let (to_x, to_y) = computer_use_coordinate(payload, "to_coordinate")?;
                args["from_x"] = json!(from_x);
                args["from_y"] = json!(from_y);
                args["to_x"] = json!(to_x);
                args["to_y"] = json!(to_y);
            }
            Ok(("drag".into(), args))
        }
        "scroll" => {
            args["direction"] = payload
                .get("direction")
                .and_then(Value::as_str)
                .unwrap_or("down")
                .into();
            args["amount"] = json!(payload
                .get("amount")
                .and_then(Value::as_u64)
                .unwrap_or(3)
                .clamp(1, 50));
            if let Some(element) = payload.get("element").and_then(Value::as_u64) {
                args["window_id"] = json!(target.window_id);
                args["element_index"] = json!(element);
            } else if payload.get("coordinate").is_some() {
                let (x, y) = computer_use_coordinate(payload, "coordinate")?;
                args["x"] = json!(x);
                args["y"] = json!(y);
            }
            Ok(("scroll".into(), args))
        }
        "type" => {
            args["text"] = payload
                .get("text")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    AppError::BadRequest("computer_use action=type requires payload.text".into())
                })?
                .into();
            Ok(("type_text".into(), args))
        }
        "key" => {
            let keys = payload.get("keys").and_then(Value::as_str).ok_or_else(|| {
                AppError::BadRequest("computer_use action=key requires payload.keys".into())
            })?;
            let (key, modifiers) = computer_use_cua_parse_key_combo(keys)?;
            if modifiers.is_empty() {
                args["key"] = json!(key);
                Ok(("press_key".into(), args))
            } else {
                let mut combo = modifiers;
                combo.push(key);
                args["keys"] = json!(combo);
                Ok(("hotkey".into(), args))
            }
        }
        "set_value" => {
            args["window_id"] = json!(target.window_id);
            args["element_index"] = payload
                .get("element")
                .and_then(Value::as_u64)
                .ok_or_else(|| {
                    AppError::BadRequest(
                        "computer_use action=set_value requires payload.element".into(),
                    )
                })?
                .into();
            args["value"] = payload.get("value").cloned().ok_or_else(|| {
                AppError::BadRequest("computer_use action=set_value requires payload.value".into())
            })?;
            Ok(("set_value".into(), args))
        }
        other => Err(AppError::BadRequest(format!(
            "unsupported macOS computer_use action: {other}"
        ))),
    }
}

#[cfg(target_os = "macos")]
fn computer_use_cua_parse_key_combo(keys: &str) -> AppResult<(String, Vec<String>)> {
    let mut modifiers = Vec::new();
    let mut key = String::new();
    for part in keys.split('+') {
        let normalized = part.trim().to_lowercase();
        if normalized.is_empty() {
            continue;
        }
        match normalized.as_str() {
            "cmd" | "command" | "meta" | "win" | "windows" => modifiers.push("cmd".into()),
            "ctrl" | "control" => modifiers.push("ctrl".into()),
            "alt" | "option" => modifiers.push("option".into()),
            "shift" => modifiers.push("shift".into()),
            other => key = other.to_string(),
        }
    }
    if key.is_empty() {
        Err(AppError::BadRequest(format!(
            "computer_use action=key could not parse key from '{keys}'"
        )))
    } else {
        Ok((key, modifiers))
    }
}

#[cfg(target_os = "macos")]
fn computer_use_cua_action_result(
    action: &str,
    tool_name: &str,
    raw: &Value,
    target: &CuaTargetWindow,
) -> Value {
    let is_error = raw
        .get("result")
        .and_then(|result| result.get("isError"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let text = computer_use_cua_text_from_result(raw);
    json!({
        "action": action,
        "tool": tool_name,
        "ok": !is_error,
        "backend": "cua-driver",
        "message": text,
        "target": target.as_json(),
        "raw": raw
    })
}

fn computer_use_backend_status() -> Value {
    #[cfg(windows)]
    {
        let powershell = find_computer_use_executable_on_path("powershell")
            .or_else(|| find_computer_use_executable_on_path("powershell.exe"));
        return json!({
            "name": "windows-uia-compat",
            "available": powershell.is_some(),
            "transport": "PowerShell + UIAutomationClient + user32",
            "powerShellPath": powershell
                .as_ref()
                .map(|path| path.to_string_lossy().to_string())
                .unwrap_or_default(),
            "backgroundInput": false,
            "supportsScreenshots": true,
            "supportsAxTree": true,
            "supportsElementTargets": true,
            "installHint": Value::Null,
            "gap": "Hermes uses macOS cua-driver over MCP for background computer-use without stealing cursor/focus; this build provides a Windows UIA compatibility backend that may move cursor or focus foreground windows."
        });
    }
    #[cfg(target_os = "macos")]
    {
        let cua = std::env::var("HERMES_CUA_DRIVER_CMD")
            .ok()
            .and_then(|cmd| find_computer_use_executable_on_path(&cmd))
            .or_else(|| find_computer_use_executable_on_path("cua-driver"));
        return json!({
            "name": "cua-driver",
            "available": cua.is_some(),
            "transport": "MCP stdio",
            "cuaDriverPath": cua
                .as_ref()
                .map(|path| path.to_string_lossy().to_string())
                .unwrap_or_default(),
            "backgroundInput": true,
            "supportsScreenshots": true,
            "supportsAxTree": true,
            "supportsElementTargets": true,
            "installHint": if cua.is_some() {
                Value::Null
            } else {
                json!("Install cua-driver with: /bin/bash -c \"$(curl -fsSL https://raw.githubusercontent.com/trycua/cua/main/libs/cua-driver/scripts/install.sh)\"")
            },
            "gap": "SynthChat routes macOS CUA tool calls through a lazy persistent cua-driver MCP session; remaining gap is full Hermes background input parity validation."
        });
    }
    #[cfg(all(not(windows), not(target_os = "macos")))]
    {
        json!({
            "name": "unsupported",
            "available": false,
            "transport": Value::Null,
            "backgroundInput": false,
            "supportsScreenshots": false,
            "supportsAxTree": false,
            "supportsElementTargets": false,
            "installHint": "Computer Use is currently implemented for Windows compatibility and Hermes macOS cua-driver parity is pending.",
            "gap": "No Linux desktop backend is implemented in this build."
        })
    }
}

fn find_computer_use_executable_on_path(name: &str) -> Option<std::path::PathBuf> {
    let direct = std::path::PathBuf::from(name);
    if (direct.is_absolute() || direct.components().count() > 1) && direct.is_file() {
        return Some(direct);
    }
    let path_var = std::env::var_os("PATH")?;
    let extensions = computer_use_executable_extensions();
    for dir in std::env::split_paths(&path_var) {
        for extension in &extensions {
            let candidate = dir.join(format!("{name}{extension}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

fn computer_use_executable_extensions() -> Vec<String> {
    #[cfg(windows)]
    {
        let mut extensions = std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".EXE;.CMD;.BAT;.COM".into())
            .split(';')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| {
                if value.starts_with('.') {
                    value.to_ascii_lowercase()
                } else {
                    format!(".{}", value.to_ascii_lowercase())
                }
            })
            .collect::<Vec<_>>();
        extensions.insert(0, String::new());
        extensions
    }
    #[cfg(not(windows))]
    {
        vec![String::new()]
    }
}

async fn computer_use_wait(store: &AppStore, run_id: &str, payload: &Value) -> AppResult<Value> {
    let seconds = payload
        .get("seconds")
        .and_then(Value::as_f64)
        .unwrap_or(1.0)
        .clamp(0.0, 30.0);
    let total_ms = (seconds * 1000.0) as u64;
    let started = tokio::time::Instant::now();
    let deadline = started + Duration::from_millis(total_ms);
    loop {
        if computer_use_run_interrupted(store, run_id)? {
            return Ok(json!({
                "action": "wait",
                "ok": false,
                "seconds": seconds,
                "waitInterrupted": true,
                "waitInterruptedReason": "agent_run_aborted",
                "elapsedMs": started.elapsed().as_millis()
            }));
        }
        let now = tokio::time::Instant::now();
        if now >= deadline {
            break;
        }
        tokio::time::sleep((deadline - now).min(Duration::from_millis(100))).await;
    }
    Ok(json!({
        "action": "wait",
        "ok": true,
        "seconds": seconds,
        "waitInterrupted": false,
        "elapsedMs": started.elapsed().as_millis()
    }))
}

fn computer_use_run_interrupted(store: &AppStore, run_id: &str) -> AppResult<bool> {
    match store.agent_run(run_id) {
        Ok(run) => Ok(matches!(
            run.state.as_str(),
            "completed" | "failed" | "aborted"
        )),
        Err(AppError::NotFound(_)) => Ok(false),
        Err(error) => Err(error),
    }
}

#[cfg(windows)]
async fn computer_use_capture(store: &AppStore, run_id: &str, payload: &Value) -> AppResult<Value> {
    use base64::Engine;

    let mode = string_arg(payload, &["mode"]).unwrap_or_else(|| "som".into());
    let max_elements = coerce_computer_use_max_elements(payload.get("max_elements"));
    let app_query = string_arg(payload, &["app"]).unwrap_or_default();
    let app_query_b64 = base64::engine::general_purpose::STANDARD.encode(app_query.as_bytes());
    let script_template = r#"
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName UIAutomationClient
$maxElements = __MAX_ELEMENTS__
$captureMode = '__CAPTURE_MODE__'
$appQuery = [System.Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('__APP_QUERY_B64__'))
$virtual = [System.Windows.Forms.SystemInformation]::VirtualScreen
$root = [System.Windows.Automation.AutomationElement]::RootElement
$windowTitle = ''
$processName = ''
if (-not [string]::IsNullOrWhiteSpace($appQuery)) {
  $candidate = Get-Process | Where-Object { $_.MainWindowHandle -ne 0 -and ($_.ProcessName -like "*$appQuery*" -or $_.MainWindowTitle -like "*$appQuery*") } | Select-Object -First 1
  if ($null -ne $candidate) {
    $candidateRoot = [System.Windows.Automation.AutomationElement]::FromHandle($candidate.MainWindowHandle)
    if ($null -ne $candidateRoot) {
      $root = $candidateRoot
      $windowTitle = [string]$candidate.MainWindowTitle
      $processName = [string]$candidate.ProcessName
    }
  }
}
$rootRect = $root.Current.BoundingRectangle
if ($root -ne [System.Windows.Automation.AutomationElement]::RootElement -and $rootRect.Width -gt 1 -and $rootRect.Height -gt 1) {
  $captureLeft = [int][Math]::Max($virtual.Left, [Math]::Floor($rootRect.Left))
  $captureTop = [int][Math]::Max($virtual.Top, [Math]::Floor($rootRect.Top))
  $captureRight = [int][Math]::Min($virtual.Left + $virtual.Width, [Math]::Ceiling($rootRect.Right))
  $captureBottom = [int][Math]::Min($virtual.Top + $virtual.Height, [Math]::Ceiling($rootRect.Bottom))
  $captureWidth = [Math]::Max(1, $captureRight - $captureLeft)
  $captureHeight = [Math]::Max(1, $captureBottom - $captureTop)
} else {
  $captureLeft = $virtual.Left
  $captureTop = $virtual.Top
  $captureWidth = $virtual.Width
  $captureHeight = $virtual.Height
}
$bitmap = New-Object System.Drawing.Bitmap $captureWidth, $captureHeight
$graphics = [System.Drawing.Graphics]::FromImage($bitmap)
$graphics.CopyFromScreen($captureLeft, $captureTop, 0, 0, (New-Object System.Drawing.Size($captureWidth, $captureHeight)))
$elements = @()
$totalElements = 0
try {
  $condition = [System.Windows.Automation.Condition]::TrueCondition
  $all = $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $condition)
  $index = 1
  foreach ($el in $all) {
    $rect = $el.Current.BoundingRectangle
    if ($rect.Width -le 1 -or $rect.Height -le 1) { continue }
    if ($rect.Right -lt $captureLeft -or $rect.Bottom -lt $captureTop -or $rect.Left -gt ($captureLeft + $captureWidth) -or $rect.Top -gt ($captureTop + $captureHeight)) { continue }
    $role = $el.Current.ControlType.ProgrammaticName -replace '^ControlType\.',''
    if ($role -notin @('Button','Edit','Hyperlink','ListItem','MenuItem','TabItem','ComboBox','CheckBox','RadioButton','TreeItem','DataItem','Text','Document','Pane')) { continue }
    $label = $el.Current.Name
    if ([string]::IsNullOrWhiteSpace($label)) { $label = $el.Current.AutomationId }
    if ([string]::IsNullOrWhiteSpace($label) -and $role -in @('Pane','Text','Document')) { continue }
    $totalElements += 1
    if ($elements.Count -lt $maxElements) {
      $elements += @{
        index = $index
        role = $role
        label = [string]$label
        automationId = [string]$el.Current.AutomationId
        className = [string]$el.Current.ClassName
        bounds = @([int]$rect.Left, [int]$rect.Top, [int]$rect.Width, [int]$rect.Height)
        center = @([int]($rect.Left + ($rect.Width / 2)), [int]($rect.Top + ($rect.Height / 2)))
      }
      $index += 1
    }
  }
} catch {
  $elements = @()
  $totalElements = 0
}
if ($captureMode -eq 'som' -and $elements.Count -gt 0) {
  $font = New-Object System.Drawing.Font('Segoe UI', 9, [System.Drawing.FontStyle]::Bold)
  $pen = New-Object System.Drawing.Pen([System.Drawing.Color]::FromArgb(230, 0, 120, 215), 2)
  $fill = New-Object System.Drawing.SolidBrush([System.Drawing.Color]::FromArgb(230, 0, 120, 215))
  $textBrush = New-Object System.Drawing.SolidBrush([System.Drawing.Color]::White)
  foreach ($item in $elements) {
    $boundsArray = $item.bounds
    if ($null -eq $boundsArray -or $boundsArray.Count -lt 4) { continue }
    $x = [int]$boundsArray[0] - $captureLeft
    $y = [int]$boundsArray[1] - $captureTop
    $w = [Math]::Max(1, [int]$boundsArray[2])
    $h = [Math]::Max(1, [int]$boundsArray[3])
    $label = [string]$item.index
    $graphics.DrawRectangle($pen, $x, $y, $w, $h)
    $size = $graphics.MeasureString($label, $font)
    $labelW = [Math]::Ceiling($size.Width) + 6
    $labelH = [Math]::Ceiling($size.Height) + 2
    $labelX = [Math]::Max(0, [Math]::Min($x, $captureWidth - $labelW))
    $labelY = [Math]::Max(0, [Math]::Min($y, $captureHeight - $labelH))
    $graphics.FillRectangle($fill, $labelX, $labelY, $labelW, $labelH)
    $graphics.DrawString($label, $font, $textBrush, $labelX + 3, $labelY)
  }
  $font.Dispose()
  $pen.Dispose()
  $fill.Dispose()
  $textBrush.Dispose()
}
$stream = New-Object System.IO.MemoryStream
$bitmap.Save($stream, [System.Drawing.Imaging.ImageFormat]::Png)
$bytes = $stream.ToArray()
$graphics.Dispose()
$bitmap.Dispose()
$stream.Dispose()
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
@{
  left = $captureLeft
  top = $captureTop
  width = $captureWidth
  height = $captureHeight
  app = $appQuery
  matchedProcessName = $processName
  windowTitle = $windowTitle
  imageBase64 = [Convert]::ToBase64String($bytes)
  elements = $elements
  totalElements = $totalElements
  truncatedElements = [Math]::Max(0, $totalElements - $elements.Count)
} | ConvertTo-Json -Depth 4
"#;
    let script = script_template
        .replace("__MAX_ELEMENTS__", &max_elements.to_string())
        .replace("__CAPTURE_MODE__", &mode)
        .replace("__APP_QUERY_B64__", &app_query_b64);
    let stdout = run_powershell_script(&script, 10).await?;
    let mut value: Value = serde_json::from_str(stdout.trim()).map_err(|error| {
        AppError::BadRequest(format!(
            "computer_use capture returned invalid JSON: {error}"
        ))
    })?;
    let image_base64 = value
        .get("imageBase64")
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::BadRequest("computer_use capture missing image".into()))?;
    let bytes = decode_base64_image(image_base64)?;
    let path = store.save_tool_binary_artifact(run_id, "computer_use", "png", &bytes)?;
    if let Some(obj) = value.as_object_mut() {
        obj.remove("imageBase64");
        obj.insert("action".into(), Value::String("capture".into()));
        obj.insert("ok".into(), Value::Bool(true));
        obj.insert("mode".into(), Value::String(mode.clone()));
        obj.insert(
            "artifactPath".into(),
            Value::String(path.to_string_lossy().into()),
        );
        obj.insert("sizeBytes".into(), json!(bytes.len()));
        let elements = obj
            .get("elements")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        COMPUTER_USE_ELEMENTS
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .map_err(|_| AppError::BadRequest("computer_use element cache lock poisoned".into()))?
            .insert(run_id.to_string(), elements.clone());
        let total_elements = obj
            .get("totalElements")
            .and_then(Value::as_u64)
            .unwrap_or(elements.len() as u64);
        let truncated_elements = obj
            .get("truncatedElements")
            .and_then(Value::as_u64)
            .unwrap_or_else(|| total_elements.saturating_sub(elements.len() as u64));
        obj.insert("maxElements".into(), json!(max_elements));
        obj.insert("totalElements".into(), json!(total_elements));
        obj.insert("truncatedElements".into(), json!(truncated_elements));
        if mode == "som" {
            obj.insert(
                "note".into(),
                Value::String(
                    "Windows compatibility backend returns a screenshot with numbered UI Automation element overlays plus the matching element list.".into(),
                ),
            );
        } else if mode == "ax" {
            obj.insert(
                "note".into(),
                Value::String(
                    "Windows compatibility backend exposes a lightweight UI Automation element list.".into(),
                ),
            );
        }
    }
    Ok(value)
}

pub(super) fn coerce_computer_use_max_elements(value: Option<&Value>) -> u64 {
    value
        .and_then(Value::as_u64)
        .filter(|value| *value >= 1)
        .unwrap_or(COMPUTER_USE_DEFAULT_MAX_ELEMENTS)
        .min(COMPUTER_USE_MAX_ALLOWED_ELEMENTS)
}

#[cfg(target_os = "macos")]
async fn computer_use_capture(store: &AppStore, run_id: &str, payload: &Value) -> AppResult<Value> {
    let mode = string_arg(payload, &["mode"]).unwrap_or_else(|| "som".into());
    let mode = match mode.trim().to_lowercase().as_str() {
        "vision" => "vision",
        "ax" => "ax",
        _ => "som",
    };
    let app_filter = string_arg(payload, &["app"]).unwrap_or_default();
    let timeout_seconds = computer_use_timeout_seconds(payload, 30);
    let list_raw = computer_use_cua_mcp_call_tool(
        "list_windows",
        json!({"on_screen_only": true}),
        timeout_seconds,
    )
    .await?;
    let mut windows = computer_use_cua_windows_from_result(&list_raw);
    windows.sort_by_key(|window| {
        window
            .get("z_index")
            .and_then(Value::as_i64)
            .unwrap_or(i64::MAX)
    });
    if !app_filter.trim().is_empty() {
        let needle = app_filter.to_lowercase();
        windows.retain(|window| {
            ["app_name", "appName", "processName", "title"]
                .iter()
                .filter_map(|key| window.get(*key).and_then(Value::as_str))
                .any(|value| value.to_lowercase().contains(&needle))
        });
    }
    let Some(target) = windows.first() else {
        return Ok(json!({
            "action": "capture",
            "ok": false,
            "backend": "cua-driver",
            "mode": mode,
            "app": app_filter,
            "error": "No on-screen window matched the requested app filter.",
            "hint": "Call computer_use action=list_apps to inspect macOS localized app names."
        }));
    };
    let pid = target
        .get("pid")
        .and_then(Value::as_i64)
        .ok_or_else(|| AppError::BadRequest("cua-driver target window missing pid".into()))?;
    let window_id = target
        .get("window_id")
        .or_else(|| target.get("windowId"))
        .and_then(Value::as_i64)
        .ok_or_else(|| AppError::BadRequest("cua-driver target window missing window_id".into()))?;

    let raw = if mode == "vision" {
        computer_use_cua_mcp_call_tool(
            "screenshot",
            json!({"window_id": window_id, "format": "jpeg", "quality": 85}),
            timeout_seconds,
        )
        .await?
    } else {
        computer_use_cua_mcp_call_tool(
            "get_window_state",
            json!({"pid": pid, "window_id": window_id}),
            timeout_seconds,
        )
        .await?
    };
    let text = computer_use_cua_text_from_result(&raw);
    let images = computer_use_cua_images_from_result(&raw);
    let image_path = if let Some(image_b64) = images.first() {
        let bytes = decode_base64_image(image_b64)?;
        let ext = computer_use_image_extension(&bytes);
        Some(store.save_tool_binary_artifact(run_id, "computer_use", ext, &bytes)?)
    } else {
        None
    };
    let elements = if mode == "vision" {
        Vec::new()
    } else {
        computer_use_cua_elements_from_tree(&text)
    };
    Ok(json!({
        "action": "capture",
        "ok": true,
        "backend": "cua-driver",
        "mode": mode,
        "app": target.get("app_name").or_else(|| target.get("appName")).cloned().unwrap_or(Value::Null),
        "windowTitle": target.get("title").cloned().unwrap_or(Value::Null),
        "pid": pid,
        "windowId": window_id,
        "screenshotPath": image_path.map(|path| path.to_string_lossy().to_string()),
        "elements": elements,
        "text": text,
        "raw": raw,
        "note": "macOS cua-driver MCP read-only capture path; element coordinates are pending full CUA parity."
    }))
}

#[cfg(all(not(windows), not(target_os = "macos")))]
async fn computer_use_capture(
    _store: &AppStore,
    _run_id: &str,
    _payload: &Value,
) -> AppResult<Value> {
    Err(AppError::BadRequest(
        "computer_use capture is only implemented on Windows in this build".into(),
    ))
}

#[cfg(windows)]
async fn computer_use_list_apps(payload: &Value) -> AppResult<Value> {
    let limit = payload
        .get("limit")
        .or_else(|| payload.get("max_elements"))
        .and_then(Value::as_u64)
        .unwrap_or(100)
        .clamp(1, 1000);
    let script = format!(
        r#"
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
Get-Process |
  Where-Object {{ $_.MainWindowTitle -and $_.MainWindowTitle.Trim().Length -gt 0 }} |
  Sort-Object ProcessName, Id |
  Select-Object -First {limit} @{{Name='processId';Expression={{$_.Id}}}}, @{{Name='processName';Expression={{$_.ProcessName}}}}, @{{Name='title';Expression={{$_.MainWindowTitle}}}} |
  ConvertTo-Json -Depth 4
"#
    );
    let stdout = run_powershell_script(&script, 10).await?;
    let apps = parse_powershell_json_array(stdout.trim())?;
    Ok(json!({
        "action": "list_apps",
        "ok": true,
        "apps": apps
    }))
}

#[cfg(target_os = "macos")]
async fn computer_use_list_apps(payload: &Value) -> AppResult<Value> {
    let timeout_seconds = computer_use_timeout_seconds(payload, 15);
    let raw = computer_use_cua_mcp_call_tool(
        "list_windows",
        json!({"on_screen_only": true}),
        timeout_seconds,
    )
    .await?;
    let windows = computer_use_cua_windows_from_result(&raw);
    Ok(json!({
        "action": "list_apps",
        "ok": true,
        "backend": "cua-driver",
        "apps": windows,
        "raw": raw
    }))
}

#[cfg(all(not(windows), not(target_os = "macos")))]
async fn computer_use_list_apps(_payload: &Value) -> AppResult<Value> {
    Err(AppError::BadRequest(
        "computer_use list_apps is only implemented on Windows in this build".into(),
    ))
}

#[cfg(windows)]
async fn computer_use_windows_action(
    store: &AppStore,
    run_id: &str,
    payload: &Value,
) -> AppResult<Value> {
    use base64::Engine;

    let payload = payload_with_resolved_computer_elements(run_id, payload)?;
    let action = computer_use_action(&payload)?;
    ensure_computer_use_safe(&action, &payload)?;
    if matches!(
        action.as_str(),
        "click" | "double_click" | "right_click" | "middle_click"
    ) {
        let _ = computer_use_coordinate(&payload, "coordinate")?;
    }
    if action == "drag" {
        let _ = computer_use_coordinate(&payload, "from_coordinate")?;
        let _ = computer_use_coordinate(&payload, "to_coordinate")?;
    }
    if action == "set_value" {
        if payload.get("value").is_none() {
            return Err(AppError::BadRequest(
                "computer_use action=set_value requires payload.value".into(),
            ));
        }
        let _ = computer_use_coordinate(&payload, "coordinate")?;
    }
    let payload_json = serde_json::to_string(&payload)?;
    let payload_b64 = base64::engine::general_purpose::STANDARD.encode(payload_json.as_bytes());
    let script_template = r#"
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName WindowsBase
Add-Type @"
using System;
using System.Runtime.InteropServices;
public class NativeInput {{
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int X, int Y);
  [DllImport("user32.dll")] public static extern void mouse_event(uint dwFlags, uint dx, uint dy, uint dwData, UIntPtr dwExtraInfo);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr hWnd, int nCmdShow);
}}
"@
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
$payloadJson = [System.Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('__PAYLOAD_B64__'))
$payload = $payloadJson | ConvertFrom-Json
$action = [string]$payload.action

function Get-Prop($name, $fallback = $null) {{
  if ($payload.PSObject.Properties.Name -contains $name) {{ return $payload.$name }}
  return $fallback
}}
function Get-Coord($name) {{
  $value = Get-Prop $name
  if ($null -eq $value -or $value.Count -lt 2) {{ throw "computer_use requires payload.$name [x,y]" }}
  return @([int]$value[0], [int]$value[1])
}}
function Move-To($coord) {{
  [NativeInput]::SetCursorPos([int]$coord[0], [int]$coord[1]) | Out-Null
  Start-Sleep -Milliseconds 80
}}
function Mouse-DownUp($down, $up) {{
  [NativeInput]::mouse_event($down, 0, 0, 0, [UIntPtr]::Zero)
  Start-Sleep -Milliseconds 40
  [NativeInput]::mouse_event($up, 0, 0, 0, [UIntPtr]::Zero)
}}
function Escape-SendKeysText($text) {{
  return ([string]$text).Replace('{{','{{{{}').Replace('}}','{{}}}').Replace('+','{{+}}').Replace('^','{{^}}').Replace('%','{{%}}').Replace('~','{{~}}').Replace('(','{{(}}').Replace(')','{{)}}').Replace('[','{{[}}').Replace(']','{{]}}')
}}
function Convert-KeyCombo($keys) {{
  $prefix = ''
  $body = ''
  foreach ($part in ([string]$keys).ToLowerInvariant().Split('+')) {{
    $p = $part.Trim()
    if ($p -eq 'ctrl' -or $p -eq 'control' -or $p -eq 'cmd' -or $p -eq 'command') {{ $prefix += '^'; continue }}
    if ($p -eq 'alt' -or $p -eq 'option') {{ $prefix += '%'; continue }}
    if ($p -eq 'shift') {{ $prefix += '+'; continue }}
    $map = @{{
      'enter'='{{ENTER}}'; 'return'='{{ENTER}}'; 'esc'='{{ESC}}'; 'escape'='{{ESC}}';
      'tab'='{{TAB}}'; 'space'=' '; 'backspace'='{{BACKSPACE}}'; 'delete'='{{DELETE}}';
      'up'='{{UP}}'; 'down'='{{DOWN}}'; 'left'='{{LEFT}}'; 'right'='{{RIGHT}}';
      'home'='{{HOME}}'; 'end'='{{END}}'; 'pageup'='{{PGUP}}'; 'pagedown'='{{PGDN}}'
    }}
    if ($map.ContainsKey($p)) {{ $body = $map[$p] }} else {{ $body = $p }}
  }}
  return $prefix + $body
}}
function Focus-App($query) {{
  if ([string]::IsNullOrWhiteSpace($query)) {{ throw "computer_use focus_app requires payload.app" }}
  $candidate = Get-Process | Where-Object {{ $_.MainWindowHandle -ne 0 -and ($_.ProcessName -like "*$query*" -or $_.MainWindowTitle -like "*$query*") }} | Select-Object -First 1
  if ($null -eq $candidate) {{ throw "No window matched app/title '$query'" }}
  [NativeInput]::ShowWindow($candidate.MainWindowHandle, 9) | Out-Null
  [NativeInput]::SetForegroundWindow($candidate.MainWindowHandle) | Out-Null
  Start-Sleep -Milliseconds 150
  return @{{
    processId = $candidate.Id
    processName = $candidate.ProcessName
    title = $candidate.MainWindowTitle
  }}
}}
function Set-ElementValue($coord, $value) {{
  $point = New-Object System.Windows.Point([double]$coord[0], [double]$coord[1])
  $element = [System.Windows.Automation.AutomationElement]::FromPoint($point)
  if ($null -eq $element) {{ throw "No UI Automation element at coordinate $($coord[0]),$($coord[1])" }}
  $pattern = $null
  if ($element.TryGetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern, [ref]$pattern)) {{
    $pattern.SetValue([string]$value)
    return @{{
      role = ($element.Current.ControlType.ProgrammaticName -replace '^ControlType\.','')
      label = [string]$element.Current.Name
      automationId = [string]$element.Current.AutomationId
      pattern = 'ValuePattern'
    }}
  }}
  $legacy = $null
  if ($element.TryGetCurrentPattern([System.Windows.Automation.LegacyIAccessiblePattern]::Pattern, [ref]$legacy)) {{
    $legacy.SetValue([string]$value)
    return @{{
      role = ($element.Current.ControlType.ProgrammaticName -replace '^ControlType\.','')
      label = [string]$element.Current.Name
      automationId = [string]$element.Current.AutomationId
      pattern = 'LegacyIAccessiblePattern'
    }}
  }}
  throw "Element at coordinate $($coord[0]),$($coord[1]) does not support ValuePattern or LegacyIAccessiblePattern"
}}

$details = @{{}}
if ($action -eq 'focus_app') {{
  $details.focused = Focus-App ([string](Get-Prop 'app'))
}} elseif ($action -in @('click','double_click','right_click','middle_click')) {{
  $coord = Get-Coord 'coordinate'
  Move-To $coord
  if ($action -eq 'right_click') {{ Mouse-DownUp 0x0008 0x0010 }}
  elseif ($action -eq 'middle_click') {{ Mouse-DownUp 0x0020 0x0040 }}
  elseif ($action -eq 'double_click') {{ Mouse-DownUp 0x0002 0x0004; Start-Sleep -Milliseconds 90; Mouse-DownUp 0x0002 0x0004 }}
  else {{ Mouse-DownUp 0x0002 0x0004 }}
  $details.coordinate = $coord
}} elseif ($action -eq 'drag') {{
  $from = Get-Coord 'from_coordinate'
  $to = Get-Coord 'to_coordinate'
  Move-To $from
  [NativeInput]::mouse_event(0x0002,0,0,0,[UIntPtr]::Zero)
  Start-Sleep -Milliseconds 100
  Move-To $to
  [NativeInput]::mouse_event(0x0004,0,0,0,[UIntPtr]::Zero)
  $details.from = $from
  $details.to = $to
}} elseif ($action -eq 'scroll') {{
  $direction = [string](Get-Prop 'direction' 'down')
  $amount = [int](Get-Prop 'amount' 3)
  $delta = 120 * [Math]::Max(1, [Math]::Abs($amount))
  if ($direction -eq 'down' -or $direction -eq 'right') {{ $delta = -$delta }}
  [NativeInput]::mouse_event(0x0800,0,0,[uint32]$delta,[UIntPtr]::Zero)
  $details.direction = $direction
  $details.amount = $amount
}} elseif ($action -eq 'type') {{
  $text = [string](Get-Prop 'text')
  if ([string]::IsNullOrEmpty($text)) {{ throw "computer_use action=type requires payload.text" }}
  [System.Windows.Forms.SendKeys]::SendWait((Escape-SendKeysText $text))
  $details.length = $text.Length
}} elseif ($action -eq 'key') {{
  $keys = [string](Get-Prop 'keys')
  if ([string]::IsNullOrWhiteSpace($keys)) {{ throw "computer_use action=key requires payload.keys" }}
  [System.Windows.Forms.SendKeys]::SendWait((Convert-KeyCombo $keys))
  $details.keys = $keys
}} elseif ($action -eq 'set_value') {{
  $value = Get-Prop 'value'
  if ($null -eq $value) {{ throw "computer_use action=set_value requires payload.value" }}
  $coord = Get-Coord 'coordinate'
  $details.coordinate = $coord
  $details.valueLength = ([string]$value).Length
  $details.element = Set-ElementValue $coord $value
}} else {{
  throw "unsupported action $action"
}}
@{{
  action = $action
  ok = $true
  details = $details
}} | ConvertTo-Json -Depth 8
"#;
    let script = script_template
        .replace("__PAYLOAD_B64__", &payload_b64)
        .replace("{{", "{")
        .replace("}}", "}");
    let stdout = run_powershell_script(&script, 15).await?;
    let mut result: Value = serde_json::from_str(stdout.trim()).map_err(|error| {
        AppError::BadRequest(format!(
            "computer_use action returned invalid JSON: {error}"
        ))
    })?;
    if payload
        .get("capture_after")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        let capture = computer_use_capture(store, run_id, &json!({"action": "capture"})).await?;
        if let Some(obj) = result.as_object_mut() {
            obj.insert("capture".into(), capture);
        }
    }
    Ok(result)
}

#[cfg(target_os = "macos")]
async fn computer_use_windows_action(
    store: &AppStore,
    run_id: &str,
    payload: &Value,
) -> AppResult<Value> {
    let action = computer_use_action(payload)?;
    ensure_computer_use_safe(&action, payload)?;
    let timeout_seconds = computer_use_timeout_seconds(payload, 30);
    let target = computer_use_cua_target_window(payload, timeout_seconds).await?;
    if action == "focus_app" {
        return Ok(json!({
            "action": "focus_app",
            "ok": true,
            "backend": "cua-driver",
            "message": format!(
                "Targeted {} (pid {}, window {}) without raising window.",
                target.app_name, target.pid, target.window_id
            ),
            "target": target.as_json()
        }));
    }
    let (tool_name, arguments) = computer_use_cua_action_call(&action, payload, &target)?;
    let raw = computer_use_cua_mcp_call_tool(&tool_name, arguments, timeout_seconds).await?;
    let mut result = computer_use_cua_action_result(&action, &tool_name, &raw, &target);
    if payload
        .get("capture_after")
        .or_else(|| payload.get("captureAfter"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        let mut capture_payload = json!({"action": "capture"});
        if !target.app_name.is_empty() {
            capture_payload["app"] = Value::String(target.app_name.clone());
        }
        let capture = computer_use_capture(store, run_id, &capture_payload).await?;
        if let Some(obj) = result.as_object_mut() {
            obj.insert("capture".into(), capture);
        }
    }
    Ok(result)
}

#[cfg(all(not(windows), not(target_os = "macos")))]
async fn computer_use_windows_action(
    _store: &AppStore,
    _run_id: &str,
    _payload: &Value,
) -> AppResult<Value> {
    Err(AppError::BadRequest(
        "computer_use desktop actions are only implemented on Windows in this build".into(),
    ))
}

#[cfg(windows)]
async fn run_powershell_script(script: &str, timeout_seconds: u64) -> AppResult<String> {
    let mut child = Command::new("powershell.exe");
    child.hide_window();
    child
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            script,
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let child = child.spawn()?;
    let output = match tokio::time::timeout(
        Duration::from_secs(timeout_seconds.max(1)),
        child.wait_with_output(),
    )
    .await
    {
        Ok(output) => output?,
        Err(_) => {
            return Err(AppError::BadRequest(format!(
                "computer_use command timed out after {timeout_seconds}s"
            )));
        }
    };
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    if output.status.success() {
        Ok(stdout)
    } else {
        Err(AppError::BadRequest(format!(
            "computer_use command failed: {}",
            truncate_output(&stderr, 4000)
        )))
    }
}

#[cfg(windows)]
fn parse_powershell_json_array(text: &str) -> AppResult<Value> {
    if text.trim().is_empty() {
        return Ok(json!([]));
    }
    let value: Value = serde_json::from_str(text).map_err(|error| {
        AppError::BadRequest(format!(
            "computer_use list_apps returned invalid JSON: {error}"
        ))
    })?;
    if value.is_array() {
        Ok(value)
    } else {
        Ok(json!([value]))
    }
}

fn payload_with_resolved_computer_elements(run_id: &str, payload: &Value) -> AppResult<Value> {
    let mut next = payload.clone();
    let Some(object) = next.as_object_mut() else {
        return Ok(next);
    };
    if let Some(element) = payload.get("element").and_then(Value::as_u64) {
        let center = cached_computer_element_center(run_id, element)?;
        object.insert("coordinate".into(), json!(center));
    }
    if let Some(element) = payload.get("from_element").and_then(Value::as_u64) {
        let center = cached_computer_element_center(run_id, element)?;
        object.insert("from_coordinate".into(), json!(center));
    }
    if let Some(element) = payload.get("to_element").and_then(Value::as_u64) {
        let center = cached_computer_element_center(run_id, element)?;
        object.insert("to_coordinate".into(), json!(center));
    }
    Ok(next)
}

fn cached_computer_element_center(run_id: &str, index: u64) -> AppResult<[i64; 2]> {
    let cache = COMPUTER_USE_ELEMENTS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .map_err(|_| AppError::BadRequest("computer_use element cache lock poisoned".into()))?;
    let elements = cache.get(run_id).ok_or_else(|| {
        AppError::BadRequest(
            "computer_use element target requires a prior capture with mode=som or mode=ax".into(),
        )
    })?;
    let element = elements
        .iter()
        .find(|element| element.get("index").and_then(Value::as_u64) == Some(index))
        .ok_or_else(|| {
            AppError::BadRequest(format!(
                "computer_use element #{index} not found in last capture"
            ))
        })?;
    if let Some(center) = element.get("center").and_then(Value::as_array) {
        if center.len() == 2 {
            if let (Some(x), Some(y)) = (center[0].as_i64(), center[1].as_i64()) {
                return Ok([x, y]);
            }
        }
    }
    let bounds = element
        .get("bounds")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            AppError::BadRequest(format!("computer_use element #{index} missing bounds"))
        })?;
    if bounds.len() != 4 {
        return Err(AppError::BadRequest(format!(
            "computer_use element #{index} bounds are invalid"
        )));
    }
    let x = bounds[0].as_i64().unwrap_or(0) + bounds[2].as_i64().unwrap_or(0) / 2;
    let y = bounds[1].as_i64().unwrap_or(0) + bounds[3].as_i64().unwrap_or(0) / 2;
    Ok([x, y])
}

pub(super) fn ensure_computer_use_safe(action: &str, payload: &Value) -> AppResult<()> {
    match action {
        "type" => {
            let text = payload.get("text").and_then(Value::as_str).unwrap_or("");
            if let Some(pattern) = blocked_computer_use_type_pattern(text) {
                return Err(AppError::BadRequest(format!(
                    "blocked pattern in computer_use type text: {pattern}; dangerous shell patterns cannot be typed via computer_use"
                )));
            }
        }
        "key" => {
            let keys = payload.get("keys").and_then(Value::as_str).unwrap_or("");
            let combo = canonical_computer_use_key_combo(keys);
            for blocked in blocked_computer_use_key_combos() {
                if blocked
                    .iter()
                    .all(|part| combo.iter().any(|item| item == part))
                {
                    return Err(AppError::BadRequest(format!(
                        "blocked computer_use key combo: {}",
                        blocked.join("+")
                    )));
                }
            }
        }
        _ => {}
    }
    Ok(())
}

fn blocked_computer_use_type_pattern(text: &str) -> Option<&'static str> {
    let lower = text.to_lowercase();
    let compact = lower.split_whitespace().collect::<Vec<_>>().join(" ");
    let pipe_to_shell = |program: &str, shell: &str| -> bool {
        compact.contains(program)
            && (compact.contains(&format!("| {shell}")) || compact.contains(&format!("|{shell}")))
    };
    if pipe_to_shell("curl ", "bash") {
        return Some("curl ... | bash");
    }
    if pipe_to_shell("curl ", "sh") {
        return Some("curl ... | sh");
    }
    if pipe_to_shell("wget ", "bash") {
        return Some("wget ... | bash");
    }
    if compact.contains("sudo rm -r") || compact.contains("sudo rm -f") {
        return Some("sudo rm -[rf]");
    }
    if compact.trim_end() == "rm -rf /" || compact.contains(" rm -rf /") {
        return Some("rm -rf /");
    }
    let fork_compact = lower
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .collect::<String>();
    if fork_compact.contains(":(){:|:&}") {
        return Some("fork bomb");
    }
    None
}

fn canonical_computer_use_key_combo(keys: &str) -> Vec<String> {
    keys.split('+')
        .filter_map(|part| {
            let key = part.trim().to_lowercase();
            if key.is_empty() {
                return None;
            }
            let canonical = match key.as_str() {
                "command" | "cmd" | "win" | "windows" | "meta" | "⌘" => "cmd",
                "control" | "ctrl" => "ctrl",
                "alt" | "option" | "⌥" => "option",
                other => other,
            };
            Some(canonical.to_string())
        })
        .collect()
}

fn blocked_computer_use_key_combos() -> Vec<Vec<&'static str>> {
    vec![
        vec!["cmd", "shift", "backspace"],
        vec!["cmd", "option", "backspace"],
        vec!["cmd", "ctrl", "q"],
        vec!["cmd", "shift", "q"],
        vec!["cmd", "option", "shift", "q"],
        vec!["ctrl", "shift", "backspace"],
        vec!["ctrl", "option", "backspace"],
        vec!["ctrl", "shift", "q"],
        vec!["option", "f4"],
    ]
}

pub(super) fn computer_use_coordinate(payload: &Value, key: &str) -> AppResult<(i32, i32)> {
    let value = payload
        .get(key)
        .ok_or_else(|| AppError::BadRequest(format!("computer_use requires payload.{key}")))?;
    let array = value
        .as_array()
        .ok_or_else(|| AppError::BadRequest(format!("computer_use payload.{key} must be [x,y]")))?;
    if array.len() != 2 {
        return Err(AppError::BadRequest(format!(
            "computer_use payload.{key} must contain exactly two numbers"
        )));
    }
    let x = array[0].as_i64().ok_or_else(|| {
        AppError::BadRequest(format!("computer_use payload.{key}[0] must be an integer"))
    })?;
    let y = array[1].as_i64().ok_or_else(|| {
        AppError::BadRequest(format!("computer_use payload.{key}[1] must be an integer"))
    })?;
    Ok((x as i32, y as i32))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn computer_use_status_actions_are_supported() {
        assert_eq!(
            computer_use_action(&json!({"action": "STATUS"})).unwrap(),
            "status"
        );
        assert_eq!(
            computer_use_action(&json!({"action": "capabilities"})).unwrap(),
            "capabilities"
        );
        assert_eq!(
            computer_use_action(&json!({"action": "requirements"})).unwrap(),
            "requirements"
        );
        assert_eq!(
            computer_use_action(&json!({"action": "setup_schema"})).unwrap(),
            "setup_schema"
        );
        assert_eq!(
            computer_use_action(&json!({"action": "backend_status"})).unwrap(),
            "backend_status"
        );
        assert_eq!(
            computer_use_action(&json!({"action": "reset_backend"})).unwrap(),
            "reset_backend"
        );
        assert_eq!(
            computer_use_action(&json!({"action": "session_status"})).unwrap(),
            "session_status"
        );
        assert_eq!(
            computer_use_action(&json!({"action": "mcp_session_status"})).unwrap(),
            "mcp_session_status"
        );
        assert_eq!(
            computer_use_action(&json!({"action": "mcp_probe"})).unwrap(),
            "mcp_probe"
        );
    }

    #[test]
    fn computer_use_status_reports_backend_and_hermes_reference() {
        let status = computer_use_status(&json!({"action": "status"})).unwrap();

        assert_eq!(status["action"].as_str(), Some("status"));
        assert_eq!(
            status["hermesParity"]["referenceBackend"].as_str(),
            Some("cua-driver MCP")
        );
        assert!(status["safeActions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value.as_str() == Some("status")));
        assert!(status["mutatingActions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value.as_str() == Some("click")));
        assert_eq!(
            status["lifecycle"]["hermesReference"]["stdioMcpClientImplemented"].as_bool(),
            Some(true)
        );
        assert_eq!(
            status["lifecycle"]["stdioMcpClientImplemented"].as_bool(),
            Some(cfg!(target_os = "macos"))
        );
        assert_eq!(
            status["lifecycle"]["oneShotMcpClientImplemented"].as_bool(),
            Some(cfg!(target_os = "macos"))
        );
        assert_eq!(
            status["lifecycle"]["stdioMcpProbeImplemented"].as_bool(),
            Some(true)
        );
        assert_eq!(
            status["lifecycle"]["lifecycleDiagnostics"]["persistentSession"].as_bool(),
            Some(cfg!(target_os = "macos"))
        );
        assert!(status["lifecycle"].get("activePersistentSession").is_some());
        assert_eq!(
            status["hermesParity"]["backgroundInputContract"]["schema"].as_str(),
            Some("synthchat_computer_use_background_input_contract_v1")
        );
        assert_eq!(
            status["hermesParity"]["backgroundInputContract"]["reference"]["schema"].as_str(),
            Some("hermes_computer_use_background_input_contract_v1")
        );
        assert_eq!(
            status["hermesParity"]["backgroundInputContract"]["approvalRequiredForMutatingActions"]
                .as_bool(),
            Some(true)
        );
        assert!(
            status["hermesParity"]["backgroundInputContract"]["hardBlockedSafetyRules"]
                .as_array()
                .unwrap()
                .iter()
                .any(|value| value.as_str() == Some("dangerous_shell_text_patterns"))
        );
    }

    #[test]
    fn computer_use_mcp_session_status_reports_lifecycle() {
        let status = computer_use_mcp_session_status().unwrap();

        assert_eq!(status["action"].as_str(), Some("mcp_session_status"));
        assert_eq!(status["ok"].as_bool(), Some(true));
        assert_eq!(
            status["persistentSessionImplemented"].as_bool(),
            Some(cfg!(target_os = "macos"))
        );
        assert!(status.get("activePersistentSession").is_some());
        assert!(status.get("lifecycleDiagnostics").is_some());
        assert_eq!(
            status["hermesReference"]["persistentSession"].as_bool(),
            Some(true)
        );
        assert_eq!(
            status["hermesReference"]["backgroundInputContract"]["defaultRaiseWindow"].as_bool(),
            Some(false)
        );
    }

    #[test]
    fn computer_use_setup_schema_reports_cua_driver_background_input_contract() {
        let schema = computer_use_setup_schema().unwrap();

        assert_eq!(schema["action"].as_str(), Some("setup_schema"));
        assert_eq!(
            schema["backgroundInputContract"]["schema"].as_str(),
            Some("hermes_computer_use_background_input_contract_v1")
        );
        let options = schema["backendOptions"].as_array().unwrap();
        let cua = options
            .iter()
            .find(|item| item["id"].as_str() == Some("cua-driver"))
            .expect("setup schema should include cua-driver backend");
        assert_eq!(cua["transport"].as_str(), Some("MCP stdio"));
        assert_eq!(cua["args"][0].as_str(), Some("mcp"));
        assert_eq!(cua["probeAction"]["action"].as_str(), Some("mcp_probe"));
        assert_eq!(
            cua["implemented"].as_bool(),
            Some(cfg!(target_os = "macos"))
        );
        assert_eq!(
            cua["persistentSessionImplemented"].as_bool(),
            Some(cfg!(target_os = "macos"))
        );
        assert_eq!(
            cua["oneShotMcpClientImplemented"].as_bool(),
            Some(cfg!(target_os = "macos"))
        );
        assert_eq!(
            cua["backgroundInputContract"]["focusWithoutRaiseDefault"].as_bool(),
            Some(true)
        );
        assert!(cua["backgroundInputContract"]["mutatingActions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value.as_str() == Some("set_value")));
    }

    #[test]
    fn computer_use_reset_backend_clears_cua_lifecycle_stats() {
        computer_use_cua_mcp_record_start("list_windows", false);
        computer_use_cua_mcp_record_finish(None);

        let before = computer_use_cua_mcp_lifecycle_snapshot();
        assert_eq!(before["toolCalls"].as_u64(), Some(1));

        let reset = computer_use_reset_backend().unwrap();
        assert_eq!(reset["reset"]["performed"].as_bool(), Some(true));

        let after = computer_use_cua_mcp_lifecycle_snapshot();
        assert_eq!(after["toolCalls"].as_u64(), Some(0));
    }

    #[test]
    fn computer_use_requirements_reports_hermes_reference_install() {
        let requirements = computer_use_requirements().unwrap();

        assert_eq!(requirements["action"].as_str(), Some("requirements"));
        assert_eq!(
            requirements["requirements"]["hermesReference"]["backend"].as_str(),
            Some("cua-driver")
        );
        assert_eq!(
            requirements["requirements"]["hermesReference"]["args"][0].as_str(),
            Some("mcp")
        );
        assert_eq!(
            requirements["requirements"]["hermesReference"]["probeAction"]["action"].as_str(),
            Some("mcp_probe")
        );
        assert_eq!(
            requirements["requirements"]["synthchat"]["backgroundInputContract"]["schema"].as_str(),
            Some("synthchat_computer_use_background_input_contract_v1")
        );
    }
}
