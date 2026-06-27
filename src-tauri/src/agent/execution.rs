use std::{
    collections::{HashMap, HashSet},
    env, fs, io,
    path::{Path, PathBuf},
    process::{Command as StdCommand, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, OnceLock,
    },
    time::{Duration, Instant},
};

use serde_json::Value;
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Emitter};
use tokio::{
    io::AsyncWriteExt,
    io::{AsyncBufReadExt, BufReader},
    net::{TcpListener, TcpStream},
    process::Command,
};

use crate::{
    error::{AppError, AppResult},
    models::{new_id, now_iso, AgentDefinition, ChatMessage},
    process_utils::CommandWindowExt,
    store::{AppStore, ManagedProcess, ManagedProcessNotificationState},
};

use super::{
    command_guard::{ensure_command_not_hardline, ensure_shell_allowed},
    file_tools::{patch_tool, read_file_tool, search_files_tool, write_file_tool},
    positive_or_default,
    redact::{is_sensitive_env_name, redact_sensitive_text},
    run_transform_terminal_output_hooks, truncate_output,
    web_tools::{web_extract_tool, web_search_tool},
    workspace::{resolve_workspace_path, workspace_root},
};
static TERMINAL_SESSION_CWDS: OnceLock<Mutex<HashMap<String, PathBuf>>> = OnceLock::new();
static SSH_TERMINAL_SESSION_CWDS: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
static SSH_SYNCED_REMOTE_PATHS: OnceLock<Mutex<HashMap<String, HashSet<String>>>> = OnceLock::new();
static SSH_PUSHED_HASHES: OnceLock<Mutex<HashMap<String, HashMap<String, String>>>> =
    OnceLock::new();
static MODAL_TERMINAL_SESSION_CWDS: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
static MODAL_TERMINAL_SNAPSHOTS: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
static DAYTONA_TERMINAL_SESSION_CWDS: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
static DOCKER_TERMINAL_CONTAINERS: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
static DOCKER_ORPHAN_REAPER_RAN: OnceLock<Mutex<bool>> = OnceLock::new();
static PROCESS_WATCH_GLOBAL_LIMITER: OnceLock<Mutex<ProcessWatchGlobalLimiter>> = OnceLock::new();
static DETACHED_PROCESS_WATCHERS: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

const DOCKER_LABEL_AGENT: &str = "synthchat-agent";
const DOCKER_LABEL_PROFILE: &str = "synthchat-profile";
const DOCKER_LABEL_KEY: &str = "synthchat-key";
const DOCKER_LABEL_WORKSPACE: &str = "synthchat-workspace";
const DOCKER_LABEL_SESSION: &str = "synthchat-session";
const WATCH_GLOBAL_MAX_PER_WINDOW: u32 = 15;
const WATCH_GLOBAL_WINDOW: Duration = Duration::from_secs(10);
const WATCH_GLOBAL_COOLDOWN: Duration = Duration::from_secs(30);

#[derive(Default)]
struct ProcessWatchGlobalLimiter {
    window_start: Option<Instant>,
    window_hits: u32,
    tripped_until: Option<Instant>,
}

pub(super) async fn terminal_tool(
    store: &AppStore,
    agent: &AgentDefinition,
    payload: &Value,
) -> AppResult<String> {
    ensure_shell_allowed(agent)?;
    let command = payload
        .get("command")
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::BadRequest("terminal requires payload.command".into()))?;
    let timeout_seconds = terminal_timeout_seconds(payload)?;
    let chat_config = store.config().map(|config| config.chat).unwrap_or_default();
    let max_output_chars = positive_or_default(chat_config.tool_output_max_bytes, 50_000);
    let allowed_env = tool_env_passthrough(store, Some(agent), &chat_config.tool_env_passthrough);
    let stdin_data = terminal_stdin_data(payload);
    let env_type = normalized_terminal_env();
    if env_type == "ssh" {
        return run_ssh_terminal_command(
            store,
            command,
            payload,
            timeout_seconds,
            max_output_chars,
            stdin_data.as_deref(),
        )
        .await;
    }
    let cwd = workspace_cwd(agent, payload.get("cwd").or_else(|| payload.get("workdir")))?;
    if env_type == "docker" {
        let docker_cwd = if payload
            .get("cwd")
            .or_else(|| payload.get("workdir"))
            .is_none()
        {
            terminal_session_id(payload)
                .and_then(|session_id| terminal_session_cwd(&session_id))
                .unwrap_or_else(|| cwd.clone())
        } else {
            cwd.clone()
        };
        return run_docker_terminal_command(
            store,
            agent,
            command,
            &docker_cwd,
            payload,
            timeout_seconds,
            max_output_chars,
            stdin_data.as_deref(),
        )
        .await;
    }
    if env_type == "singularity" {
        let singularity_cwd = if payload
            .get("cwd")
            .or_else(|| payload.get("workdir"))
            .is_none()
        {
            terminal_session_id(payload)
                .and_then(|session_id| terminal_session_cwd(&session_id))
                .unwrap_or_else(|| cwd.clone())
        } else {
            cwd.clone()
        };
        return run_singularity_terminal_command(
            store,
            agent,
            command,
            &singularity_cwd,
            payload,
            timeout_seconds,
            max_output_chars,
            stdin_data.as_deref(),
        )
        .await;
    }
    if env_type == "modal" {
        return run_modal_terminal_command(
            store,
            command,
            payload,
            timeout_seconds,
            max_output_chars,
            stdin_data.as_deref(),
        )
        .await;
    }
    if env_type == "daytona" {
        return run_daytona_terminal_command(
            store,
            command,
            payload,
            timeout_seconds,
            max_output_chars,
            stdin_data.as_deref(),
        )
        .await;
    }
    if env_type != "local" {
        return Err(AppError::BadRequest(format!(
            "terminal does not yet support TERMINAL_ENV={env_type}; supported backends are local, docker, ssh, singularity, modal, and daytona"
        )));
    }
    if payload
        .get("cwd")
        .or_else(|| payload.get("workdir"))
        .is_none()
    {
        if let Some(session_id) = terminal_session_id(payload) {
            let root = workspace_root(agent)?;
            let session_cwd = terminal_session_cwd(&session_id).unwrap_or(cwd);
            return run_shell_command_with_cwd_capture(
                store,
                shell_hook_run_id(payload, "terminal"),
                command,
                &session_cwd,
                &root,
                &session_id,
                timeout_seconds,
                max_output_chars,
                &allowed_env,
                stdin_data.as_deref(),
            )
            .await;
        }
    }
    run_shell_command(
        store,
        shell_hook_run_id(payload, "terminal"),
        command,
        &cwd,
        timeout_seconds,
        max_output_chars,
        &allowed_env,
        stdin_data.as_deref(),
    )
    .await
}

pub(super) fn terminal_background_requested(payload: &Value) -> bool {
    value_bool_like(payload, &["background", "backgroundProcess", "background_process", "bg"])
        .unwrap_or(false)
}

pub(super) fn terminal_timeout_seconds(payload: &Value) -> AppResult<u64> {
    let explicit_timeout =
        value_u64_like(payload, &["timeoutSeconds", "timeout"]);
    let max_foreground = env_u64("TERMINAL_MAX_FOREGROUND_TIMEOUT", 600).max(1);
    if let Some(timeout) = explicit_timeout {
        if terminal_background_requested(payload) {
            return Ok(timeout.max(1));
        }
        if timeout > max_foreground {
            return Err(AppError::BadRequest(format!(
                "Foreground timeout {timeout}s exceeds the maximum of {max_foreground}s. Use background=true with notify_on_complete=true for longer commands."
            )));
        }
        return Ok(timeout.max(1));
    }
    Ok(env_u64("TERMINAL_TIMEOUT", 180).max(1))
}

pub(super) async fn execute_code_tool(
    store: &AppStore,
    agent: &AgentDefinition,
    payload: &Value,
) -> AppResult<String> {
    ensure_shell_allowed(agent)?;
    let code = payload
        .get("code")
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::BadRequest("execute_code requires payload.code".into()))?;
    let language = payload
        .get("language")
        .and_then(Value::as_str)
        .unwrap_or("python")
        .to_lowercase();
    let timeout_seconds = payload
        .get("timeoutSeconds")
        .or_else(|| payload.get("timeout"))
        .and_then(Value::as_u64)
        .unwrap_or(60)
        .clamp(1, 600);
    let env_type = normalized_terminal_env();
    if matches!(
        env_type.as_str(),
        "docker" | "ssh" | "singularity" | "modal" | "daytona"
    ) {
        if matches!(language.as_str(), "python" | "py") {
            return execute_code_with_remote_python_file_rpc(
                store,
                agent,
                payload,
                code,
                timeout_seconds,
            )
            .await;
        }
        let command = execute_code_backend_command(&language, code)?;
        let mut terminal_payload = serde_json::json!({
            "command": command,
            "timeoutSeconds": timeout_seconds,
        });
        if let Some(cwd) = payload.get("cwd").or_else(|| payload.get("workdir")) {
            terminal_payload["cwd"] = cwd.clone();
        }
        if let Some(task_id) = payload.get("taskId").or_else(|| payload.get("task_id")) {
            terminal_payload["taskId"] = task_id.clone();
        }
        if let Some(session_id) = payload
            .get("sessionId")
            .or_else(|| payload.get("session_id"))
        {
            terminal_payload["sessionId"] = session_id.clone();
        }
        return terminal_tool(store, agent, &terminal_payload).await;
    }
    if env_type != "local" {
        return Err(AppError::BadRequest(format!(
            "execute_code does not yet support TERMINAL_ENV={env_type}; supported backends are local, docker, ssh, singularity, modal, and daytona"
        )));
    }
    if matches!(language.as_str(), "python" | "py") {
        return execute_code_with_local_python_rpc(store, agent, payload, code, timeout_seconds)
            .await;
    }
    let root = workspace_root(agent)?;
    let scratch = root.join(".synthchat").join("tmp");
    fs::create_dir_all(&scratch)?;
    let (extension, runner) = match language.as_str() {
        "python" | "py" => ("py", "python"),
        "javascript" | "js" | "node" => ("js", "node"),
        "powershell" | "pwsh" | "ps1" => ("ps1", "powershell"),
        other => {
            return Err(AppError::BadRequest(format!(
                "unsupported execute_code language: {other}"
            )));
        }
    };
    let path = scratch.join(format!("execute-{}.{}", new_id("code"), extension));
    fs::write(&path, code)?;
    let command = if extension == "ps1" {
        format!(
            "{} -NoProfile -ExecutionPolicy Bypass -File \"{}\"",
            runner,
            path.display()
        )
    } else {
        format!("{} \"{}\"", runner, path.display())
    };
    let chat_config = store.config()?.chat;
    let allowed_env = tool_env_passthrough(store, Some(agent), &chat_config.tool_env_passthrough);
    let max_output_chars = positive_or_default(chat_config.tool_output_max_bytes, 50_000);
    let result = run_shell_command_unchecked(
        store,
        shell_hook_run_id(payload, "execute_code"),
        &command,
        &root,
        timeout_seconds,
        max_output_chars,
        &allowed_env,
        None,
    )
    .await;
    let _ = fs::remove_file(&path);
    result
}

async fn execute_code_with_local_python_rpc(
    store: &AppStore,
    agent: &AgentDefinition,
    payload: &Value,
    code: &str,
    timeout_seconds: u64,
) -> AppResult<String> {
    let root = workspace_root(agent)?;
    let cwd = workspace_cwd(agent, payload.get("cwd").or_else(|| payload.get("workdir")))?;
    let scratch = root.join(".synthchat").join("tmp").join(new_id("ptc"));
    fs::create_dir_all(&scratch)?;
    let script_path = scratch.join("execute.py");
    let tools_path = scratch.join("hermes_tools.py");
    fs::write(&script_path, code)?;
    fs::write(&tools_path, synthchat_hermes_tools_module())?;

    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("tcp://{}", listener.local_addr()?);
    let server_store = store.clone();
    let server_agent = agent.clone();
    let server_task = tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let store = server_store.clone();
            let agent = server_agent.clone();
            tokio::spawn(async move {
                let _ = handle_execute_code_rpc_stream(stream, store, agent).await;
            });
        }
    });

    let python = find_executable("python")
        .or_else(|| find_executable("python3"))
        .or_else(common_python_path)
        .ok_or_else(|| AppError::BadRequest("execute_code requires python".into()))?;
    let chat_config = store.config()?.chat;
    let allowed_env = tool_env_passthrough(store, Some(agent), &chat_config.tool_env_passthrough);
    let max_output_chars = positive_or_default(chat_config.tool_output_max_bytes, 50_000);
    let mut child = Command::new(&python);
    child.hide_window();
    apply_command_env_guard(&mut child, &allowed_env);
    child
        .arg(&script_path)
        .current_dir(&cwd)
        .env("HERMES_RPC_SOCKET", endpoint)
        .env("SYNTHCHAT_RPC_SOCKET", "1")
        .env("PYTHONPATH", pythonpath_with_prepend(&scratch))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let run_id = shell_hook_run_id(payload, "execute_code");
    let output =
        match wait_for_shell_output_interruptible(store, run_id, timeout_seconds, child.spawn()?)
            .await
        {
            Ok(output) => output,
            Err(error) => {
                server_task.abort();
                let _ = fs::remove_dir_all(&scratch);
                let error_text = error.to_string();
                if error_text.contains("command timed out after") {
                    return Err(AppError::BadRequest(format!(
                        "execute_code timed out after {timeout_seconds}s"
                    )));
                }
                return Err(error);
            }
        };
    server_task.abort();
    let _ = fs::remove_dir_all(&scratch);
    let stdout = decode_terminal_output(&output.stdout);
    let stderr = decode_terminal_output(&output.stderr);
    let stdout = execute_code_sanitize_output(&stdout);
    let stderr = execute_code_sanitize_output(&stderr);
    Ok(format!(
        "cwd: {}\ntransport: hermes_tools_rpc\nexitCode: {}\nstdout:\n{}\nstderr:\n{}",
        cwd.display(),
        output.status.code().unwrap_or(-1),
        truncate_output(&stdout, max_output_chars),
        truncate_output(&stderr, max_output_chars / 2)
    ))
}

async fn execute_code_with_remote_python_file_rpc(
    store: &AppStore,
    agent: &AgentDefinition,
    payload: &Value,
    code: &str,
    timeout_seconds: u64,
) -> AppResult<String> {
    let chat_config = store.config()?.chat;
    let max_output_chars = positive_or_default(chat_config.tool_output_max_bytes, 50_000);
    let run_id = shell_hook_run_id(payload, "execute_code");
    let sandbox_id = new_id("ptc");
    let sandbox_dir = format!("/tmp/synthchat_exec_{sandbox_id}");
    let rpc_dir = format!("{sandbox_dir}/rpc");
    let session_id = payload
        .get("taskId")
        .or_else(|| payload.get("task_id"))
        .or_else(|| payload.get("sessionId"))
        .or_else(|| payload.get("session_id"))
        .cloned();

    remote_terminal_command_for_run(
        store,
        agent,
        &format!("mkdir -p {}", posix_shell_quote(&rpc_dir)),
        30,
        session_id.as_ref(),
        Some(run_id),
    )
    .await?;
    ship_remote_execute_code_file(
        store,
        agent,
        &format!("{sandbox_dir}/hermes_tools.py"),
        synthchat_hermes_tools_file_module(),
        session_id.as_ref(),
        Some(run_id),
    )
    .await?;
    ship_remote_execute_code_file(
        store,
        agent,
        &format!("{sandbox_dir}/execute.py"),
        code,
        session_id.as_ref(),
        Some(run_id),
    )
    .await?;

    let stop = Arc::new(AtomicBool::new(false));
    let poll_task = tokio::spawn(remote_execute_code_rpc_poll_loop(
        store.clone(),
        agent.clone(),
        rpc_dir.clone(),
        session_id.clone(),
        Some(run_id.to_string()),
        stop.clone(),
    ));
    let script_command = format!(
        "cd {} && HERMES_RPC_DIR={} PYTHONDONTWRITEBYTECODE=1 python3 execute.py",
        posix_shell_quote(&sandbox_dir),
        posix_shell_quote(&rpc_dir)
    );
    let script_result = remote_terminal_command_for_run(
        store,
        agent,
        &script_command,
        timeout_seconds,
        session_id.as_ref(),
        Some(run_id),
    )
    .await;
    stop.store(true, Ordering::SeqCst);
    let _ = poll_task.await;
    let _ = remote_terminal_command_for_run(
        store,
        agent,
        &format!("rm -rf {}", posix_shell_quote(&sandbox_dir)),
        30,
        session_id.as_ref(),
        Some(run_id),
    )
    .await;

    let output = script_result?;
    let stdout = remote_terminal_stdout(&output).unwrap_or(output.as_str());
    let stderr = remote_terminal_stderr(&output).unwrap_or("");
    let stdout = execute_code_sanitize_output(stdout);
    let stderr = execute_code_sanitize_output(stderr);
    Ok(format!(
        "transport: hermes_tools_file_rpc\n{}\nstdout:\n{}\nstderr:\n{}",
        remote_terminal_exit_code_line(&output),
        truncate_output(&stdout, max_output_chars),
        truncate_output(&stderr, max_output_chars / 2)
    ))
}

async fn remote_execute_code_rpc_poll_loop(
    store: AppStore,
    agent: AgentDefinition,
    rpc_dir: String,
    session_id: Option<Value>,
    run_id: Option<String>,
    stop: Arc<AtomicBool>,
) {
    let mut tool_calls = 0u32;
    while !stop.load(Ordering::SeqCst) {
        let list_command = format!(
            "find {} -maxdepth 1 -type f -name 'req_*' ! -name '*.tmp' -print 2>/dev/null | sort",
            posix_shell_quote(&rpc_dir)
        );
        if let Ok(output) = remote_terminal_command_for_run(
            &store,
            &agent,
            &list_command,
            10,
            session_id.as_ref(),
            run_id.as_deref(),
        )
        .await
        {
            if let Some(stdout) = remote_terminal_stdout(&output) {
                for request_path in stdout
                    .lines()
                    .map(str::trim)
                    .filter(|line| !line.is_empty())
                {
                    if tool_calls >= 50 {
                        let _ = remote_terminal_command_for_run(
                            &store,
                            &agent,
                            &format!("rm -f {} 2>/dev/null", posix_shell_quote(request_path)),
                            10,
                            session_id.as_ref(),
                            run_id.as_deref(),
                        )
                        .await;
                        let response = serde_json::json!({"error": "execute_code RPC tool call limit exceeded"});
                        let _ = write_remote_execute_code_response(
                            &store,
                            &agent,
                            &rpc_dir,
                            request_path,
                            &response,
                            session_id.as_ref(),
                            run_id.as_deref(),
                        )
                        .await;
                        continue;
                    }
                    let read_command = format!(
                        "cat {} 2>/dev/null; rm -f {} 2>/dev/null",
                        posix_shell_quote(request_path),
                        posix_shell_quote(request_path)
                    );
                    let Ok(request_output) = remote_terminal_command_for_run(
                        &store,
                        &agent,
                        &read_command,
                        10,
                        session_id.as_ref(),
                        run_id.as_deref(),
                    )
                    .await
                    else {
                        continue;
                    };
                    let request_body = remote_terminal_stdout(&request_output).unwrap_or("");
                    if request_body.trim().is_empty() {
                        continue;
                    }
                    tool_calls += 1;
                    let response = execute_code_rpc_request(&store, &agent, request_body).await;
                    let _ = write_remote_execute_code_response(
                        &store,
                        &agent,
                        &rpc_dir,
                        request_path,
                        &response,
                        session_id.as_ref(),
                        run_id.as_deref(),
                    )
                    .await;
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn write_remote_execute_code_response(
    store: &AppStore,
    agent: &AgentDefinition,
    rpc_dir: &str,
    request_path: &str,
    response: &Value,
    session_id: Option<&Value>,
    run_id: Option<&str>,
) -> AppResult<()> {
    let seq = request_path
        .rsplit('/')
        .next()
        .and_then(|name| name.strip_prefix("req_"))
        .unwrap_or("000000");
    let response_path = format!("{rpc_dir}/res_{seq}");
    ship_remote_execute_code_file_atomic(
        store,
        agent,
        &response_path,
        &response.to_string(),
        session_id,
        run_id,
    )
    .await
}

async fn ship_remote_execute_code_file(
    store: &AppStore,
    agent: &AgentDefinition,
    remote_path: &str,
    content: &str,
    session_id: Option<&Value>,
    run_id: Option<&str>,
) -> AppResult<()> {
    use base64::Engine;
    let encoded = base64::engine::general_purpose::STANDARD.encode(content.as_bytes());
    let parent = remote_path
        .rsplit_once('/')
        .map(|(parent, _)| parent)
        .unwrap_or("/tmp");
    let command = format!(
        "mkdir -p {} && printf %s {} | base64 -d > {}",
        posix_shell_quote(parent),
        posix_shell_quote(&encoded),
        posix_shell_quote(remote_path)
    );
    remote_terminal_command_for_run(store, agent, &command, 30, session_id, run_id).await?;
    Ok(())
}

async fn ship_remote_execute_code_file_atomic(
    store: &AppStore,
    agent: &AgentDefinition,
    remote_path: &str,
    content: &str,
    session_id: Option<&Value>,
    run_id: Option<&str>,
) -> AppResult<()> {
    use base64::Engine;
    let encoded = base64::engine::general_purpose::STANDARD.encode(content.as_bytes());
    let tmp_path = format!("{remote_path}.tmp");
    let parent = remote_path
        .rsplit_once('/')
        .map(|(parent, _)| parent)
        .unwrap_or("/tmp");
    let command = format!(
        "mkdir -p {} && printf %s {} | base64 -d > {} && mv {} {}",
        posix_shell_quote(parent),
        posix_shell_quote(&encoded),
        posix_shell_quote(&tmp_path),
        posix_shell_quote(&tmp_path),
        posix_shell_quote(remote_path)
    );
    remote_terminal_command_for_run(store, agent, &command, 30, session_id, run_id).await?;
    Ok(())
}

pub(super) async fn remote_terminal_command_for_run(
    store: &AppStore,
    agent: &AgentDefinition,
    command: &str,
    timeout_seconds: u64,
    session_id: Option<&Value>,
    run_id: Option<&str>,
) -> AppResult<String> {
    let mut payload = serde_json::json!({
        "command": command,
        "timeoutSeconds": timeout_seconds.clamp(1, 600),
    });
    if let Some(session_id) = session_id {
        payload["taskId"] = session_id.clone();
        payload["sessionId"] = session_id.clone();
    }
    if let Some(run_id) = run_id.map(str::trim).filter(|value| !value.is_empty()) {
        payload["runId"] = serde_json::json!(run_id);
        payload["run_id"] = serde_json::json!(run_id);
    }
    terminal_tool(store, agent, &payload).await
}

fn remote_terminal_stdout(output: &str) -> Option<&str> {
    let (_, rest) = output.split_once("\nstdout:\n")?;
    Some(
        rest.split_once("\nstderr:\n")
            .map(|(stdout, _)| stdout)
            .unwrap_or(rest),
    )
}

fn remote_terminal_stderr(output: &str) -> Option<&str> {
    output.split_once("\nstderr:\n").map(|(_, stderr)| stderr)
}

fn remote_terminal_exit_code_line(output: &str) -> String {
    output
        .lines()
        .find(|line| line.starts_with("exitCode:"))
        .unwrap_or("exitCode: unknown")
        .to_string()
}

fn execute_code_sanitize_output(value: &str) -> String {
    redact_sensitive_text(&strip_ansi_escape_codes(value))
}

fn strip_ansi_escape_codes(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for next in chars.by_ref() {
                if ('@'..='~').contains(&next) {
                    break;
                }
            }
            continue;
        }
        output.push(ch);
    }
    output
}

fn pythonpath_with_prepend(path: &Path) -> String {
    let mut paths = vec![path.to_path_buf()];
    if let Some(existing) = env::var_os("PYTHONPATH") {
        paths.extend(env::split_paths(&existing));
    }
    env::join_paths(paths)
        .map(|value| value.to_string_lossy().to_string())
        .unwrap_or_else(|_| path.display().to_string())
}

async fn handle_execute_code_rpc_stream(
    stream: TcpStream,
    store: AppStore,
    agent: AgentDefinition,
) -> AppResult<()> {
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader).lines();
    while let Some(line) = reader.next_line().await? {
        let response = execute_code_rpc_request(&store, &agent, &line).await;
        writer.write_all(response.to_string().as_bytes()).await?;
        writer.write_all(b"\n").await?;
    }
    Ok(())
}

async fn execute_code_rpc_request(store: &AppStore, agent: &AgentDefinition, line: &str) -> Value {
    let parsed = match serde_json::from_str::<Value>(line) {
        Ok(value) => value,
        Err(error) => {
            return serde_json::json!({"error": format!("invalid RPC JSON: {error}")});
        }
    };
    let Some(tool_name) = parsed.get("tool").and_then(Value::as_str) else {
        return serde_json::json!({"error": "RPC request missing tool"});
    };
    let args = parsed
        .get("args")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    let result = match tool_name {
        "read_file" => read_file_tool(store, agent, &args),
        "write_file" => write_file_tool(store, agent, &args),
        "search_files" => search_files_tool(agent, &args),
        "patch" => patch_tool(store, agent, &args),
        "terminal" => terminal_tool(store, agent, &execute_code_rpc_terminal_payload(args)).await,
        "web_search" => web_search_tool(store, &args).await,
        "web_extract" => web_extract_tool(store, &args).await,
        other => Err(AppError::BadRequest(format!(
            "tool is not available in execute_code: {other}"
        ))),
    };
    match result {
        Ok(value) => serde_json::json!({"result": value}),
        Err(error) => serde_json::json!({"error": error.to_string()}),
    }
}

fn execute_code_rpc_terminal_payload(mut args: Value) -> Value {
    if let Some(object) = args.as_object_mut() {
        for blocked in [
            "background",
            "backgroundProcess",
            "background_process",
            "bg",
            "pty",
            "notify_on_complete",
            "notifyOnComplete",
            "watch_patterns",
            "watchPatterns",
        ] {
            object.remove(blocked);
        }
        if let Some(timeout) = object.remove("timeout") {
            object.entry("timeoutSeconds").or_insert(timeout);
        }
        if let Some(workdir) = object.remove("workdir") {
            object.entry("cwd").or_insert(workdir);
        }
    }
    args
}

fn synthchat_hermes_tools_module() -> &'static str {
    r#"
import json
import os
import shlex
import socket
import threading
import time

_sock = None
_call_lock = threading.Lock()

def json_parse(text: str):
    return json.loads(text, strict=False)

def shell_quote(s: str) -> str:
    return shlex.quote(s)

def retry(fn, max_attempts=3, delay=2):
    last_err = None
    for attempt in range(max_attempts):
        try:
            return fn()
        except Exception as exc:
            last_err = exc
            if attempt < max_attempts - 1:
                time.sleep(delay * (2 ** attempt))
    raise last_err

def _connect():
    global _sock
    if _sock is None:
        endpoint = os.environ["HERMES_RPC_SOCKET"]
        if not endpoint.startswith("tcp://"):
            raise RuntimeError("SynthChat execute_code local RPC expects tcp:// endpoint")
        host_port = endpoint[len("tcp://"):]
        host, _, port = host_port.rpartition(":")
        _sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        _sock.connect((host or "127.0.0.1", int(port)))
        _sock.settimeout(300)
    return _sock

def _call(tool_name, args):
    request = json.dumps({"tool": tool_name, "args": args}) + "\n"
    with _call_lock:
        conn = _connect()
        conn.sendall(request.encode("utf-8"))
        buf = b""
        while not buf.endswith(b"\n"):
            chunk = conn.recv(65536)
            if not chunk:
                raise RuntimeError("Agent process disconnected")
            buf += chunk
    response = json.loads(buf.decode("utf-8").strip())
    if "error" in response:
        raise RuntimeError(response["error"])
    result = response.get("result")
    if isinstance(result, str):
        try:
            return json.loads(result)
        except Exception:
            return result
    return result

def web_search(query: str, limit: int = 5):
    return _call("web_search", {"query": query, "limit": limit})

def web_extract(urls: list):
    return _call("web_extract", {"urls": urls})

def read_file(path: str, offset: int = 1, limit: int = 500):
    return _call("read_file", {"path": path, "offset": offset, "limit": limit})

def write_file(path: str, content: str, cross_profile: bool = False):
    return _call("write_file", {"path": path, "content": content, "cross_profile": cross_profile})

def search_files(pattern: str, target: str = "content", path: str = ".", file_glob: str = None, limit: int = 50, offset: int = 0, output_mode: str = "content", context: int = 0):
    return _call("search_files", {"pattern": pattern, "target": target, "path": path, "file_glob": file_glob, "limit": limit, "offset": offset, "output_mode": output_mode, "context": context})

def patch(path: str = None, old_string: str = None, new_string: str = None, replace_all: bool = False, mode: str = "replace", patch: str = None, cross_profile: bool = False):
    return _call("patch", {"path": path, "old_string": old_string, "new_string": new_string, "replace_all": replace_all, "mode": mode, "patch": patch, "cross_profile": cross_profile})

def terminal(command: str, timeout: int = None, workdir: str = None):
    return _call("terminal", {"command": command, "timeout": timeout, "workdir": workdir})
"#
}

fn synthchat_hermes_tools_file_module() -> &'static str {
    r#"
import json
import os
import shlex
import tempfile
import threading
import time

_RPC_DIR = os.environ.get("HERMES_RPC_DIR") or os.path.join(tempfile.gettempdir(), "synthchat_rpc")
_seq = 0
_seq_lock = threading.Lock()

def json_parse(text: str):
    return json.loads(text, strict=False)

def shell_quote(s: str) -> str:
    return shlex.quote(s)

def retry(fn, max_attempts=3, delay=2):
    last_err = None
    for attempt in range(max_attempts):
        try:
            return fn()
        except Exception as exc:
            last_err = exc
            if attempt < max_attempts - 1:
                time.sleep(delay * (2 ** attempt))
    raise last_err

def _call(tool_name, args):
    global _seq
    os.makedirs(_RPC_DIR, exist_ok=True)
    with _seq_lock:
        _seq += 1
        seq = _seq
    seq_str = f"{seq:06d}"
    req_file = os.path.join(_RPC_DIR, f"req_{seq_str}")
    res_file = os.path.join(_RPC_DIR, f"res_{seq_str}")
    tmp_file = req_file + ".tmp"
    with open(tmp_file, "w", encoding="utf-8") as handle:
        json.dump({"seq": seq, "tool": tool_name, "args": args}, handle, ensure_ascii=False)
    os.replace(tmp_file, req_file)
    deadline = time.monotonic() + 300
    while time.monotonic() < deadline:
        if os.path.exists(res_file):
            with open(res_file, "r", encoding="utf-8") as handle:
                response = json.load(handle)
            try:
                os.unlink(res_file)
            except OSError:
                pass
            if "error" in response:
                raise RuntimeError(response["error"])
            result = response.get("result")
            if isinstance(result, str):
                try:
                    return json.loads(result)
                except Exception:
                    return result
            return result
        time.sleep(0.1)
    raise TimeoutError(f"Timed out waiting for SynthChat RPC response to {tool_name}")

def web_search(query: str, limit: int = 5):
    return _call("web_search", {"query": query, "limit": limit})

def web_extract(urls: list):
    return _call("web_extract", {"urls": urls})

def read_file(path: str, offset: int = 1, limit: int = 500):
    return _call("read_file", {"path": path, "offset": offset, "limit": limit})

def write_file(path: str, content: str, cross_profile: bool = False):
    return _call("write_file", {"path": path, "content": content, "cross_profile": cross_profile})

def search_files(pattern: str, target: str = "content", path: str = ".", file_glob: str = None, limit: int = 50, offset: int = 0, output_mode: str = "content", context: int = 0):
    return _call("search_files", {"pattern": pattern, "target": target, "path": path, "file_glob": file_glob, "limit": limit, "offset": offset, "output_mode": output_mode, "context": context})

def patch(path: str = None, old_string: str = None, new_string: str = None, replace_all: bool = False, mode: str = "replace", patch: str = None, cross_profile: bool = False):
    return _call("patch", {"path": path, "old_string": old_string, "new_string": new_string, "replace_all": replace_all, "mode": mode, "patch": patch, "cross_profile": cross_profile})

def terminal(command: str, timeout: int = None, workdir: str = None):
    return _call("terminal", {"command": command, "timeout": timeout, "workdir": workdir})
"#
}

fn execute_code_backend_command(language: &str, code: &str) -> AppResult<String> {
    let delimiter = unique_heredoc_delimiter(code, "SYNTHCHAT_EXECUTE_CODE");
    let command = match language {
        "python" | "py" => format!(
            "if command -v python3 >/dev/null 2>&1; then python3 - <<'{delimiter}'\n{code}\n{delimiter}\nelif command -v python >/dev/null 2>&1; then python - <<'{delimiter}'\n{code}\n{delimiter}\nelse echo 'execute_code requires python3 or python in the selected terminal backend' >&2; exit 127; fi"
        ),
        "javascript" | "js" | "node" => format!(
            "node - <<'{delimiter}'\n{code}\n{delimiter}"
        ),
        "powershell" | "pwsh" | "ps1" => format!(
            "pwsh -NoProfile -NonInteractive -Command - <<'{delimiter}'\n{code}\n{delimiter}"
        ),
        other => {
            return Err(AppError::BadRequest(format!(
                "unsupported execute_code language: {other}"
            )));
        }
    };
    Ok(command)
}

fn unique_heredoc_delimiter(code: &str, prefix: &str) -> String {
    let mut delimiter = prefix.to_string();
    let mut counter = 0usize;
    while code.lines().any(|line| line.trim() == delimiter) {
        counter += 1;
        delimiter = format!("{prefix}_{counter}");
    }
    delimiter
}

pub(super) async fn process_tool(
    store: &AppStore,
    agent: &AgentDefinition,
    conversation_id: &str,
    run_id: &str,
    payload: &Value,
    app: Option<&AppHandle>,
) -> AppResult<String> {
    ensure_shell_allowed(agent)?;
    let action = payload
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("list")
        .to_lowercase();
    match action.as_str() {
        "list" => {
            let task_filter = payload
                .get("taskId")
                .or_else(|| payload.get("task_id"))
                .or_else(|| payload.get("sessionId"))
                .or_else(|| payload.get("session_id"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty());
            let conversation_filter = payload
                .get("conversationId")
                .or_else(|| payload.get("conversation_id"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty());
            let run_filter = payload
                .get("runId")
                .or_else(|| payload.get("run_id"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty());
            let backend_filter = payload
                .get("backend")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty());
            let env_filter = payload
                .get("envType")
                .or_else(|| payload.get("env_type"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty());
            let mut processes = store.managed_processes()?;
            if let Some(task_id) = task_filter {
                processes.retain(|process| {
                    process
                        .get("taskId")
                        .or_else(|| process.get("task_id"))
                        .and_then(Value::as_str)
                        == Some(task_id)
                });
            }
            if let Some(conversation_id) = conversation_filter {
                processes.retain(|process| {
                    process
                        .get("conversationId")
                        .or_else(|| process.get("conversation_id"))
                        .and_then(Value::as_str)
                        == Some(conversation_id)
                });
            }
            if let Some(run_id) = run_filter {
                processes.retain(|process| {
                    process
                        .get("runId")
                        .or_else(|| process.get("run_id"))
                        .and_then(Value::as_str)
                        == Some(run_id)
                });
            }
            if let Some(backend) = backend_filter {
                processes.retain(|process| {
                    process.get("backend").and_then(Value::as_str) == Some(backend)
                });
            }
            if let Some(env_type) = env_filter {
                processes.retain(|process| {
                    process
                        .get("envType")
                        .or_else(|| process.get("env_type"))
                        .and_then(Value::as_str)
                        == Some(env_type)
                });
            }
            let running_count = processes
                .iter()
                .filter(|process| process.get("status").and_then(Value::as_str) == Some("running"))
                .count();
            let exited_count = processes
                .iter()
                .filter(|process| process.get("status").and_then(Value::as_str) == Some("exited"))
                .count();
            Ok(serde_json::to_string_pretty(&serde_json::json!({
                "action": "list",
                "count": processes.len(),
                "runningCount": running_count,
                "running_count": running_count,
                "exitedCount": exited_count,
                "exited_count": exited_count,
                "hasActive": running_count > 0,
                "has_active": running_count > 0,
                "filtered": task_filter.is_some() || conversation_filter.is_some() || run_filter.is_some() || backend_filter.is_some() || env_filter.is_some(),
                "taskId": task_filter,
                "sessionId": task_filter,
                "session_id": task_filter,
                "conversationId": conversation_filter,
                "runId": run_filter,
                "backend": backend_filter,
                "envType": env_filter,
                "env_type": env_filter,
                "processes": processes
            }))?)
        }
        "count" | "running" | "active" | "has_active" => {
            let task_filter = payload
                .get("taskId")
                .or_else(|| payload.get("task_id"))
                .or_else(|| payload.get("sessionId"))
                .or_else(|| payload.get("session_id"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty());
            let conversation_filter = payload
                .get("conversationId")
                .or_else(|| payload.get("conversation_id"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty());
            let run_filter = payload
                .get("runId")
                .or_else(|| payload.get("run_id"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty());
            let backend_filter = payload
                .get("backend")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty());
            let env_filter = payload
                .get("envType")
                .or_else(|| payload.get("env_type"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty());
            let mut processes = store.managed_processes()?;
            if let Some(task_id) = task_filter {
                processes.retain(|process| {
                    process
                        .get("taskId")
                        .or_else(|| process.get("task_id"))
                        .and_then(Value::as_str)
                        == Some(task_id)
                });
            }
            if let Some(conversation_id) = conversation_filter {
                processes.retain(|process| {
                    process
                        .get("conversationId")
                        .or_else(|| process.get("conversation_id"))
                        .and_then(Value::as_str)
                        == Some(conversation_id)
                });
            }
            if let Some(run_id) = run_filter {
                processes.retain(|process| {
                    process
                        .get("runId")
                        .or_else(|| process.get("run_id"))
                        .and_then(Value::as_str)
                        == Some(run_id)
                });
            }
            if let Some(backend) = backend_filter {
                processes.retain(|process| {
                    process.get("backend").and_then(Value::as_str) == Some(backend)
                });
            }
            if let Some(env_type) = env_filter {
                processes.retain(|process| {
                    process
                        .get("envType")
                        .or_else(|| process.get("env_type"))
                        .and_then(Value::as_str)
                        == Some(env_type)
                });
            }
            let running_count = processes
                .iter()
                .filter(|process| process.get("status").and_then(Value::as_str) == Some("running"))
                .count();
            let exited_count = processes
                .iter()
                .filter(|process| process.get("status").and_then(Value::as_str) == Some("exited"))
                .count();
            Ok(serde_json::to_string_pretty(&serde_json::json!({
                "action": action,
                "count": processes.len(),
                "runningCount": running_count,
                "running_count": running_count,
                "exitedCount": exited_count,
                "exited_count": exited_count,
                "hasActive": running_count > 0,
                "has_active": running_count > 0,
                "filtered": task_filter.is_some() || conversation_filter.is_some() || run_filter.is_some() || backend_filter.is_some() || env_filter.is_some(),
                "taskId": task_filter,
                "sessionId": task_filter,
                "session_id": task_filter,
                "conversationId": conversation_filter,
                "runId": run_filter,
                "backend": backend_filter,
                "envType": env_filter,
                "env_type": env_filter,
            }))?)
        }
        "environment" | "env" | "requirements" => Ok(serde_json::to_string_pretty(
            &terminal_environment_status(store, agent, payload)?,
        )?),
        "environment_cleanup" | "env_cleanup" | "cleanup_environment" => Ok(
            serde_json::to_string_pretty(&terminal_environment_cleanup(store, payload)?)?,
        ),
        "checkpoint" => Ok(serde_json::to_string_pretty(
            &store.managed_process_checkpoint_status()?,
        )?),
        "recover" | "recovery" => {
            let result = store.recover_managed_processes_from_checkpoint()?;
            spawn_detached_watchers_for_recovered(store, app, &result);
            Ok(serde_json::to_string_pretty(&result)?)
        }
        "state" | "status" | "poll" => {
            let process_id = payload
                .get("processId")
                .or_else(|| payload.get("id"))
                .or_else(|| payload.get("sessionId"))
                .or_else(|| payload.get("session_id"))
                .and_then(Value::as_str)
                .ok_or_else(|| AppError::BadRequest("process state requires processId".into()))?;
            Ok(serde_json::to_string_pretty(
                &store.managed_process_state(process_id)?,
            )?)
        }
        "log" => {
            let process_id = payload
                .get("processId")
                .or_else(|| payload.get("id"))
                .or_else(|| payload.get("sessionId"))
                .or_else(|| payload.get("session_id"))
                .and_then(Value::as_str)
                .ok_or_else(|| AppError::BadRequest("process log requires processId".into()))?;
            let offset = payload.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize;
            let limit = payload.get("limit").and_then(Value::as_u64).unwrap_or(200) as usize;
            Ok(serde_json::to_string_pretty(
                &store.managed_process_log(process_id, offset, limit)?,
            )?)
        }
        "write" | "submit" => {
            let process_id = payload
                .get("processId")
                .or_else(|| payload.get("id"))
                .or_else(|| payload.get("sessionId"))
                .or_else(|| payload.get("session_id"))
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    AppError::BadRequest(format!("process {action} requires processId"))
                })?;
            let data = payload
                .get("data")
                .or_else(|| payload.get("text"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            let append_newline = action == "submit";
            Ok(serde_json::to_string_pretty(
                &write_managed_process_stdin(store, process_id, data, append_newline).await?,
            )?)
        }
        "close" | "close_stdin" => {
            let process_id = payload
                .get("processId")
                .or_else(|| payload.get("id"))
                .or_else(|| payload.get("sessionId"))
                .or_else(|| payload.get("session_id"))
                .and_then(Value::as_str)
                .ok_or_else(|| AppError::BadRequest("process close requires processId".into()))?;
            Ok(serde_json::to_string_pretty(
                &close_managed_process_stdin(store, process_id).await?,
            )?)
        }
        "wait" => {
            let process_id = payload
                .get("processId")
                .or_else(|| payload.get("id"))
                .or_else(|| payload.get("sessionId"))
                .or_else(|| payload.get("session_id"))
                .and_then(Value::as_str)
                .ok_or_else(|| AppError::BadRequest("process wait requires processId".into()))?;
            let timeout_seconds = payload
                .get("timeoutSeconds")
                .or_else(|| payload.get("timeout"))
                .and_then(Value::as_u64)
                .unwrap_or(60)
                .clamp(1, 600);
            Ok(serde_json::to_string_pretty(
                &wait_for_managed_process(store, process_id, run_id, timeout_seconds).await?,
            )?)
        }
        "stop" | "kill" => {
            let process_id = payload
                .get("processId")
                .or_else(|| payload.get("id"))
                .or_else(|| payload.get("sessionId"))
                .or_else(|| payload.get("session_id"))
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    AppError::BadRequest(format!("process {action} requires processId"))
                })?;
            let forget = payload
                .get("forget")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let mut state = store.stop_managed_process(process_id, forget)?;
            let event = managed_process_event_from_snapshot(
                "stopped",
                conversation_id,
                run_id,
                &state,
                serde_json::json!({
                    "status": state.get("status").cloned().unwrap_or_else(|| serde_json::json!("unknown")),
                    "exitCode": state.get("exitCode").cloned().unwrap_or(serde_json::Value::Null),
                    "forgotten": forget,
                }),
            );
            let _ = store.push_managed_process_notification(process_id, event.clone());
            emit_managed_process_event(app, event.clone());
            persist_managed_process_event(store, app, conversation_id, event.clone());
            state["notificationEvent"] = event;
            Ok(serde_json::to_string_pretty(&state)?)
        }
        "stop_all" | "kill_all" | "killall" => {
            let task_filter = payload
                .get("taskId")
                .or_else(|| payload.get("task_id"))
                .or_else(|| payload.get("sessionId"))
                .or_else(|| payload.get("session_id"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty());
            let conversation_filter = payload
                .get("conversationId")
                .or_else(|| payload.get("conversation_id"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty());
            let run_filter = payload
                .get("runId")
                .or_else(|| payload.get("run_id"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty());
            let backend_filter = payload
                .get("backend")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty());
            let env_filter = payload
                .get("envType")
                .or_else(|| payload.get("env_type"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty());
            if task_filter.is_none()
                && conversation_filter.is_none()
                && run_filter.is_none()
                && backend_filter.is_none()
                && env_filter.is_none()
            {
                return Err(AppError::BadRequest(
                    "process kill_all requires taskId/sessionId, conversationId, runId, backend, or envType filter"
                        .into(),
                ));
            }
            let forget = payload
                .get("forget")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let mut result = store.stop_managed_processes(
                task_filter,
                conversation_filter,
                run_filter,
                backend_filter,
                env_filter,
                forget,
            )?;
            let stopped = result
                .get("processes")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let mut events = Vec::new();
            for state in stopped {
                let process_id = state
                    .get("sessionId")
                    .or_else(|| state.get("session_id"))
                    .or_else(|| state.get("id"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let event_conversation_id = state
                    .get("conversationId")
                    .or_else(|| state.get("conversation_id"))
                    .and_then(Value::as_str)
                    .unwrap_or(conversation_id);
                let event_run_id = state
                    .get("runId")
                    .or_else(|| state.get("run_id"))
                    .and_then(Value::as_str)
                    .unwrap_or(run_id);
                let event = managed_process_event_from_snapshot(
                    "stopped",
                    event_conversation_id,
                    event_run_id,
                    &state,
                    serde_json::json!({
                        "status": state.get("status").cloned().unwrap_or_else(|| serde_json::json!("unknown")),
                        "exitCode": state.get("exitCode").cloned().unwrap_or(serde_json::Value::Null),
                        "forgotten": forget,
                        "bulk": true,
                        "action": action,
                    }),
                );
                if !process_id.is_empty() {
                    let _ = store.push_managed_process_notification(process_id, event.clone());
                }
                emit_managed_process_event(app, event.clone());
                persist_managed_process_event(store, app, event_conversation_id, event.clone());
                events.push(event);
            }
            result["notificationEvents"] = serde_json::json!(events);
            Ok(serde_json::to_string_pretty(&result)?)
        }
        "start" | "run" => {
            match normalized_terminal_env().as_str() {
                "docker" => {
                    return start_docker_managed_process(
                        store,
                        agent,
                        conversation_id,
                        run_id,
                        payload,
                        app,
                    )
                    .await;
                }
                "ssh" => {
                    return start_ssh_managed_process(
                        store,
                        agent,
                        conversation_id,
                        run_id,
                        payload,
                        app,
                    )
                    .await;
                }
                "singularity" => {
                    return start_singularity_managed_process(
                        store,
                        agent,
                        conversation_id,
                        run_id,
                        payload,
                        app,
                    )
                    .await;
                }
                "modal" => {
                    return start_modal_managed_process(
                        store,
                        agent,
                        conversation_id,
                        run_id,
                        payload,
                        app,
                    )
                    .await;
                }
                "daytona" => {
                    return start_daytona_managed_process(
                        store,
                        agent,
                        conversation_id,
                        run_id,
                        payload,
                        app,
                    )
                    .await;
                }
                _ => {}
            }
            start_managed_process(store, agent, conversation_id, run_id, payload, app).await
        }
        other => Err(AppError::BadRequest(format!(
            "unsupported process action: {other}"
        ))),
    }
}

pub(super) fn terminal_environment_status(
    store: &AppStore,
    agent: &AgentDefinition,
    payload: &Value,
) -> AppResult<Value> {
    let env_type = normalized_terminal_env();
    let container_base = payload
        .get("containerBase")
        .or_else(|| payload.get("container_base"))
        .and_then(Value::as_str)
        .unwrap_or("/root/.synthchat");
    let sync_limit = payload
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(100)
        .min(1000) as usize;
    let cwd = terminal_environment_cwd(agent, &env_type)?;
    let requirements = terminal_environment_requirements(&env_type);
    let sync_files = store.remote_sync_files(container_base, sync_limit)?;
    let mut config = serde_json::json!({
        "cwd": cwd,
        "timeoutSeconds": env_u64("TERMINAL_TIMEOUT", 180),
        "lifetimeSeconds": env_u64("TERMINAL_LIFETIME_SECONDS", 300),
        "dockerImage": env_string("TERMINAL_DOCKER_IMAGE", "nikolaik/python-nodejs:python3.11-nodejs20"),
        "singularityImage": env_string("TERMINAL_SINGULARITY_IMAGE", "docker://nikolaik/python-nodejs:python3.11-nodejs20")
    });
    if let Some(config_map) = config.as_object_mut() {
        config_map.insert(
            "modalImage".into(),
            serde_json::json!(env_string(
                "TERMINAL_MODAL_IMAGE",
                "nikolaik/python-nodejs:python3.11-nodejs20"
            )),
        );
        config_map.insert(
            "modalRemoteBase".into(),
            serde_json::json!(env_string("TERMINAL_MODAL_REMOTE_BASE", "/root/.synthchat")),
        );
        config_map.insert(
            "modalSyncFiles".into(),
            serde_json::json!(env_bool("TERMINAL_MODAL_SYNC_FILES", true)),
        );
        config_map.insert(
            "modalSyncBack".into(),
            serde_json::json!(env_bool("TERMINAL_MODAL_SYNC_BACK", true)),
        );
        config_map.insert(
            "modalSyncLimit".into(),
            serde_json::json!(env_u64("TERMINAL_MODAL_SYNC_LIMIT", 100)),
        );
        config_map.insert(
            "modalPersistedSnapshots".into(),
            serde_json::json!(modal_persisted_snapshot_count(store)),
        );
        config_map.insert(
            "daytonaImage".into(),
            serde_json::json!(env_string(
                "TERMINAL_DAYTONA_IMAGE",
                "nikolaik/python-nodejs:python3.11-nodejs20"
            )),
        );
        config_map.insert(
            "daytonaRemoteBase".into(),
            serde_json::json!(env_string(
                "TERMINAL_DAYTONA_REMOTE_BASE",
                "/home/daytona/.synthchat"
            )),
        );
        config_map.insert(
            "daytonaSyncFiles".into(),
            serde_json::json!(env_bool("TERMINAL_DAYTONA_SYNC_FILES", true)),
        );
        config_map.insert(
            "daytonaSyncBack".into(),
            serde_json::json!(env_bool("TERMINAL_DAYTONA_SYNC_BACK", true)),
        );
        config_map.insert(
            "daytonaSyncLimit".into(),
            serde_json::json!(env_u64("TERMINAL_DAYTONA_SYNC_LIMIT", 100)),
        );
        config_map.insert(
            "modalMode".into(),
            serde_json::json!(env_string("TERMINAL_MODAL_MODE", "auto")),
        );
        config_map.insert(
            "sshHost".into(),
            serde_json::json!(env::var("TERMINAL_SSH_HOST").unwrap_or_default()),
        );
        config_map.insert(
            "sshUser".into(),
            serde_json::json!(env::var("TERMINAL_SSH_USER").unwrap_or_default()),
        );
        config_map.insert(
            "sshPort".into(),
            serde_json::json!(env_u64("TERMINAL_SSH_PORT", 22)),
        );
        config_map.insert(
            "sshKeyConfigured".into(),
            serde_json::json!(env::var("TERMINAL_SSH_KEY")
                .ok()
                .is_some_and(|value| !value.trim().is_empty())),
        );
        config_map.insert(
            "sshPersistent".into(),
            serde_json::json!(env_bool("TERMINAL_SSH_PERSISTENT", true)),
        );
        config_map.insert(
            "sshSyncFiles".into(),
            serde_json::json!(env_bool("TERMINAL_SSH_SYNC_FILES", true)),
        );
        config_map.insert(
            "sshTarSync".into(),
            serde_json::json!(env_bool("TERMINAL_SSH_TAR_SYNC", true)),
        );
        config_map.insert(
            "sshSyncDelete".into(),
            serde_json::json!(env_bool("TERMINAL_SSH_SYNC_DELETE", true)),
        );
        config_map.insert(
            "sshSyncBack".into(),
            serde_json::json!(env_bool("TERMINAL_SSH_SYNC_BACK", true)),
        );
        config_map.insert(
            "sshRemoteBase".into(),
            serde_json::json!(env_string("TERMINAL_SSH_REMOTE_BASE", "~/.synthchat")),
        );
        config_map.insert(
            "sshSyncLimit".into(),
            serde_json::json!(env_u64("TERMINAL_SSH_SYNC_LIMIT", 100)),
        );
        config_map.insert(
            "containerCpu".into(),
            serde_json::json!(env_f64("TERMINAL_CONTAINER_CPU", 1.0)),
        );
        config_map.insert(
            "containerMemoryMb".into(),
            serde_json::json!(env_u64("TERMINAL_CONTAINER_MEMORY", 5120)),
        );
        config_map.insert(
            "containerDiskMb".into(),
            serde_json::json!(env_u64("TERMINAL_CONTAINER_DISK", 51200)),
        );
        config_map.insert(
            "containerPersistent".into(),
            serde_json::json!(env_bool("TERMINAL_CONTAINER_PERSISTENT", true)),
        );
        config_map.insert(
            "dockerPersistAcrossProcesses".into(),
            serde_json::json!(env_bool("TERMINAL_DOCKER_PERSIST_ACROSS_PROCESSES", true)),
        );
        config_map.insert(
            "dockerOrphanReaper".into(),
            serde_json::json!(env_bool("TERMINAL_DOCKER_ORPHAN_REAPER", true)),
        );
        config_map.insert(
            "dockerMountCwdToWorkspace".into(),
            serde_json::json!(env_bool("TERMINAL_DOCKER_MOUNT_CWD_TO_WORKSPACE", false)),
        );
        config_map.insert(
            "dockerRunAsHostUser".into(),
            serde_json::json!(env_bool("TERMINAL_DOCKER_RUN_AS_HOST_USER", false)),
        );
        config_map.insert(
            "dockerForwardEnv".into(),
            env_json_array("TERMINAL_DOCKER_FORWARD_ENV"),
        );
        config_map.insert(
            "dockerVolumes".into(),
            env_json_array("TERMINAL_DOCKER_VOLUMES"),
        );
        config_map.insert("dockerEnv".into(), env_json_object("TERMINAL_DOCKER_ENV"));
        config_map.insert(
            "dockerExtraArgs".into(),
            env_json_array("TERMINAL_DOCKER_EXTRA_ARGS"),
        );
        config_map.insert(
            "singularityEnv".into(),
            env_json_object("TERMINAL_SINGULARITY_ENV"),
        );
        config_map.insert(
            "singularityForwardEnv".into(),
            env_json_array("TERMINAL_SINGULARITY_FORWARD_ENV"),
        );
        config_map.insert(
            "singularityExtraArgs".into(),
            env_json_array("TERMINAL_SINGULARITY_EXTRA_ARGS"),
        );
    }
    let modal_lifecycle = modal_lifecycle_contract(store);
    Ok(serde_json::json!({
        "envType": env_type,
        "config": config,
        "requirements": requirements,
        "modalLifecycle": modal_lifecycle.clone(),
        "modal_lifecycle": modal_lifecycle,
        "syncFiles": sync_files,
        "terminalSessions": terminal_session_snapshot(),
        "sshTerminalSessions": ssh_terminal_session_snapshot(),
        "modalTerminalSessions": modal_terminal_session_snapshot(),
        "daytonaTerminalSessions": daytona_terminal_session_snapshot(),
        "dockerContainers": docker_container_snapshot(),
        "note": "SynthChat supports local, Docker, SSH, Singularity, direct Modal terminal/background execution, managed Modal gateway terminal execution, and basic Daytona terminal/background execution. Modal lifecycle parity is exposed through modalLifecycle; remaining gaps are live credential/provider availability and external managed-gateway ownership, not missing lifecycle diagnostics."
    }))
}

fn modal_lifecycle_contract(store: &AppStore) -> Value {
    let mode = env_string("TERMINAL_MODAL_MODE", "auto");
    let sync_files = env_bool("TERMINAL_MODAL_SYNC_FILES", true);
    let sync_back = env_bool("TERMINAL_MODAL_SYNC_BACK", true);
    let persistent = env_bool("TERMINAL_CONTAINER_PERSISTENT", true);
    let managed_gateway_configured = managed_modal_gateway_ready();
    let direct_credentials_configured = has_direct_modal_credentials();
    let persisted_snapshot_count = modal_persisted_snapshot_count(store);
    serde_json::json!({
        "schema": "hermes_modal_lifecycle_desktop_v1",
        "source": [
            "tools/environments/modal.py::ModalEnvironment",
            "tools/environments/managed_modal.py::ManagedModalEnvironment",
            "tools/environments/modal_utils.py::BaseModalExecutionEnvironment"
        ],
        "mode": mode,
        "direct": {
            "implemented": true,
            "credentialsConfigured": direct_credentials_configured,
            "sdkProbe": terminal_environment_requirements("modal")["modalSdk"].clone(),
            "appName": "hermes-agent",
            "sandboxCommand": ["sleep", "infinity"],
            "defaultCwd": "/root",
            "remoteHermesHome": "/root/.hermes",
            "imageEnv": "TERMINAL_MODAL_IMAGE",
            "persistentFilesystem": persistent,
            "snapshotRestore": true,
            "snapshotRestoreSources": ["data_dir/modal_snapshots.json direct:<taskId>", "legacy <taskId> key"],
            "snapshotSaveOnCleanup": persistent,
            "persistedSnapshotCount": persisted_snapshot_count,
            "staleSnapshotFallbackToBaseImage": true,
            "terminateOnCancel": true,
            "terminateOnCleanup": true,
            "credentialFileMounts": true,
            "skillsAndCacheMounts": true,
            "syncFiles": sync_files,
            "syncBack": sync_back,
            "bulkUploadTarStream": true,
            "bulkDownloadHermesHomeTar": true,
            "stdinChunkBytes": 1048576,
            "backgroundProcess": {
                "implemented": true,
                "launch": "nohup inside direct Modal sandbox",
                "statusCommand": "Modal SDK exec status/log/kill tail",
                "stdin": false,
                "watcherRecovery": true
            }
        },
        "managed": {
            "implemented": true,
            "gatewayConfigured": managed_gateway_configured,
            "modeEnv": "TERMINAL_MODAL_MODE=managed",
            "gatewayEnv": ["TERMINAL_MANAGED_MODAL_GATEWAY_URL", "TERMINAL_MANAGED_MODAL_TOKEN"],
            "createEndpoint": "POST /v1/sandboxes",
            "execEndpoint": "POST /v1/sandboxes/{sandbox_id}/execs",
            "pollEndpoint": "GET /v1/sandboxes/{sandbox_id}/execs/{exec_id}",
            "cancelEndpoint": "POST /v1/sandboxes/{sandbox_id}/execs/{exec_id}/cancel",
            "terminateEndpoint": "POST /v1/sandboxes/{sandbox_id}/terminate",
            "idempotencyKey": true,
            "persistentFilesystem": persistent,
            "snapshotBeforeTerminate": persistent,
            "logicalKey": "taskId/sessionId",
            "remoteCwdOwnedByGateway": true,
            "environmentSnapshotsOwnedByGateway": true,
            "credentialFilePassthrough": false,
            "credentialBoundary": "Hermes managed Modal rejects host credential-file passthrough; use direct mode when mounted credentials are required.",
            "backgroundProcess": {
                "implemented": true,
                "launch": "nohup through managed Modal gateway exec",
                "statusCommand": "managed gateway exec/log/kill endpoints",
                "stdin": false,
                "watcherRecovery": true
            }
        },
        "cleanup": {
            "environmentCleanupStopsManagedProcesses": true,
            "environmentCleanupClearsSessionCwd": true,
            "environmentCleanupClearsPersistedSnapshots": true,
            "targetedCleanupByTaskId": true
        },
        "remainingBoundary": "Live Modal execution still depends on configured Modal credentials/SDK or a configured Nous managed Modal gateway; managed mode intentionally leaves sandbox filesystem snapshots and remote cwd ownership to the gateway."
    })
}

fn terminal_environment_cleanup(store: &AppStore, payload: &Value) -> AppResult<Value> {
    let target = terminal_session_id(payload);
    let ssh_cleanup = ssh_environment_cleanup_sync_back(store, payload);
    let daytona_cleanup = daytona_environment_cleanup(payload, target.as_deref());
    let stopped_ssh_processes = store.stop_managed_processes(
        target.as_deref(),
        None,
        None,
        Some("ssh"),
        Some("ssh"),
        false,
    )?;
    let stopped_docker_processes = store.stop_managed_processes(
        target.as_deref(),
        None,
        None,
        Some("docker"),
        Some("docker"),
        false,
    )?;
    let stopped_singularity_processes = store.stop_managed_processes(
        target.as_deref(),
        None,
        None,
        Some("singularity"),
        Some("singularity"),
        false,
    )?;
    let stopped_modal_processes = store.stop_managed_processes(
        target.as_deref(),
        None,
        None,
        Some("modal"),
        Some("modal"),
        false,
    )?;
    let stopped_daytona_processes = store.stop_managed_processes(
        target.as_deref(),
        None,
        None,
        Some("daytona"),
        Some("daytona"),
        false,
    )?;
    let cleared = clear_terminal_session_cwds(target.as_deref());
    let cleared_ssh = clear_ssh_terminal_session_cwds(target.as_deref());
    let cleared_modal = clear_modal_terminal_sessions(target.as_deref());
    let cleared_modal_persisted = clear_modal_persisted_snapshots(store, target.as_deref())?;
    let cleared_daytona = clear_daytona_terminal_sessions(target.as_deref());
    let cleared_ssh_sync_state = clear_ssh_sync_state();
    let stopped_containers = cleanup_docker_terminal_containers(target.as_deref());
    Ok(serde_json::json!({
        "action": "environment_cleanup",
        "targetSession": target,
        "sshSyncBack": ssh_cleanup,
        "daytonaCleanup": daytona_cleanup,
        "clearedTerminalSessions": cleared,
        "clearedSshTerminalSessions": cleared_ssh,
        "clearedModalTerminalSessions": cleared_modal,
        "clearedModalPersistedSnapshots": cleared_modal_persisted,
        "clearedDaytonaTerminalSessions": cleared_daytona,
        "clearedSshSyncState": cleared_ssh_sync_state,
        "stoppedDockerContainers": stopped_containers,
        "stoppedSshManagedProcesses": stopped_ssh_processes,
        "stoppedDockerManagedProcesses": stopped_docker_processes,
        "stoppedSingularityManagedProcesses": stopped_singularity_processes,
        "stoppedModalManagedProcesses": stopped_modal_processes,
        "stoppedDaytonaManagedProcesses": stopped_daytona_processes,
        "remainingTerminalSessions": terminal_session_snapshot(),
        "remainingSshTerminalSessions": ssh_terminal_session_snapshot(),
        "remainingModalTerminalSessions": modal_terminal_session_snapshot(),
        "remainingDaytonaTerminalSessions": daytona_terminal_session_snapshot()
    }))
}

fn ssh_environment_cleanup_sync_back(store: &AppStore, payload: &Value) -> Value {
    if normalized_terminal_env() != "ssh"
        || !env_bool("TERMINAL_SSH_SYNC_FILES", true)
        || !env_bool("TERMINAL_SSH_SYNC_BACK", true)
    {
        return serde_json::json!({
            "attempted": false,
            "reason": "ssh sync-back disabled or inactive"
        });
    }
    let host = env::var("TERMINAL_SSH_HOST")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let user = env::var("TERMINAL_SSH_USER")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let (Some(host), Some(user)) = (host, user) else {
        return serde_json::json!({
            "attempted": false,
            "reason": "TERMINAL_SSH_HOST or TERMINAL_SSH_USER is not configured"
        });
    };
    let port = env_u64("TERMINAL_SSH_PORT", 22);
    let key_path = env::var("TERMINAL_SSH_KEY")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    match sync_ssh_remote_files_back(store, payload, &user, &host, port, key_path.as_deref()) {
        Ok(note) => serde_json::json!({
            "attempted": true,
            "ok": true,
            "note": note.unwrap_or_else(|| "syncBack: disabled".into())
        }),
        Err(error) => serde_json::json!({
            "attempted": true,
            "ok": false,
            "error": error.to_string()
        }),
    }
}

fn daytona_environment_cleanup(payload: &Value, target: Option<&str>) -> Value {
    if normalized_terminal_env() != "daytona" {
        return serde_json::json!({
            "attempted": false,
            "reason": "daytona backend is not active"
        });
    }
    let python = match find_executable("python")
        .or_else(|| find_executable("python3"))
        .or_else(common_python_path)
    {
        Some(python) => python,
        None => {
            return serde_json::json!({
                "attempted": false,
                "reason": "Python was not found"
            });
        }
    };
    if !python_module_available("daytona") {
        return serde_json::json!({
            "attempted": false,
            "reason": "Python daytona SDK was not found"
        });
    }
    let task_ids = daytona_terminal_session_ids(target)
        .into_iter()
        .map(|session_id| sanitize_terminal_session_key(&session_id))
        .collect::<Vec<_>>();
    let config = serde_json::json!({
        "taskIds": task_ids,
        "persistent": env_bool("TERMINAL_CONTAINER_PERSISTENT", true),
        "delete": payload
            .get("delete")
            .or_else(|| payload.get("deleteSandbox"))
            .or_else(|| payload.get("delete_sandbox"))
            .and_then(Value::as_bool)
            .unwrap_or(!env_bool("TERMINAL_CONTAINER_PERSISTENT", true)),
    });
    let script = r#"
import json
import sys

cfg = json.load(sys.stdin)
try:
    from daytona import Daytona
except Exception as exc:
    print(json.dumps({"ok": False, "error": f"failed to import daytona SDK: {exc}"}))
    raise SystemExit(0)

daytona = Daytona()
rows = []
for task_id in cfg.get("taskIds") or ["default"]:
    name = f"synthchat-{task_id}"
    row = {"taskId": task_id, "sandboxName": name, "ok": False}
    try:
        sandbox = daytona.get(name)
        if cfg.get("delete"):
            daytona.delete(sandbox)
            row.update({"ok": True, "action": "delete", "sandboxId": getattr(sandbox, "id", None)})
        else:
            sandbox.stop()
            row.update({"ok": True, "action": "stop", "sandboxId": getattr(sandbox, "id", None)})
    except Exception as exc:
        row["error"] = str(exc)
    rows.append(row)
print(json.dumps({"ok": True, "results": rows}, ensure_ascii=False))
"#;
    let mut cleanup_command = StdCommand::new(&python);
    cleanup_command.hide_window();
    let mut child = match cleanup_command
        .args(["-c", script])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            return serde_json::json!({
                "attempted": true,
                "ok": false,
                "error": error.to_string()
            });
        }
    };
    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        let _ = stdin.write_all(config.to_string().as_bytes());
    }
    let output = match child.wait_with_output() {
        Ok(output) => output,
        Err(error) => {
            return serde_json::json!({
                "attempted": true,
                "ok": false,
                "error": error.to_string()
            });
        }
    };
    let stdout = decode_terminal_output(&output.stdout);
    let stderr = decode_terminal_output(&output.stderr);
    let parsed = serde_json::from_str::<Value>(stdout.trim()).unwrap_or_else(|_| {
        serde_json::json!({
            "ok": false,
            "stdout": stdout.to_string(),
            "stderr": stderr.to_string()
        })
    });
    serde_json::json!({
        "attempted": true,
        "ok": parsed.get("ok").and_then(Value::as_bool).unwrap_or(false),
        "delete": config.get("delete").cloned().unwrap_or(Value::Bool(false)),
        "results": parsed.get("results").cloned().unwrap_or(Value::Null),
        "error": parsed.get("error").cloned().unwrap_or(Value::Null),
        "stderr": stderr.trim()
    })
}

fn normalized_terminal_env() -> String {
    let raw = env::var("TERMINAL_ENV").unwrap_or_else(|_| "local".into());
    let value = raw.trim().to_lowercase();
    if value.is_empty() {
        "local".into()
    } else {
        value
    }
}

fn terminal_environment_cwd(agent: &AgentDefinition, env_type: &str) -> AppResult<String> {
    if let Ok(raw) = env::var("TERMINAL_CWD") {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            return Ok(trimmed.into());
        }
    }
    if env_type == "local" {
        return Ok(workspace_root(agent)?.display().to_string());
    }
    if env_type == "ssh" {
        return Ok("~".into());
    }
    Ok("/root".into())
}

fn terminal_environment_requirements(env_type: &str) -> Value {
    match env_type {
        "local" => serde_json::json!({
            "ok": true,
            "backend": "local"
        }),
        "docker" => {
            let docker = find_executable("docker").or_else(common_docker_path);
            let version_check = docker
                .as_deref()
                .map(|path| run_version_probe(path, &["version"], 5));
            let ok = version_check
                .as_ref()
                .and_then(|value| value.get("ok"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            serde_json::json!({
                "ok": ok,
                "backend": "docker",
                "executable": docker,
                "versionCheck": version_check,
                "detail": if ok {
                    "docker executable found and docker version completed"
                } else if docker.is_some() {
                    "docker executable found but docker version failed or timed out"
                } else {
                    "docker executable not found in PATH or common Docker Desktop locations"
                }
            })
        }
        "singularity" => {
            let executable = find_singularity_executable();
            let version_check = executable
                .as_deref()
                .map(|path| run_version_probe(path, &["--version"], 5));
            let ok = version_check
                .as_ref()
                .and_then(|value| value.get("ok"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            serde_json::json!({
                "ok": ok,
                "backend": "singularity",
                "executable": executable,
                "versionCheck": version_check,
                "detail": if ok {
                    "apptainer/singularity executable found and --version completed"
                } else if executable.is_some() {
                    "apptainer/singularity executable found but --version failed or timed out"
                } else {
                    "apptainer or singularity executable not found"
                }
            })
        }
        "ssh" => {
            let host = env::var("TERMINAL_SSH_HOST").unwrap_or_default();
            let user = env::var("TERMINAL_SSH_USER").unwrap_or_default();
            let ssh = find_executable("ssh").or_else(common_ssh_path);
            let scp = find_executable("scp").or_else(common_scp_path);
            let ssh_found = ssh.is_some();
            serde_json::json!({
                "ok": !host.trim().is_empty() && !user.trim().is_empty() && ssh_found,
                "backend": "ssh",
                "executable": ssh,
                "scpExecutable": scp,
                "detail": if host.trim().is_empty() || user.trim().is_empty() {
                    "SSH backend requires TERMINAL_SSH_HOST and TERMINAL_SSH_USER"
                } else if ssh_found {
                    "TERMINAL_SSH_HOST/USER are configured and ssh executable was found"
                } else {
                    "SSH backend requires an ssh executable in PATH"
                }
            })
        }
        "modal" => {
            let mode = modal_mode();
            let has_direct = has_direct_modal_credentials();
            let modal_sdk_available = python_module_available("modal");
            let managed_enabled = managed_tools_enabled();
            let managed_ready = managed_modal_gateway_ready();
            let managed_mode_blocked = mode == "managed" && !managed_enabled;
            let selected_backend = match mode.as_str() {
                "managed" if managed_enabled && managed_ready => Some("managed"),
                "direct" if has_direct => Some("direct"),
                "auto" if managed_enabled && managed_ready => Some("managed"),
                "auto" if has_direct => Some("direct"),
                _ => None,
            };
            serde_json::json!({
                "ok": selected_backend == Some("managed") || (selected_backend == Some("direct") && modal_sdk_available),
                "backend": "modal",
                "requestedMode": env_string("TERMINAL_MODAL_MODE", "auto"),
                "mode": mode,
                "hasDirect": has_direct,
                "modalSdkAvailable": modal_sdk_available,
                "managedEnabled": managed_enabled,
                "managedReady": managed_ready,
                "managedModeBlocked": managed_mode_blocked,
                "selectedBackend": selected_backend,
                "detail": if managed_mode_blocked {
                    "Modal managed mode is requested but managed tools are not enabled"
                } else if selected_backend == Some("managed") {
                    "Modal managed gateway appears configured"
                } else if selected_backend == Some("direct") && modal_sdk_available {
                    "direct Modal credentials and Python modal SDK are available"
                } else if selected_backend == Some("direct") {
                    "direct Modal credentials are present but Python modal SDK was not found"
                } else {
                    "Modal backend requires direct Modal credentials or a ready managed gateway"
                }
            })
        }
        "daytona" => {
            let has_key = env::var("DAYTONA_API_KEY")
                .ok()
                .is_some_and(|value| !value.trim().is_empty());
            let sdk_available = python_module_available("daytona");
            let disk_mb = env_u64("TERMINAL_CONTAINER_DISK", 51200);
            let disk_gib = disk_mb.div_ceil(1024).max(1);
            let effective_disk_gib = disk_gib.min(10);
            serde_json::json!({
                "ok": has_key && sdk_available,
                "backend": "daytona",
                "hasApiKey": has_key,
                "sdkAvailable": sdk_available,
                "image": env_string("TERMINAL_DAYTONA_IMAGE", "nikolaik/python-nodejs:python3.11-nodejs20"),
                "resources": {
                    "cpu": env_f64("TERMINAL_CONTAINER_CPU", 1.0),
                    "memoryGiB": env_u64("TERMINAL_CONTAINER_MEMORY", 5120).div_ceil(1024).max(1),
                    "requestedDiskGiB": disk_gib,
                    "effectiveDiskGiB": effective_disk_gib,
                    "diskCapped": disk_gib > effective_disk_gib,
                    "persistent": env_bool("TERMINAL_CONTAINER_PERSISTENT", true)
                },
                "detail": if has_key && sdk_available {
                    "DAYTONA_API_KEY is configured and Python daytona SDK is available"
                } else if has_key {
                    "DAYTONA_API_KEY is configured but Python daytona SDK was not found"
                } else if sdk_available {
                    "Python daytona SDK is available but DAYTONA_API_KEY is missing"
                } else {
                    "Daytona backend requires DAYTONA_API_KEY and Python daytona SDK"
                }
            })
        }
        other => serde_json::json!({
            "ok": false,
            "backend": other,
            "detail": "Unknown TERMINAL_ENV. Use local, docker, singularity, modal, daytona, or ssh."
        }),
    }
}

fn env_string(name: &str, default: &str) -> String {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| default.into())
}

fn env_bool(name: &str, default: bool) -> bool {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_lowercase())
        .map(|value| matches!(value.as_str(), "true" | "1" | "yes" | "on"))
        .unwrap_or(default)
}

fn env_u64(name: &str, default: u64) -> u64 {
    env::var(name)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(default)
}

fn env_u64_if_set(name: &str) -> Option<u64> {
    env::var(name)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
}

fn env_f64(name: &str, default: f64) -> f64 {
    env::var(name)
        .ok()
        .and_then(|value| value.trim().parse::<f64>().ok())
        .unwrap_or(default)
}

fn env_json_array(name: &str) -> Value {
    env::var(name)
        .ok()
        .and_then(|value| serde_json::from_str::<Value>(&value).ok())
        .filter(Value::is_array)
        .unwrap_or_else(|| serde_json::json!([]))
}

fn env_json_object(name: &str) -> Value {
    env::var(name)
        .ok()
        .and_then(|value| serde_json::from_str::<Value>(&value).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| serde_json::json!({}))
}

fn modal_mode() -> String {
    match env_string("TERMINAL_MODAL_MODE", "auto")
        .trim()
        .to_lowercase()
        .as_str()
    {
        "direct" => "direct".into(),
        "managed" => "managed".into(),
        _ => "auto".into(),
    }
}

fn has_direct_modal_credentials() -> bool {
    let token_pair = env::var("MODAL_TOKEN_ID")
        .ok()
        .is_some_and(|value| !value.trim().is_empty())
        && env::var("MODAL_TOKEN_SECRET")
            .ok()
            .is_some_and(|value| !value.trim().is_empty());
    if token_pair {
        return true;
    }
    env::var("MODAL_PROFILE")
        .ok()
        .is_some_and(|value| !value.trim().is_empty())
        || home_file_exists(".modal.toml")
}

fn managed_tools_enabled() -> bool {
    env_bool("SYNTHCHAT_MANAGED_TOOLS_ENABLED", false)
        || env_bool("NOUS_MANAGED_TOOLS_ENABLED", false)
        || env_bool("TERMINAL_MANAGED_MODAL_ENABLED", false)
}

fn managed_modal_gateway_ready() -> bool {
    let gateway = env::var("TERMINAL_MANAGED_MODAL_GATEWAY_URL")
        .or_else(|_| env::var("NOUS_MODAL_GATEWAY_URL"))
        .or_else(|_| env::var("NOUS_TOOL_GATEWAY_URL"))
        .ok()
        .is_some_and(|value| !value.trim().is_empty());
    let token = env::var("NOUS_ACCESS_TOKEN")
        .or_else(|_| env::var("SYNTHCHAT_NOUS_ACCESS_TOKEN"))
        .ok()
        .is_some_and(|value| !value.trim().is_empty());
    gateway && token
}

fn home_file_exists(relative: &str) -> bool {
    let Some(home) = env::var_os("USERPROFILE").or_else(|| env::var_os("HOME")) else {
        return false;
    };
    PathBuf::from(home).join(relative).is_file()
}

fn python_module_available(module: &str) -> bool {
    let Some(python) = find_executable("python")
        .or_else(|| find_executable("python3"))
        .or_else(common_python_path)
    else {
        return false;
    };
    let code = format!(
        "import importlib.util, sys; sys.exit(0 if importlib.util.find_spec({module:?}) else 1)"
    );
    StdCommand::new(python)
        .hide_window()
        .args(["-c", &code])
        .output()
        .ok()
        .is_some_and(|output| output.status.success())
}

fn run_version_probe(executable: &str, args: &[&str], timeout_seconds: u64) -> Value {
    let started = std::time::Instant::now();
    let mut command = StdCommand::new(executable);
    command.hide_window();
    let mut child = match command
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            return serde_json::json!({
                "ok": false,
                "error": error.to_string(),
                "elapsedMs": started.elapsed().as_millis() as u64
            });
        }
    };
    let timeout = Duration::from_secs(timeout_seconds);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => match child.wait_with_output() {
                Ok(output) => {
                    let stdout = decode_terminal_output(&output.stdout);
                    let stderr = decode_terminal_output(&output.stderr);
                    return serde_json::json!({
                        "ok": output.status.success(),
                        "exitCode": output.status.code().unwrap_or(-1),
                        "stdout": truncate_output(&stdout, 2_000),
                        "stderr": truncate_output(&stderr, 2_000),
                        "elapsedMs": started.elapsed().as_millis() as u64
                    });
                }
                Err(error) => {
                    return serde_json::json!({
                        "ok": false,
                        "error": error.to_string(),
                        "elapsedMs": started.elapsed().as_millis() as u64
                    });
                }
            },
            Ok(None) => {
                if started.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return serde_json::json!({
                        "ok": false,
                        "timedOut": true,
                        "timeoutSeconds": timeout_seconds,
                        "elapsedMs": started.elapsed().as_millis() as u64
                    });
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(error) => {
                return serde_json::json!({
                    "ok": false,
                    "error": error.to_string(),
                    "elapsedMs": started.elapsed().as_millis() as u64
                });
            }
        }
    }
}

fn hidden_std_command_output<P, I, S>(program: P, args: I) -> io::Result<std::process::Output>
where
    P: AsRef<std::ffi::OsStr>,
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let mut command = StdCommand::new(program);
    command.hide_window();
    command.args(args).output()
}

fn hidden_std_command_spawn<P>(program: P) -> StdCommand
where
    P: AsRef<std::ffi::OsStr>,
{
    let mut command = StdCommand::new(program);
    command.hide_window();
    command
}

fn find_executable(name: &str) -> Option<String> {
    let paths = env::var_os("PATH")?;
    for dir in env::split_paths(&paths) {
        let direct = dir.join(name);
        if direct.is_file() {
            return Some(direct.to_string_lossy().to_string());
        }
        #[cfg(windows)]
        {
            let exe = dir.join(format!("{name}.exe"));
            if exe.is_file() {
                return Some(exe.to_string_lossy().to_string());
            }
        }
    }
    None
}

fn common_docker_path() -> Option<String> {
    #[cfg(windows)]
    {
        let candidates = [
            r"C:\Program Files\Docker\Docker\resources\bin\docker.exe",
            r"C:\Program Files\Docker\Docker\Docker Desktop.exe",
        ];
        for candidate in candidates {
            if Path::new(candidate).is_file() {
                return Some(candidate.into());
            }
        }
    }
    #[cfg(not(windows))]
    {
        let candidates = [
            "/usr/local/bin/docker",
            "/opt/homebrew/bin/docker",
            "/Applications/Docker.app/Contents/Resources/bin/docker",
        ];
        for candidate in candidates {
            if Path::new(candidate).is_file() {
                return Some(candidate.into());
            }
        }
    }
    None
}

fn common_ssh_path() -> Option<String> {
    #[cfg(windows)]
    {
        let candidates = [
            r"C:\Windows\System32\OpenSSH\ssh.exe",
            r"C:\Program Files\Git\usr\bin\ssh.exe",
        ];
        for candidate in candidates {
            if Path::new(candidate).is_file() {
                return Some(candidate.into());
            }
        }
    }
    #[cfg(not(windows))]
    {
        for candidate in [
            "/usr/bin/ssh",
            "/usr/local/bin/ssh",
            "/opt/homebrew/bin/ssh",
        ] {
            if Path::new(candidate).is_file() {
                return Some(candidate.into());
            }
        }
    }
    None
}

fn common_scp_path() -> Option<String> {
    #[cfg(windows)]
    {
        let candidates = [
            r"C:\Windows\System32\OpenSSH\scp.exe",
            r"C:\Program Files\Git\usr\bin\scp.exe",
        ];
        for candidate in candidates {
            if Path::new(candidate).is_file() {
                return Some(candidate.into());
            }
        }
    }
    #[cfg(not(windows))]
    {
        for candidate in [
            "/usr/bin/scp",
            "/usr/local/bin/scp",
            "/opt/homebrew/bin/scp",
        ] {
            if Path::new(candidate).is_file() {
                return Some(candidate.into());
            }
        }
    }
    None
}

fn common_python_path() -> Option<String> {
    #[cfg(windows)]
    {
        for candidate in [
            r"C:\Windows\py.exe",
            r"C:\Python312\python.exe",
            r"C:\Python311\python.exe",
        ] {
            if Path::new(candidate).is_file() {
                return Some(candidate.into());
            }
        }
    }
    #[cfg(not(windows))]
    {
        for candidate in [
            "/usr/bin/python3",
            "/usr/local/bin/python3",
            "/opt/homebrew/bin/python3",
        ] {
            if Path::new(candidate).is_file() {
                return Some(candidate.into());
            }
        }
    }
    None
}

fn find_singularity_executable() -> Option<String> {
    find_executable("apptainer")
        .or_else(|| find_executable("singularity"))
        .or_else(|| {
            #[cfg(windows)]
            {
                None
            }
            #[cfg(not(windows))]
            {
                for candidate in [
                    "/usr/bin/apptainer",
                    "/usr/local/bin/apptainer",
                    "/usr/bin/singularity",
                    "/usr/local/bin/singularity",
                ] {
                    if Path::new(candidate).is_file() {
                        return Some(candidate.into());
                    }
                }
                None
            }
        })
}

async fn write_managed_process_stdin(
    store: &AppStore,
    process_id: &str,
    data: &str,
    append_newline: bool,
) -> AppResult<Value> {
    let stdin = managed_process_stdin(store, process_id)?;
    let mut guard = stdin.lock().await;
    let Some(stdin) = guard.as_mut() else {
        return Err(AppError::BadRequest(format!(
            "managed process stdin is closed: {process_id}"
        )));
    };
    let mut bytes = data.as_bytes().to_vec();
    if append_newline {
        bytes.push(b'\n');
    }
    stdin.write_all(&bytes).await?;
    stdin.flush().await?;
    Ok(serde_json::json!({
        "processId": process_id,
        "bytesWritten": bytes.len(),
        "stdinOpen": true,
        "submitted": append_newline,
    }))
}

async fn close_managed_process_stdin(store: &AppStore, process_id: &str) -> AppResult<Value> {
    let stdin = managed_process_stdin(store, process_id)?;
    let mut guard = stdin.lock().await;
    let was_open = guard.take().is_some();
    Ok(serde_json::json!({
        "processId": process_id,
        "stdinOpen": false,
        "wasOpen": was_open,
    }))
}

fn managed_process_stdin(
    store: &AppStore,
    process_id: &str,
) -> AppResult<Arc<tokio::sync::Mutex<Option<tokio::process::ChildStdin>>>> {
    let processes = store.managed_process_registry();
    let processes = processes
        .lock()
        .map_err(|_| AppError::BadRequest("managed process lock poisoned".into()))?;
    let process = processes
        .get(process_id.trim())
        .ok_or_else(|| AppError::NotFound(format!("managed process not found: {process_id}")))?;
    Ok(process.stdin.clone())
}

async fn wait_for_managed_process(
    store: &AppStore,
    process_id: &str,
    run_id: &str,
    timeout_seconds: u64,
) -> AppResult<Value> {
    let deadline = std::time::Instant::now() + Duration::from_secs(timeout_seconds);
    loop {
        let mut state = store.managed_process_state(process_id)?;
        if state.get("status").and_then(Value::as_str) == Some("exited") {
            state["waitTimedOut"] = serde_json::json!(false);
            return Ok(state);
        }
        if std::time::Instant::now() >= deadline {
            state["waitTimedOut"] = serde_json::json!(true);
            state["waitTimeoutSeconds"] = serde_json::json!(timeout_seconds);
            return Ok(state);
        }
        if agent_run_wait_was_interrupted(store, run_id)? {
            state["waitTimedOut"] = serde_json::json!(false);
            state["waitInterrupted"] = serde_json::json!(true);
            state["waitInterruptedReason"] = serde_json::json!("agent_run_aborted");
            state["runId"] = serde_json::json!(run_id);
            state["run_id"] = serde_json::json!(run_id);
            return Ok(state);
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

fn agent_run_wait_was_interrupted(store: &AppStore, run_id: &str) -> AppResult<bool> {
    match store.agent_run(run_id) {
        Ok(run) => Ok(matches!(
            run.state.as_str(),
            "completed" | "failed" | "aborted"
        )),
        Err(AppError::NotFound(_)) => Ok(false),
        Err(error) => Err(error),
    }
}

async fn start_ssh_managed_process(
    store: &AppStore,
    _agent: &AgentDefinition,
    conversation_id: &str,
    run_id: &str,
    payload: &Value,
    app: Option<&AppHandle>,
) -> AppResult<String> {
    let command = payload
        .get("command")
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::BadRequest("process start requires command".into()))?;
    ensure_command_not_hardline(command)?;
    let ssh = find_executable("ssh")
        .or_else(common_ssh_path)
        .ok_or_else(|| AppError::BadRequest("TERMINAL_ENV=ssh but ssh was not found".into()))?;
    let host = env::var("TERMINAL_SSH_HOST")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            AppError::BadRequest("TERMINAL_ENV=ssh requires TERMINAL_SSH_HOST".into())
        })?;
    let user = env::var("TERMINAL_SSH_USER")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            AppError::BadRequest("TERMINAL_ENV=ssh requires TERMINAL_SSH_USER".into())
        })?;
    let port = env_u64("TERMINAL_SSH_PORT", 22);
    let key_path = env::var("TERMINAL_SSH_KEY")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let sync_note = sync_ssh_remote_files(store, payload, &user, &host, port, key_path.as_deref())?;
    let task_id = terminal_session_id(payload);
    let cwd = ssh_remote_cwd(payload, task_id.as_deref());
    set_ssh_terminal_session_cwd(task_id.as_deref().unwrap_or("default"), cwd.clone());
    let label = payload
        .get("label")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(command)
        .to_string();
    let notification_options = process_notification_options(payload);
    let chat_config = store.config()?.chat;
    let tail_retention_lines = positive_or_default(chat_config.tool_output_max_lines, 2_000);
    let process_id = new_id("proc");
    let safe_process_id = sanitize_terminal_session_key(&process_id);
    let stdout_path = format!("/tmp/synthchat-{safe_process_id}.out");
    let stderr_path = format!("/tmp/synthchat-{safe_process_id}.err");
    let exit_path = format!("/tmp/synthchat-{safe_process_id}.exit");
    let remote_spawn = format!(
        "cd {} && ( nohup sh -lc {} >{} 2>{} < /dev/null; rc=$?; printf '%s\\n' \"$rc\" >{} ) & echo $!",
        posix_quote_cwd(&cwd),
        posix_shell_quote(command),
        posix_shell_quote(&stdout_path),
        posix_shell_quote(&stderr_path),
        posix_shell_quote(&exit_path)
    );
    let mut args = ssh_base_args(&user, &host, port, key_path.as_deref())?;
    args.push(remote_spawn);
    let mut child = Command::new(&ssh);
    child.hide_window();
    child
        .args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = tokio::time::timeout(Duration::from_secs(30), child.output())
        .await
        .map_err(|_| AppError::BadRequest("SSH background process start timed out".into()))??;
    if !output.status.success() {
        return Err(AppError::BadRequest(format!(
            "SSH background process start failed: {}",
            decode_terminal_output(&output.stderr).trim()
        )));
    }
    let stdout = decode_terminal_output(&output.stdout);
    let pid = stdout
        .split_whitespace()
        .find_map(|token| token.parse::<u32>().ok())
        .ok_or_else(|| {
            AppError::BadRequest(format!(
                "SSH background process did not return a pid: {}",
                stdout.trim()
            ))
        })?;
    let status_remote = format!("kill -0 {pid} 2>/dev/null");
    let kill_remote = format!("kill {pid} 2>/dev/null");
    let mut status_command = vec![ssh.clone()];
    status_command.extend(ssh_base_args(&user, &host, port, key_path.as_deref())?);
    status_command.push(status_remote);
    let mut kill_command = vec![ssh.clone()];
    kill_command.extend(ssh_base_args(&user, &host, port, key_path.as_deref())?);
    kill_command.push(kill_remote);
    let stdout_remote = format!(
        "tail -n {} {} 2>/dev/null",
        tail_retention_lines,
        posix_shell_quote(&stdout_path)
    );
    let stderr_remote = format!(
        "tail -n {} {} 2>/dev/null",
        tail_retention_lines,
        posix_shell_quote(&stderr_path)
    );
    let mut stdout_command = vec![ssh.clone()];
    stdout_command.extend(ssh_base_args(&user, &host, port, key_path.as_deref())?);
    stdout_command.push(stdout_remote);
    let mut stderr_command = vec![ssh.clone()];
    stderr_command.extend(ssh_base_args(&user, &host, port, key_path.as_deref())?);
    stderr_command.push(stderr_remote);
    let exit_remote = format!("cat {} 2>/dev/null", posix_shell_quote(&exit_path));
    let mut exit_command = vec![ssh.clone()];
    exit_command.extend(ssh_base_args(&user, &host, port, key_path.as_deref())?);
    exit_command.push(exit_remote);
    let cleanup_remote = format!(
        "rm -f {} {} {} 2>/dev/null",
        posix_shell_quote(&stdout_path),
        posix_shell_quote(&stderr_path),
        posix_shell_quote(&exit_path)
    );
    let mut cleanup_command = vec![ssh.clone()];
    cleanup_command.extend(ssh_base_args(&user, &host, port, key_path.as_deref())?);
    cleanup_command.push(cleanup_remote);
    let notifications = Arc::new(Mutex::new(ManagedProcessNotificationState::default()));
    let event_context = ManagedProcessEventContext {
        process_id: process_id.clone(),
        label: label.clone(),
        command: command.to_string(),
        cwd: Some(cwd.clone()),
        pid: Some(pid),
        conversation_id: conversation_id.to_string(),
        run_id: run_id.to_string(),
    };
    let process = ManagedProcess {
        id: process_id.clone(),
        label,
        command: command.to_string(),
        cwd: Some(cwd.clone()),
        pid: Some(pid),
        backend: "ssh".into(),
        env_type: "ssh".into(),
        status_command: Some(status_command),
        kill_command: Some(kill_command),
        stdout_command: Some(stdout_command),
        stderr_command: Some(stderr_command),
        exit_command: Some(exit_command),
        cleanup_command: Some(cleanup_command),
        exit_code: None,
        task_id,
        conversation_id: conversation_id.to_string(),
        run_id: run_id.to_string(),
        detached: true,
        pid_scope: "sandbox".into(),
        started_at: now_iso(),
        finished_at: None,
        finished_at_instant: None,
        notify_on_complete: notification_options.notify_on_complete,
        watch_patterns: notification_options.watch_patterns.clone(),
        tail_retention_lines,
        notifications: notifications.clone(),
        stdout: Arc::new(Mutex::new(Vec::new())),
        stderr: Arc::new(Mutex::new(Vec::new())),
        stdin: Arc::new(tokio::sync::Mutex::new(None)),
        child: None,
    };
    let mut state = store.register_managed_process(process)?;
    spawn_detached_process_watcher(
        store.managed_process_registry(),
        store.clone(),
        event_context,
        notifications,
        app.cloned(),
    );
    state["backend"] = serde_json::json!("ssh");
    state["pidScope"] = serde_json::json!("sandbox");
    state["pid_scope"] = serde_json::json!("sandbox");
    state["sync"] = serde_json::json!(sync_note);
    state["stdoutPath"] = serde_json::json!(stdout_path);
    state["stderrPath"] = serde_json::json!(stderr_path);
    state["_hint"] = serde_json::json!(
        "SSH background process is tracked as a detached sandbox PID. Use process(action='log'|'state'|'count'|'kill') to inspect or manage it; stdin is unavailable."
    );
    Ok(serde_json::to_string_pretty(&state)?)
}

async fn start_docker_managed_process(
    store: &AppStore,
    agent: &AgentDefinition,
    conversation_id: &str,
    run_id: &str,
    payload: &Value,
    app: Option<&AppHandle>,
) -> AppResult<String> {
    let command = payload
        .get("command")
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::BadRequest("process start requires command".into()))?;
    ensure_command_not_hardline(command)?;
    let docker = find_executable("docker")
        .or_else(common_docker_path)
        .ok_or_else(|| {
            AppError::BadRequest("TERMINAL_ENV=docker but docker was not found".into())
        })?;
    let workspace = workspace_root(agent)?;
    let workspace = workspace.canonicalize().unwrap_or(workspace);
    let cwd = workspace_cwd(agent, payload.get("cwd").or_else(|| payload.get("workdir")))?;
    let cwd = cwd.canonicalize().unwrap_or(cwd);
    let container_cwd = host_workspace_path_to_container(&workspace, &cwd)?;
    let image = env_string(
        "TERMINAL_DOCKER_IMAGE",
        "nikolaik/python-nodejs:python3.11-nodejs20",
    );
    let container_base = payload
        .get("containerBase")
        .or_else(|| payload.get("container_base"))
        .and_then(Value::as_str)
        .unwrap_or("/root/.synthchat");
    let task_id = terminal_session_id(payload);
    let key = docker_container_key(task_id.as_deref(), &workspace);
    let image_uses_init_entrypoint = docker_image_uses_init_entrypoint(&docker, &image);
    let container_id = ensure_docker_terminal_container(
        store,
        &docker,
        &image,
        &workspace,
        container_base,
        &key,
        task_id.as_deref(),
        image_uses_init_entrypoint,
    )?;
    let label = payload
        .get("label")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(command)
        .to_string();
    let notification_options = process_notification_options(payload);
    let chat_config = store.config()?.chat;
    let tail_retention_lines = positive_or_default(chat_config.tool_output_max_lines, 2_000);
    let process_id = new_id("proc");
    let safe_process_id = sanitize_terminal_session_key(&process_id);
    let stdout_path = format!("/tmp/synthchat-{safe_process_id}.out");
    let stderr_path = format!("/tmp/synthchat-{safe_process_id}.err");
    let pid_path = format!("/tmp/synthchat-{safe_process_id}.pid");
    let exit_path = format!("/tmp/synthchat-{safe_process_id}.exit");
    let docker_spawn = format!(
        "mkdir -p /tmp && ( nohup sh -lc {} >{} 2>{} < /dev/null; rc=$?; printf '%s\\n' \"$rc\" >{} ) & pid=$!; printf '%s\\n' \"$pid\" >{}; printf '%s\\n' \"$pid\"",
        posix_shell_quote(command),
        posix_shell_quote(&stdout_path),
        posix_shell_quote(&stderr_path),
        posix_shell_quote(&exit_path),
        posix_shell_quote(&pid_path),
    );
    let mut docker_command = Command::new(&docker);
    docker_command.hide_window();
    let output = tokio::time::timeout(
        Duration::from_secs(30),
        docker_command
            .args([
                "exec",
                "--workdir",
                &container_cwd,
                &container_id,
                "sh",
                "-lc",
                &docker_spawn,
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output(),
    )
    .await
    .map_err(|_| AppError::BadRequest("Docker background process start timed out".into()))??;
    if !output.status.success() {
        return Err(AppError::BadRequest(format!(
            "Docker background process start failed: {}",
            decode_terminal_output(&output.stderr).trim()
        )));
    }
    let stdout = decode_terminal_output(&output.stdout);
    let pid = stdout
        .split_whitespace()
        .find_map(|token| token.parse::<u32>().ok())
        .ok_or_else(|| {
            AppError::BadRequest(format!(
                "Docker background process did not return a pid: {}",
                stdout.trim()
            ))
        })?;
    let docker_exec_command = |script: String| -> Vec<String> {
        vec![
            docker.clone(),
            "exec".into(),
            container_id.clone(),
            "sh".into(),
            "-lc".into(),
            script,
        ]
    };
    let status_command = docker_exec_command(format!(
        "pid=$(cat {} 2>/dev/null); test -n \"$pid\" && kill -0 \"$pid\" 2>/dev/null",
        posix_shell_quote(&pid_path)
    ));
    let kill_command = docker_exec_command(format!(
        "pid=$(cat {} 2>/dev/null); test -n \"$pid\" && kill \"$pid\" 2>/dev/null",
        posix_shell_quote(&pid_path)
    ));
    let stdout_command = docker_exec_command(format!(
        "tail -n {} {} 2>/dev/null",
        tail_retention_lines,
        posix_shell_quote(&stdout_path)
    ));
    let stderr_command = docker_exec_command(format!(
        "tail -n {} {} 2>/dev/null",
        tail_retention_lines,
        posix_shell_quote(&stderr_path)
    ));
    let exit_command =
        docker_exec_command(format!("cat {} 2>/dev/null", posix_shell_quote(&exit_path)));
    let cleanup_command = docker_exec_command(format!(
        "rm -f {} {} {} {} 2>/dev/null",
        posix_shell_quote(&stdout_path),
        posix_shell_quote(&stderr_path),
        posix_shell_quote(&pid_path),
        posix_shell_quote(&exit_path)
    ));
    let notifications = Arc::new(Mutex::new(ManagedProcessNotificationState::default()));
    let event_context = ManagedProcessEventContext {
        process_id: process_id.clone(),
        label: label.clone(),
        command: command.to_string(),
        cwd: Some(container_cwd.clone()),
        pid: Some(pid),
        conversation_id: conversation_id.to_string(),
        run_id: run_id.to_string(),
    };
    let process = ManagedProcess {
        id: process_id.clone(),
        label,
        command: command.to_string(),
        cwd: Some(container_cwd.clone()),
        pid: Some(pid),
        backend: "docker".into(),
        env_type: "docker".into(),
        status_command: Some(status_command),
        kill_command: Some(kill_command),
        stdout_command: Some(stdout_command),
        stderr_command: Some(stderr_command),
        exit_command: Some(exit_command),
        cleanup_command: Some(cleanup_command),
        exit_code: None,
        task_id,
        conversation_id: conversation_id.to_string(),
        run_id: run_id.to_string(),
        detached: true,
        pid_scope: "sandbox".into(),
        started_at: now_iso(),
        finished_at: None,
        finished_at_instant: None,
        notify_on_complete: notification_options.notify_on_complete,
        watch_patterns: notification_options.watch_patterns.clone(),
        tail_retention_lines,
        notifications: notifications.clone(),
        stdout: Arc::new(Mutex::new(Vec::new())),
        stderr: Arc::new(Mutex::new(Vec::new())),
        stdin: Arc::new(tokio::sync::Mutex::new(None)),
        child: None,
    };
    let mut state = store.register_managed_process(process)?;
    spawn_detached_process_watcher(
        store.managed_process_registry(),
        store.clone(),
        event_context,
        notifications,
        app.cloned(),
    );
    state["backend"] = serde_json::json!("docker");
    state["pidScope"] = serde_json::json!("sandbox");
    state["pid_scope"] = serde_json::json!("sandbox");
    state["containerId"] = serde_json::json!(container_id);
    state["containerCwd"] = serde_json::json!(container_cwd);
    state["stdoutPath"] = serde_json::json!(stdout_path);
    state["stderrPath"] = serde_json::json!(stderr_path);
    state["_hint"] = serde_json::json!(
        "Docker background process is tracked as a detached sandbox PID. Use process(action='log'|'state'|'count'|'kill') to inspect or manage it; stdin is unavailable."
    );
    if let Some(note) = notification_options.conflict_note {
        state["watchPatternsIgnored"] = serde_json::json!(note);
    }
    Ok(serde_json::to_string_pretty(&state)?)
}

async fn start_singularity_managed_process(
    store: &AppStore,
    agent: &AgentDefinition,
    conversation_id: &str,
    run_id: &str,
    payload: &Value,
    app: Option<&AppHandle>,
) -> AppResult<String> {
    let command = payload
        .get("command")
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::BadRequest("process start requires command".into()))?;
    ensure_command_not_hardline(command)?;
    let executable = find_singularity_executable().ok_or_else(|| {
        AppError::BadRequest(
            "TERMINAL_ENV=singularity but apptainer/singularity was not found".into(),
        )
    })?;
    let workspace = workspace_root(agent)?;
    let workspace = workspace.canonicalize().unwrap_or(workspace);
    let cwd = workspace_cwd(agent, payload.get("cwd").or_else(|| payload.get("workdir")))?;
    let cwd = cwd.canonicalize().unwrap_or(cwd);
    let container_cwd = host_workspace_path_to_container(&workspace, &cwd)?;
    let image = env_string(
        "TERMINAL_SINGULARITY_IMAGE",
        "docker://nikolaik/python-nodejs:python3.11-nodejs20",
    );
    let container_base = payload
        .get("containerBase")
        .or_else(|| payload.get("container_base"))
        .and_then(Value::as_str)
        .unwrap_or("/root/.synthchat");
    let notification_options = process_notification_options(payload);
    let chat_config = store.config()?.chat;
    let tail_retention_lines = positive_or_default(chat_config.tool_output_max_lines, 2_000);
    let process_id = new_id("proc");
    let safe_process_id = sanitize_terminal_session_key(&process_id);
    let instance_name = format!(
        "synthchat_{}",
        safe_process_id
            .chars()
            .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
            .collect::<String>()
    );
    let stdout_path = format!("/tmp/synthchat-{safe_process_id}.out");
    let stderr_path = format!("/tmp/synthchat-{safe_process_id}.err");
    let pid_path = format!("/tmp/synthchat-{safe_process_id}.pid");
    let exit_path = format!("/tmp/synthchat-{safe_process_id}.exit");

    let mut instance_args = vec![
        "instance".to_string(),
        "start".to_string(),
        "--containall".to_string(),
        "--no-home".to_string(),
        "--writable-tmpfs".to_string(),
    ];
    push_singularity_resource_args(&mut instance_args);
    push_singularity_env_args(&mut instance_args);
    push_singularity_extra_args(&mut instance_args);
    push_singularity_bind(&mut instance_args, &workspace, "/workspace", false);
    push_singularity_remote_mounts(store, &mut instance_args, container_base)?;
    instance_args.push(image.clone());
    instance_args.push(instance_name.clone());
    let mut instance_command = Command::new(&executable);
    instance_command.hide_window();
    let instance_output = tokio::time::timeout(
        Duration::from_secs(120),
        instance_command
            .args(&instance_args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output(),
    )
    .await
    .map_err(|_| AppError::BadRequest("Singularity instance start timed out".into()))??;
    if !instance_output.status.success() {
        return Err(AppError::BadRequest(format!(
            "Singularity instance start failed: {}",
            decode_terminal_output(&instance_output.stderr).trim()
        )));
    }

    let singularity_spawn = format!(
        "mkdir -p /tmp && ( nohup sh -lc {} >{} 2>{} < /dev/null; rc=$?; printf '%s\\n' \"$rc\" >{} ) & pid=$!; printf '%s\\n' \"$pid\" >{}; printf '%s\\n' \"$pid\"",
        posix_shell_quote(command),
        posix_shell_quote(&stdout_path),
        posix_shell_quote(&stderr_path),
        posix_shell_quote(&exit_path),
        posix_shell_quote(&pid_path),
    );
    let instance_ref = format!("instance://{instance_name}");
    let mut exec_command = Command::new(&executable);
    exec_command.hide_window();
    let output = tokio::time::timeout(
        Duration::from_secs(30),
        exec_command
            .args([
                "exec",
                "--pwd",
                &container_cwd,
                &instance_ref,
                "sh",
                "-lc",
                &singularity_spawn,
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output(),
    )
    .await
    .map_err(|_| AppError::BadRequest("Singularity background process start timed out".into()))??;
    if !output.status.success() {
        let _ = StdCommand::new(&executable)
            .hide_window()
            .args(["instance", "stop", &instance_name])
            .output();
        return Err(AppError::BadRequest(format!(
            "Singularity background process start failed: {}",
            decode_terminal_output(&output.stderr).trim()
        )));
    }
    let stdout = decode_terminal_output(&output.stdout);
    let pid = stdout
        .split_whitespace()
        .find_map(|token| token.parse::<u32>().ok())
        .ok_or_else(|| {
            AppError::BadRequest(format!(
                "Singularity background process did not return a pid: {}",
                stdout.trim()
            ))
        })?;
    let singularity_exec_command = |script: String| -> Vec<String> {
        vec![
            executable.clone(),
            "exec".into(),
            "--pwd".into(),
            container_cwd.clone(),
            instance_ref.clone(),
            "sh".into(),
            "-lc".into(),
            script,
        ]
    };
    let status_command = singularity_exec_command(format!(
        "pid=$(cat {} 2>/dev/null); test -n \"$pid\" && kill -0 \"$pid\" 2>/dev/null",
        posix_shell_quote(&pid_path)
    ));
    let kill_command = singularity_exec_command(format!(
        "pid=$(cat {} 2>/dev/null); test -n \"$pid\" && kill \"$pid\" 2>/dev/null",
        posix_shell_quote(&pid_path)
    ));
    let stdout_command = singularity_exec_command(format!(
        "tail -n {} {} 2>/dev/null",
        tail_retention_lines,
        posix_shell_quote(&stdout_path)
    ));
    let stderr_command = singularity_exec_command(format!(
        "tail -n {} {} 2>/dev/null",
        tail_retention_lines,
        posix_shell_quote(&stderr_path)
    ));
    let exit_command =
        singularity_exec_command(format!("cat {} 2>/dev/null", posix_shell_quote(&exit_path)));
    let cleanup_command = vec![
        executable.clone(),
        "instance".into(),
        "stop".into(),
        instance_name.clone(),
    ];
    let label = payload
        .get("label")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(command)
        .to_string();
    let notifications = Arc::new(Mutex::new(ManagedProcessNotificationState::default()));
    let event_context = ManagedProcessEventContext {
        process_id: process_id.clone(),
        label: label.clone(),
        command: command.to_string(),
        cwd: Some(container_cwd.clone()),
        pid: Some(pid),
        conversation_id: conversation_id.to_string(),
        run_id: run_id.to_string(),
    };
    let process = ManagedProcess {
        id: process_id.clone(),
        label,
        command: command.to_string(),
        cwd: Some(container_cwd.clone()),
        pid: Some(pid),
        backend: "singularity".into(),
        env_type: "singularity".into(),
        status_command: Some(status_command),
        kill_command: Some(kill_command),
        stdout_command: Some(stdout_command),
        stderr_command: Some(stderr_command),
        exit_command: Some(exit_command),
        cleanup_command: Some(cleanup_command),
        exit_code: None,
        task_id: terminal_session_id(payload),
        conversation_id: conversation_id.to_string(),
        run_id: run_id.to_string(),
        detached: true,
        pid_scope: "sandbox".into(),
        started_at: now_iso(),
        finished_at: None,
        finished_at_instant: None,
        notify_on_complete: notification_options.notify_on_complete,
        watch_patterns: notification_options.watch_patterns.clone(),
        tail_retention_lines,
        notifications: notifications.clone(),
        stdout: Arc::new(Mutex::new(Vec::new())),
        stderr: Arc::new(Mutex::new(Vec::new())),
        stdin: Arc::new(tokio::sync::Mutex::new(None)),
        child: None,
    };
    let mut state = store.register_managed_process(process)?;
    spawn_detached_process_watcher(
        store.managed_process_registry(),
        store.clone(),
        event_context,
        notifications,
        app.cloned(),
    );
    state["backend"] = serde_json::json!("singularity");
    state["pidScope"] = serde_json::json!("sandbox");
    state["pid_scope"] = serde_json::json!("sandbox");
    state["instanceName"] = serde_json::json!(instance_name);
    state["containerCwd"] = serde_json::json!(container_cwd);
    state["stdoutPath"] = serde_json::json!(stdout_path);
    state["stderrPath"] = serde_json::json!(stderr_path);
    state["_hint"] = serde_json::json!(
        "Singularity background process is tracked as a detached sandbox PID inside a dedicated instance. Use process(action='log'|'state'|'count'|'kill') to inspect or manage it; stdin is unavailable."
    );
    if let Some(note) = notification_options.conflict_note {
        state["watchPatternsIgnored"] = serde_json::json!(note);
    }
    Ok(serde_json::to_string_pretty(&state)?)
}

async fn start_modal_managed_process(
    store: &AppStore,
    _agent: &AgentDefinition,
    conversation_id: &str,
    run_id: &str,
    payload: &Value,
    app: Option<&AppHandle>,
) -> AppResult<String> {
    let command = payload
        .get("command")
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::BadRequest("process start requires command".into()))?;
    ensure_command_not_hardline(command)?;
    let python = find_executable("python")
        .or_else(|| find_executable("python3"))
        .or_else(common_python_path)
        .ok_or_else(|| AppError::BadRequest("TERMINAL_ENV=modal requires Python".into()))?;
    if !python_module_available("modal") {
        return Err(AppError::BadRequest(
            "TERMINAL_ENV=modal requires the Python modal SDK".into(),
        ));
    }
    let mode = modal_mode();
    if mode == "managed" {
        return start_managed_modal_gateway_process(
            store,
            conversation_id,
            run_id,
            payload,
            app,
            command,
        )
        .await;
    }
    if !has_direct_modal_credentials() {
        return Err(AppError::BadRequest(
            "TERMINAL_ENV=modal direct process start requires Modal credentials".into(),
        ));
    }
    let task_id = terminal_session_id(payload).unwrap_or_else(|| "default".into());
    let explicit_cwd = payload
        .get("cwd")
        .or_else(|| payload.get("workdir"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string);
    let cwd = explicit_cwd
        .clone()
        .or_else(|| modal_terminal_session_cwd(&task_id))
        .or_else(|| {
            env::var("TERMINAL_CWD")
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        })
        .unwrap_or_else(|| "/root".into());
    set_modal_terminal_session_cwd(&task_id, cwd.clone());

    let remote_base = env_string("TERMINAL_MODAL_REMOTE_BASE", "/root/.synthchat");
    let sync_limit = env_u64("TERMINAL_MODAL_SYNC_LIMIT", 100).min(2000) as usize;
    let sync_files = if env_bool("TERMINAL_MODAL_SYNC_FILES", true) {
        store.remote_sync_files(&remote_base, sync_limit)?
    } else {
        serde_json::json!({
            "containerBase": remote_base,
            "count": 0,
            "fileLimit": sync_limit,
            "files": []
        })
    };
    let snapshot_id = modal_persisted_snapshot_id(store, &task_id)?
        .or_else(|| modal_terminal_snapshot_id(&task_id));
    let process_id = new_id("proc");
    let safe_process_id = sanitize_terminal_session_key(&process_id);
    let safe_task_id = sanitize_terminal_session_key(&task_id);
    let stdout_path = format!("/tmp/synthchat-{safe_process_id}.out");
    let stderr_path = format!("/tmp/synthchat-{safe_process_id}.err");
    let pid_path = format!("/tmp/synthchat-{safe_process_id}.pid");
    let exit_path = format!("/tmp/synthchat-{safe_process_id}.exit");
    let notification_options = process_notification_options(payload);
    let chat_config = store.config()?.chat;
    let tail_retention_lines = positive_or_default(chat_config.tool_output_max_lines, 2_000);
    let config = serde_json::json!({
        "command": command,
        "cwd": cwd,
        "taskId": safe_task_id,
        "image": env_string("TERMINAL_MODAL_IMAGE", "nikolaik/python-nodejs:python3.11-nodejs20"),
        "lifetime": env_u64("TERMINAL_LIFETIME_SECONDS", 3600),
        "persistent": env_bool("TERMINAL_CONTAINER_PERSISTENT", true),
        "snapshotId": snapshot_id,
        "stdoutPath": stdout_path,
        "stderrPath": stderr_path,
        "pidPath": pid_path,
        "exitPath": exit_path,
        "syncFiles": sync_files.get("files").cloned().unwrap_or_else(|| serde_json::json!([])),
        "syncFilesEnabled": env_bool("TERMINAL_MODAL_SYNC_FILES", true),
    });
    let script = r#"
import asyncio
import base64
import json
import os
import shlex
import sys

cfg = json.load(sys.stdin)
try:
    import modal
except Exception as exc:
    print(json.dumps({"ok": False, "error": f"failed to import modal SDK: {exc}"}))
    raise SystemExit(0)

def quote_cwd(cwd: str) -> str:
    if cwd == "~":
        return '"$HOME"'
    if cwd.startswith("~/"):
        return '"$HOME"/' + shlex.quote(cwd[2:])
    return shlex.quote(cwd)

def resolve_image(image_spec: str):
    if image_spec.startswith("im-"):
        return modal.Image.from_id(image_spec)
    return modal.Image.from_registry(image_spec)

async def write_stdin(proc, text: str):
    offset = 0
    chunk_size = 1024 * 1024
    while offset < len(text):
        proc.stdin.write(text[offset:offset + chunk_size])
        await proc.stdin.drain.aio()
        offset += chunk_size
    proc.stdin.write_eof()
    await proc.stdin.drain.aio()

async def main():
    app = await modal.App.lookup.aio("synthchat-agent", create_if_missing=True)
    snapshot_id = str(cfg.get("snapshotId") or "")
    base_image_spec = str(cfg.get("image") or "nikolaik/python-nodejs:python3.11-nodejs20")
    discarded_snapshot_id = None

    async def create_sandbox(image_spec: str):
        image = resolve_image(image_spec)
        return await modal.Sandbox.create.aio(
            "sleep",
            "infinity",
            image=image,
            app=app,
            timeout=int(cfg.get("lifetime") or 3600),
        )

    try:
        sandbox = await create_sandbox(snapshot_id or base_image_spec)
    except Exception:
        if not snapshot_id:
            raise
        discarded_snapshot_id = snapshot_id
        sandbox = await create_sandbox(base_image_spec)

    sync_stats = {"enabled": bool(cfg.get("syncFilesEnabled", True)), "uploaded": 0}
    sync_files = cfg.get("syncFiles") or []
    if sync_stats["enabled"] and sync_files:
        for entry in sync_files:
            host_path = str(entry.get("hostPath") or "")
            remote_path = str(entry.get("containerPath") or "")
            if not host_path or not remote_path or not os.path.isfile(host_path):
                continue
            parent = os.path.dirname(remote_path)
            payload = base64.b64encode(open(host_path, "rb").read()).decode("ascii")
            proc = await sandbox.exec.aio(
                "bash",
                "-c",
                f"mkdir -p {shlex.quote(parent)} && base64 -d > {shlex.quote(remote_path)}",
            )
            await write_stdin(proc, payload)
            exit_code = await proc.wait.aio()
            if exit_code != 0:
                raise RuntimeError(f"Modal sync upload failed for {remote_path} (exit {exit_code})")
            sync_stats["uploaded"] += 1

    cwd = quote_cwd(str(cfg.get("cwd") or "/root"))
    cmd = str(cfg.get("command") or "")
    stdout_path = str(cfg.get("stdoutPath"))
    stderr_path = str(cfg.get("stderrPath"))
    pid_path = str(cfg.get("pidPath"))
    exit_path = str(cfg.get("exitPath"))
    wrapper = (
        "mkdir -p /tmp && "
        f"rm -f {shlex.quote(exit_path)}; "
        f"( cd {cwd} && nohup sh -lc {shlex.quote(cmd)} >{shlex.quote(stdout_path)} 2>{shlex.quote(stderr_path)} < /dev/null; "
        f"rc=$?; printf '%s\\n' \"$rc\" > {shlex.quote(exit_path)} ) & "
        f"pid=$!; printf '%s\\n' \"$pid\" > {shlex.quote(pid_path)}; printf '%s\\n' \"$pid\""
    )
    proc = await sandbox.exec.aio("bash", "-c", wrapper, timeout=30)
    stdout = await proc.stdout.read.aio()
    stderr = await proc.stderr.read.aio()
    exit_code = await proc.wait.aio()
    if isinstance(stdout, bytes):
        stdout = stdout.decode("utf-8", errors="replace")
    if isinstance(stderr, bytes):
        stderr = stderr.decode("utf-8", errors="replace")
    pid = None
    for token in str(stdout).split():
        if token.isdigit():
            pid = int(token)
            break
    if exit_code != 0 or pid is None:
        try:
            await sandbox.terminate.aio()
        except Exception:
            pass
        print(json.dumps({"ok": False, "exitCode": exit_code, "stdout": stdout, "stderr": stderr}))
        return
    result = {
        "ok": True,
        "pid": pid,
        "sandboxId": getattr(sandbox, "object_id", None),
        "discardedSnapshotId": discarded_snapshot_id,
        "sync": sync_stats,
    }
    try:
        await sandbox.detach.aio()
    except Exception:
        pass
    print(json.dumps(result, ensure_ascii=False))

asyncio.run(main())
"#;
    let mut child = Command::new(&python);
    child.hide_window();
    child
        .args(["-c", script])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = child.spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(config.to_string().as_bytes()).await?;
        stdin.shutdown().await?;
    }
    let output = tokio::time::timeout(Duration::from_secs(180), child.wait_with_output())
        .await
        .map_err(|_| AppError::BadRequest("Modal background process start timed out".into()))??;
    let stdout = decode_terminal_output(&output.stdout);
    let stderr = decode_terminal_output(&output.stderr);
    let parsed = serde_json::from_str::<Value>(stdout.trim()).ok();
    let ok = parsed
        .as_ref()
        .and_then(|value| value.get("ok"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !ok {
        let error = parsed
            .as_ref()
            .and_then(|value| value.get("error"))
            .and_then(Value::as_str)
            .map(ToString::to_string)
            .unwrap_or_else(|| {
                parsed
                    .as_ref()
                    .map(Value::to_string)
                    .unwrap_or_else(|| stderr.to_string())
            });
        return Err(AppError::BadRequest(format!(
            "Modal background process start failed: {error}"
        )));
    }
    let pid = parsed
        .as_ref()
        .and_then(|value| value.get("pid"))
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| {
            AppError::BadRequest("Modal background process did not return a pid".into())
        })?;
    let sandbox_id = parsed
        .as_ref()
        .and_then(|value| value.get("sandboxId"))
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    if parsed
        .as_ref()
        .and_then(|value| value.get("discardedSnapshotId"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .is_some()
    {
        clear_modal_terminal_snapshot_id(&task_id);
        let _ = clear_modal_persisted_snapshots(store, Some(&task_id))?;
    }

    let modal_command = |action: &str, path: Option<&str>| -> Vec<String> {
        vec![
            python.clone(),
            "-c".into(),
            MODAL_PROCESS_COMMAND_SCRIPT.into(),
            action.into(),
            sandbox_id.clone(),
            path.unwrap_or_default().into(),
        ]
    };
    let status_command = modal_command("status", Some(&pid_path));
    let kill_command = modal_command("kill", Some(&pid_path));
    let stdout_command = modal_command("stdout", Some(&stdout_path));
    let stderr_command = modal_command("stderr", Some(&stderr_path));
    let exit_command = modal_command("exit", Some(&exit_path));
    let cleanup_command = modal_command(
        "cleanup",
        Some(&format!(
            "{pid_path}|{stdout_path}|{stderr_path}|{exit_path}"
        )),
    );
    let label = payload
        .get("label")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(command)
        .to_string();
    let notifications = Arc::new(Mutex::new(ManagedProcessNotificationState::default()));
    let event_context = ManagedProcessEventContext {
        process_id: process_id.clone(),
        label: label.clone(),
        command: command.to_string(),
        cwd: Some(cwd.clone()),
        pid: Some(pid),
        conversation_id: conversation_id.to_string(),
        run_id: run_id.to_string(),
    };
    let process = ManagedProcess {
        id: process_id.clone(),
        label,
        command: command.to_string(),
        cwd: Some(cwd.clone()),
        pid: Some(pid),
        backend: "modal".into(),
        env_type: "modal".into(),
        status_command: Some(status_command),
        kill_command: Some(kill_command),
        stdout_command: Some(stdout_command),
        stderr_command: Some(stderr_command),
        exit_command: Some(exit_command),
        cleanup_command: Some(cleanup_command),
        exit_code: None,
        task_id: Some(task_id),
        conversation_id: conversation_id.to_string(),
        run_id: run_id.to_string(),
        detached: true,
        pid_scope: "sandbox".into(),
        started_at: now_iso(),
        finished_at: None,
        finished_at_instant: None,
        notify_on_complete: notification_options.notify_on_complete,
        watch_patterns: notification_options.watch_patterns.clone(),
        tail_retention_lines,
        notifications: notifications.clone(),
        stdout: Arc::new(Mutex::new(Vec::new())),
        stderr: Arc::new(Mutex::new(Vec::new())),
        stdin: Arc::new(tokio::sync::Mutex::new(None)),
        child: None,
    };
    let mut state = store.register_managed_process(process)?;
    spawn_detached_process_watcher(
        store.managed_process_registry(),
        store.clone(),
        event_context,
        notifications,
        app.cloned(),
    );
    state["backend"] = serde_json::json!("modal");
    state["pidScope"] = serde_json::json!("sandbox");
    state["pid_scope"] = serde_json::json!("sandbox");
    state["sandboxId"] = serde_json::json!(sandbox_id);
    state["stdoutPath"] = serde_json::json!(stdout_path);
    state["stderrPath"] = serde_json::json!(stderr_path);
    state["_hint"] = serde_json::json!(
        "Modal background process is tracked as a detached sandbox PID. Use process(action='log'|'state'|'count'|'kill') to inspect or manage it; stdin is unavailable."
    );
    if let Some(note) = notification_options.conflict_note {
        state["watchPatternsIgnored"] = serde_json::json!(note);
    }
    Ok(serde_json::to_string_pretty(&state)?)
}

async fn start_managed_modal_gateway_process(
    store: &AppStore,
    conversation_id: &str,
    run_id: &str,
    payload: &Value,
    app: Option<&AppHandle>,
    command: &str,
) -> AppResult<String> {
    let (gateway_origin, token) = managed_modal_gateway_config()?;
    let task_id = terminal_session_id(payload).unwrap_or_else(|| "default".into());
    let explicit_cwd = payload
        .get("cwd")
        .or_else(|| payload.get("workdir"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string);
    let cwd = explicit_cwd
        .clone()
        .or_else(|| modal_terminal_session_cwd(&task_id))
        .or_else(|| {
            env::var("TERMINAL_CWD")
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        })
        .unwrap_or_else(|| "/root".into());
    set_modal_terminal_session_cwd(&task_id, cwd.clone());

    let process_id = new_id("proc");
    let safe_process_id = sanitize_terminal_session_key(&process_id);
    let stdout_path = format!("/tmp/synthchat-{safe_process_id}.out");
    let stderr_path = format!("/tmp/synthchat-{safe_process_id}.err");
    let pid_path = format!("/tmp/synthchat-{safe_process_id}.pid");
    let exit_path = format!("/tmp/synthchat-{safe_process_id}.exit");
    let notification_options = process_notification_options(payload);
    let chat_config = store.config()?.chat;
    let tail_retention_lines = positive_or_default(chat_config.tool_output_max_lines, 2_000);
    let timeout_seconds = env_u64("TERMINAL_MODAL_START_TIMEOUT_SECONDS", 120);
    let persistent = env_bool("TERMINAL_CONTAINER_PERSISTENT", true);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(timeout_seconds.saturating_add(30)))
        .build()
        .map_err(|error| AppError::BadRequest(format!("managed Modal client failed: {error}")))?;
    let sandbox_payload = serde_json::json!({
        "image": env_string("TERMINAL_MODAL_IMAGE", "nikolaik/python-nodejs:python3.11-nodejs20"),
        "cwd": cwd.clone(),
        "cpu": env_f64("TERMINAL_MODAL_CPU", 1.0),
        "memoryMiB": env_u64("TERMINAL_MODAL_MEMORY_MB", 5120),
        "timeoutMs": env_u64("TERMINAL_LIFETIME_SECONDS", 3600) * 1000,
        "idleTimeoutMs": env_u64("TERMINAL_MODAL_IDLE_TIMEOUT_SECONDS", 300) * 1000,
        "persistentFilesystem": persistent,
        "logicalKey": sanitize_terminal_session_key(&task_id)
    });
    let sandbox = managed_modal_request(
        &client,
        "POST",
        &format!("{gateway_origin}/v1/sandboxes"),
        &token,
        Some(sandbox_payload),
        Some(("x-idempotency-key", new_id("modal-managed-process-create"))),
    )
    .await?;
    let sandbox_id =
        acp_json_string(&sandbox, &["id", "sandboxId", "sandbox_id"]).ok_or_else(|| {
            AppError::BadRequest("managed Modal create did not return sandbox id".into())
        })?;
    let remote_command = managed_modal_background_start_command(
        command,
        &cwd,
        &stdout_path,
        &stderr_path,
        &pid_path,
        &exit_path,
    );
    let exec_id = new_id("modal-managed-process-start");
    let start = managed_modal_request(
        &client,
        "POST",
        &format!("{gateway_origin}/v1/sandboxes/{sandbox_id}/execs"),
        &token,
        Some(serde_json::json!({
            "execId": exec_id,
            "command": remote_command,
            "cwd": cwd.clone(),
            "timeoutMs": timeout_seconds * 1000
        })),
        None,
    )
    .await?;
    let result = if managed_modal_exec_finished(&start) {
        start
    } else {
        managed_modal_poll_exec(
            &client,
            &gateway_origin,
            &token,
            &sandbox_id,
            &exec_id,
            timeout_seconds,
        )
        .await?
    };
    let exit_code = result
        .get("returncode")
        .or_else(|| result.get("returnCode"))
        .or_else(|| result.get("exitCode"))
        .and_then(Value::as_i64)
        .unwrap_or(-1);
    let stdout = acp_json_string(&result, &["output", "stdout", "result"]).unwrap_or_default();
    let stderr = acp_json_string(&result, &["stderr", "error"]).unwrap_or_default();
    let pid = stdout
        .split_whitespace()
        .find_map(|token| token.parse::<u32>().ok())
        .ok_or_else(|| {
            AppError::BadRequest(format!(
                "managed Modal background process did not return a pid (exitCode={exit_code}, stderr={})",
                truncate_output(&stderr, 500)
            ))
        })?;
    if exit_code != 0 {
        return Err(AppError::BadRequest(format!(
            "managed Modal background process start failed (exitCode={exit_code}): {}",
            truncate_output(&stderr, 500)
        )));
    }

    let python = find_executable("python")
        .or_else(|| find_executable("python3"))
        .or_else(common_python_path)
        .ok_or_else(|| {
            AppError::BadRequest("managed Modal process watcher requires Python".into())
        })?;
    let managed_modal_command = |action: &str, path: Option<&str>| -> Vec<String> {
        vec![
            python.clone(),
            "-c".into(),
            MANAGED_MODAL_PROCESS_COMMAND_SCRIPT.into(),
            action.into(),
            gateway_origin.clone(),
            token.clone(),
            sandbox_id.clone(),
            path.unwrap_or_default().into(),
        ]
    };
    let status_command = managed_modal_command("status", Some(&pid_path));
    let kill_command = managed_modal_command("kill", Some(&pid_path));
    let stdout_command = managed_modal_command("stdout", Some(&stdout_path));
    let stderr_command = managed_modal_command("stderr", Some(&stderr_path));
    let exit_command = managed_modal_command("exit", Some(&exit_path));
    let cleanup_command = managed_modal_command(
        "cleanup",
        Some(&format!(
            "{pid_path}|{stdout_path}|{stderr_path}|{exit_path}"
        )),
    );
    let label = payload
        .get("label")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(command)
        .to_string();
    let notifications = Arc::new(Mutex::new(ManagedProcessNotificationState::default()));
    let event_context = ManagedProcessEventContext {
        process_id: process_id.clone(),
        label: label.clone(),
        command: command.to_string(),
        cwd: Some(cwd.clone()),
        pid: Some(pid),
        conversation_id: conversation_id.to_string(),
        run_id: run_id.to_string(),
    };
    let process = ManagedProcess {
        id: process_id.clone(),
        label,
        command: command.to_string(),
        cwd: Some(cwd.clone()),
        pid: Some(pid),
        backend: "modal".into(),
        env_type: "modal".into(),
        status_command: Some(status_command),
        kill_command: Some(kill_command),
        stdout_command: Some(stdout_command),
        stderr_command: Some(stderr_command),
        exit_command: Some(exit_command),
        cleanup_command: Some(cleanup_command),
        exit_code: None,
        task_id: Some(task_id),
        conversation_id: conversation_id.to_string(),
        run_id: run_id.to_string(),
        detached: true,
        pid_scope: "sandbox".into(),
        started_at: now_iso(),
        finished_at: None,
        finished_at_instant: None,
        notify_on_complete: notification_options.notify_on_complete,
        watch_patterns: notification_options.watch_patterns.clone(),
        tail_retention_lines,
        notifications: notifications.clone(),
        stdout: Arc::new(Mutex::new(Vec::new())),
        stderr: Arc::new(Mutex::new(Vec::new())),
        stdin: Arc::new(tokio::sync::Mutex::new(None)),
        child: None,
    };
    let mut state = store.register_managed_process(process)?;
    spawn_detached_process_watcher(
        store.managed_process_registry(),
        store.clone(),
        event_context,
        notifications,
        app.cloned(),
    );
    state["backend"] = serde_json::json!("modal");
    state["mode"] = serde_json::json!("managed");
    state["pidScope"] = serde_json::json!("sandbox");
    state["pid_scope"] = serde_json::json!("sandbox");
    state["sandboxId"] = serde_json::json!(sandbox_id);
    state["stdoutPath"] = serde_json::json!(stdout_path);
    state["stderrPath"] = serde_json::json!(stderr_path);
    state["_hint"] = serde_json::json!(
        "Managed Modal background process is tracked through the configured gateway. Use process(action='log'|'state'|'count'|'kill') to inspect or manage it; stdin is unavailable."
    );
    if let Some(note) = notification_options.conflict_note {
        state["watchPatternsIgnored"] = serde_json::json!(note);
    }
    Ok(serde_json::to_string_pretty(&state)?)
}

fn managed_modal_background_start_command(
    command: &str,
    cwd: &str,
    stdout_path: &str,
    stderr_path: &str,
    pid_path: &str,
    exit_path: &str,
) -> String {
    format!(
        "mkdir -p /tmp && rm -f {exit_path}; ( cd {cwd} && nohup sh -lc {command} >{stdout_path} 2>{stderr_path} < /dev/null; rc=$?; printf '%s\\n' \"$rc\" > {exit_path} ) & pid=$!; printf '%s\\n' \"$pid\" > {pid_path}; printf '%s\\n' \"$pid\"",
        command = posix_shell_quote(command),
        cwd = posix_shell_quote(cwd),
        stdout_path = posix_shell_quote(stdout_path),
        stderr_path = posix_shell_quote(stderr_path),
        pid_path = posix_shell_quote(pid_path),
        exit_path = posix_shell_quote(exit_path),
    )
}

const MODAL_PROCESS_COMMAND_SCRIPT: &str = r#"
import asyncio
import shlex
import sys

action = sys.argv[1] if len(sys.argv) > 1 else "status"
sandbox_id = sys.argv[2] if len(sys.argv) > 2 else ""
path = sys.argv[3] if len(sys.argv) > 3 else ""

async def main():
    try:
        import modal
        if hasattr(modal.Sandbox.from_id, "aio"):
            sandbox = await modal.Sandbox.from_id.aio(sandbox_id)
        else:
            sandbox = modal.Sandbox.from_id(sandbox_id)
    except Exception:
        raise SystemExit(2)

    async def run_shell(command: str) -> tuple[int, str, str]:
        proc = await sandbox.exec.aio("bash", "-c", command, timeout=30)
        stdout = await proc.stdout.read.aio()
        stderr = await proc.stderr.read.aio()
        exit_code = await proc.wait.aio()
        if isinstance(stdout, bytes):
            stdout = stdout.decode("utf-8", errors="replace")
        if isinstance(stderr, bytes):
            stderr = stderr.decode("utf-8", errors="replace")
        return int(exit_code or 0), stdout or "", stderr or ""

    if action == "status":
        quoted = shlex.quote(path)
        exit_code, _, _ = await run_shell(
            f"pid=$(cat {quoted} 2>/dev/null || true); "
            "test -n \"$pid\" && kill -0 \"$pid\" 2>/dev/null"
        )
        try:
            await sandbox.detach.aio()
        except Exception:
            pass
        raise SystemExit(0 if exit_code == 0 else 1)
    if action == "kill":
        quoted = shlex.quote(path)
        await run_shell(
            f"pid=$(cat {quoted} 2>/dev/null || true); "
            "test -n \"$pid\" && kill \"$pid\" 2>/dev/null || true"
        )
        try:
            await sandbox.detach.aio()
        except Exception:
            pass
        raise SystemExit(0)
    if action in {"stdout", "stderr"}:
        quoted = shlex.quote(path)
        _, stdout, _ = await run_shell(f"tail -n 2000 {quoted} 2>/dev/null || true")
        sys.stdout.write(stdout)
        try:
            await sandbox.detach.aio()
        except Exception:
            pass
        raise SystemExit(0)
    if action == "exit":
        quoted = shlex.quote(path)
        exit_code, stdout, _ = await run_shell(f"cat {quoted} 2>/dev/null || true")
        sys.stdout.write(stdout)
        try:
            await sandbox.detach.aio()
        except Exception:
            pass
        raise SystemExit(0 if exit_code == 0 else 1)
    if action == "cleanup":
        paths = [item for item in path.split("|") if item]
        if paths:
            await run_shell("rm -f " + " ".join(shlex.quote(item) for item in paths))
        try:
            await sandbox.terminate.aio()
        except Exception:
            pass
        raise SystemExit(0)
    raise SystemExit(2)

asyncio.run(main())
"#;

const MANAGED_MODAL_PROCESS_COMMAND_SCRIPT: &str = r#"
import json
import shlex
import sys
import time
import urllib.error
import urllib.request

action = sys.argv[1] if len(sys.argv) > 1 else "status"
gateway = (sys.argv[2] if len(sys.argv) > 2 else "").rstrip("/")
token = sys.argv[3] if len(sys.argv) > 3 else ""
sandbox_id = sys.argv[4] if len(sys.argv) > 4 else ""
path = sys.argv[5] if len(sys.argv) > 5 else ""

def request(method, suffix, body=None):
    data = None
    headers = {"Authorization": "Bearer " + token, "Content-Type": "application/json"}
    if body is not None:
        data = json.dumps(body).encode("utf-8")
    req = urllib.request.Request(gateway + suffix, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            text = resp.read().decode("utf-8", errors="replace")
    except urllib.error.HTTPError as exc:
        text = exc.read().decode("utf-8", errors="replace")
        sys.stderr.write(text)
        raise SystemExit(2)
    return json.loads(text) if text.strip() else {}

def finished(value):
    return str(value.get("status") or "").lower() in {"completed", "failed", "cancelled", "canceled", "timeout"}

def run_shell(command):
    exec_id = "watcher-" + str(int(time.time() * 1000))
    start = request("POST", f"/v1/sandboxes/{sandbox_id}/execs", {
        "execId": exec_id,
        "command": command,
        "timeoutMs": 30000,
    })
    result = start
    deadline = time.monotonic() + 35
    while not finished(result) and time.monotonic() < deadline:
        time.sleep(0.5)
        result = request("GET", f"/v1/sandboxes/{sandbox_id}/execs/{exec_id}")
    code = result.get("returncode", result.get("returnCode", result.get("exitCode", -1)))
    stdout = result.get("output") or result.get("stdout") or result.get("result") or ""
    stderr = result.get("stderr") or result.get("error") or ""
    return int(code if code is not None else -1), str(stdout), str(stderr)

if action == "status":
    quoted = shlex.quote(path)
    code, _, _ = run_shell(f"pid=$(cat {quoted} 2>/dev/null || true); test -n \"$pid\" && kill -0 \"$pid\" 2>/dev/null")
    raise SystemExit(0 if code == 0 else 1)
if action == "kill":
    quoted = shlex.quote(path)
    run_shell(f"pid=$(cat {quoted} 2>/dev/null || true); test -n \"$pid\" && kill \"$pid\" 2>/dev/null || true")
    raise SystemExit(0)
if action in {"stdout", "stderr"}:
    _, stdout, _ = run_shell(f"tail -n 2000 {shlex.quote(path)} 2>/dev/null || true")
    sys.stdout.write(stdout)
    raise SystemExit(0)
if action == "exit":
    code, stdout, _ = run_shell(f"cat {shlex.quote(path)} 2>/dev/null || true")
    sys.stdout.write(stdout)
    raise SystemExit(0 if code == 0 else 1)
if action == "cleanup":
    paths = [item for item in path.split("|") if item]
    if paths:
        run_shell("rm -f " + " ".join(shlex.quote(item) for item in paths))
    request("POST", f"/v1/sandboxes/{sandbox_id}/terminate", {"snapshotBeforeTerminate": True})
    raise SystemExit(0)
sys.stderr.write("unknown action: " + action)
raise SystemExit(2)
"#;

async fn start_daytona_managed_process(
    store: &AppStore,
    _agent: &AgentDefinition,
    conversation_id: &str,
    run_id: &str,
    payload: &Value,
    app: Option<&AppHandle>,
) -> AppResult<String> {
    let command = payload
        .get("command")
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::BadRequest("process start requires command".into()))?;
    ensure_command_not_hardline(command)?;
    let python = find_executable("python")
        .or_else(|| find_executable("python3"))
        .or_else(common_python_path)
        .ok_or_else(|| AppError::BadRequest("TERMINAL_ENV=daytona requires Python".into()))?;
    if !python_module_available("daytona") {
        return Err(AppError::BadRequest(
            "TERMINAL_ENV=daytona requires the Python daytona SDK".into(),
        ));
    }
    let task_id = terminal_session_id(payload).unwrap_or_else(|| "default".into());
    let explicit_cwd = payload
        .get("cwd")
        .or_else(|| payload.get("workdir"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string);
    let cwd = explicit_cwd
        .clone()
        .or_else(|| daytona_terminal_session_cwd(&task_id))
        .or_else(|| {
            env::var("TERMINAL_CWD")
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        })
        .unwrap_or_else(|| "/home/daytona".into());
    set_daytona_terminal_session_cwd(&task_id, cwd.clone());

    let sync_limit = env_u64("TERMINAL_DAYTONA_SYNC_LIMIT", 100).min(2000) as usize;
    let remote_base = env_string("TERMINAL_DAYTONA_REMOTE_BASE", "/home/daytona/.synthchat");
    let sync_files = if env_bool("TERMINAL_DAYTONA_SYNC_FILES", true) {
        store.remote_sync_files(&remote_base, sync_limit)?
    } else {
        serde_json::json!({
            "containerBase": remote_base,
            "count": 0,
            "fileLimit": sync_limit,
            "files": []
        })
    };
    let process_id = new_id("proc");
    let safe_process_id = sanitize_terminal_session_key(&process_id);
    let safe_task_id = sanitize_terminal_session_key(&task_id);
    let sandbox_name = format!("synthchat-{safe_task_id}");
    let stdout_path = format!("/tmp/synthchat-{safe_process_id}.out");
    let stderr_path = format!("/tmp/synthchat-{safe_process_id}.err");
    let pid_path = format!("/tmp/synthchat-{safe_process_id}.pid");
    let exit_path = format!("/tmp/synthchat-{safe_process_id}.exit");
    let notification_options = process_notification_options(payload);
    let chat_config = store.config()?.chat;
    let tail_retention_lines = positive_or_default(chat_config.tool_output_max_lines, 2_000);
    let config = serde_json::json!({
        "command": command,
        "cwd": cwd,
        "taskId": safe_task_id,
        "sandboxName": sandbox_name,
        "image": env_string("TERMINAL_DAYTONA_IMAGE", "nikolaik/python-nodejs:python3.11-nodejs20"),
        "cpu": env_f64("TERMINAL_CONTAINER_CPU", 1.0).ceil().max(1.0) as u64,
        "memoryMb": env_u64("TERMINAL_CONTAINER_MEMORY", 5120),
        "diskMb": env_u64("TERMINAL_CONTAINER_DISK", 51200),
        "persistent": env_bool("TERMINAL_CONTAINER_PERSISTENT", true),
        "stdoutPath": stdout_path,
        "stderrPath": stderr_path,
        "pidPath": pid_path,
        "exitPath": exit_path,
        "syncFiles": sync_files.get("files").cloned().unwrap_or_else(|| serde_json::json!([])),
        "syncFilesEnabled": env_bool("TERMINAL_DAYTONA_SYNC_FILES", true),
    });
    let script = r#"
import json
import math
import os
import shlex
import sys

cfg = json.load(sys.stdin)
try:
    from daytona import Daytona, CreateSandboxFromImageParams, Resources
except Exception as exc:
    print(json.dumps({"ok": False, "error": f"failed to import daytona SDK: {exc}"}))
    raise SystemExit(0)

def quote_cwd(cwd: str) -> str:
    if cwd == "~":
        return '"$HOME"'
    if cwd.startswith("~/"):
        return '"$HOME"/' + shlex.quote(cwd[2:])
    return shlex.quote(cwd)

daytona = Daytona()
task_id = cfg.get("taskId") or "default"
name = cfg.get("sandboxName") or f"synthchat-{task_id}"
labels = {"synthchat_task_id": task_id}
sandbox = None
if bool(cfg.get("persistent", True)):
    try:
        sandbox = daytona.get(name)
        sandbox.start()
    except Exception:
        sandbox = None
    if sandbox is None:
        try:
            results = daytona.list(labels=labels, limit=1)
            sandbox = next(iter(results), None)
            if sandbox is not None:
                sandbox.start()
        except Exception:
            sandbox = None
if sandbox is None:
    memory_gib = max(1, math.ceil(int(cfg.get("memoryMb") or 5120) / 1024))
    disk_gib = min(max(1, math.ceil(int(cfg.get("diskMb") or 51200) / 1024)), 10)
    sandbox = daytona.create(CreateSandboxFromImageParams(
        image=cfg.get("image") or "nikolaik/python-nodejs:python3.11-nodejs20",
        name=name,
        labels=labels,
        auto_stop_interval=0,
        resources=Resources(cpu=max(1, int(cfg.get("cpu") or 1)), memory=memory_gib, disk=disk_gib),
    ))

sync_stats = {"enabled": bool(cfg.get("syncFilesEnabled", True)), "uploaded": 0}
if sync_stats["enabled"]:
    sync_files = cfg.get("syncFiles") or []
    parents = sorted({
        os.path.dirname(str(entry.get("containerPath") or ""))
        for entry in sync_files
        if entry.get("containerPath")
    })
    if parents:
        sandbox.process.exec("mkdir -p " + " ".join(shlex.quote(parent) for parent in parents))
    for entry in sync_files:
        host_path = str(entry.get("hostPath") or "")
        remote_path = str(entry.get("containerPath") or "")
        if host_path and remote_path and os.path.isfile(host_path):
            sandbox.fs.upload_file(host_path, remote_path)
            sync_stats["uploaded"] += 1

cwd = quote_cwd(str(cfg.get("cwd") or "/home/daytona"))
cmd = str(cfg.get("command") or "")
stdout_path = str(cfg.get("stdoutPath"))
stderr_path = str(cfg.get("stderrPath"))
pid_path = str(cfg.get("pidPath"))
exit_path = str(cfg.get("exitPath"))
wrapper = (
    "mkdir -p /tmp && "
    f"rm -f {shlex.quote(exit_path)}; "
    f"( cd {cwd} && nohup sh -lc {shlex.quote(cmd)} >{shlex.quote(stdout_path)} 2>{shlex.quote(stderr_path)} < /dev/null; "
    f"rc=$?; printf '%s\\n' \"$rc\" > {shlex.quote(exit_path)} ) & "
    f"pid=$!; printf '%s\\n' \"$pid\" > {shlex.quote(pid_path)}; printf '%s\\n' \"$pid\""
)
response = sandbox.process.exec(wrapper)
output = getattr(response, "result", "") or ""
exit_code = int(getattr(response, "exit_code", 0) or 0)
pid = None
for token in output.split():
    if token.isdigit():
        pid = int(token)
        break
if exit_code != 0 or pid is None:
    print(json.dumps({"ok": False, "exitCode": exit_code, "output": output}))
else:
    print(json.dumps({
        "ok": True,
        "pid": pid,
        "sandboxId": getattr(sandbox, "id", None),
        "sandboxName": name,
        "sync": sync_stats,
    }, ensure_ascii=False))
"#;
    let mut child = Command::new(&python);
    child.hide_window();
    child
        .args(["-c", script])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = child.spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(config.to_string().as_bytes()).await?;
        stdin.shutdown().await?;
    }
    let output = tokio::time::timeout(Duration::from_secs(120), child.wait_with_output())
        .await
        .map_err(|_| AppError::BadRequest("Daytona background process start timed out".into()))??;
    let stdout = decode_terminal_output(&output.stdout);
    let stderr = decode_terminal_output(&output.stderr);
    let parsed = serde_json::from_str::<Value>(stdout.trim()).ok();
    let ok = parsed
        .as_ref()
        .and_then(|value| value.get("ok"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !ok {
        let error = parsed
            .as_ref()
            .and_then(|value| value.get("error"))
            .and_then(Value::as_str)
            .map(ToString::to_string)
            .unwrap_or_else(|| {
                parsed
                    .as_ref()
                    .map(Value::to_string)
                    .unwrap_or_else(|| stderr.to_string())
            });
        return Err(AppError::BadRequest(format!(
            "Daytona background process start failed: {error}"
        )));
    }
    let pid = parsed
        .as_ref()
        .and_then(|value| value.get("pid"))
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| {
            AppError::BadRequest("Daytona background process did not return a pid".into())
        })?;
    let sandbox_id = parsed
        .as_ref()
        .and_then(|value| value.get("sandboxId"))
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();

    let daytona_command = |action: &str, path: Option<&str>| -> Vec<String> {
        vec![
            python.clone(),
            "-c".into(),
            DAYTONA_PROCESS_COMMAND_SCRIPT.into(),
            action.into(),
            sandbox_name.clone(),
            path.unwrap_or_default().into(),
        ]
    };
    let status_command = daytona_command("status", Some(&pid_path));
    let kill_command = daytona_command("kill", Some(&pid_path));
    let stdout_command = daytona_command("stdout", Some(&stdout_path));
    let stderr_command = daytona_command("stderr", Some(&stderr_path));
    let exit_command = daytona_command("exit", Some(&exit_path));
    let cleanup_command = daytona_command(
        "cleanup",
        Some(&format!(
            "{pid_path}|{stdout_path}|{stderr_path}|{exit_path}"
        )),
    );
    let label = payload
        .get("label")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(command)
        .to_string();
    let notifications = Arc::new(Mutex::new(ManagedProcessNotificationState::default()));
    let event_context = ManagedProcessEventContext {
        process_id: process_id.clone(),
        label: label.clone(),
        command: command.to_string(),
        cwd: Some(cwd.clone()),
        pid: Some(pid),
        conversation_id: conversation_id.to_string(),
        run_id: run_id.to_string(),
    };
    let process = ManagedProcess {
        id: process_id.clone(),
        label,
        command: command.to_string(),
        cwd: Some(cwd.clone()),
        pid: Some(pid),
        backend: "daytona".into(),
        env_type: "daytona".into(),
        status_command: Some(status_command),
        kill_command: Some(kill_command),
        stdout_command: Some(stdout_command),
        stderr_command: Some(stderr_command),
        exit_command: Some(exit_command),
        cleanup_command: Some(cleanup_command),
        exit_code: None,
        task_id: Some(task_id),
        conversation_id: conversation_id.to_string(),
        run_id: run_id.to_string(),
        detached: true,
        pid_scope: "sandbox".into(),
        started_at: now_iso(),
        finished_at: None,
        finished_at_instant: None,
        notify_on_complete: notification_options.notify_on_complete,
        watch_patterns: notification_options.watch_patterns.clone(),
        tail_retention_lines,
        notifications: notifications.clone(),
        stdout: Arc::new(Mutex::new(Vec::new())),
        stderr: Arc::new(Mutex::new(Vec::new())),
        stdin: Arc::new(tokio::sync::Mutex::new(None)),
        child: None,
    };
    let mut state = store.register_managed_process(process)?;
    spawn_detached_process_watcher(
        store.managed_process_registry(),
        store.clone(),
        event_context,
        notifications,
        app.cloned(),
    );
    state["backend"] = serde_json::json!("daytona");
    state["pidScope"] = serde_json::json!("sandbox");
    state["pid_scope"] = serde_json::json!("sandbox");
    state["sandboxId"] = serde_json::json!(sandbox_id);
    state["sandboxName"] = serde_json::json!(sandbox_name);
    state["stdoutPath"] = serde_json::json!(stdout_path);
    state["stderrPath"] = serde_json::json!(stderr_path);
    state["_hint"] = serde_json::json!(
        "Daytona background process is tracked as a detached sandbox PID. Use process(action='log'|'state'|'count'|'kill') to inspect or manage it; stdin is unavailable."
    );
    if let Some(note) = notification_options.conflict_note {
        state["watchPatternsIgnored"] = serde_json::json!(note);
    }
    Ok(serde_json::to_string_pretty(&state)?)
}

const DAYTONA_PROCESS_COMMAND_SCRIPT: &str = r#"
import shlex
import sys

action = sys.argv[1] if len(sys.argv) > 1 else "status"
name = sys.argv[2] if len(sys.argv) > 2 else ""
path = sys.argv[3] if len(sys.argv) > 3 else ""

try:
    from daytona import Daytona
    sandbox = Daytona().get(name)
    try:
        sandbox.start()
    except Exception:
        pass
except Exception as exc:
    print(str(exc))
    raise SystemExit(1)

def exec_shell(command: str):
    response = sandbox.process.exec(command)
    output = getattr(response, "result", "") or ""
    code = int(getattr(response, "exit_code", 0) or 0)
    if output:
        print(output, end="" if output.endswith("\n") else "\n")
    raise SystemExit(code)

if action == "status":
    exec_shell(f"pid=$(cat {shlex.quote(path)} 2>/dev/null); test -n \"$pid\" && kill -0 \"$pid\" 2>/dev/null")
elif action == "kill":
    exec_shell(f"pid=$(cat {shlex.quote(path)} 2>/dev/null); test -n \"$pid\" && kill \"$pid\" 2>/dev/null")
elif action == "stdout" or action == "stderr":
    exec_shell(f"tail -n 2000 {shlex.quote(path)} 2>/dev/null")
elif action == "exit":
    exec_shell(f"cat {shlex.quote(path)} 2>/dev/null")
elif action == "cleanup":
    paths = [p for p in path.split("|") if p]
    if paths:
        exec_shell("rm -f " + " ".join(shlex.quote(p) for p in paths))
    raise SystemExit(0)
else:
    print(f"unknown action: {action}")
    raise SystemExit(2)
"#;

pub(super) async fn start_managed_process(
    store: &AppStore,
    agent: &AgentDefinition,
    conversation_id: &str,
    run_id: &str,
    payload: &Value,
    app: Option<&AppHandle>,
) -> AppResult<String> {
    let command = payload
        .get("command")
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::BadRequest("process start requires command".into()))?;
    let cwd = workspace_cwd(agent, payload.get("cwd").or_else(|| payload.get("workdir")))?;
    let label = payload
        .get("label")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(command)
        .to_string();
    let task_id = terminal_session_id(payload);
    let notification_options = process_notification_options(payload);
    ensure_command_not_hardline(command)?;
    let chat_config = store.config()?.chat;
    let allowed_env = tool_env_passthrough(store, Some(agent), &chat_config.tool_env_passthrough);
    let tail_retention_lines = positive_or_default(chat_config.tool_output_max_lines, 2_000);
    let mut child = shell_command(command);
    apply_command_env_guard(&mut child, &allowed_env);
    child
        .current_dir(&cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = child.spawn()?;
    let stdout_lines = Arc::new(Mutex::new(Vec::new()));
    let stderr_lines = Arc::new(Mutex::new(Vec::new()));
    let stdin = Arc::new(tokio::sync::Mutex::new(child.stdin.take()));
    let notifications = Arc::new(Mutex::new(ManagedProcessNotificationState::default()));
    let process_id = new_id("proc");
    let pid = child.id();
    let event_context = ManagedProcessEventContext {
        process_id: process_id.clone(),
        label: label.clone(),
        command: command.to_string(),
        cwd: Some(cwd.display().to_string()),
        pid,
        conversation_id: conversation_id.to_string(),
        run_id: run_id.to_string(),
    };
    if let Some(stdout) = child.stdout.take() {
        spawn_output_collector(
            stdout,
            stdout_lines.clone(),
            "stdout",
            notification_options.watch_patterns.clone(),
            notifications.clone(),
            store.clone(),
            app.cloned(),
            event_context.clone(),
            tail_retention_lines,
        );
    }
    if let Some(stderr) = child.stderr.take() {
        spawn_output_collector(
            stderr,
            stderr_lines.clone(),
            "stderr",
            notification_options.watch_patterns.clone(),
            notifications.clone(),
            store.clone(),
            app.cloned(),
            event_context.clone(),
            tail_retention_lines,
        );
    }
    let process = ManagedProcess {
        id: process_id,
        label,
        command: command.to_string(),
        cwd: Some(cwd.display().to_string()),
        pid,
        backend: "local".into(),
        env_type: normalized_terminal_env(),
        status_command: None,
        kill_command: None,
        stdout_command: None,
        stderr_command: None,
        exit_command: None,
        cleanup_command: None,
        exit_code: None,
        task_id,
        conversation_id: conversation_id.to_string(),
        run_id: run_id.to_string(),
        detached: false,
        pid_scope: "host".into(),
        started_at: now_iso(),
        finished_at: None,
        finished_at_instant: None,
        notify_on_complete: notification_options.notify_on_complete,
        watch_patterns: notification_options.watch_patterns.clone(),
        tail_retention_lines,
        notifications: notifications.clone(),
        stdout: stdout_lines,
        stderr: stderr_lines,
        stdin,
        child: Some(child),
    };
    let mut state = store.register_managed_process(process)?;
    spawn_completion_watcher(
        store.managed_process_registry(),
        store.clone(),
        event_context,
        notifications,
        app.cloned(),
    );
    if let Some(hint) = silent_background_process_hint(payload) {
        state["_hint"] = serde_json::json!(hint);
    }
    if let Some(note) = notification_options.conflict_note {
        state["watchPatternsIgnored"] = serde_json::json!(note);
    }
    Ok(serde_json::to_string_pretty(&state)?)
}

pub(super) fn silent_background_process_hint(payload: &Value) -> Option<&'static str> {
    let options = process_notification_options(payload);
    if options.notify_on_complete || !options.watch_patterns.is_empty() {
        return None;
    }
    Some(
        "This managed process is running silently. Poll it with process(action='state', processId='...') or process(action='list') before relying on its result. Hermes uses notify_on_complete/watch_patterns to avoid silent background jobs; SynthChat can emit automatic completion/watch events when either option is set.",
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ProcessNotificationOptions {
    pub(super) notify_on_complete: bool,
    pub(super) watch_patterns: Vec<String>,
    pub(super) conflict_note: Option<String>,
}

pub(super) fn process_notification_options(payload: &Value) -> ProcessNotificationOptions {
    let notify_on_complete = value_bool_like(payload, &["notifyOnComplete", "notify_on_complete"])
        .unwrap_or(false);
    let mut watch_patterns = value_string_list_like(payload, &["watchPatterns", "watch_patterns"]);
    let conflict_note = if notify_on_complete && !watch_patterns.is_empty() {
        watch_patterns.clear();
        Some(
            "watchPatterns ignored because notifyOnComplete=true; completion notification is the preferred signal for bounded background jobs."
                .into(),
        )
    } else {
        None
    };
    ProcessNotificationOptions {
        notify_on_complete,
        watch_patterns,
        conflict_note,
    }
}

fn value_bool_like(payload: &Value, keys: &[&str]) -> Option<bool> {
    for key in keys {
        let Some(value) = payload.get(*key) else {
            continue;
        };
        if let Some(flag) = value.as_bool() {
            return Some(flag);
        }
        if let Some(text) = value.as_str() {
            let normalized = text.trim().to_ascii_lowercase();
            match normalized.as_str() {
                "true" | "1" | "yes" | "y" | "on" => return Some(true),
                "false" | "0" | "no" | "n" | "off" => return Some(false),
                _ => {}
            }
        }
    }
    None
}

fn value_u64_like(payload: &Value, keys: &[&str]) -> Option<u64> {
    for key in keys {
        let Some(value) = payload.get(*key) else {
            continue;
        };
        if let Some(number) = value.as_u64() {
            return Some(number);
        }
        if let Some(text) = value.as_str() {
            if let Ok(number) = text.trim().parse::<u64>() {
                return Some(number);
            }
        }
    }
    None
}

fn value_string_list_like(payload: &Value, keys: &[&str]) -> Vec<String> {
    for key in keys {
        let Some(value) = payload.get(*key) else {
            continue;
        };
        if let Some(items) = value.as_array() {
            return items
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(ToString::to_string)
                .collect();
        }
        if let Some(text) = value.as_str() {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Ok(Value::Array(items)) = serde_json::from_str::<Value>(trimmed) {
                let parsed = items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::trim)
                    .filter(|text| !text.is_empty())
                    .map(ToString::to_string)
                    .collect::<Vec<_>>();
                if !parsed.is_empty() {
                    return parsed;
                }
            }
            let fallback = trimmed
                .trim_start_matches('[')
                .trim_end_matches(']')
                .split(|ch: char| matches!(ch, ',' | ';' | '\n' | '\r' | '\t'))
                .map(str::trim)
                .map(|item| item.trim_matches('"').trim_matches('\'').trim())
                .filter(|item| !item.is_empty())
                .map(ToString::to_string)
                .collect::<Vec<_>>();
            if !fallback.is_empty() {
                return fallback;
            }
        }
    }
    Vec::new()
}

#[derive(Clone)]
struct ManagedProcessEventContext {
    process_id: String,
    label: String,
    command: String,
    cwd: Option<String>,
    pid: Option<u32>,
    conversation_id: String,
    run_id: String,
}

fn spawn_output_collector<R>(
    stream: R,
    lines: Arc<Mutex<Vec<String>>>,
    stream_name: &'static str,
    watch_patterns: Vec<String>,
    notifications: Arc<Mutex<ManagedProcessNotificationState>>,
    store: AppStore,
    app: Option<AppHandle>,
    context: ManagedProcessEventContext,
    tail_retention_lines: usize,
) where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut reader = BufReader::new(stream).lines();
        while let Ok(Some(line)) = reader.next_line().await {
            let Ok(mut lines) = lines.lock() else {
                break;
            };
            lines.push(line.clone());
            let overflow = lines.len().saturating_sub(tail_retention_lines);
            if overflow > 0 {
                lines.drain(0..overflow);
            }
            drop(lines);
            emit_watch_match_events(
                &watch_patterns,
                &notifications,
                &store,
                app.as_ref(),
                &context,
                stream_name,
                &line,
            );
        }
    });
}

fn emit_watch_match_events(
    watch_patterns: &[String],
    notifications: &Arc<Mutex<ManagedProcessNotificationState>>,
    store: &AppStore,
    app: Option<&AppHandle>,
    context: &ManagedProcessEventContext,
    stream_name: &str,
    line: &str,
) {
    if watch_patterns.is_empty() {
        return;
    }
    for pattern in watch_patterns {
        if !line.contains(pattern) {
            continue;
        }
        let mut disable_event = None;
        let mut match_event = None;
        {
            let Ok(mut state) = notifications.lock() else {
                return;
            };
            if state.watch_disabled {
                return;
            }
            let matched_at = now_iso();
            state.watch_match_count = state.watch_match_count.saturating_add(1);
            state
                .watch_first_match_at
                .get_or_insert_with(|| matched_at.clone());
            state.watch_last_match_at = Some(matched_at.clone());
            *state
                .watch_matches_by_pattern
                .entry(pattern.clone())
                .or_insert(0) += 1;
            *state
                .watch_matches_by_stream
                .entry(stream_name.to_string())
                .or_insert(0) += 1;
            if state
                .last_watch_emit
                .is_some_and(|last| last.elapsed() < Duration::from_secs(15))
            {
                state.watch_dropped_count += 1;
                state.watch_strike_count += 1;
                if state.watch_strike_count >= 3 {
                    state.watch_disabled = true;
                    let event = managed_process_event(
                        "watch_disabled",
                        context,
                        serde_json::json!({
                            "reason": "watchPatterns produced too many notifications inside the 15s cooldown; falling back to notifyOnComplete semantics.",
                            "watchStrikeCount": state.watch_strike_count,
                            "watchDroppedCount": state.watch_dropped_count,
                            "watchMatchCount": state.watch_match_count,
                            "watchEmitCount": state.watch_emit_count,
                        }),
                    );
                    push_managed_process_notification(&mut state, event.clone());
                    disable_event = Some(event);
                }
            } else {
                state.last_watch_emit = Some(std::time::Instant::now());
                state.watch_strike_count = 0;
                let (global_admit, global_tripped) = admit_global_watch_notification();
                if !global_admit {
                    state.watch_global_suppressed_count =
                        state.watch_global_suppressed_count.saturating_add(1);
                    state.watch_global_last_suppressed_at = Some(matched_at);
                    if global_tripped {
                        state.watch_global_tripped_count =
                            state.watch_global_tripped_count.saturating_add(1);
                    }
                    return;
                }
                state.watch_emit_count = state.watch_emit_count.saturating_add(1);
                state.watch_last_emit_at = Some(matched_at);
                let event = managed_process_event(
                    "watch_match",
                    context,
                    serde_json::json!({
                        "stream": stream_name,
                        "pattern": pattern,
                        "line": line,
                        "watchMatchCount": state.watch_match_count,
                        "watchEmitCount": state.watch_emit_count,
                        "watchGlobalSuppressedCount": state.watch_global_suppressed_count,
                        "watchGlobalTrippedCount": state.watch_global_tripped_count,
                    }),
                );
                push_managed_process_notification(&mut state, event.clone());
                match_event = Some(event);
            }
        }
        if let Some(event) = match_event.or(disable_event) {
            emit_managed_process_event(app, event.clone());
            persist_managed_process_event(store, app, &context.conversation_id, event);
        }
    }
}

fn admit_global_watch_notification() -> (bool, bool) {
    let now = Instant::now();
    let limiter = PROCESS_WATCH_GLOBAL_LIMITER.get_or_init(|| {
        Mutex::new(ProcessWatchGlobalLimiter {
            window_start: Some(now),
            window_hits: 0,
            tripped_until: None,
        })
    });
    let Ok(mut limiter) = limiter.lock() else {
        return (true, false);
    };
    if let Some(tripped_until) = limiter.tripped_until {
        if now < tripped_until {
            return (false, false);
        }
        limiter.tripped_until = None;
        limiter.window_start = Some(now);
        limiter.window_hits = 0;
    }
    let window_start = limiter.window_start.get_or_insert(now);
    if now.duration_since(*window_start) >= WATCH_GLOBAL_WINDOW {
        limiter.window_start = Some(now);
        limiter.window_hits = 0;
    }
    if limiter.window_hits >= WATCH_GLOBAL_MAX_PER_WINDOW {
        limiter.tripped_until = Some(now + WATCH_GLOBAL_COOLDOWN);
        return (false, true);
    }
    limiter.window_hits += 1;
    (true, false)
}

fn spawn_completion_watcher(
    processes: Arc<Mutex<std::collections::HashMap<String, ManagedProcess>>>,
    store: AppStore,
    context: ManagedProcessEventContext,
    notifications: Arc<Mutex<ManagedProcessNotificationState>>,
    app: Option<AppHandle>,
) {
    tokio::spawn(async move {
        loop {
            let mut should_sleep = false;
            let mut completion_event = None;
            {
                let Ok(mut processes) = processes.lock() else {
                    break;
                };
                let Some(process) = processes.get_mut(&context.process_id) else {
                    break;
                };
                let Some(child) = process.child.as_mut() else {
                    break;
                };
                match child.try_wait() {
                    Ok(Some(status)) => {
                        if process.finished_at.is_none() {
                            process.finished_at = Some(now_iso());
                            process.finished_at_instant = Some(std::time::Instant::now());
                        }
                        let should_notify = process.notify_on_complete
                            || notifications
                                .lock()
                                .ok()
                                .map(|state| state.watch_disabled)
                                .unwrap_or(false);
                        if let Ok(mut state) = notifications.lock() {
                            if state.completion_notified {
                                break;
                            }
                            state.completion_notified = true;
                            if should_notify {
                                let event = managed_process_event(
                                    "completed",
                                    &context,
                                    serde_json::json!({
                                        "status": "exited",
                                        "exitCode": status.code(),
                                    }),
                                );
                                push_managed_process_notification(&mut state, event.clone());
                                completion_event = Some(event);
                            }
                        }
                    }
                    Ok(None) => {
                        should_sleep = true;
                    }
                    Err(_) => {
                        should_sleep = true;
                    }
                }
            }
            if should_sleep {
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }
            if let Some(event) = completion_event {
                emit_managed_process_event(app.as_ref(), event.clone());
                persist_managed_process_event(
                    &store,
                    app.as_ref(),
                    &context.conversation_id,
                    event,
                );
            }
            let _ = store.persist_managed_process_checkpoint();
            break;
        }
    });
}

fn spawn_detached_process_watcher(
    processes: Arc<Mutex<std::collections::HashMap<String, ManagedProcess>>>,
    store: AppStore,
    context: ManagedProcessEventContext,
    notifications: Arc<Mutex<ManagedProcessNotificationState>>,
    app: Option<AppHandle>,
) -> bool {
    if !register_detached_process_watcher(&context.process_id) {
        return false;
    }
    tokio::spawn(async move {
        let mut stdout_seen = 0usize;
        let mut stderr_seen = 0usize;
        loop {
            let (
                status_command,
                stdout_command,
                stderr_command,
                exit_command,
                stdout_lines,
                stderr_lines,
                watch_patterns,
                tail_retention_lines,
            ) = {
                let Ok(processes) = processes.lock() else {
                    break;
                };
                let Some(process) = processes.get(&context.process_id) else {
                    break;
                };
                if !process.detached || process.finished_at.is_some() {
                    break;
                }
                let Some(status_command) = process.status_command.clone() else {
                    break;
                };
                (
                    status_command,
                    process.stdout_command.clone(),
                    process.stderr_command.clone(),
                    process.exit_command.clone(),
                    process.stdout.clone(),
                    process.stderr.clone(),
                    process.watch_patterns.clone(),
                    process.tail_retention_lines,
                )
            };

            if let Some(lines) = stdout_command
                .as_deref()
                .and_then(command_vec_output_lines_for_watcher)
            {
                let emit_from = if lines.len() < stdout_seen {
                    0
                } else {
                    stdout_seen
                };
                for line in lines.iter().skip(emit_from) {
                    emit_watch_match_events(
                        &watch_patterns,
                        &notifications,
                        &store,
                        app.as_ref(),
                        &context,
                        "stdout",
                        line,
                    );
                }
                stdout_seen = lines.len();
                if let Ok(mut stdout) = stdout_lines.lock() {
                    *stdout = lines;
                    let overflow = stdout.len().saturating_sub(tail_retention_lines);
                    if overflow > 0 {
                        stdout.drain(0..overflow);
                        stdout_seen = stdout.len();
                    }
                }
            }

            if let Some(lines) = stderr_command
                .as_deref()
                .and_then(command_vec_output_lines_for_watcher)
            {
                let emit_from = if lines.len() < stderr_seen {
                    0
                } else {
                    stderr_seen
                };
                for line in lines.iter().skip(emit_from) {
                    emit_watch_match_events(
                        &watch_patterns,
                        &notifications,
                        &store,
                        app.as_ref(),
                        &context,
                        "stderr",
                        line,
                    );
                }
                stderr_seen = lines.len();
                if let Ok(mut stderr) = stderr_lines.lock() {
                    *stderr = lines;
                    let overflow = stderr.len().saturating_sub(tail_retention_lines);
                    if overflow > 0 {
                        stderr.drain(0..overflow);
                        stderr_seen = stderr.len();
                    }
                }
            }

            if command_vec_success_for_watcher(&status_command) {
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }

            let exit_code = exit_command
                .as_deref()
                .and_then(command_vec_output_i32_for_watcher);
            let mut completion_event = None;
            {
                let Ok(mut processes) = processes.lock() else {
                    break;
                };
                let Some(process) = processes.get_mut(&context.process_id) else {
                    break;
                };
                if process.finished_at.is_some() {
                    break;
                }
                process.exit_code = exit_code;
                process.finished_at = Some(now_iso());
                process.finished_at_instant = Some(std::time::Instant::now());
                let should_notify = process.notify_on_complete
                    || notifications
                        .lock()
                        .ok()
                        .map(|state| state.watch_disabled)
                        .unwrap_or(false);
                if let Ok(mut state) = notifications.lock() {
                    if state.completion_notified {
                        break;
                    }
                    state.completion_notified = true;
                    if should_notify {
                        let event = managed_process_event(
                            "completed",
                            &context,
                            serde_json::json!({
                                "status": "exited",
                                "exitCode": exit_code,
                                "detached": true,
                            }),
                        );
                        push_managed_process_notification(&mut state, event.clone());
                        completion_event = Some(event);
                    }
                }
            }
            if let Some(event) = completion_event {
                emit_managed_process_event(app.as_ref(), event.clone());
                persist_managed_process_event(
                    &store,
                    app.as_ref(),
                    &context.conversation_id,
                    event,
                );
            }
            let _ = store.persist_managed_process_checkpoint();
            break;
        }
        unregister_detached_process_watcher(&context.process_id);
    });
    true
}

fn register_detached_process_watcher(process_id: &str) -> bool {
    let watchers = DETACHED_PROCESS_WATCHERS.get_or_init(|| Mutex::new(HashSet::new()));
    let Ok(mut watchers) = watchers.lock() else {
        return false;
    };
    watchers.insert(process_id.to_string())
}

fn unregister_detached_process_watcher(process_id: &str) {
    if let Ok(mut watchers) = DETACHED_PROCESS_WATCHERS
        .get_or_init(|| Mutex::new(HashSet::new()))
        .lock()
    {
        watchers.remove(process_id);
    }
}

fn spawn_detached_watchers_for_recovered(
    store: &AppStore,
    app: Option<&AppHandle>,
    result: &Value,
) {
    let recovered = result
        .get("recovered")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for entry in recovered {
        let Some(process_id) = entry
            .get("session_id")
            .or_else(|| entry.get("sessionId"))
            .and_then(Value::as_str)
            .map(str::to_string)
        else {
            continue;
        };
        let Some((context, notifications)) = detached_process_watcher_context(store, &process_id)
        else {
            continue;
        };
        spawn_detached_process_watcher(
            store.managed_process_registry(),
            store.clone(),
            context,
            notifications,
            app.cloned(),
        );
    }
}

pub(super) fn reattach_detached_process_watchers(
    store: &AppStore,
    app: Option<&AppHandle>,
) -> usize {
    let process_ids = {
        let processes = store.managed_process_registry();
        let Ok(processes) = processes.lock() else {
            return 0;
        };
        processes
            .values()
            .filter(|process| {
                process.detached
                    && process.finished_at.is_none()
                    && process.status_command.is_some()
            })
            .map(|process| process.id.clone())
            .collect::<Vec<_>>()
    };
    let mut count = 0usize;
    for process_id in process_ids {
        let Some((context, notifications)) = detached_process_watcher_context(store, &process_id)
        else {
            continue;
        };
        let spawned = spawn_detached_process_watcher(
            store.managed_process_registry(),
            store.clone(),
            context,
            notifications,
            app.cloned(),
        );
        if spawned {
            count += 1;
        }
    }
    count
}

fn detached_process_watcher_context(
    store: &AppStore,
    process_id: &str,
) -> Option<(
    ManagedProcessEventContext,
    Arc<Mutex<ManagedProcessNotificationState>>,
)> {
    let processes = store.managed_process_registry();
    let processes = processes.lock().ok()?;
    let process = processes.get(process_id)?;
    if !process.detached || process.finished_at.is_some() || process.status_command.is_none() {
        return None;
    }
    Some((
        ManagedProcessEventContext {
            process_id: process.id.clone(),
            label: process.label.clone(),
            command: process.command.clone(),
            cwd: process.cwd.clone(),
            pid: process.pid,
            conversation_id: process.conversation_id.clone(),
            run_id: process.run_id.clone(),
        },
        process.notifications.clone(),
    ))
}

fn command_vec_success_for_watcher(command: &[String]) -> bool {
    let Some((program, args)) = command.split_first() else {
        return false;
    };
    StdCommand::new(program)
        .hide_window()
        .args(args)
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn command_vec_output_lines_for_watcher(command: &[String]) -> Option<Vec<String>> {
    let (program, args) = command.split_first()?;
    let output = StdCommand::new(program)
        .hide_window()
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(
        decode_terminal_output(&output.stdout)
            .lines()
            .map(str::to_string)
            .collect(),
    )
}

fn command_vec_output_i32_for_watcher(command: &[String]) -> Option<i32> {
    let line = command_vec_output_lines_for_watcher(command)?
        .into_iter()
        .find(|line| !line.trim().is_empty())?;
    line.trim().parse::<i32>().ok()
}

fn managed_process_event(
    event_type: &str,
    context: &ManagedProcessEventContext,
    detail: Value,
) -> Value {
    serde_json::json!({
        "type": event_type,
        "processId": context.process_id,
        "label": context.label,
        "command": context.command,
        "cwd": context.cwd,
        "pid": context.pid,
        "conversationId": context.conversation_id,
        "runId": context.run_id,
        "detail": detail,
        "createdAt": now_iso(),
    })
}

fn managed_process_event_from_snapshot(
    event_type: &str,
    conversation_id: &str,
    run_id: &str,
    snapshot: &Value,
    detail: Value,
) -> Value {
    serde_json::json!({
        "type": event_type,
        "processId": snapshot.get("id").and_then(Value::as_str).unwrap_or(""),
        "label": snapshot.get("label").and_then(Value::as_str).unwrap_or(""),
        "command": snapshot.get("command").and_then(Value::as_str).unwrap_or(""),
        "cwd": snapshot.get("cwd").cloned().unwrap_or(serde_json::Value::Null),
        "pid": snapshot.get("pid").cloned().unwrap_or(serde_json::Value::Null),
        "conversationId": conversation_id,
        "runId": run_id,
        "detail": detail,
        "createdAt": now_iso(),
    })
}

fn push_managed_process_notification(state: &mut ManagedProcessNotificationState, event: Value) {
    state.recent_events.push(event);
    let overflow = state.recent_events.len().saturating_sub(80);
    if overflow > 0 {
        state.recent_events.drain(0..overflow);
    }
}

fn emit_managed_process_event(app: Option<&AppHandle>, event: Value) {
    if let Some(app) = app {
        let _ = app.emit("synthchat-managed-process-event", event);
    }
}

fn persist_managed_process_event(
    store: &AppStore,
    app: Option<&AppHandle>,
    conversation_id: &str,
    event: Value,
) {
    if conversation_id.trim().is_empty() {
        return;
    }
    let Ok(message) = store.append_message(ChatMessage::new(
        conversation_id.to_string(),
        "tool",
        serde_json::json!({
            "type": "managedProcessEvent",
            "event": event,
        })
        .to_string(),
        "desktop-agent-process",
    )) else {
        return;
    };
    if let Some(app) = app {
        let _ = app.emit(
            "synthchat-chat-event",
            serde_json::json!({
                "type": "tool_message",
                "conversationId": conversation_id,
                "message": message,
            }),
        );
    }
}

fn apply_command_env_guard(child: &mut Command, allowed_env: &[String]) {
    for name in sensitive_env_names_to_remove(allowed_env) {
        child.env_remove(name);
    }
}

pub(super) fn sensitive_env_names_to_remove(allowed_env: &[String]) -> Vec<String> {
    let allowed = allowed_env
        .iter()
        .map(|name| name.trim().to_ascii_uppercase())
        .filter(|name| !name.is_empty())
        .collect::<HashSet<_>>();
    env::vars()
        .filter_map(|(name, _)| {
            let upper = name.to_ascii_uppercase();
            (is_sensitive_env_name(&name)
                && (!allowed.contains(&upper) || is_provider_credential_env_name(&upper)))
            .then_some(name)
        })
        .collect()
}

pub(super) fn tool_env_passthrough(
    store: &AppStore,
    agent: Option<&AgentDefinition>,
    configured: &[String],
) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut allowed = Vec::new();
    for name in configured {
        push_env_passthrough_name(&mut allowed, &mut seen, name);
    }
    if let Ok(skills) = store.skills() {
        for skill in skills {
            let enabled_for_agent = agent.is_some_and(|agent| {
                agent.skills_enabled
                    && (skill.enabled || agent.enabled_skills.iter().any(|id| id == &skill.id))
            });
            if !enabled_for_agent && !skill.enabled {
                continue;
            }
            for name in skill.required_environment_variables {
                push_env_passthrough_name(&mut allowed, &mut seen, &name);
            }
        }
    }
    allowed
}

fn push_env_passthrough_name(allowed: &mut Vec<String>, seen: &mut HashSet<String>, name: &str) {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return;
    }
    let normalized = trimmed.to_ascii_uppercase();
    if seen.insert(normalized) {
        allowed.push(trimmed.to_string());
    }
}

fn is_provider_credential_env_name(upper: &str) -> bool {
    matches!(
        upper,
        "OPENAI_API_KEY"
            | "OPENAI_TOKEN"
            | "ANTHROPIC_API_KEY"
            | "ANTHROPIC_AUTH_TOKEN"
            | "ANTHROPIC_TOKEN"
            | "AZURE_OPENAI_API_KEY"
            | "GOOGLE_API_KEY"
            | "GEMINI_API_KEY"
            | "GROQ_API_KEY"
            | "MISTRAL_API_KEY"
            | "COHERE_API_KEY"
            | "OPENROUTER_API_KEY"
            | "DEEPSEEK_API_KEY"
            | "XAI_API_KEY"
            | "DASHSCOPE_API_KEY"
            | "MOONSHOT_API_KEY"
            | "QWEN_API_KEY"
            | "ZHIPUAI_API_KEY"
    ) || ((upper.starts_with("OPENAI_")
        || upper.starts_with("ANTHROPIC_")
        || upper.starts_with("AZURE_OPENAI_"))
        && is_sensitive_env_name(upper))
}

async fn run_shell_command(
    store: &AppStore,
    run_id: &str,
    command: &str,
    cwd: &Path,
    timeout_seconds: u64,
    max_output_chars: usize,
    allowed_env: &[String],
    stdin_data: Option<&str>,
) -> AppResult<String> {
    ensure_command_not_hardline(command)?;
    run_shell_command_unchecked(
        store,
        run_id,
        command,
        cwd,
        timeout_seconds,
        max_output_chars,
        allowed_env,
        stdin_data,
    )
    .await
}

pub(crate) fn decode_terminal_output(bytes: &[u8]) -> String {
    if let Ok(text) = std::str::from_utf8(bytes) {
        return text.to_string();
    }
    if let Some(text) = decode_utf16_terminal_output(bytes) {
        return text;
    }
    decode_platform_terminal_output(bytes)
}

fn decode_utf16_terminal_output(bytes: &[u8]) -> Option<String> {
    let (bytes, little_endian) = if bytes.starts_with(&[0xff, 0xfe]) {
        (&bytes[2..], true)
    } else if bytes.starts_with(&[0xfe, 0xff]) {
        (&bytes[2..], false)
    } else if bytes.len() >= 4
        && bytes
            .iter()
            .skip(1)
            .step_by(2)
            .take(16)
            .filter(|byte| **byte == 0)
            .count()
            >= 4
    {
        (bytes, true)
    } else {
        return None;
    };
    if bytes.len() % 2 != 0 {
        return None;
    }
    let units = bytes
        .chunks_exact(2)
        .map(|chunk| {
            if little_endian {
                u16::from_le_bytes([chunk[0], chunk[1]])
            } else {
                u16::from_be_bytes([chunk[0], chunk[1]])
            }
        })
        .collect::<Vec<_>>();
    Some(String::from_utf16_lossy(&units))
}

#[cfg(not(windows))]
fn decode_platform_terminal_output(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[cfg(windows)]
fn decode_platform_terminal_output(bytes: &[u8]) -> String {
    use windows_sys::Win32::Globalization::{GetACP, GetOEMCP};

    unsafe {
        for code_page in [GetACP(), GetOEMCP()] {
            if let Some(decoded) = decode_windows_code_page(bytes, code_page) {
                return decoded;
            }
        }
    }
    String::from_utf8_lossy(bytes).into_owned()
}

#[cfg(windows)]
unsafe fn decode_windows_code_page(bytes: &[u8], code_page: u32) -> Option<String> {
    use windows_sys::Win32::Globalization::MultiByteToWideChar;

    if bytes.is_empty() {
        return Some(String::new());
    }
    let input_len = i32::try_from(bytes.len()).ok()?;
    let wide_len = MultiByteToWideChar(
        code_page,
        0,
        bytes.as_ptr(),
        input_len,
        std::ptr::null_mut(),
        0,
    );
    if wide_len <= 0 {
        return None;
    }
    let mut wide = vec![0u16; wide_len as usize];
    let written = MultiByteToWideChar(
        code_page,
        0,
        bytes.as_ptr(),
        input_len,
        wide.as_mut_ptr(),
        wide_len,
    );
    if written <= 0 {
        return None;
    }
    wide.truncate(written as usize);
    Some(String::from_utf16_lossy(&wide))
}

#[cfg(all(test, windows))]
mod terminal_output_decode_tests {
    use super::{decode_terminal_output, decode_utf16_terminal_output, decode_windows_code_page};

    #[test]
    fn decodes_gbk_terminal_output() {
        let text = unsafe { decode_windows_code_page(&[0xd6, 0xd0, 0xce, 0xc4], 936) };
        assert_eq!(text.as_deref(), Some("中文"));
    }

    #[test]
    fn terminal_output_decodes_gbk_filename_bytes() {
        let bytes = b"C:\\Users\\33908\\Desktop\\2026\xc4\xea6\xd4\xc222\xc8\xd5\xc8\xc8\xb5\xe3\xd0\xc2\xce\xc5\xd5\xaa\xd2\xaa.txt\r\n";
        let text = decode_terminal_output(bytes);
        assert!(text.contains("2026年6月22日热点新闻摘要.txt"), "{text}");
    }

    #[test]
    fn terminal_output_decodes_utf16le_bytes() {
        let mut bytes = vec![0xff, 0xfe];
        for unit in "中文路径".encode_utf16() {
            bytes.extend(unit.to_le_bytes());
        }
        assert_eq!(
            decode_utf16_terminal_output(&bytes).as_deref(),
            Some("中文路径")
        );
    }
}

async fn run_shell_command_unchecked(
    store: &AppStore,
    run_id: &str,
    command: &str,
    cwd: &Path,
    timeout_seconds: u64,
    max_output_chars: usize,
    allowed_env: &[String],
    stdin_data: Option<&str>,
) -> AppResult<String> {
    let mut child = shell_command(command);
    apply_command_env_guard(&mut child, allowed_env);
    child
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if stdin_data.is_some() {
        child.stdin(Stdio::piped());
    }
    let mut child = child.spawn()?;
    if let Some(stdin_data) = stdin_data {
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(stdin_data.as_bytes()).await?;
            stdin.shutdown().await?;
        }
    }
    let output = wait_for_shell_output_interruptible(store, run_id, timeout_seconds, child).await?;
    let stdout = decode_terminal_output(&output.stdout);
    let stderr = decode_terminal_output(&output.stderr);
    let stdout = run_transform_terminal_output_hooks(
        store,
        run_id,
        command,
        &stdout,
        output.status.code().unwrap_or(-1),
    )
    .await;
    Ok(format!(
        "cwd: {}\nexitCode: {}\nstdout:\n{}\nstderr:\n{}",
        cwd.display(),
        output.status.code().unwrap_or(-1),
        truncate_output(&stdout, max_output_chars),
        truncate_output(&stderr, max_output_chars / 2)
    ))
}

async fn run_shell_command_with_cwd_capture(
    store: &AppStore,
    run_id: &str,
    command: &str,
    cwd: &Path,
    workspace_root: &Path,
    session_id: &str,
    timeout_seconds: u64,
    max_output_chars: usize,
    allowed_env: &[String],
    stdin_data: Option<&str>,
) -> AppResult<String> {
    ensure_command_not_hardline(command)?;
    let marker = format!(
        "__SYNTHCHAT_CWD_{}__",
        sanitize_terminal_session_key(session_id)
    );
    let wrapped = wrap_command_with_cwd_marker(command, &marker);
    let mut child = shell_command(&wrapped);
    apply_command_env_guard(&mut child, allowed_env);
    child
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if stdin_data.is_some() {
        child.stdin(Stdio::piped());
    }
    let mut child = child.spawn()?;
    if let Some(stdin_data) = stdin_data {
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(stdin_data.as_bytes()).await?;
            stdin.shutdown().await?;
        }
    }
    let output = wait_for_shell_output_interruptible(store, run_id, timeout_seconds, child).await?;
    let stdout = decode_terminal_output(&output.stdout);
    let stderr = decode_terminal_output(&output.stderr);
    let (stdout, next_cwd) = extract_cwd_marker(&stdout, &marker);
    let stdout = run_transform_terminal_output_hooks(
        store,
        run_id,
        command,
        &stdout,
        output.status.code().unwrap_or(-1),
    )
    .await;
    let mut session_note = None;
    if let Some(next_cwd) = next_cwd {
        match normalize_session_cwd(workspace_root, &next_cwd) {
            Some(next_cwd) => {
                set_terminal_session_cwd(session_id, next_cwd.clone());
                session_note = Some(format!("sessionCwd: {}", next_cwd.display()));
            }
            None => {
                session_note = Some(format!(
                    "sessionCwd: unchanged; marker path is outside workspace: {}",
                    next_cwd.display()
                ));
            }
        }
    }
    let session_note = session_note.unwrap_or_else(|| format!("sessionCwd: {}", cwd.display()));
    Ok(format!(
        "cwd: {}\n{}\nexitCode: {}\nstdout:\n{}\nstderr:\n{}",
        cwd.display(),
        session_note,
        output.status.code().unwrap_or(-1),
        truncate_output(&stdout, max_output_chars),
        truncate_output(&stderr, max_output_chars / 2)
    ))
}

async fn wait_for_shell_output_interruptible(
    store: &AppStore,
    run_id: &str,
    timeout_seconds: u64,
    child: tokio::process::Child,
) -> AppResult<std::process::Output> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_seconds);
    let wait = child.wait_with_output();
    tokio::pin!(wait);
    loop {
        tokio::select! {
            output = &mut wait => return Ok(output?),
            _ = tokio::time::sleep_until(deadline) => {
                return Err(AppError::BadRequest(format!(
                    "command timed out after {timeout_seconds}s"
                )));
            }
            _ = tokio::time::sleep(Duration::from_millis(250)) => {
                if agent_run_wait_was_interrupted(store, run_id)? {
                    return Err(AppError::BadRequest(
                        "tool canceled because the agent run ended".into(),
                    ));
                }
            }
        }
    }
}

fn terminal_session_id(payload: &Value) -> Option<String> {
    payload
        .get("taskId")
        .or_else(|| payload.get("task_id"))
        .or_else(|| payload.get("sessionId"))
        .or_else(|| payload.get("session_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn shell_hook_run_id<'a>(payload: &'a Value, fallback: &'a str) -> &'a str {
    payload
        .get("runId")
        .or_else(|| payload.get("run_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(fallback)
}

fn terminal_stdin_data(payload: &Value) -> Option<String> {
    payload
        .get("stdin")
        .or_else(|| payload.get("stdinData"))
        .or_else(|| payload.get("stdin_data"))
        .or_else(|| payload.get("input"))
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

fn terminal_session_cwd(session_id: &str) -> Option<PathBuf> {
    TERMINAL_SESSION_CWDS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .ok()
        .and_then(|sessions| sessions.get(session_id).cloned())
}

fn set_terminal_session_cwd(session_id: &str, cwd: PathBuf) {
    if let Ok(mut sessions) = TERMINAL_SESSION_CWDS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
    {
        sessions.insert(session_id.to_string(), cwd);
    }
}

fn terminal_session_snapshot() -> Value {
    let sessions = TERMINAL_SESSION_CWDS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock();
    let Ok(sessions) = sessions else {
        return serde_json::json!({
            "count": 0,
            "sessions": [],
            "error": "terminal session cwd lock poisoned"
        });
    };
    let mut rows = sessions
        .iter()
        .map(|(session_id, cwd)| {
            serde_json::json!({
                "sessionId": session_id,
                "cwd": cwd.to_string_lossy()
            })
        })
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        left.get("sessionId")
            .and_then(Value::as_str)
            .unwrap_or("")
            .cmp(right.get("sessionId").and_then(Value::as_str).unwrap_or(""))
    });
    serde_json::json!({
        "count": rows.len(),
        "sessions": rows
    })
}

fn clear_terminal_session_cwds(target: Option<&str>) -> usize {
    let sessions = TERMINAL_SESSION_CWDS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock();
    let Ok(mut sessions) = sessions else {
        return 0;
    };
    if let Some(target) = target {
        return sessions.remove(target).map(|_| 1).unwrap_or(0);
    }
    let count = sessions.len();
    sessions.clear();
    count
}

fn ssh_terminal_session_cwd(session_id: &str) -> Option<String> {
    SSH_TERMINAL_SESSION_CWDS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .ok()
        .and_then(|sessions| sessions.get(session_id).cloned())
}

fn set_ssh_terminal_session_cwd(session_id: &str, cwd: String) {
    if let Ok(mut sessions) = SSH_TERMINAL_SESSION_CWDS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
    {
        sessions.insert(session_id.to_string(), cwd);
    }
}

fn ssh_terminal_session_snapshot() -> Value {
    let sessions = SSH_TERMINAL_SESSION_CWDS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock();
    let Ok(sessions) = sessions else {
        return serde_json::json!({
            "count": 0,
            "sessions": [],
            "error": "ssh terminal session cwd lock poisoned"
        });
    };
    let mut rows = sessions
        .iter()
        .map(|(session_id, cwd)| {
            serde_json::json!({
                "sessionId": session_id,
                "cwd": cwd
            })
        })
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        left.get("sessionId")
            .and_then(Value::as_str)
            .unwrap_or("")
            .cmp(right.get("sessionId").and_then(Value::as_str).unwrap_or(""))
    });
    serde_json::json!({
        "count": rows.len(),
        "sessions": rows
    })
}

fn clear_ssh_terminal_session_cwds(target: Option<&str>) -> usize {
    let sessions = SSH_TERMINAL_SESSION_CWDS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock();
    let Ok(mut sessions) = sessions else {
        return 0;
    };
    if let Some(target) = target {
        return sessions.remove(target).map(|_| 1).unwrap_or(0);
    }
    let count = sessions.len();
    sessions.clear();
    count
}

fn modal_terminal_session_cwd(session_id: &str) -> Option<String> {
    MODAL_TERMINAL_SESSION_CWDS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .ok()
        .and_then(|sessions| sessions.get(session_id).cloned())
}

fn set_modal_terminal_session_cwd(session_id: &str, cwd: String) {
    if let Ok(mut sessions) = MODAL_TERMINAL_SESSION_CWDS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
    {
        sessions.insert(session_id.to_string(), cwd);
    }
}

fn modal_terminal_snapshot_id(session_id: &str) -> Option<String> {
    MODAL_TERMINAL_SNAPSHOTS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .ok()
        .and_then(|snapshots| snapshots.get(session_id).cloned())
}

fn modal_snapshot_store_path(store: &AppStore) -> PathBuf {
    store.data_dir().join("modal_snapshots.json")
}

fn modal_snapshot_key(session_id: &str) -> String {
    format!("direct:{session_id}")
}

fn load_modal_snapshot_store(store: &AppStore) -> AppResult<HashMap<String, String>> {
    let path = modal_snapshot_store_path(store);
    if !path.exists() {
        return Ok(HashMap::new());
    }
    let text = fs::read_to_string(&path)?;
    let value = serde_json::from_str::<Value>(&text).unwrap_or_else(|_| serde_json::json!({}));
    let snapshots = value
        .as_object()
        .map(|object| {
            object
                .iter()
                .filter_map(|(key, value)| {
                    value
                        .as_str()
                        .map(str::trim)
                        .filter(|snapshot_id| !snapshot_id.is_empty())
                        .map(|snapshot_id| (key.clone(), snapshot_id.to_string()))
                })
                .collect::<HashMap<_, _>>()
        })
        .unwrap_or_default();
    Ok(snapshots)
}

fn save_modal_snapshot_store(
    store: &AppStore,
    snapshots: &HashMap<String, String>,
) -> AppResult<()> {
    let path = modal_snapshot_store_path(store);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, serde_json::to_vec_pretty(snapshots)?)?;
    Ok(())
}

fn modal_persisted_snapshot_id(store: &AppStore, session_id: &str) -> AppResult<Option<String>> {
    let mut snapshots = load_modal_snapshot_store(store)?;
    let key = modal_snapshot_key(session_id);
    if let Some(snapshot_id) = snapshots.get(&key).cloned() {
        return Ok(Some(snapshot_id));
    }
    let Some(legacy_snapshot_id) = snapshots.get(session_id).cloned() else {
        return Ok(None);
    };
    snapshots.insert(key, legacy_snapshot_id.clone());
    snapshots.remove(session_id);
    save_modal_snapshot_store(store, &snapshots)?;
    Ok(Some(legacy_snapshot_id))
}

fn set_modal_persisted_snapshot_id(
    store: &AppStore,
    session_id: &str,
    snapshot_id: &str,
) -> AppResult<()> {
    let snapshot_id = snapshot_id.trim();
    if snapshot_id.is_empty() {
        return Ok(());
    }
    let mut snapshots = load_modal_snapshot_store(store)?;
    snapshots.insert(modal_snapshot_key(session_id), snapshot_id.to_string());
    snapshots.remove(session_id);
    save_modal_snapshot_store(store, &snapshots)
}

fn clear_modal_persisted_snapshots(store: &AppStore, target: Option<&str>) -> AppResult<usize> {
    let mut snapshots = load_modal_snapshot_store(store)?;
    if snapshots.is_empty() {
        return Ok(0);
    }
    let before = snapshots.len();
    if let Some(target) = target {
        let key = modal_snapshot_key(target);
        snapshots.retain(|snapshot_key, _| snapshot_key != &key && snapshot_key != target);
    } else {
        snapshots.clear();
    }
    let cleared = before.saturating_sub(snapshots.len());
    if cleared > 0 {
        save_modal_snapshot_store(store, &snapshots)?;
    }
    Ok(cleared)
}

fn modal_persisted_snapshot_count(store: &AppStore) -> usize {
    load_modal_snapshot_store(store)
        .map(|snapshots| snapshots.len())
        .unwrap_or(0)
}

fn set_modal_terminal_snapshot_id(session_id: &str, snapshot_id: String) {
    if snapshot_id.trim().is_empty() {
        return;
    }
    if let Ok(mut snapshots) = MODAL_TERMINAL_SNAPSHOTS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
    {
        snapshots.insert(session_id.to_string(), snapshot_id);
    }
}

fn clear_modal_terminal_snapshot_id(session_id: &str) -> usize {
    MODAL_TERMINAL_SNAPSHOTS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .ok()
        .and_then(|mut snapshots| snapshots.remove(session_id))
        .map(|_| 1)
        .unwrap_or(0)
}

fn modal_terminal_session_snapshot() -> Value {
    let sessions = MODAL_TERMINAL_SESSION_CWDS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock();
    let snapshots = MODAL_TERMINAL_SNAPSHOTS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock();
    let (Ok(sessions), Ok(snapshots)) = (sessions, snapshots) else {
        return serde_json::json!({
            "count": 0,
            "sessions": [],
            "error": "modal terminal session lock poisoned"
        });
    };
    let mut keys = sessions.keys().cloned().collect::<HashSet<_>>();
    keys.extend(snapshots.keys().cloned());
    let mut rows = keys
        .into_iter()
        .map(|session_id| {
            serde_json::json!({
                "sessionId": session_id,
                "cwd": sessions.get(&session_id).cloned().unwrap_or_default(),
                "snapshotId": snapshots.get(&session_id).cloned().unwrap_or_default()
            })
        })
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        left.get("sessionId")
            .and_then(Value::as_str)
            .unwrap_or("")
            .cmp(right.get("sessionId").and_then(Value::as_str).unwrap_or(""))
    });
    serde_json::json!({
        "count": rows.len(),
        "sessions": rows
    })
}

fn clear_modal_terminal_sessions(target: Option<&str>) -> usize {
    let mut cleared = 0usize;
    if let Ok(mut sessions) = MODAL_TERMINAL_SESSION_CWDS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
    {
        if let Some(target) = target {
            cleared += sessions.remove(target).map(|_| 1).unwrap_or(0);
        } else {
            cleared += sessions.len();
            sessions.clear();
        }
    }
    if let Ok(mut snapshots) = MODAL_TERMINAL_SNAPSHOTS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
    {
        if let Some(target) = target {
            cleared += snapshots.remove(target).map(|_| 1).unwrap_or(0);
        } else {
            cleared += snapshots.len();
            snapshots.clear();
        }
    }
    cleared
}

fn daytona_terminal_session_cwd(session_id: &str) -> Option<String> {
    DAYTONA_TERMINAL_SESSION_CWDS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .ok()
        .and_then(|sessions| sessions.get(session_id).cloned())
}

fn set_daytona_terminal_session_cwd(session_id: &str, cwd: String) {
    if let Ok(mut sessions) = DAYTONA_TERMINAL_SESSION_CWDS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
    {
        sessions.insert(session_id.to_string(), cwd);
    }
}

fn daytona_terminal_session_snapshot() -> Value {
    let sessions = DAYTONA_TERMINAL_SESSION_CWDS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock();
    let Ok(sessions) = sessions else {
        return serde_json::json!({
            "count": 0,
            "sessions": [],
            "error": "daytona terminal session lock poisoned"
        });
    };
    let mut rows = sessions
        .iter()
        .map(|(session_id, cwd)| {
            serde_json::json!({
                "sessionId": session_id,
                "cwd": cwd,
                "sandboxName": format!("synthchat-{}", sanitize_terminal_session_key(session_id))
            })
        })
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        left.get("sessionId")
            .and_then(Value::as_str)
            .unwrap_or("")
            .cmp(right.get("sessionId").and_then(Value::as_str).unwrap_or(""))
    });
    serde_json::json!({
        "count": rows.len(),
        "sessions": rows
    })
}

fn daytona_terminal_session_ids(target: Option<&str>) -> Vec<String> {
    if let Some(target) = target {
        return vec![target.to_string()];
    }
    let sessions = DAYTONA_TERMINAL_SESSION_CWDS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock();
    let Ok(sessions) = sessions else {
        return vec!["default".into()];
    };
    if sessions.is_empty() {
        vec!["default".into()]
    } else {
        let mut rows = sessions.keys().cloned().collect::<Vec<_>>();
        rows.sort();
        rows
    }
}

fn clear_daytona_terminal_sessions(target: Option<&str>) -> usize {
    let sessions = DAYTONA_TERMINAL_SESSION_CWDS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock();
    let Ok(mut sessions) = sessions else {
        return 0;
    };
    if let Some(target) = target {
        return sessions.remove(target).map(|_| 1).unwrap_or(0);
    }
    let count = sessions.len();
    sessions.clear();
    count
}

fn sanitize_terminal_session_key(value: &str) -> String {
    let sanitized = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    if sanitized.is_empty() {
        "default".into()
    } else {
        sanitized
    }
}

fn wrap_command_with_cwd_marker(command: &str, marker: &str) -> String {
    #[cfg(windows)]
    {
        format!(
            "& {{ {command} }}; $synthchatEc = if ($LASTEXITCODE -ne $null) {{ $LASTEXITCODE }} else {{ 0 }}; Write-Output \"{marker}$((Get-Location).ProviderPath){marker}\"; exit $synthchatEc"
        )
    }
    #[cfg(not(windows))]
    {
        wrap_posix_command_with_cwd_marker(command, marker)
    }
}

fn wrap_posix_command_with_cwd_marker(command: &str, marker: &str) -> String {
    format!(
        "{{ {command}; }}; synthchat_ec=$?; printf '\\n{marker}%s{marker}\\n' \"$(pwd -P)\"; exit $synthchat_ec"
    )
}

pub(super) fn extract_cwd_marker(stdout: &str, marker: &str) -> (String, Option<PathBuf>) {
    let mut cwd = None;
    let mut kept = Vec::new();
    for line in stdout.lines() {
        if let Some(start) = line.find(marker) {
            let after = start + marker.len();
            if let Some(end) = line[after..].rfind(marker) {
                cwd = Some(PathBuf::from(&line[after..after + end]));
                continue;
            }
        }
        kept.push(line);
    }
    let mut output = kept.join("\n");
    if stdout.ends_with('\n') && !output.is_empty() {
        output.push('\n');
    }
    (output, cwd)
}

fn normalize_session_cwd(workspace_root: &Path, cwd: &Path) -> Option<PathBuf> {
    let root = workspace_root.canonicalize().ok()?;
    let cwd = cwd.canonicalize().ok()?;
    cwd.starts_with(&root).then_some(cwd)
}

async fn run_ssh_terminal_command(
    store: &AppStore,
    command: &str,
    payload: &Value,
    timeout_seconds: u64,
    max_output_chars: usize,
    stdin_data: Option<&str>,
) -> AppResult<String> {
    ensure_command_not_hardline(command)?;
    let ssh = find_executable("ssh")
        .or_else(common_ssh_path)
        .ok_or_else(|| AppError::BadRequest("TERMINAL_ENV=ssh but ssh was not found".into()))?;
    let host = env::var("TERMINAL_SSH_HOST")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            AppError::BadRequest("TERMINAL_ENV=ssh requires TERMINAL_SSH_HOST".into())
        })?;
    let user = env::var("TERMINAL_SSH_USER")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            AppError::BadRequest("TERMINAL_ENV=ssh requires TERMINAL_SSH_USER".into())
        })?;
    let port = env_u64("TERMINAL_SSH_PORT", 22);
    let key_path = env::var("TERMINAL_SSH_KEY")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let sync_note = sync_ssh_remote_files(store, payload, &user, &host, port, key_path.as_deref())?;
    let session_id = terminal_session_id(payload);
    let cwd = ssh_remote_cwd(payload, session_id.as_deref());
    let marker = session_id.as_ref().map(|session_id| {
        format!(
            "__SYNTHCHAT_SSH_CWD_{}__",
            sanitize_terminal_session_key(session_id)
        )
    });
    let remote_command = marker
        .as_ref()
        .map(|marker| wrap_posix_command_with_cwd_marker(command, marker))
        .unwrap_or_else(|| command.to_string());
    let remote_shell = format!(
        "cd {} && sh -lc {}",
        posix_quote_cwd(&cwd),
        posix_shell_quote(&remote_command)
    );
    let mut args = ssh_base_args(&user, &host, port, key_path.as_deref())?;
    args.push(remote_shell);

    let mut child = Command::new(&ssh);
    child.hide_window();
    child
        .args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if stdin_data.is_some() {
        child.stdin(Stdio::piped());
    }
    let mut child = child.spawn()?;
    if let Some(stdin_data) = stdin_data {
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(stdin_data.as_bytes()).await?;
            stdin.shutdown().await?;
        }
    }
    let output = match tokio::time::timeout(
        Duration::from_secs(timeout_seconds),
        child.wait_with_output(),
    )
    .await
    {
        Ok(output) => output?,
        Err(_) => {
            return Err(AppError::BadRequest(format!(
                "ssh command timed out after {timeout_seconds}s"
            )));
        }
    };
    let stdout = decode_terminal_output(&output.stdout);
    let stderr = decode_terminal_output(&output.stderr);
    let (stdout, session_note) =
        update_ssh_session_cwd(marker.as_deref(), session_id.as_deref(), &stdout);
    let sync_back_note =
        match sync_ssh_remote_files_back(store, payload, &user, &host, port, key_path.as_deref()) {
            Ok(note) => note.unwrap_or_else(|| "syncBack: disabled".into()),
            Err(error) => format!("syncBack: failed: {error}"),
        };
    Ok(format!(
        "backend: ssh\ntarget: {user}@{host}:{port}\ncwd: {cwd}\n{}\n{}\n{}\nexitCode: {}\nstdout:\n{}\nstderr:\n{}",
        sync_note.unwrap_or_else(|| "sync: disabled".into()),
        sync_back_note,
        session_note.unwrap_or_else(|| "sessionCwd: none".into()),
        output.status.code().unwrap_or(-1),
        truncate_output(&stdout, max_output_chars),
        truncate_output(&stderr, max_output_chars / 2)
    ))
}

async fn run_daytona_terminal_command(
    store: &AppStore,
    command: &str,
    payload: &Value,
    timeout_seconds: u64,
    max_output_chars: usize,
    stdin_data: Option<&str>,
) -> AppResult<String> {
    ensure_command_not_hardline(command)?;
    let python = find_executable("python")
        .or_else(|| find_executable("python3"))
        .or_else(common_python_path)
        .ok_or_else(|| AppError::BadRequest("TERMINAL_ENV=daytona requires Python".into()))?;
    if !python_module_available("daytona") {
        return Err(AppError::BadRequest(
            "TERMINAL_ENV=daytona requires the Python daytona SDK".into(),
        ));
    }
    let task_id = terminal_session_id(payload).unwrap_or_else(|| "default".into());
    let explicit_cwd = payload
        .get("cwd")
        .or_else(|| payload.get("workdir"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string);
    let cwd = explicit_cwd
        .clone()
        .or_else(|| daytona_terminal_session_cwd(&task_id))
        .or_else(|| {
            env::var("TERMINAL_CWD")
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        })
        .unwrap_or_else(|| "/home/daytona".into());
    set_daytona_terminal_session_cwd(&task_id, cwd.clone());
    let marker = format!(
        "__SYNTHCHAT_DAYTONA_CWD_{}__",
        sanitize_terminal_session_key(&task_id)
    );
    let remote_command = wrap_posix_command_with_cwd_marker(command, &marker);
    let requested_disk = env_u64("TERMINAL_CONTAINER_DISK", 51200);
    let remote_base = env_string("TERMINAL_DAYTONA_REMOTE_BASE", "/home/daytona/.synthchat");
    let sync_limit = env_u64("TERMINAL_DAYTONA_SYNC_LIMIT", 100).min(2000) as usize;
    let sync_files = if env_bool("TERMINAL_DAYTONA_SYNC_FILES", true) {
        store.remote_sync_files(&remote_base, sync_limit)?
    } else {
        serde_json::json!({
            "containerBase": remote_base,
            "count": 0,
            "fileLimit": sync_limit,
            "files": []
        })
    };
    let config = serde_json::json!({
        "command": remote_command,
        "cwd": cwd,
        "timeout": timeout_seconds,
        "stdin": stdin_data,
        "image": env_string("TERMINAL_DAYTONA_IMAGE", "nikolaik/python-nodejs:python3.11-nodejs20"),
        "cpu": env_f64("TERMINAL_CONTAINER_CPU", 1.0).ceil().max(1.0) as u64,
        "memoryMb": env_u64("TERMINAL_CONTAINER_MEMORY", 5120),
        "diskMb": requested_disk,
        "persistent": env_bool("TERMINAL_CONTAINER_PERSISTENT", true),
        "taskId": sanitize_terminal_session_key(&task_id),
        "syncFiles": sync_files.get("files").cloned().unwrap_or_else(|| serde_json::json!([])),
        "syncFilesEnabled": env_bool("TERMINAL_DAYTONA_SYNC_FILES", true),
        "syncBackEnabled": env_bool("TERMINAL_DAYTONA_SYNC_BACK", true),
        "remoteBase": remote_base,
    });
    let script = r#"
import base64
import hashlib
import json
import math
import os
import shutil
import shlex
import sys
import tempfile

cfg = json.load(sys.stdin)

try:
    from daytona import Daytona, CreateSandboxFromImageParams, Resources
    try:
        from daytona import DaytonaError
    except Exception:
        DaytonaError = Exception
except Exception as exc:
    print(json.dumps({"ok": False, "error": f"failed to import daytona SDK: {exc}"}))
    raise SystemExit(0)

def quote_cwd(cwd: str) -> str:
    if cwd == "~":
        return '"$HOME"'
    if cwd.startswith("~/"):
        return '"$HOME"/' + shlex.quote(cwd[2:])
    return shlex.quote(cwd)

def sha256_file(path: str) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1024 * 1024), b""):
            h.update(chunk)
    return h.hexdigest()

daytona = Daytona()
task_id = cfg.get("taskId") or "default"
name = f"synthchat-{task_id}"
labels = {"synthchat_task_id": task_id}
persistent = bool(cfg.get("persistent", True))
sandbox = None

if persistent:
    try:
        sandbox = daytona.get(name)
        sandbox.start()
    except Exception:
        sandbox = None
    if sandbox is None:
        try:
            results = daytona.list(labels=labels, limit=1)
            sandbox = next(iter(results), None)
            if sandbox is not None:
                sandbox.start()
        except Exception:
            sandbox = None

if sandbox is None:
    memory_gib = max(1, math.ceil(int(cfg.get("memoryMb") or 5120) / 1024))
    disk_gib = max(1, math.ceil(int(cfg.get("diskMb") or 51200) / 1024))
    disk_gib = min(disk_gib, 10)
    resources = Resources(
        cpu=max(1, int(cfg.get("cpu") or 1)),
        memory=memory_gib,
        disk=disk_gib,
    )
    sandbox = daytona.create(
        CreateSandboxFromImageParams(
            image=cfg.get("image") or "nikolaik/python-nodejs:python3.11-nodejs20",
            name=name,
            labels=labels,
            auto_stop_interval=0,
            resources=resources,
        )
    )

sync_stats = {
    "enabled": bool(cfg.get("syncFilesEnabled", True)),
    "uploaded": 0,
    "checked": 0,
    "applied": 0,
    "missing": 0,
    "conflicts": 0,
}
pushed_hashes = {}
sync_files = cfg.get("syncFiles") or []
if sync_stats["enabled"] and sync_files:
    parents = sorted({
        os.path.dirname(str(entry.get("containerPath") or ""))
        for entry in sync_files
        if entry.get("containerPath")
    })
    if parents:
        mkdir_cmd = "mkdir -p " + " ".join(shlex.quote(parent) for parent in parents)
        sandbox.process.exec(mkdir_cmd)
    for entry in sync_files:
        host_path = str(entry.get("hostPath") or "")
        remote_path = str(entry.get("containerPath") or "")
        if not host_path or not remote_path or not os.path.isfile(host_path):
            continue
        sandbox.fs.upload_file(host_path, remote_path)
        pushed_hashes[remote_path] = sha256_file(host_path)
        sync_stats["uploaded"] += 1

cwd = quote_cwd(str(cfg.get("cwd") or "/home/daytona"))
cmd = str(cfg.get("command") or "")
stdin_text = cfg.get("stdin")
if stdin_text is None:
    shell_cmd = f"cd {cwd} && sh -lc {shlex.quote(cmd)}"
else:
    stdin_b64 = base64.b64encode(str(stdin_text).encode("utf-8")).decode("ascii")
    shell_cmd = (
        f"printf %s {shlex.quote(stdin_b64)} | base64 -d | "
        f"(cd {cwd} && sh -lc {shlex.quote(cmd)})"
    )

try:
    response = sandbox.process.exec(shell_cmd, timeout=int(cfg.get("timeout") or 60))
    result = getattr(response, "result", "") or ""
    exit_code = getattr(response, "exit_code", 0)
    if sync_stats["enabled"] and bool(cfg.get("syncBackEnabled", True)) and sync_files:
        for entry in sync_files:
            host_path = str(entry.get("hostPath") or "")
            remote_path = str(entry.get("containerPath") or "")
            if not host_path or not remote_path:
                continue
            tmp = tempfile.NamedTemporaryFile(prefix="synthchat-daytona-sync-", delete=False)
            tmp_path = tmp.name
            tmp.close()
            try:
                try:
                    sandbox.fs.download_file(remote_path, tmp_path)
                except Exception:
                    sync_stats["missing"] += 1
                    continue
                sync_stats["checked"] += 1
                remote_hash = sha256_file(tmp_path)
                pushed_hash = pushed_hashes.get(remote_path)
                if pushed_hash and pushed_hash == remote_hash:
                    continue
                host_hash = sha256_file(host_path) if os.path.isfile(host_path) else None
                if host_hash == remote_hash:
                    continue
                if pushed_hash and host_hash != pushed_hash:
                    sync_stats["conflicts"] += 1
                os.makedirs(os.path.dirname(host_path), exist_ok=True)
                shutil.copy2(tmp_path, host_path)
                sync_stats["applied"] += 1
            finally:
                try:
                    os.unlink(tmp_path)
                except OSError:
                    pass
    print(json.dumps({
        "ok": True,
        "sandboxId": getattr(sandbox, "id", None),
        "name": name,
        "exitCode": exit_code,
        "output": result,
        "sync": sync_stats,
    }, ensure_ascii=False))
finally:
    if not persistent:
        try:
            daytona.delete(sandbox)
        except Exception:
            pass
"#;
    let mut child = Command::new(&python);
    child.hide_window();
    child
        .args(["-c", script])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = child.spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(config.to_string().as_bytes()).await?;
        stdin.shutdown().await?;
    }
    let output = match tokio::time::timeout(
        Duration::from_secs(timeout_seconds.saturating_add(60)),
        child.wait_with_output(),
    )
    .await
    {
        Ok(output) => output?,
        Err(_) => {
            return Err(AppError::BadRequest(format!(
                "daytona command timed out after {timeout_seconds}s"
            )));
        }
    };
    let stdout = decode_terminal_output(&output.stdout);
    let stderr = decode_terminal_output(&output.stderr);
    let parsed = serde_json::from_str::<Value>(stdout.trim()).ok();
    let ok = parsed
        .as_ref()
        .and_then(|value| value.get("ok"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !ok {
        let error = parsed
            .as_ref()
            .and_then(|value| value.get("error"))
            .and_then(Value::as_str)
            .map(ToString::to_string)
            .unwrap_or_else(|| stderr.to_string());
        return Err(AppError::BadRequest(format!(
            "daytona terminal failed: {error}"
        )));
    }
    let sandbox_id = parsed
        .as_ref()
        .and_then(|value| value.get("sandboxId"))
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let exit_code = parsed
        .as_ref()
        .and_then(|value| value.get("exitCode"))
        .and_then(Value::as_i64)
        .unwrap_or(-1);
    let remote_output = parsed
        .as_ref()
        .and_then(|value| value.get("output"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let (remote_output, session_note) =
        update_daytona_session_cwd(&marker, &task_id, remote_output);
    let sync_note = parsed
        .as_ref()
        .and_then(|value| value.get("sync"))
        .map(|value| value.to_string())
        .unwrap_or_else(|| "null".into());
    Ok(format!(
        "backend: daytona\nsandbox: {sandbox_id}\ncwd: {}\n{}\nsync: {sync_note}\nexitCode: {exit_code}\nstdout:\n{}\nstderr:\n{}",
        config.get("cwd").and_then(Value::as_str).unwrap_or(""),
        session_note,
        truncate_output(&remote_output, max_output_chars),
        truncate_output(&stderr, max_output_chars / 2)
    ))
}

async fn run_modal_terminal_command(
    store: &AppStore,
    command: &str,
    payload: &Value,
    timeout_seconds: u64,
    max_output_chars: usize,
    stdin_data: Option<&str>,
) -> AppResult<String> {
    ensure_command_not_hardline(command)?;
    let mode = modal_mode();
    if mode == "managed" {
        return run_managed_modal_terminal_command(
            command,
            payload,
            timeout_seconds,
            max_output_chars,
            stdin_data,
        )
        .await;
    }
    let python = find_executable("python")
        .or_else(|| find_executable("python3"))
        .or_else(common_python_path)
        .ok_or_else(|| AppError::BadRequest("TERMINAL_ENV=modal requires Python".into()))?;
    if !python_module_available("modal") {
        return Err(AppError::BadRequest(
            "TERMINAL_ENV=modal requires the Python modal SDK".into(),
        ));
    }
    if !has_direct_modal_credentials() {
        return Err(AppError::BadRequest(
            "TERMINAL_ENV=modal direct execution requires Modal credentials".into(),
        ));
    }
    let task_id = terminal_session_id(payload).unwrap_or_else(|| "default".into());
    let explicit_cwd = payload
        .get("cwd")
        .or_else(|| payload.get("workdir"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string);
    let cwd = explicit_cwd
        .clone()
        .or_else(|| modal_terminal_session_cwd(&task_id))
        .or_else(|| {
            env::var("TERMINAL_CWD")
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        })
        .unwrap_or_else(|| "/root".into());
    set_modal_terminal_session_cwd(&task_id, cwd.clone());
    let marker = format!(
        "__SYNTHCHAT_MODAL_CWD_{}__",
        sanitize_terminal_session_key(&task_id)
    );
    let remote_command = wrap_posix_command_with_cwd_marker(command, &marker);
    let remote_base = env_string("TERMINAL_MODAL_REMOTE_BASE", "/root/.synthchat");
    let sync_limit = env_u64("TERMINAL_MODAL_SYNC_LIMIT", 100).min(2000) as usize;
    let sync_files = if env_bool("TERMINAL_MODAL_SYNC_FILES", true) {
        store.remote_sync_files(&remote_base, sync_limit)?
    } else {
        serde_json::json!({
            "containerBase": remote_base,
            "count": 0,
            "fileLimit": sync_limit,
            "files": []
        })
    };
    let snapshot_id = modal_persisted_snapshot_id(store, &task_id)?
        .or_else(|| modal_terminal_snapshot_id(&task_id));
    let config = serde_json::json!({
        "command": remote_command,
        "cwd": cwd,
        "timeout": timeout_seconds,
        "stdin": stdin_data,
        "image": env_string("TERMINAL_MODAL_IMAGE", "nikolaik/python-nodejs:python3.11-nodejs20"),
        "lifetime": env_u64("TERMINAL_LIFETIME_SECONDS", 300).max(timeout_seconds),
        "taskId": sanitize_terminal_session_key(&task_id),
        "persistent": env_bool("TERMINAL_CONTAINER_PERSISTENT", true),
        "snapshotId": snapshot_id,
        "syncFiles": sync_files.get("files").cloned().unwrap_or_else(|| serde_json::json!([])),
        "syncFilesEnabled": env_bool("TERMINAL_MODAL_SYNC_FILES", true),
        "syncBackEnabled": env_bool("TERMINAL_MODAL_SYNC_BACK", true),
        "remoteBase": remote_base,
    });
    let script = r#"
import asyncio
import base64
import hashlib
import json
import os
import shutil
import shlex
import sys

cfg = json.load(sys.stdin)

try:
    import modal
except Exception as exc:
    print(json.dumps({"ok": False, "error": f"failed to import modal SDK: {exc}"}))
    raise SystemExit(0)

def quote_cwd(cwd: str) -> str:
    if cwd == "~":
        return '"$HOME"'
    if cwd.startswith("~/"):
        return '"$HOME"/' + shlex.quote(cwd[2:])
    return shlex.quote(cwd)

def resolve_image(image_spec: str):
    if image_spec.startswith("im-"):
        return modal.Image.from_id(image_spec)
    return modal.Image.from_registry(image_spec)

def sha256_file(path: str) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1024 * 1024), b""):
            h.update(chunk)
    return h.hexdigest()

async def write_stdin(proc, text: str):
    offset = 0
    chunk_size = 1024 * 1024
    while offset < len(text):
        proc.stdin.write(text[offset:offset + chunk_size])
        await proc.stdin.drain.aio()
        offset += chunk_size
    proc.stdin.write_eof()
    await proc.stdin.drain.aio()

async def main():
    app = await modal.App.lookup.aio("synthchat-agent", create_if_missing=True)
    snapshot_id = str(cfg.get("snapshotId") or "")
    base_image_spec = str(cfg.get("image") or "nikolaik/python-nodejs:python3.11-nodejs20")
    discarded_snapshot_id = None

    async def create_sandbox(image_spec: str):
        image = resolve_image(image_spec)
        return await modal.Sandbox.create.aio(
            "sleep",
            "infinity",
            image=image,
            app=app,
            timeout=int(cfg.get("lifetime") or 300),
        )

    try:
        sandbox = await create_sandbox(snapshot_id or base_image_spec)
    except Exception:
        if not snapshot_id:
            raise
        discarded_snapshot_id = snapshot_id
        sandbox = await create_sandbox(base_image_spec)
    sync_stats = {
        "enabled": bool(cfg.get("syncFilesEnabled", True)),
        "uploaded": 0,
        "checked": 0,
        "applied": 0,
        "missing": 0,
        "conflicts": 0,
    }
    pushed_hashes = {}
    try:
        sync_files = cfg.get("syncFiles") or []
        if sync_stats["enabled"] and sync_files:
            for entry in sync_files:
                host_path = str(entry.get("hostPath") or "")
                remote_path = str(entry.get("containerPath") or "")
                if not host_path or not remote_path or not os.path.isfile(host_path):
                    continue
                parent = os.path.dirname(remote_path)
                payload = base64.b64encode(open(host_path, "rb").read()).decode("ascii")
                proc = await sandbox.exec.aio(
                    "bash",
                    "-c",
                    f"mkdir -p {shlex.quote(parent)} && base64 -d > {shlex.quote(remote_path)}",
                )
                await write_stdin(proc, payload)
                exit_code = await proc.wait.aio()
                if exit_code != 0:
                    raise RuntimeError(f"Modal sync upload failed for {remote_path} (exit {exit_code})")
                pushed_hashes[remote_path] = sha256_file(host_path)
                sync_stats["uploaded"] += 1

        cwd = quote_cwd(str(cfg.get("cwd") or "/root"))
        cmd = str(cfg.get("command") or "")
        stdin_text = cfg.get("stdin")
        if stdin_text is None:
            shell_cmd = f"cd {cwd} && sh -lc {shlex.quote(cmd)}"
        else:
            stdin_b64 = base64.b64encode(str(stdin_text).encode("utf-8")).decode("ascii")
            shell_cmd = (
                f"printf %s {shlex.quote(stdin_b64)} | base64 -d | "
                f"(cd {cwd} && sh -lc {shlex.quote(cmd)})"
            )
        proc = await sandbox.exec.aio("bash", "-c", shell_cmd, timeout=int(cfg.get("timeout") or 60))
        stdout = await proc.stdout.read.aio()
        stderr = await proc.stderr.read.aio()
        exit_code = await proc.wait.aio()
        if isinstance(stdout, bytes):
            stdout = stdout.decode("utf-8", errors="replace")
        if isinstance(stderr, bytes):
            stderr = stderr.decode("utf-8", errors="replace")
        if sync_stats["enabled"] and bool(cfg.get("syncBackEnabled", True)) and sync_files:
            for entry in sync_files:
                host_path = str(entry.get("hostPath") or "")
                remote_path = str(entry.get("containerPath") or "")
                if not host_path or not remote_path:
                    continue
                proc = await sandbox.exec.aio(
                    "bash",
                    "-c",
                    f"if test -f {shlex.quote(remote_path)}; then base64 {shlex.quote(remote_path)}; else exit 44; fi",
                )
                remote_b64 = await proc.stdout.read.aio()
                remote_exit = await proc.wait.aio()
                if remote_exit == 44:
                    sync_stats["missing"] += 1
                    continue
                if remote_exit != 0:
                    raise RuntimeError(f"Modal sync-back download failed for {remote_path} (exit {remote_exit})")
                if isinstance(remote_b64, bytes):
                    remote_b64 = remote_b64.decode("ascii", errors="replace")
                remote_bytes = base64.b64decode(remote_b64)
                sync_stats["checked"] += 1
                remote_hash = hashlib.sha256(remote_bytes).hexdigest()
                pushed_hash = pushed_hashes.get(remote_path)
                if pushed_hash and pushed_hash == remote_hash:
                    continue
                host_hash = sha256_file(host_path) if os.path.isfile(host_path) else None
                if host_hash == remote_hash:
                    continue
                if pushed_hash and host_hash != pushed_hash:
                    sync_stats["conflicts"] += 1
                os.makedirs(os.path.dirname(host_path), exist_ok=True)
                with open(host_path, "wb") as f:
                    f.write(remote_bytes)
                sync_stats["applied"] += 1
        saved_snapshot_id = None
        if bool(cfg.get("persistent", True)):
            try:
                snapshot_image = await sandbox.snapshot_filesystem.aio()
                saved_snapshot_id = getattr(snapshot_image, "object_id", None)
            except Exception:
                saved_snapshot_id = None
        print(json.dumps({
            "ok": True,
            "sandboxId": getattr(sandbox, "object_id", None),
            "snapshotId": saved_snapshot_id,
            "discardedSnapshotId": discarded_snapshot_id,
            "exitCode": exit_code,
            "stdout": stdout or "",
            "stderr": stderr or "",
            "sync": sync_stats,
        }, ensure_ascii=False))
    finally:
        try:
            await sandbox.terminate.aio()
        except Exception:
            pass

asyncio.run(main())
"#;
    let mut child = Command::new(&python);
    child.hide_window();
    child
        .args(["-c", script])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = child.spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(config.to_string().as_bytes()).await?;
        stdin.shutdown().await?;
    }
    let output = match tokio::time::timeout(
        Duration::from_secs(timeout_seconds.saturating_add(120)),
        child.wait_with_output(),
    )
    .await
    {
        Ok(output) => output?,
        Err(_) => {
            return Err(AppError::BadRequest(format!(
                "modal command timed out after {timeout_seconds}s"
            )));
        }
    };
    let stdout = decode_terminal_output(&output.stdout);
    let stderr = decode_terminal_output(&output.stderr);
    let parsed = serde_json::from_str::<Value>(stdout.trim()).ok();
    let ok = parsed
        .as_ref()
        .and_then(|value| value.get("ok"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !ok {
        let error = parsed
            .as_ref()
            .and_then(|value| value.get("error"))
            .and_then(Value::as_str)
            .map(ToString::to_string)
            .unwrap_or_else(|| stderr.to_string());
        return Err(AppError::BadRequest(format!(
            "modal terminal failed: {error}"
        )));
    }
    let sandbox_id = parsed
        .as_ref()
        .and_then(|value| value.get("sandboxId"))
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let exit_code = parsed
        .as_ref()
        .and_then(|value| value.get("exitCode"))
        .and_then(Value::as_i64)
        .unwrap_or(-1);
    let saved_snapshot_id = parsed
        .as_ref()
        .and_then(|value| value.get("snapshotId"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty());
    if let Some(snapshot_id) = saved_snapshot_id {
        set_modal_terminal_snapshot_id(&task_id, snapshot_id.to_string());
        set_modal_persisted_snapshot_id(store, &task_id, snapshot_id)?;
    } else if parsed
        .as_ref()
        .and_then(|value| value.get("discardedSnapshotId"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .is_some()
    {
        clear_modal_terminal_snapshot_id(&task_id);
        let _ = clear_modal_persisted_snapshots(store, Some(&task_id))?;
    }
    let remote_stdout = parsed
        .as_ref()
        .and_then(|value| value.get("stdout"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let (remote_stdout, session_note) = update_modal_session_cwd(&marker, &task_id, remote_stdout);
    let remote_stderr = parsed
        .as_ref()
        .and_then(|value| value.get("stderr"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let sync_note = parsed
        .as_ref()
        .and_then(|value| value.get("sync"))
        .map(|value| value.to_string())
        .unwrap_or_else(|| "null".into());
    let stderr = if remote_stderr.is_empty() {
        stderr.to_string()
    } else if stderr.trim().is_empty() {
        remote_stderr.to_string()
    } else {
        format!("{remote_stderr}\n{stderr}")
    };
    Ok(format!(
        "backend: modal\nsandbox: {sandbox_id}\ncwd: {}\nmode: direct\n{}\nsync: {sync_note}\nexitCode: {exit_code}\nstdout:\n{}\nstderr:\n{}",
        config.get("cwd").and_then(Value::as_str).unwrap_or(""),
        session_note,
        truncate_output(&remote_stdout, max_output_chars),
        truncate_output(&stderr, max_output_chars / 2)
    ))
}

async fn run_managed_modal_terminal_command(
    command: &str,
    payload: &Value,
    timeout_seconds: u64,
    max_output_chars: usize,
    stdin_data: Option<&str>,
) -> AppResult<String> {
    let (gateway_origin, token) = managed_modal_gateway_config()?;
    let task_id = terminal_session_id(payload).unwrap_or_else(|| "default".into());
    let explicit_cwd = payload
        .get("cwd")
        .or_else(|| payload.get("workdir"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string);
    let cwd = explicit_cwd
        .clone()
        .or_else(|| modal_terminal_session_cwd(&task_id))
        .or_else(|| {
            env::var("TERMINAL_CWD")
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        })
        .unwrap_or_else(|| "/root".into());
    set_modal_terminal_session_cwd(&task_id, cwd.clone());
    let marker = format!(
        "__SYNTHCHAT_MODAL_CWD_{}__",
        sanitize_terminal_session_key(&task_id)
    );
    let remote_command = wrap_posix_command_with_cwd_marker(command, &marker);
    let persistent = env_bool("TERMINAL_CONTAINER_PERSISTENT", true);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(timeout_seconds.saturating_add(30)))
        .build()
        .map_err(|error| AppError::BadRequest(format!("managed Modal client failed: {error}")))?;
    let sandbox_payload = serde_json::json!({
        "image": env_string("TERMINAL_MODAL_IMAGE", "nikolaik/python-nodejs:python3.11-nodejs20"),
        "cwd": cwd.clone(),
        "cpu": env_f64("TERMINAL_MODAL_CPU", 1.0),
        "memoryMiB": env_u64("TERMINAL_MODAL_MEMORY_MB", 5120),
        "timeoutMs": env_u64("TERMINAL_LIFETIME_SECONDS", 3600) * 1000,
        "idleTimeoutMs": env_u64("TERMINAL_MODAL_IDLE_TIMEOUT_SECONDS", 300).max(timeout_seconds) * 1000,
        "persistentFilesystem": persistent,
        "logicalKey": sanitize_terminal_session_key(&task_id)
    });
    let sandbox = managed_modal_request(
        &client,
        "POST",
        &format!("{gateway_origin}/v1/sandboxes"),
        &token,
        Some(sandbox_payload),
        Some(("x-idempotency-key", new_id("modal-create"))),
    )
    .await?;
    let sandbox_id =
        acp_json_string(&sandbox, &["id", "sandboxId", "sandbox_id"]).ok_or_else(|| {
            AppError::BadRequest("managed Modal create did not return sandbox id".into())
        })?;
    let exec_id = new_id("modal-exec");
    let exec_payload = serde_json::json!({
        "execId": exec_id,
        "command": remote_command,
        "cwd": cwd.clone(),
        "timeoutMs": timeout_seconds * 1000,
        "stdinData": stdin_data
    });
    let start = managed_modal_request(
        &client,
        "POST",
        &format!("{gateway_origin}/v1/sandboxes/{sandbox_id}/execs"),
        &token,
        Some(exec_payload),
        None,
    )
    .await?;
    let result = if managed_modal_exec_finished(&start) {
        start
    } else {
        managed_modal_poll_exec(
            &client,
            &gateway_origin,
            &token,
            &sandbox_id,
            &exec_id,
            timeout_seconds,
        )
        .await?
    };
    let _ = managed_modal_request(
        &client,
        "POST",
        &format!("{gateway_origin}/v1/sandboxes/{sandbox_id}/terminate"),
        &token,
        Some(serde_json::json!({"snapshotBeforeTerminate": persistent})),
        None,
    )
    .await;
    let exit_code = result
        .get("returncode")
        .or_else(|| result.get("returnCode"))
        .or_else(|| result.get("exitCode"))
        .and_then(Value::as_i64)
        .unwrap_or(-1);
    let remote_stdout =
        acp_json_string(&result, &["output", "stdout", "result"]).unwrap_or_default();
    let (remote_stdout, session_note) = update_modal_session_cwd(&marker, &task_id, &remote_stdout);
    let remote_stderr = acp_json_string(&result, &["stderr", "error"]).unwrap_or_default();
    let sync_note = serde_json::json!({
        "enabled": false,
        "reason": "managed Modal gateway owns cwd, environment snapshots, and remote filesystem state"
    });
    Ok(format!(
        "backend: modal\nsandbox: {sandbox_id}\ncwd: {}\nmode: managed\n{}\nsync: {sync_note}\nexitCode: {exit_code}\nstdout:\n{}\nstderr:\n{}",
        cwd,
        session_note,
        truncate_output(&remote_stdout, max_output_chars),
        truncate_output(&remote_stderr, max_output_chars / 2)
    ))
}

fn managed_modal_gateway_config() -> AppResult<(String, String)> {
    if !managed_tools_enabled() {
        return Err(AppError::BadRequest(
            "TERMINAL_ENV=modal managed mode requires SYNTHCHAT_MANAGED_TOOLS_ENABLED, NOUS_MANAGED_TOOLS_ENABLED, or TERMINAL_MANAGED_MODAL_ENABLED".into(),
        ));
    }
    let gateway = env::var("TERMINAL_MANAGED_MODAL_GATEWAY_URL")
        .or_else(|_| env::var("NOUS_MODAL_GATEWAY_URL"))
        .or_else(|_| env::var("NOUS_TOOL_GATEWAY_URL"))
        .ok()
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            AppError::BadRequest(
                "TERMINAL_ENV=modal managed mode requires TERMINAL_MANAGED_MODAL_GATEWAY_URL, NOUS_MODAL_GATEWAY_URL, or NOUS_TOOL_GATEWAY_URL".into(),
            )
        })?;
    let token = env::var("NOUS_ACCESS_TOKEN")
        .or_else(|_| env::var("SYNTHCHAT_NOUS_ACCESS_TOKEN"))
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            AppError::BadRequest(
                "TERMINAL_ENV=modal managed mode requires NOUS_ACCESS_TOKEN or SYNTHCHAT_NOUS_ACCESS_TOKEN".into(),
            )
        })?;
    Ok((gateway, token))
}

async fn managed_modal_request(
    client: &reqwest::Client,
    method: &str,
    url: &str,
    token: &str,
    body: Option<Value>,
    extra_header: Option<(&str, String)>,
) -> AppResult<Value> {
    let method = match method {
        "GET" => reqwest::Method::GET,
        "POST" => reqwest::Method::POST,
        other => {
            return Err(AppError::BadRequest(format!(
                "unsupported managed Modal HTTP method: {other}"
            )));
        }
    };
    let mut request = client
        .request(method, url)
        .bearer_auth(token)
        .header("Content-Type", "application/json");
    if let Some((name, value)) = extra_header {
        request = request.header(name, value);
    }
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request
        .send()
        .await
        .map_err(|error| AppError::BadRequest(format!("managed Modal request failed: {error}")))?;
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(AppError::BadRequest(format!(
            "managed Modal request failed (HTTP {status}): {}",
            truncate_output(&text, 1000)
        )));
    }
    if text.trim().is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_str(&text).map_err(|error| {
        AppError::BadRequest(format!("managed Modal response was not JSON: {error}"))
    })
}

async fn managed_modal_poll_exec(
    client: &reqwest::Client,
    gateway_origin: &str,
    token: &str,
    sandbox_id: &str,
    exec_id: &str,
    timeout_seconds: u64,
) -> AppResult<Value> {
    let started = Instant::now();
    let timeout = Duration::from_secs(timeout_seconds.saturating_add(10));
    loop {
        if started.elapsed() >= timeout {
            let _ = managed_modal_request(
                client,
                "POST",
                &format!("{gateway_origin}/v1/sandboxes/{sandbox_id}/execs/{exec_id}/cancel"),
                token,
                None,
                None,
            )
            .await;
            return Err(AppError::BadRequest(format!(
                "managed Modal exec timed out after {timeout_seconds}s"
            )));
        }
        tokio::time::sleep(Duration::from_millis(750)).await;
        let status = managed_modal_request(
            client,
            "GET",
            &format!("{gateway_origin}/v1/sandboxes/{sandbox_id}/execs/{exec_id}"),
            token,
            None,
            None,
        )
        .await?;
        if managed_modal_exec_finished(&status) {
            return Ok(status);
        }
    }
}

fn managed_modal_exec_finished(value: &Value) -> bool {
    matches!(
        acp_json_string(value, &["status"]).as_deref(),
        Some("completed" | "failed" | "cancelled" | "canceled" | "timeout")
    )
}

fn acp_json_string(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn sync_ssh_remote_files(
    store: &AppStore,
    payload: &Value,
    user: &str,
    host: &str,
    port: u64,
    key_path: Option<&str>,
) -> AppResult<Option<String>> {
    if !env_bool("TERMINAL_SSH_SYNC_FILES", true) {
        return Ok(None);
    }
    let remote_base = ssh_remote_base(payload);
    let limit = env_u64("TERMINAL_SSH_SYNC_LIMIT", 100).min(2000) as usize;
    let sync_files = store.remote_sync_files(&remote_base, limit)?;
    let files = sync_files
        .get("files")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if files.is_empty() {
        return Ok(Some(format!(
            "sync: no files for {remote_base} (limit {limit})"
        )));
    }
    let sync_pairs = files
        .iter()
        .filter_map(|file| {
            let host_path = file.get("hostPath").and_then(Value::as_str)?;
            let remote_path = file.get("containerPath").and_then(Value::as_str)?;
            let host_path = PathBuf::from(host_path);
            host_path
                .is_file()
                .then_some((host_path, remote_path.to_string()))
        })
        .collect::<Vec<_>>();
    if sync_pairs.is_empty() {
        return Ok(Some(format!(
            "sync: no existing host files for {remote_base} (limit {limit})"
        )));
    }
    let sync_key = ssh_sync_key(user, host, port, &remote_base);
    let current_remote_paths = sync_pairs
        .iter()
        .map(|(_, remote_path)| remote_path.clone())
        .collect::<HashSet<_>>();
    let pushed_hashes = ssh_host_file_hashes(&sync_pairs)?;
    let deleted = if env_bool("TERMINAL_SSH_SYNC_DELETE", true) {
        let stale = stale_ssh_synced_paths(&sync_key, &current_remote_paths);
        if stale.is_empty() {
            0
        } else {
            ssh_delete_remote_paths(user, host, port, key_path, &stale)?;
            stale.len()
        }
    } else {
        0
    };
    if env_bool("TERMINAL_SSH_TAR_SYNC", true)
        && sync_pairs.len() > 1
        && find_executable("tar").is_some()
    {
        let uploaded = ssh_bulk_upload_tar(&sync_pairs, &remote_base, user, host, port, key_path)?;
        set_ssh_synced_paths(&sync_key, current_remote_paths);
        set_ssh_pushed_hashes(&sync_key, pushed_hashes);
        return Ok(Some(format!(
            "sync: tar-uploaded {uploaded}/{} files to {remote_base}; deleted {deleted} stale files",
            sync_files
                .get("count")
                .and_then(Value::as_u64)
                .unwrap_or(uploaded as u64)
        )));
    }
    let scp = find_executable("scp")
        .or_else(common_scp_path)
        .ok_or_else(|| AppError::BadRequest("TERMINAL_ENV=ssh sync requires scp".into()))?;
    let mut uploaded = 0usize;
    for (host_path, remote_path) in sync_pairs {
        ssh_mkdir_parent(user, host, port, key_path, &remote_path)?;
        let mut args = scp_base_args(user, host, port, key_path)?;
        args.push(host_path.to_string_lossy().to_string());
        args.push(format!(
            "{user}@{host}:{}",
            scp_remote_target_path(&remote_path)
        ));
        let output = hidden_std_command_output(&scp, &args)?;
    if !output.status.success() {
        return Err(AppError::BadRequest(format!(
            "ssh sync scp failed for {} -> {}: {}",
            host_path.display(),
            remote_path,
            decode_terminal_output(&output.stderr)
        )));
    }
        uploaded += 1;
    }
    set_ssh_synced_paths(&sync_key, current_remote_paths);
    set_ssh_pushed_hashes(&sync_key, pushed_hashes);
    Ok(Some(format!(
        "sync: uploaded {uploaded}/{} files to {remote_base}; deleted {deleted} stale files",
        sync_files
            .get("count")
            .and_then(Value::as_u64)
            .unwrap_or(uploaded as u64)
    )))
}

fn sync_ssh_remote_files_back(
    store: &AppStore,
    payload: &Value,
    user: &str,
    host: &str,
    port: u64,
    key_path: Option<&str>,
) -> AppResult<Option<String>> {
    if !env_bool("TERMINAL_SSH_SYNC_FILES", true) || !env_bool("TERMINAL_SSH_SYNC_BACK", true) {
        return Ok(None);
    }
    let remote_base = ssh_remote_base(payload);
    let limit = env_u64("TERMINAL_SSH_SYNC_LIMIT", 100).min(2000) as usize;
    let sync_files = store.remote_sync_files(&remote_base, limit)?;
    let files = sync_files
        .get("files")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let sync_pairs = files
        .iter()
        .filter_map(|file| {
            let host_path = file.get("hostPath").and_then(Value::as_str)?;
            let remote_path = file.get("containerPath").and_then(Value::as_str)?;
            Some((PathBuf::from(host_path), remote_path.to_string()))
        })
        .collect::<Vec<_>>();
    if sync_pairs.is_empty() {
        return Ok(Some(format!(
            "syncBack: no files for {remote_base} (limit {limit})"
        )));
    }
    let sync_key = ssh_sync_key(user, host, port, &remote_base);
    let pushed_hashes = ssh_pushed_hashes(&sync_key);
    if env_bool("TERMINAL_SSH_TAR_SYNC", true)
        && sync_pairs.len() > 1
        && find_executable("tar").is_some()
    {
        let stats = ssh_bulk_download_and_apply_sync_back(
            &sync_pairs,
            &remote_base,
            user,
            host,
            port,
            key_path,
            &pushed_hashes,
        )?;
        return Ok(Some(format!(
            "syncBack: tar-applied {}/{} changed files from {}; missing {}; conflicts {}",
            stats.applied, stats.checked, remote_base, stats.missing, stats.conflicts
        )));
    }
    let scp = find_executable("scp")
        .or_else(common_scp_path)
        .ok_or_else(|| AppError::BadRequest("TERMINAL_ENV=ssh sync-back requires scp".into()))?;
    let mut checked = 0usize;
    let mut applied = 0usize;
    let mut missing = 0usize;
    let mut conflicts = 0usize;
    for (host_path, remote_path) in sync_pairs {
        if !ssh_remote_file_exists(user, host, port, key_path, &remote_path)? {
            missing += 1;
            continue;
        }
        checked += 1;
        let temp_path = env::temp_dir().join(format!("synthchat-ssh-sync-back-{}", new_id("file")));
        let result = (|| -> AppResult<SshSyncBackApply> {
            let mut args = scp_base_args(user, host, port, key_path)?;
            args.push(format!(
                "{user}@{host}:{}",
                scp_remote_target_path(&remote_path)
            ));
            args.push(temp_path.to_string_lossy().to_string());
            let output = hidden_std_command_output(&scp, &args)?;
            if !output.status.success() {
                return Err(AppError::BadRequest(format!(
                    "ssh sync-back scp failed for {} -> {}: {}",
                    remote_path,
                    host_path.display(),
                    decode_terminal_output(&output.stderr)
                )));
            }
            let result =
                apply_ssh_sync_back_file(&temp_path, &host_path, &remote_path, &pushed_hashes)?;
            Ok(result)
        })();
        let _ = fs::remove_file(&temp_path);
        let result = result?;
        if result.applied {
            applied += 1;
        }
        if result.conflict {
            conflicts += 1;
        }
    }
    Ok(Some(format!(
        "syncBack: applied {applied}/{checked} changed files from {remote_base}; missing {missing}; conflicts {conflicts}"
    )))
}

#[derive(Default)]
struct SshSyncBackStats {
    checked: usize,
    applied: usize,
    missing: usize,
    conflicts: usize,
}

struct SshSyncBackApply {
    applied: bool,
    conflict: bool,
}

fn ssh_bulk_download_and_apply_sync_back(
    sync_pairs: &[(PathBuf, String)],
    remote_base: &str,
    user: &str,
    host: &str,
    port: u64,
    key_path: Option<&str>,
    pushed_hashes: &HashMap<String, String>,
) -> AppResult<SshSyncBackStats> {
    let tar = find_executable("tar").ok_or_else(|| {
        AppError::BadRequest("TERMINAL_ENV=ssh tar sync-back requires tar".into())
    })?;
    let ssh = find_executable("ssh")
        .or_else(common_ssh_path)
        .ok_or_else(|| AppError::BadRequest("TERMINAL_ENV=ssh but ssh was not found".into()))?;
    let archive = env::temp_dir().join(format!("synthchat-ssh-sync-back-{}.tar", new_id("tar")));
    let staging = env::temp_dir().join(format!("synthchat-ssh-sync-back-{}", new_id("dir")));
    fs::create_dir_all(&staging)?;
    let result = (|| -> AppResult<SshSyncBackStats> {
        let mut ssh_args = ssh_base_args(user, host, port, key_path)?;
        ssh_args.push(format!("tar cf - -C {} .", posix_quote_cwd(remote_base)));
        let archive_file = fs::File::create(&archive)?;
        let child = hidden_std_command_spawn(&ssh)
            .args(&ssh_args)
            .stdout(archive_file)
            .stderr(std::process::Stdio::piped())
            .spawn()?;
        let output = child.wait_with_output()?;
        if !output.status.success() {
            return Err(AppError::BadRequest(format!(
                "ssh sync-back tar download failed for {remote_base}: {}",
                decode_terminal_output(&output.stderr)
            )));
        }
        let output = hidden_std_command_spawn(&tar)
            .args(["-xf"])
            .arg(&archive)
            .args(["-C"])
            .arg(&staging)
            .output()?;
        if !output.status.success() {
            return Err(AppError::BadRequest(format!(
                "ssh sync-back tar extract failed: {}",
                decode_terminal_output(&output.stderr)
            )));
        }
        let mut stats = SshSyncBackStats::default();
        for (host_path, remote_path) in sync_pairs {
            let Some(relative) = remote_relative_path(remote_path, remote_base) else {
                stats.missing += 1;
                continue;
            };
            let staged_path = staging.join(relative);
            if !staged_path.is_file() {
                stats.missing += 1;
                continue;
            }
            stats.checked += 1;
            let apply =
                apply_ssh_sync_back_file(&staged_path, host_path, remote_path, pushed_hashes)?;
            if apply.applied {
                stats.applied += 1;
            }
            if apply.conflict {
                stats.conflicts += 1;
            }
        }
        Ok(stats)
    })();
    let _ = fs::remove_file(&archive);
    let _ = fs::remove_dir_all(&staging);
    result
}

fn apply_ssh_sync_back_file(
    remote_file: &Path,
    host_path: &Path,
    remote_path: &str,
    pushed_hashes: &HashMap<String, String>,
) -> AppResult<SshSyncBackApply> {
    let remote_hash = sha256_file_hex(remote_file)?;
    if pushed_hashes
        .get(remote_path)
        .is_some_and(|pushed_hash| pushed_hash == &remote_hash)
    {
        return Ok(SshSyncBackApply {
            applied: false,
            conflict: false,
        });
    }
    let host_hash = if host_path.is_file() {
        Some(sha256_file_hex(host_path)?)
    } else {
        None
    };
    if host_hash.as_deref() == Some(remote_hash.as_str()) {
        return Ok(SshSyncBackApply {
            applied: false,
            conflict: false,
        });
    }
    let conflict = pushed_hashes
        .get(remote_path)
        .is_some_and(|pushed_hash| host_hash.as_deref() != Some(pushed_hash.as_str()));
    if let Some(parent) = host_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(remote_file, host_path)?;
    Ok(SshSyncBackApply {
        applied: true,
        conflict,
    })
}

fn ssh_remote_base(payload: &Value) -> String {
    payload
        .get("containerBase")
        .or_else(|| payload.get("container_base"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .or_else(|| {
            env::var("TERMINAL_SSH_REMOTE_BASE")
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        })
        .unwrap_or_else(|| "~/.synthchat".into())
}

fn ssh_remote_file_exists(
    user: &str,
    host: &str,
    port: u64,
    key_path: Option<&str>,
    remote_path: &str,
) -> AppResult<bool> {
    let mut args = ssh_base_args(user, host, port, key_path)?;
    args.push(format!("test -f {}", posix_quote_cwd(remote_path)));
    let ssh = find_executable("ssh")
        .or_else(common_ssh_path)
        .ok_or_else(|| AppError::BadRequest("TERMINAL_ENV=ssh but ssh was not found".into()))?;
    let output = hidden_std_command_output(&ssh, &args)?;
    Ok(output.status.success())
}

fn ssh_mkdir_parent(
    user: &str,
    host: &str,
    port: u64,
    key_path: Option<&str>,
    remote_path: &str,
) -> AppResult<()> {
    let parent = remote_parent_path(remote_path);
    if parent.is_empty() {
        return Ok(());
    }
    ssh_mkdir_path(user, host, port, key_path, &parent)
}

fn ssh_mkdir_path(
    user: &str,
    host: &str,
    port: u64,
    key_path: Option<&str>,
    remote_path: &str,
) -> AppResult<()> {
    let mut args = ssh_base_args(user, host, port, key_path)?;
    args.push(format!("mkdir -p {}", posix_quote_cwd(remote_path)));
    let ssh = find_executable("ssh")
        .or_else(common_ssh_path)
        .ok_or_else(|| AppError::BadRequest("TERMINAL_ENV=ssh but ssh was not found".into()))?;
    let output = hidden_std_command_output(&ssh, &args)?;
    if !output.status.success() {
        return Err(AppError::BadRequest(format!(
            "ssh sync mkdir failed for {remote_path}: {}",
            decode_terminal_output(&output.stderr)
        )));
    }
    Ok(())
}

fn ssh_bulk_upload_tar(
    files: &[(PathBuf, String)],
    remote_base: &str,
    user: &str,
    host: &str,
    port: u64,
    key_path: Option<&str>,
) -> AppResult<usize> {
    let tar = find_executable("tar")
        .ok_or_else(|| AppError::BadRequest("TERMINAL_ENV=ssh tar sync requires tar".into()))?;
    let ssh = find_executable("ssh")
        .or_else(common_ssh_path)
        .ok_or_else(|| AppError::BadRequest("TERMINAL_ENV=ssh but ssh was not found".into()))?;
    ssh_mkdir_path(user, host, port, key_path, remote_base)?;
    let staging = env::temp_dir().join(format!("synthchat-ssh-sync-{}", new_id("tar")));
    fs::create_dir_all(&staging)?;
    let result = (|| -> AppResult<usize> {
        let mut staged = 0usize;
        for (host_path, remote_path) in files {
            let Some(relative) = remote_relative_path(remote_path, remote_base) else {
                continue;
            };
            let staged_path = staging.join(relative);
            if let Some(parent) = staged_path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(host_path, &staged_path)?;
            staged += 1;
        }
        if staged == 0 {
            return Ok(0);
        }
        let mut tar_child = hidden_std_command_spawn(&tar)
            .args(["-cf", "-", "-C"])
            .arg(&staging)
            .arg(".")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()?;
        let mut ssh_args = ssh_base_args(user, host, port, key_path)?;
        ssh_args.push(format!("tar xf - -C {}", posix_quote_cwd(remote_base)));
        let mut ssh_child = hidden_std_command_spawn(&ssh)
            .args(&ssh_args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()?;
        {
            let mut tar_stdout = tar_child
                .stdout
                .take()
                .ok_or_else(|| AppError::BadRequest("tar stdout pipe missing".into()))?;
            let mut ssh_stdin = ssh_child
                .stdin
                .take()
                .ok_or_else(|| AppError::BadRequest("ssh stdin pipe missing".into()))?;
            io::copy(&mut tar_stdout, &mut ssh_stdin)?;
        }
        let tar_output = tar_child.wait_with_output()?;
        let ssh_output = ssh_child.wait_with_output()?;
        if !tar_output.status.success() {
            return Err(AppError::BadRequest(format!(
                "ssh sync tar create failed: {}",
                decode_terminal_output(&tar_output.stderr)
            )));
        }
        if !ssh_output.status.success() {
            return Err(AppError::BadRequest(format!(
                "ssh sync tar extract failed: {}",
                decode_terminal_output(&ssh_output.stderr)
            )));
        }
        Ok(staged)
    })();
    let _ = fs::remove_dir_all(&staging);
    result
}

fn ssh_sync_key(user: &str, host: &str, port: u64, remote_base: &str) -> String {
    format!("{user}@{host}:{port}:{remote_base}")
}

fn stale_ssh_synced_paths(sync_key: &str, current: &HashSet<String>) -> Vec<String> {
    let synced = SSH_SYNCED_REMOTE_PATHS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock();
    let Ok(synced) = synced else {
        return Vec::new();
    };
    let Some(previous) = synced.get(sync_key) else {
        return Vec::new();
    };
    let mut stale = previous
        .difference(current)
        .cloned()
        .collect::<Vec<String>>();
    stale.sort();
    stale
}

fn set_ssh_synced_paths(sync_key: &str, current: HashSet<String>) {
    if let Ok(mut synced) = SSH_SYNCED_REMOTE_PATHS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
    {
        synced.insert(sync_key.to_string(), current);
    }
}

fn ssh_host_file_hashes(files: &[(PathBuf, String)]) -> AppResult<HashMap<String, String>> {
    let mut hashes = HashMap::new();
    for (host_path, remote_path) in files {
        hashes.insert(remote_path.clone(), sha256_file_hex(host_path)?);
    }
    Ok(hashes)
}

fn ssh_pushed_hashes(sync_key: &str) -> HashMap<String, String> {
    SSH_PUSHED_HASHES
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .ok()
        .and_then(|hashes| hashes.get(sync_key).cloned())
        .unwrap_or_default()
}

fn set_ssh_pushed_hashes(sync_key: &str, hashes: HashMap<String, String>) {
    if let Ok(mut pushed) = SSH_PUSHED_HASHES
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
    {
        pushed.insert(sync_key.to_string(), hashes);
    }
}

fn clear_ssh_sync_state() -> Value {
    let cleared_paths = SSH_SYNCED_REMOTE_PATHS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .map(|mut synced| {
            let count = synced.len();
            synced.clear();
            count
        })
        .unwrap_or(0);
    let cleared_hashes = SSH_PUSHED_HASHES
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .map(|mut pushed| {
            let count = pushed.len();
            pushed.clear();
            count
        })
        .unwrap_or(0);
    serde_json::json!({
        "remotePathSets": cleared_paths,
        "pushedHashSets": cleared_hashes
    })
}

fn sha256_file_hex(path: &Path) -> AppResult<String> {
    let bytes = fs::read(path)?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Ok(hex::encode(hasher.finalize()))
}

fn ssh_delete_remote_paths(
    user: &str,
    host: &str,
    port: u64,
    key_path: Option<&str>,
    remote_paths: &[String],
) -> AppResult<()> {
    if remote_paths.is_empty() {
        return Ok(());
    }
    let mut args = ssh_base_args(user, host, port, key_path)?;
    args.push(quoted_rm_command(remote_paths));
    let ssh = find_executable("ssh")
        .or_else(common_ssh_path)
        .ok_or_else(|| AppError::BadRequest("TERMINAL_ENV=ssh but ssh was not found".into()))?;
    let output = hidden_std_command_output(&ssh, &args)?;
    if !output.status.success() {
        return Err(AppError::BadRequest(format!(
            "ssh sync delete failed: {}",
            decode_terminal_output(&output.stderr)
        )));
    }
    Ok(())
}

fn quoted_rm_command(remote_paths: &[String]) -> String {
    let paths = remote_paths
        .iter()
        .map(|path| posix_quote_cwd(path))
        .collect::<Vec<_>>()
        .join(" ");
    format!("rm -f -- {paths}")
}

fn scp_base_args(
    user: &str,
    host: &str,
    port: u64,
    key_path: Option<&str>,
) -> AppResult<Vec<String>> {
    let mut args = vec![
        "-o".into(),
        "BatchMode=yes".into(),
        "-o".into(),
        "StrictHostKeyChecking=accept-new".into(),
        "-o".into(),
        "ConnectTimeout=10".into(),
    ];
    if env_bool("TERMINAL_SSH_PERSISTENT", true) {
        let control_dir = env::temp_dir().join("synthchat-ssh");
        fs::create_dir_all(&control_dir)?;
        args.extend([
            "-o".into(),
            format!(
                "ControlPath={}",
                ssh_control_socket(&control_dir, user, host, port).display()
            ),
        ]);
    }
    if port != 22 {
        args.extend(["-P".into(), port.to_string()]);
    }
    if let Some(key_path) = key_path {
        args.extend(["-i".into(), key_path.to_string()]);
    }
    Ok(args)
}

fn ssh_remote_cwd(payload: &Value, session_id: Option<&str>) -> String {
    if payload
        .get("cwd")
        .or_else(|| payload.get("workdir"))
        .is_none()
    {
        if let Some(cwd) = session_id.and_then(ssh_terminal_session_cwd) {
            return cwd;
        }
    }
    if let Some(cwd) = payload
        .get("cwd")
        .or_else(|| payload.get("workdir"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return cwd.to_string();
    }
    env::var("TERMINAL_CWD")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "~".into())
}

fn ssh_base_args(
    user: &str,
    host: &str,
    port: u64,
    key_path: Option<&str>,
) -> AppResult<Vec<String>> {
    let mut args = vec![
        "-o".into(),
        "BatchMode=yes".into(),
        "-o".into(),
        "StrictHostKeyChecking=accept-new".into(),
        "-o".into(),
        "ConnectTimeout=10".into(),
    ];
    if env_bool("TERMINAL_SSH_PERSISTENT", true) {
        let control_dir = env::temp_dir().join("synthchat-ssh");
        fs::create_dir_all(&control_dir)?;
        args.extend([
            "-o".into(),
            format!(
                "ControlPath={}",
                ssh_control_socket(&control_dir, user, host, port).display()
            ),
            "-o".into(),
            "ControlMaster=auto".into(),
            "-o".into(),
            "ControlPersist=300".into(),
        ]);
    }
    if port != 22 {
        args.extend(["-p".into(), port.to_string()]);
    }
    if let Some(key_path) = key_path {
        args.extend(["-i".into(), key_path.to_string()]);
    }
    args.push(format!("{user}@{host}"));
    Ok(args)
}

fn ssh_control_socket(control_dir: &Path, user: &str, host: &str, port: u64) -> PathBuf {
    let key = sanitize_terminal_session_key(&format!("{user}_{host}_{port}"));
    control_dir.join(format!("{}.sock", key.chars().take(48).collect::<String>()))
}

fn posix_shell_quote(value: &str) -> String {
    if value.is_empty() {
        return "''".into();
    }
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn posix_quote_cwd(cwd: &str) -> String {
    if cwd == "~" {
        "\"$HOME\"".into()
    } else if let Some(rest) = cwd.strip_prefix("~/") {
        format!("\"$HOME\"/{}", posix_shell_quote(rest))
    } else {
        posix_shell_quote(cwd)
    }
}

fn remote_parent_path(remote_path: &str) -> String {
    let trimmed = remote_path.trim_end_matches('/');
    let Some((parent, _)) = trimmed.rsplit_once('/') else {
        return String::new();
    };
    if parent.is_empty() {
        "/".into()
    } else {
        parent.into()
    }
}

fn remote_relative_path(remote_path: &str, remote_base: &str) -> Option<PathBuf> {
    let base = remote_base.trim_end_matches('/');
    let path = remote_path.trim();
    let relative = if path == base {
        ""
    } else {
        path.strip_prefix(&format!("{base}/"))?
    };
    if relative.is_empty()
        || relative
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return None;
    }
    let mut out = PathBuf::new();
    for part in relative.split('/') {
        out.push(part);
    }
    Some(out)
}

fn scp_remote_target_path(remote_path: &str) -> String {
    if remote_path == "~" {
        "~".into()
    } else if let Some(rest) = remote_path.strip_prefix("~/") {
        format!("~/{}", posix_shell_quote(rest))
    } else {
        posix_shell_quote(remote_path)
    }
}

fn update_ssh_session_cwd(
    marker: Option<&str>,
    session_id: Option<&str>,
    stdout: &str,
) -> (String, Option<String>) {
    let Some(marker) = marker else {
        return (stdout.to_string(), None);
    };
    let (stdout, remote_cwd) = extract_cwd_marker(stdout, marker);
    let Some(session_id) = session_id else {
        return (stdout, None);
    };
    if let Some(remote_cwd) = remote_cwd {
        let remote_cwd = remote_cwd.to_string_lossy().to_string();
        if !remote_cwd.trim().is_empty() {
            set_ssh_terminal_session_cwd(session_id, remote_cwd.clone());
            return (stdout, Some(format!("sessionCwd: {remote_cwd}")));
        }
    }
    (stdout, None)
}

fn update_daytona_session_cwd(marker: &str, session_id: &str, stdout: &str) -> (String, String) {
    let (stdout, remote_cwd) = extract_cwd_marker(stdout, marker);
    if let Some(remote_cwd) = remote_cwd {
        let remote_cwd = remote_cwd.to_string_lossy().to_string();
        if !remote_cwd.trim().is_empty() {
            set_daytona_terminal_session_cwd(session_id, remote_cwd.clone());
            return (stdout, format!("sessionCwd: {remote_cwd}"));
        }
    }
    (stdout, "sessionCwd: unchanged".into())
}

fn update_modal_session_cwd(marker: &str, session_id: &str, stdout: &str) -> (String, String) {
    let (stdout, remote_cwd) = extract_cwd_marker(stdout, marker);
    if let Some(remote_cwd) = remote_cwd {
        let remote_cwd = remote_cwd.to_string_lossy().to_string();
        if !remote_cwd.trim().is_empty() {
            set_modal_terminal_session_cwd(session_id, remote_cwd.clone());
            return (stdout, format!("sessionCwd: {remote_cwd}"));
        }
    }
    (stdout, "sessionCwd: unchanged".into())
}

async fn run_singularity_terminal_command(
    store: &AppStore,
    agent: &AgentDefinition,
    command: &str,
    cwd: &Path,
    payload: &Value,
    timeout_seconds: u64,
    max_output_chars: usize,
    stdin_data: Option<&str>,
) -> AppResult<String> {
    ensure_command_not_hardline(command)?;
    let executable = find_singularity_executable().ok_or_else(|| {
        AppError::BadRequest(
            "TERMINAL_ENV=singularity but apptainer/singularity was not found".into(),
        )
    })?;
    let workspace = workspace_root(agent)?;
    let workspace = workspace.canonicalize().unwrap_or(workspace);
    let cwd = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
    let container_cwd = host_workspace_path_to_container(&workspace, &cwd)?;
    let image = env_string(
        "TERMINAL_SINGULARITY_IMAGE",
        "docker://nikolaik/python-nodejs:python3.11-nodejs20",
    );
    let container_base = payload
        .get("containerBase")
        .or_else(|| payload.get("container_base"))
        .and_then(Value::as_str)
        .unwrap_or("/root/.synthchat");
    let session_id = terminal_session_id(payload);
    let marker = session_id.as_ref().map(|session_id| {
        format!(
            "__SYNTHCHAT_CWD_{}__",
            sanitize_terminal_session_key(session_id)
        )
    });
    let singularity_command = marker
        .as_ref()
        .map(|marker| wrap_posix_command_with_cwd_marker(command, marker))
        .unwrap_or_else(|| command.to_string());
    let mut args = vec![
        "exec".to_string(),
        "--containall".to_string(),
        "--no-home".to_string(),
        "--writable-tmpfs".to_string(),
        "--pwd".to_string(),
        container_cwd.clone(),
    ];
    push_singularity_resource_args(&mut args);
    push_singularity_env_args(&mut args);
    push_singularity_extra_args(&mut args);
    push_singularity_bind(&mut args, &workspace, "/workspace", false);
    push_singularity_remote_mounts(store, &mut args, container_base)?;
    args.push(image.clone());
    args.push("sh".into());
    args.push("-lc".into());
    args.push(singularity_command);

    let mut child = Command::new(&executable);
    child.hide_window();
    child
        .args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if stdin_data.is_some() {
        child.stdin(Stdio::piped());
    }
    let mut child = child.spawn()?;
    if let Some(stdin_data) = stdin_data {
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(stdin_data.as_bytes()).await?;
            stdin.shutdown().await?;
        }
    }
    let output = match tokio::time::timeout(
        Duration::from_secs(timeout_seconds),
        child.wait_with_output(),
    )
    .await
    {
        Ok(output) => output?,
        Err(_) => {
            return Err(AppError::BadRequest(format!(
                "singularity command timed out after {timeout_seconds}s"
            )));
        }
    };
    let stdout = decode_terminal_output(&output.stdout);
    let stderr = decode_terminal_output(&output.stderr);
    let (stdout, session_note) = update_docker_session_cwd(
        marker.as_deref(),
        session_id.as_deref(),
        &workspace,
        &stdout,
    );
    Ok(format!(
        "backend: singularity\nimage: {image}\ncwd: {container_cwd}\n{}\nexitCode: {}\nstdout:\n{}\nstderr:\n{}",
        session_note.unwrap_or_else(|| "sessionCwd: none".into()),
        output.status.code().unwrap_or(-1),
        truncate_output(&stdout, max_output_chars),
        truncate_output(&stderr, max_output_chars / 2)
    ))
}

async fn run_docker_terminal_command(
    store: &AppStore,
    agent: &AgentDefinition,
    command: &str,
    cwd: &Path,
    payload: &Value,
    timeout_seconds: u64,
    max_output_chars: usize,
    stdin_data: Option<&str>,
) -> AppResult<String> {
    ensure_command_not_hardline(command)?;
    let docker = find_executable("docker")
        .or_else(common_docker_path)
        .ok_or_else(|| {
            AppError::BadRequest("TERMINAL_ENV=docker but docker was not found".into())
        })?;
    let workspace = workspace_root(agent)?;
    let workspace = workspace.canonicalize().unwrap_or(workspace);
    let cwd = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
    let container_cwd = host_workspace_path_to_container(&workspace, &cwd)?;
    let image = env_string(
        "TERMINAL_DOCKER_IMAGE",
        "nikolaik/python-nodejs:python3.11-nodejs20",
    );
    let container_base = payload
        .get("containerBase")
        .or_else(|| payload.get("container_base"))
        .and_then(Value::as_str)
        .unwrap_or("/root/.synthchat");
    let session_id = terminal_session_id(payload);
    let marker = session_id.as_ref().map(|session_id| {
        format!(
            "__SYNTHCHAT_CWD_{}__",
            sanitize_terminal_session_key(session_id)
        )
    });
    let docker_command = marker
        .as_ref()
        .map(|marker| wrap_posix_command_with_cwd_marker(command, marker))
        .unwrap_or_else(|| command.to_string());
    let image_uses_init_entrypoint = docker_image_uses_init_entrypoint(&docker, &image);
    let docker_args = if env_bool("TERMINAL_CONTAINER_PERSISTENT", true) {
        let key = docker_container_key(session_id.as_deref(), &workspace);
        let container_id = ensure_docker_terminal_container(
            store,
            &docker,
            &image,
            &workspace,
            container_base,
            &key,
            session_id.as_deref(),
            image_uses_init_entrypoint,
        )?;
        vec![
            "exec".to_string(),
            "-i".to_string(),
            "--workdir".to_string(),
            container_cwd.clone(),
            container_id,
            "sh".into(),
            "-lc".into(),
            docker_command,
        ]
    } else {
        let mut args = vec![
            "run".to_string(),
            "--rm".to_string(),
            "-i".to_string(),
            "--workdir".to_string(),
            container_cwd.clone(),
        ];
        push_docker_security_args(&mut args, image_uses_init_entrypoint);
        push_docker_user_args(&mut args);
        push_docker_resource_args(&mut args, Some(&docker));
        push_docker_volume_args(&mut args);
        push_docker_env_args(&mut args);
        push_docker_extra_args(&mut args);
        push_docker_bind_mount(&mut args, &workspace, "/workspace", false);
        push_docker_remote_mounts(store, &mut args, container_base)?;
        args.push(image.clone());
        args.push("sh".into());
        args.push("-lc".into());
        args.push(docker_command);
        args
    };

    let mut child = Command::new(&docker);
    child.hide_window();
    child
        .args(&docker_args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if stdin_data.is_some() {
        child.stdin(Stdio::piped());
    }
    let mut child = child.spawn()?;
    if let Some(stdin_data) = stdin_data {
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(stdin_data.as_bytes()).await?;
            stdin.shutdown().await?;
        }
    }
    let output = match tokio::time::timeout(
        Duration::from_secs(timeout_seconds),
        child.wait_with_output(),
    )
    .await
    {
        Ok(output) => output?,
        Err(_) => {
            return Err(AppError::BadRequest(format!(
                "docker command timed out after {timeout_seconds}s"
            )));
        }
    };
    let stdout = decode_terminal_output(&output.stdout);
    let stderr = decode_terminal_output(&output.stderr);
    let (stdout, session_note) = update_docker_session_cwd(
        marker.as_deref(),
        session_id.as_deref(),
        &workspace,
        &stdout,
    );
    Ok(format!(
        "backend: docker\nimage: {image}\ncwd: {container_cwd}\n{}\nexitCode: {}\nstdout:\n{}\nstderr:\n{}",
        session_note.unwrap_or_else(|| "sessionCwd: none".into()),
        output.status.code().unwrap_or(-1),
        truncate_output(&stdout, max_output_chars),
        truncate_output(&stderr, max_output_chars / 2)
    ))
}

fn host_workspace_path_to_container(workspace: &Path, cwd: &Path) -> AppResult<String> {
    let relative = cwd.strip_prefix(workspace).map_err(|_| {
        AppError::BadRequest(format!(
            "docker cwd must stay inside workspace: {}",
            cwd.display()
        ))
    })?;
    let rel = relative.to_string_lossy().replace('\\', "/");
    if rel.is_empty() {
        Ok("/workspace".into())
    } else {
        Ok(format!("/workspace/{rel}"))
    }
}

fn container_workspace_path_to_host(workspace: &Path, container_path: &Path) -> Option<PathBuf> {
    let prefix = Path::new("/workspace");
    let relative = container_path.strip_prefix(prefix).ok()?;
    Some(workspace.join(relative))
}

fn push_docker_bind_mount(
    args: &mut Vec<String>,
    host_path: &Path,
    container_path: &str,
    read_only: bool,
) {
    let mut mount = format!(
        "type=bind,source={},target={}",
        host_path.to_string_lossy(),
        container_path
    );
    if read_only {
        mount.push_str(",readonly");
    }
    args.push("--mount".into());
    args.push(mount);
}

fn push_singularity_bind(
    args: &mut Vec<String>,
    host_path: &Path,
    container_path: &str,
    read_only: bool,
) {
    let mut bind = format!("{}:{container_path}", host_path.to_string_lossy());
    if read_only {
        bind.push_str(":ro");
    }
    args.push("--bind".into());
    args.push(bind);
}

fn push_singularity_resource_args(args: &mut Vec<String>) {
    let cpu = env_f64("TERMINAL_CONTAINER_CPU", 1.0);
    if cpu > 0.0 {
        args.push("--cpus".into());
        args.push(cpu.to_string());
    }
    let memory_mb = env_u64("TERMINAL_CONTAINER_MEMORY", 5120);
    if memory_mb > 0 {
        args.push("--memory".into());
        args.push(format!("{memory_mb}M"));
    }
}

fn push_singularity_env_args(args: &mut Vec<String>) {
    if let Some(items) = env_json_object("TERMINAL_SINGULARITY_ENV").as_object() {
        let mut keys = items.keys().cloned().collect::<Vec<_>>();
        keys.sort();
        for key in keys {
            if !valid_env_name(&key) {
                continue;
            }
            let Some(value) = items.get(&key).and_then(Value::as_str) else {
                continue;
            };
            args.push("--env".into());
            args.push(format!("{key}={value}"));
        }
    }
    if let Some(items) = env_json_array("TERMINAL_SINGULARITY_FORWARD_ENV").as_array() {
        for item in items {
            let Some(key) = item
                .as_str()
                .map(str::trim)
                .filter(|key| valid_env_name(key))
            else {
                continue;
            };
            if env::var_os(key).is_some() {
                args.push("--env".into());
                args.push(key.to_string());
            }
        }
    }
}

fn push_singularity_extra_args(args: &mut Vec<String>) {
    if let Some(items) = env_json_array("TERMINAL_SINGULARITY_EXTRA_ARGS").as_array() {
        for item in items {
            let Some(value) = item
                .as_str()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            else {
                continue;
            };
            args.push(value.to_string());
        }
    }
}

fn push_docker_security_args(args: &mut Vec<String>, run_exec: bool) {
    args.extend([
        "--cap-drop".into(),
        "ALL".into(),
        "--cap-add".into(),
        "DAC_OVERRIDE".into(),
        "--cap-add".into(),
        "CHOWN".into(),
        "--cap-add".into(),
        "FOWNER".into(),
        "--security-opt".into(),
        "no-new-privileges".into(),
        "--pids-limit".into(),
        "256".into(),
        "--tmpfs".into(),
        "/tmp:rw,nosuid,size=512m".into(),
        "--tmpfs".into(),
        "/var/tmp:rw,noexec,nosuid,size=256m".into(),
        "--tmpfs".into(),
        if run_exec {
            "/run:rw,exec,nosuid,size=64m".into()
        } else {
            "/run:rw,noexec,nosuid,size=64m".into()
        },
    ]);
    if !env_bool("TERMINAL_DOCKER_RUN_AS_HOST_USER", false) {
        args.extend([
            "--cap-add".into(),
            "SETUID".into(),
            "--cap-add".into(),
            "SETGID".into(),
        ]);
    }
}

fn push_docker_user_args(args: &mut Vec<String>) {
    if !env_bool("TERMINAL_DOCKER_RUN_AS_HOST_USER", false) {
        return;
    }
    if let Some(user_spec) = host_user_spec() {
        args.push("--user".into());
        args.push(user_spec);
    }
}

fn host_user_spec() -> Option<String> {
    #[cfg(windows)]
    {
        None
    }
    #[cfg(not(windows))]
    {
        let uid = hidden_std_command_output("id", ["-u"]).ok()?;
        let gid = hidden_std_command_output("id", ["-g"]).ok()?;
        if !uid.status.success() || !gid.status.success() {
            return None;
        }
        let uid = decode_terminal_output(&uid.stdout).trim().to_string();
        let gid = decode_terminal_output(&gid.stdout).trim().to_string();
        if uid.is_empty() || gid.is_empty() {
            None
        } else {
            Some(format!("{uid}:{gid}"))
        }
    }
}

fn docker_image_uses_init_entrypoint(docker: &str, image: &str) -> bool {
    hidden_std_command_output(
        docker,
        [
            "image",
            "inspect",
            image,
            "--format",
            "{{json .Config.Entrypoint}}",
        ],
    )
    .ok()
    .filter(|output| output.status.success())
    .map(|output| docker_entrypoint_uses_init(&decode_terminal_output(&output.stdout)))
    .unwrap_or(false)
}

fn docker_entrypoint_uses_init(raw: &str) -> bool {
    let raw = raw.trim();
    if raw.is_empty() || raw == "null" {
        return false;
    }
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    let first = match value {
        Value::String(entrypoint) => entrypoint,
        Value::Array(items) => items
            .first()
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        _ => String::new(),
    };
    matches!(
        first.trim(),
        "/init" | "/package/admin/s6-overlay/command/init"
    )
}

fn push_docker_resource_args(args: &mut Vec<String>, docker: Option<&str>) {
    let cpu = env_f64("TERMINAL_CONTAINER_CPU", 1.0);
    if cpu > 0.0 {
        args.push("--cpus".into());
        args.push(cpu.to_string());
    }
    let memory_mb = env_u64("TERMINAL_CONTAINER_MEMORY", 5120);
    if memory_mb > 0 {
        args.push("--memory".into());
        args.push(format!("{memory_mb}m"));
    }
    let Some(disk_mb) = env_u64_if_set("TERMINAL_CONTAINER_DISK") else {
        return;
    };
    if disk_mb > 0 && docker.is_some_and(docker_storage_opt_supported) {
        args.push("--storage-opt".into());
        args.push(format!("size={disk_mb}m"));
    }
}

fn docker_storage_opt_supported(docker: &str) -> bool {
    #[cfg(target_os = "macos")]
    {
        let _ = docker;
        false
    }
    #[cfg(not(target_os = "macos"))]
    {
        hidden_std_command_output(docker, ["info", "--format", "{{.Driver}}"])
            .ok()
            .filter(|output| output.status.success())
            .map(|output| decode_terminal_output(&output.stdout).trim() == "overlay2")
            .unwrap_or(false)
    }
}

fn push_docker_volume_args(args: &mut Vec<String>) {
    if let Some(items) = env_json_array("TERMINAL_DOCKER_VOLUMES").as_array() {
        for item in items {
            let Some(volume) = item
                .as_str()
                .map(str::trim)
                .filter(|value| !value.is_empty() && value.contains(':'))
            else {
                continue;
            };
            args.push("-v".into());
            args.push(volume.to_string());
        }
    }
}

fn push_docker_env_args(args: &mut Vec<String>) {
    if let Some(items) = env_json_object("TERMINAL_DOCKER_ENV").as_object() {
        let mut keys = items.keys().cloned().collect::<Vec<_>>();
        keys.sort();
        for key in keys {
            if !valid_env_name(&key) {
                continue;
            }
            let Some(value) = items.get(&key).and_then(Value::as_str) else {
                continue;
            };
            args.push("--env".into());
            args.push(format!("{key}={value}"));
        }
    }
    if let Some(items) = env_json_array("TERMINAL_DOCKER_FORWARD_ENV").as_array() {
        for item in items {
            let Some(key) = item
                .as_str()
                .map(str::trim)
                .filter(|key| valid_env_name(key))
            else {
                continue;
            };
            if env::var_os(key).is_some() {
                args.push("--env".into());
                args.push(key.to_string());
            }
        }
    }
}

fn push_docker_extra_args(args: &mut Vec<String>) {
    if let Some(items) = env_json_array("TERMINAL_DOCKER_EXTRA_ARGS").as_array() {
        for item in items {
            let Some(value) = item
                .as_str()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            else {
                continue;
            };
            args.push(value.to_string());
        }
    }
}

fn valid_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first == '_' || first.is_ascii_alphabetic())
        && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

fn push_docker_remote_mounts(
    store: &AppStore,
    args: &mut Vec<String>,
    container_base: &str,
) -> AppResult<()> {
    for source in [
        store.credential_file_mounts(container_base)?,
        store.skills_directory_mounts(container_base, 0)?,
        store.cache_directory_mounts(container_base, 0)?,
    ] {
        if let Some(mounts) = source.get("mounts").and_then(Value::as_array) {
            for mount in mounts {
                let Some(host_path) = mount.get("hostPath").and_then(Value::as_str) else {
                    continue;
                };
                let Some(container_path) = mount.get("containerPath").and_then(Value::as_str)
                else {
                    continue;
                };
                let read_only = mount
                    .get("readOnly")
                    .and_then(Value::as_bool)
                    .unwrap_or(true);
                push_docker_bind_mount(args, Path::new(host_path), container_path, read_only);
            }
        }
    }
    Ok(())
}

fn push_singularity_remote_mounts(
    store: &AppStore,
    args: &mut Vec<String>,
    container_base: &str,
) -> AppResult<()> {
    for source in [
        store.credential_file_mounts(container_base)?,
        store.skills_directory_mounts(container_base, 0)?,
        store.cache_directory_mounts(container_base, 0)?,
    ] {
        if let Some(mounts) = source.get("mounts").and_then(Value::as_array) {
            for mount in mounts {
                let Some(host_path) = mount.get("hostPath").and_then(Value::as_str) else {
                    continue;
                };
                let Some(container_path) = mount.get("containerPath").and_then(Value::as_str)
                else {
                    continue;
                };
                let read_only = mount
                    .get("readOnly")
                    .and_then(Value::as_bool)
                    .unwrap_or(true);
                push_singularity_bind(args, Path::new(host_path), container_path, read_only);
            }
        }
    }
    Ok(())
}

fn docker_container_key(session_id: Option<&str>, workspace: &Path) -> String {
    if let Some(session_id) = session_id {
        return format!("session:{}", sanitize_terminal_session_key(session_id));
    }
    format!("workspace:{}", workspace.to_string_lossy())
}

fn docker_label_value(value: &str) -> String {
    let mut cleaned = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '.' || ch == '-' {
                ch
            } else {
                '_'
            }
        })
        .take(63)
        .collect::<String>();
    if cleaned.is_empty() {
        cleaned.push_str("unknown");
    }
    cleaned
}

fn docker_profile_label() -> String {
    env::var("SYNTHCHAT_PROFILE")
        .or_else(|_| env::var("HERMES_PROFILE"))
        .unwrap_or_else(|_| "default".into())
        .trim()
        .to_string()
}

fn docker_container_name(key: &str) -> String {
    format!("synthchat-{}", docker_label_value(key))
        .chars()
        .take(63)
        .collect()
}

fn push_docker_label(args: &mut Vec<String>, key: &str, value: &str) {
    args.push("--label".into());
    args.push(format!("{key}={}", docker_label_value(value)));
}

fn push_docker_terminal_labels(
    args: &mut Vec<String>,
    key: &str,
    workspace: &Path,
    session_id: Option<&str>,
) {
    push_docker_label(args, DOCKER_LABEL_AGENT, "1");
    push_docker_label(args, DOCKER_LABEL_PROFILE, &docker_profile_label());
    push_docker_label(args, DOCKER_LABEL_KEY, key);
    push_docker_label(args, DOCKER_LABEL_WORKSPACE, &workspace.to_string_lossy());
    if let Some(session_id) = session_id {
        push_docker_label(args, DOCKER_LABEL_SESSION, session_id);
    }
}

fn ensure_docker_terminal_container(
    store: &AppStore,
    docker: &str,
    image: &str,
    workspace: &Path,
    container_base: &str,
    key: &str,
    session_id: Option<&str>,
    image_uses_init_entrypoint: bool,
) -> AppResult<String> {
    maybe_reap_docker_orphans(docker);
    if let Some(container_id) = docker_container_for_key(key) {
        if docker_container_running(docker, &container_id) {
            return Ok(container_id);
        }
        remove_docker_container_key(key);
    }
    if env_bool("TERMINAL_DOCKER_PERSIST_ACROSS_PROCESSES", true) {
        if let Some(container_id) = find_docker_terminal_container(docker, key, true) {
            set_docker_container_for_key(key, &container_id);
            return Ok(container_id);
        }
    }
    if let Some(stale_container_id) = find_docker_terminal_container(docker, key, false) {
        let _ = hidden_std_command_spawn(docker)
            .args(["rm", "-f", &stale_container_id])
            .output();
    }
    let name = docker_container_name(key);
    if let Some(existing_id) = docker_container_id_by_name(docker, &name) {
        let _ = hidden_std_command_spawn(docker)
            .args(["rm", "-f", &existing_id])
            .output();
    }
    let mut args = vec!["run".to_string(), "-d".to_string(), "-i".to_string()];
    args.push("--name".into());
    args.push(name);
    push_docker_terminal_labels(&mut args, key, workspace, session_id);
    push_docker_security_args(&mut args, image_uses_init_entrypoint);
    push_docker_user_args(&mut args);
    push_docker_resource_args(&mut args, Some(docker));
    push_docker_volume_args(&mut args);
    push_docker_env_args(&mut args);
    push_docker_extra_args(&mut args);
    push_docker_bind_mount(&mut args, workspace, "/workspace", false);
    push_docker_remote_mounts(store, &mut args, container_base)?;
    args.push(image.to_string());
    args.push("sh".into());
    args.push("-lc".into());
    args.push("trap 'exit 0' TERM INT; while true; do sleep 3600; done".into());
    let output = hidden_std_command_output(docker, &args)?;
    if !output.status.success() {
        return Err(AppError::BadRequest(format!(
            "failed to start docker terminal container: {}",
            decode_terminal_output(&output.stderr)
        )));
    }
    let container_id = decode_terminal_output(&output.stdout).trim().to_string();
    if container_id.is_empty() {
        return Err(AppError::BadRequest(
            "failed to start docker terminal container: empty container id".into(),
        ));
    }
    set_docker_container_for_key(key, &container_id);
    Ok(container_id)
}

fn find_docker_terminal_container(docker: &str, key: &str, running_only: bool) -> Option<String> {
    let mut args = vec![
        "ps".to_string(),
        "-a".to_string(),
        "--filter".to_string(),
        format!("label={DOCKER_LABEL_AGENT}=1"),
        "--filter".to_string(),
        format!(
            "label={DOCKER_LABEL_PROFILE}={}",
            docker_label_value(&docker_profile_label())
        ),
        "--filter".to_string(),
        format!("label={DOCKER_LABEL_KEY}={}", docker_label_value(key)),
    ];
    if running_only {
        args.push("--filter".into());
        args.push("status=running".into());
    }
    args.push("--format".into());
    args.push("{{.ID}}".into());
    hidden_std_command_output(docker, &args)
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| {
            decode_terminal_output(&output.stdout)
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .map(str::to_string)
        })
}

fn docker_container_id_by_name(docker: &str, name: &str) -> Option<String> {
    hidden_std_command_output(
        docker,
        [
            "ps",
            "-a",
            "--filter",
            &format!("name=^/{name}$"),
            "--format",
            "{{.ID}}",
        ],
    )
    .ok()
    .filter(|output| output.status.success())
    .and_then(|output| {
        decode_terminal_output(&output.stdout)
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .map(str::to_string)
    })
}

fn maybe_reap_docker_orphans(docker: &str) -> usize {
    if !env_bool("TERMINAL_DOCKER_ORPHAN_REAPER", true) {
        return 0;
    }
    if let Ok(mut ran) = DOCKER_ORPHAN_REAPER_RAN
        .get_or_init(|| Mutex::new(false))
        .lock()
    {
        if *ran {
            return 0;
        }
        *ran = true;
    } else {
        return 0;
    }
    let lifetime = env_u64("TERMINAL_LIFETIME_SECONDS", 300).max(60);
    let max_age_seconds = lifetime.saturating_mul(2);
    let output = hidden_std_command_output(
        docker,
        [
            "container",
            "prune",
            "-f",
            "--filter",
            &format!("label={DOCKER_LABEL_AGENT}=1"),
            "--filter",
            &format!(
                "label={DOCKER_LABEL_PROFILE}={}",
                docker_label_value(&docker_profile_label())
            ),
            "--filter",
            &format!("until={max_age_seconds}s"),
        ],
    );
    let Ok(output) = output else {
        return 0;
    };
    if !output.status.success() {
        return 0;
    }
    count_pruned_docker_containers(&decode_terminal_output(&output.stdout))
}

fn count_pruned_docker_containers(stdout: &str) -> usize {
    let mut count = 0;
    let mut in_deleted = false;
    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed == "Deleted Containers:" {
            in_deleted = true;
            continue;
        }
        if trimmed.starts_with("Total reclaimed space:") {
            break;
        }
        if in_deleted && !trimmed.is_empty() {
            count += 1;
        }
    }
    count
}

fn docker_container_for_key(key: &str) -> Option<String> {
    DOCKER_TERMINAL_CONTAINERS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .ok()
        .and_then(|containers| containers.get(key).cloned())
}

fn set_docker_container_for_key(key: &str, container_id: &str) {
    if let Ok(mut containers) = DOCKER_TERMINAL_CONTAINERS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
    {
        containers.insert(key.to_string(), container_id.to_string());
    }
}

fn remove_docker_container_key(key: &str) {
    if let Ok(mut containers) = DOCKER_TERMINAL_CONTAINERS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
    {
        containers.remove(key);
    }
}

fn docker_container_snapshot() -> Value {
    let containers = DOCKER_TERMINAL_CONTAINERS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock();
    let Ok(containers) = containers else {
        return serde_json::json!({
            "count": 0,
            "containers": [],
            "error": "docker terminal container lock poisoned"
        });
    };
    let mut rows = containers
        .iter()
        .map(|(key, container_id)| {
            serde_json::json!({
                "key": key,
                "containerId": container_id
            })
        })
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        left.get("key")
            .and_then(Value::as_str)
            .unwrap_or("")
            .cmp(right.get("key").and_then(Value::as_str).unwrap_or(""))
    });
    serde_json::json!({
        "count": rows.len(),
        "containers": rows
    })
}

fn docker_container_running(docker: &str, container_id: &str) -> bool {
    hidden_std_command_output(
        docker,
        ["inspect", "-f", "{{.State.Running}}", container_id],
    )
    .ok()
    .filter(|output| output.status.success())
    .map(|output| decode_terminal_output(&output.stdout).trim() == "true")
    .unwrap_or(false)
}

fn cleanup_docker_terminal_containers(target_session: Option<&str>) -> usize {
    let Some(docker) = find_executable("docker").or_else(common_docker_path) else {
        return 0;
    };
    let target_key =
        target_session.map(|session| format!("session:{}", sanitize_terminal_session_key(session)));
    let mut removed = Vec::new();
    if let Ok(mut containers) = DOCKER_TERMINAL_CONTAINERS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
    {
        let keys = containers.keys().cloned().collect::<Vec<_>>();
        for key in keys {
            if target_key.as_ref().is_some_and(|target| target != &key) {
                continue;
            }
            if let Some(container_id) = containers.remove(&key) {
                removed.push(container_id);
            }
        }
    }
    for container_id in &removed {
        let _ = hidden_std_command_spawn(&docker)
            .args(["rm", "-f", container_id])
            .output();
    }
    let mut removed_count = removed.len();
    if let Some(target_key) = target_key {
        if let Some(container_id) = find_docker_terminal_container(&docker, &target_key, false) {
            let _ = hidden_std_command_spawn(&docker)
                .args(["rm", "-f", &container_id])
                .output();
            removed_count += 1;
        }
    } else {
        removed_count += cleanup_all_labeled_docker_terminal_containers(&docker);
    }
    removed_count
}

fn cleanup_all_labeled_docker_terminal_containers(docker: &str) -> usize {
    let output = hidden_std_command_output(
        docker,
        [
            "ps",
            "-a",
            "--filter",
            &format!("label={DOCKER_LABEL_AGENT}=1"),
            "--filter",
            &format!(
                "label={DOCKER_LABEL_PROFILE}={}",
                docker_label_value(&docker_profile_label())
            ),
            "--format",
            "{{.ID}}",
        ],
    );
    let Ok(output) = output else {
        return 0;
    };
    if !output.status.success() {
        return 0;
    }
    let ids = decode_terminal_output(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    for container_id in &ids {
        let _ = hidden_std_command_spawn(docker)
            .args(["rm", "-f", container_id])
            .output();
    }
    ids.len()
}

fn update_docker_session_cwd(
    marker: Option<&str>,
    session_id: Option<&str>,
    workspace: &Path,
    stdout: &str,
) -> (String, Option<String>) {
    let Some(marker) = marker else {
        return (stdout.to_string(), None);
    };
    let (stdout, container_cwd) = extract_cwd_marker(stdout, marker);
    let Some(session_id) = session_id else {
        return (stdout, None);
    };
    if let Some(container_cwd) = container_cwd {
        if let Some(host_cwd) = container_workspace_path_to_host(workspace, &container_cwd)
            .and_then(|path| normalize_session_cwd(workspace, &path))
        {
            set_terminal_session_cwd(session_id, host_cwd.clone());
            return (stdout, Some(format!("sessionCwd: {}", host_cwd.display())));
        }
        return (
            stdout,
            Some(format!(
                "sessionCwd: unchanged; marker path is outside /workspace: {}",
                container_cwd.display()
            )),
        );
    }
    (stdout, None)
}

fn shell_command(command: &str) -> Command {
    #[cfg(windows)]
    {
        let mut child = Command::new("powershell.exe");
        child.hide_window();
        child.args(["-NoProfile", "-Command", command]);
        child
    }
    #[cfg(not(windows))]
    {
        let mut child = Command::new("sh");
        child.hide_window();
        child.args(["-lc", command]);
        child
    }
}

fn workspace_cwd(agent: &AgentDefinition, value: Option<&Value>) -> AppResult<PathBuf> {
    let root = workspace_root(agent)?;
    let cwd = value.and_then(Value::as_str).unwrap_or(".");
    let path = resolve_workspace_path(&root, cwd)?;
    if path.is_dir() {
        Ok(path)
    } else {
        Err(AppError::BadRequest(format!(
            "cwd is not a directory: {}",
            path.display()
        )))
    }
}

#[cfg(test)]
mod docker_lifecycle_tests {
    use super::*;

    #[test]
    fn docker_label_value_is_query_safe_and_bounded() {
        let value =
            docker_label_value("session:abc/def 中文 repeated repeated repeated repeated repeated");
        assert!(value.len() <= 63);
        assert!(value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '.' || ch == '-'));
        assert!(value.starts_with("session_abc_def"));
        assert_eq!(docker_label_value("///"), "___");
        assert_eq!(docker_label_value(""), "unknown");
    }

    #[test]
    fn docker_container_name_uses_label_safe_key() {
        let name = docker_container_name("session:run/id with spaces");
        assert!(name.len() <= 63);
        assert!(name.starts_with("synthchat-session_run_id_with_spaces"));
        assert!(!name.contains(':'));
        assert!(!name.contains('/'));
    }

    #[test]
    fn docker_prune_output_count_counts_deleted_container_ids() {
        let stdout = "Deleted Containers:\nabc123\n def456 \n\nTotal reclaimed space: 12B\n";
        assert_eq!(count_pruned_docker_containers(stdout), 2);
        assert_eq!(
            count_pruned_docker_containers("Total reclaimed space: 0B\n"),
            0
        );
    }

    #[test]
    fn docker_entrypoint_parser_detects_s6_init() {
        assert!(docker_entrypoint_uses_init(r#"["/init"]"#));
        assert!(docker_entrypoint_uses_init(
            r#"["/package/admin/s6-overlay/command/init"]"#
        ));
        assert!(docker_entrypoint_uses_init(r#""/init""#));
        assert!(!docker_entrypoint_uses_init(r#"["/bin/sh"]"#));
        assert!(!docker_entrypoint_uses_init("null"));
        assert!(!docker_entrypoint_uses_init(""));
    }

    #[test]
    fn docker_security_args_switch_run_tmpfs_exec_for_init_images() {
        let mut normal = Vec::new();
        push_docker_security_args(&mut normal, false);
        assert!(normal
            .iter()
            .any(|arg| arg == "/run:rw,noexec,nosuid,size=64m"));
        assert!(!normal
            .iter()
            .any(|arg| arg == "/run:rw,exec,nosuid,size=64m"));

        let mut init = Vec::new();
        push_docker_security_args(&mut init, true);
        assert!(init.iter().any(|arg| arg == "/run:rw,exec,nosuid,size=64m"));
        assert!(!init
            .iter()
            .any(|arg| arg == "/run:rw,noexec,nosuid,size=64m"));
    }

    #[test]
    fn singularity_bind_args_include_readonly_suffix() {
        let mut args = Vec::new();
        push_singularity_bind(&mut args, Path::new("/host/path"), "/container/path", true);
        assert_eq!(args, vec!["--bind", "/host/path:/container/path:ro"]);
    }
}

#[cfg(test)]
mod ssh_backend_tests {
    use super::*;

    #[test]
    fn posix_shell_quote_handles_empty_and_single_quotes() {
        assert_eq!(posix_shell_quote(""), "''");
        assert_eq!(posix_shell_quote("simple"), "'simple'");
        assert_eq!(posix_shell_quote("a'b"), "'a'\"'\"'b'");
    }

    #[test]
    fn posix_quote_cwd_preserves_home_expansion() {
        assert_eq!(posix_quote_cwd("~"), "\"$HOME\"");
        assert_eq!(posix_quote_cwd("~/work dir"), "\"$HOME\"/'work dir'");
        assert_eq!(posix_quote_cwd("/tmp/a b"), "'/tmp/a b'");
    }

    #[test]
    fn ssh_session_cwd_updates_from_marker() {
        let marker = "__SYNTHCHAT_SSH_CWD_test__";
        let (stdout, note) = update_ssh_session_cwd(
            Some(marker),
            Some("ssh-test-session"),
            &format!("hello\n{marker}/home/user/project{marker}\n"),
        );
        assert_eq!(stdout, "hello\n");
        assert_eq!(note.unwrap(), "sessionCwd: /home/user/project");
        assert_eq!(
            ssh_terminal_session_cwd("ssh-test-session").unwrap(),
            "/home/user/project"
        );
        clear_ssh_terminal_session_cwds(Some("ssh-test-session"));
    }

    #[test]
    fn ssh_remote_path_helpers_quote_parent_and_scp_target() {
        assert_eq!(
            remote_parent_path("~/.synthchat/cache/a.txt"),
            "~/.synthchat/cache"
        );
        assert_eq!(remote_parent_path("/tmp/a.txt"), "/tmp");
        assert_eq!(remote_parent_path("file.txt"), "");
        assert_eq!(
            scp_remote_target_path("~/.synthchat/cache/a b.txt"),
            "~/'.synthchat/cache/a b.txt'"
        );
        assert_eq!(scp_remote_target_path("/tmp/a b.txt"), "'/tmp/a b.txt'");
    }

    #[test]
    fn ssh_remote_relative_path_stays_under_base() {
        assert_eq!(
            remote_relative_path("~/.synthchat/cache/a.txt", "~/.synthchat")
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/"),
            "cache/a.txt"
        );
        assert!(remote_relative_path("~/.synthchat/../escape", "~/.synthchat").is_none());
        assert!(remote_relative_path("/tmp/a.txt", "~/.synthchat").is_none());
    }

    #[test]
    fn ssh_sync_stale_path_tracking_reports_removed_paths() {
        let key = "test@example:22:~/.synthchat";
        set_ssh_synced_paths(
            key,
            HashSet::from([
                "~/.synthchat/cache/a.txt".to_string(),
                "~/.synthchat/cache/b.txt".to_string(),
            ]),
        );
        let stale = stale_ssh_synced_paths(
            key,
            &HashSet::from(["~/.synthchat/cache/a.txt".to_string()]),
        );
        assert_eq!(stale, vec!["~/.synthchat/cache/b.txt"]);
        set_ssh_synced_paths(key, HashSet::new());
    }

    #[test]
    fn quoted_rm_command_quotes_remote_paths() {
        let command = quoted_rm_command(&[
            "~/.synthchat/cache/a b.txt".to_string(),
            "/tmp/quote'file".to_string(),
        ]);
        assert_eq!(
            command,
            "rm -f -- \"$HOME\"/'.synthchat/cache/a b.txt' '/tmp/quote'\"'\"'file'"
        );
    }
}
