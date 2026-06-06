use std::{
    collections::HashMap,
    env, fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command as StdCommand, ExitStatus, Stdio},
    sync::{Mutex, OnceLock},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde_json::{json, Value};
use tokio::{io::AsyncWriteExt, process::Command};

use crate::{
    error::{AppError, AppResult},
    models::AgentRunRecord,
    store::AppStore,
};

use super::{delegation_request::DelegateTaskRequest, redact_sensitive_text, truncate_for_prompt};

const DEFAULT_TIMEOUT_SECONDS: u64 = 60;
const MAX_TIMEOUT_SECONDS: u64 = 300;
const PYTHON_PLUGIN_HOOK_TIMEOUT_SECONDS: u64 = 60;
const PYTHON_PLUGIN_TOOL_CACHE_TTL: Duration = Duration::from_secs(30);
const PYTHON_PLUGIN_HOOK_RUNNER: &str = r#"
import asyncio
import importlib.util
import inspect
import json
import os
import sys
import traceback

class PluginContext:
    def __init__(self, plugin_dir=""):
        self.plugin_dir = plugin_dir
        self.hooks = {}
        self.commands = {}
        self.tools = {}
        self.skills = {}
        self.injected_messages = []

    def register_hook(self, hook_name, callback):
        self.hooks.setdefault(str(hook_name), []).append(callback)

    def register_tool(
        self,
        name,
        toolset="plugin",
        schema=None,
        handler=None,
        check_fn=None,
        requires_env=None,
        is_async=False,
        description="",
        emoji="",
        override=False,
    ):
        clean = str(name or "").strip()
        if clean and callable(handler):
            self.tools[clean] = {
                "handler": handler,
                "toolset": toolset,
                "schema": schema or {},
                "check_fn": check_fn,
                "requires_env": requires_env or [],
                "is_async": bool(is_async),
                "description": description or "",
                "emoji": emoji or "",
            }
        return None

    def register_command(self, name, handler=None, description="", args_hint=""):
        clean = str(name or "").lower().strip().lstrip("/").replace(" ", "-")
        if clean and callable(handler):
            self.commands[clean] = {
                "handler": handler,
                "description": description or "Plugin command",
                "args_hint": (args_hint or "").strip(),
            }
        return None

    def register_cli_command(
        self,
        name,
        help="",
        setup_fn=None,
        handler_fn=None,
        description="",
    ):
        clean = str(name or "").lower().strip().lstrip("/").replace(" ", "-")
        if clean and callable(handler_fn):
            self.commands[clean] = {
                "handler": handler_fn,
                "description": description or help or "Plugin CLI command",
                "args_hint": "",
            }
        return None

    def register_skill(self, name, path, description=""):
        clean = str(name or "").strip()
        if not clean or ":" in clean:
            raise ValueError("plugin skill name must be non-empty and must not contain ':'")
        raw_path = os.fspath(path)
        skill_path = raw_path if os.path.isabs(raw_path) else os.path.join(self.plugin_dir, raw_path)
        if not os.path.isfile(skill_path):
            raise FileNotFoundError("SKILL.md not found at " + skill_path)
        self.skills[clean] = {
            "name": clean,
            "path": skill_path,
            "description": description or "",
        }
        return None

    def register_auxiliary_task(self, *args, **kwargs):
        return None

    def inject_message(self, content, role="user"):
        text = str(content or "").strip()
        clean_role = str(role or "user").strip().lower()
        if not text:
            return False
        if clean_role not in ("user", "assistant", "system", "tool"):
            clean_role = "user"
        self.injected_messages.append({"role": clean_role, "content": text})
        return True

def _jsonable(value):
    try:
        json.dumps(value)
        return value
    except Exception:
        return str(value)

async def _call(callback, kwargs):
    result = callback(**kwargs)
    if inspect.isawaitable(result):
        result = await result
    return _jsonable(result)

async def main():
    request = json.loads(sys.stdin.read() or "{}")
    plugin_dir = request.get("plugin_dir") or ""
    plugin_id = request.get("plugin_id") or "plugin"
    event = request.get("event") or ""
    command_name = request.get("command_name") or ""
    raw_args = request.get("raw_args") or ""
    tool_name = request.get("tool_name") or ""
    tool_args = request.get("tool_args") or {}
    list_tools = bool(request.get("list_tools"))
    list_skills = bool(request.get("list_skills"))
    kwargs = request.get("kwargs") or {}
    init_file = os.path.join(plugin_dir, "__init__.py")
    if not os.path.isfile(init_file):
        print(json.dumps({"results": []}))
        return
    parent_dir = os.path.dirname(plugin_dir)
    if parent_dir and parent_dir not in sys.path:
        sys.path.insert(0, parent_dir)
    module_name = "synthchat_hermes_plugin_" + "".join(
        ch if ch.isalnum() else "_" for ch in plugin_id
    )
    spec = importlib.util.spec_from_file_location(
        module_name,
        init_file,
        submodule_search_locations=[plugin_dir],
    )
    if spec is None or spec.loader is None:
        raise RuntimeError("cannot load plugin module")
    module = importlib.util.module_from_spec(spec)
    sys.modules[module_name] = module
    spec.loader.exec_module(module)
    ctx = PluginContext(plugin_dir)
    register = getattr(module, "register", None)
    if callable(register):
        register(ctx)
    if list_skills:
        print(json.dumps({"skills": list(ctx.skills.values())}))
        return
    if list_tools:
        tools = []
        for name, tool in ctx.tools.items():
            available = True
            error = ""
            for env_name in tool.get("requires_env") or []:
                if isinstance(env_name, dict):
                    env_name = env_name.get("name") or env_name.get("key") or env_name.get("var") or env_name.get("env")
                if env_name and not os.environ.get(str(env_name)):
                    available = False
                    error = "missing required env: " + str(env_name)
                    break
            if available:
                check_fn = tool.get("check_fn")
                if callable(check_fn):
                    check = check_fn()
                    if inspect.isawaitable(check):
                        check = await check
                    if check is False:
                        available = False
                        error = "plugin tool requirement check failed"
            if not available:
                continue
            tools.append({
                "name": name,
                "toolset": _jsonable(tool.get("toolset") or "plugin"),
                "schema": _jsonable(tool.get("schema") or {}),
                "description": _jsonable(tool.get("description") or ""),
                "emoji": _jsonable(tool.get("emoji") or ""),
            })
        print(json.dumps({"tools": tools}))
        return
    if command_name:
        clean = str(command_name).lower().strip().lstrip("/").replace(" ", "-")
        command = ctx.commands.get(clean)
        if not command:
            print(json.dumps({"handled": False}))
            return
        result = command["handler"](raw_args)
        if inspect.isawaitable(result):
            result = await result
        print(json.dumps({
            "handled": True,
            "result": _jsonable(result),
            "injected_messages": ctx.injected_messages,
        }))
        return
    if tool_name:
        clean_tool = str(tool_name).strip()
        tool = ctx.tools.get(clean_tool)
        if not tool:
            print(json.dumps({"ok": False, "error": "plugin did not register requested tool"}))
            return
        for name in tool.get("requires_env") or []:
            if isinstance(name, dict):
                name = name.get("name") or name.get("key") or name.get("var") or name.get("env")
            if name and not os.environ.get(str(name)):
                print(json.dumps({"ok": False, "error": "missing required env: " + str(name)}))
                return
        check_fn = tool.get("check_fn")
        if callable(check_fn):
            check = check_fn()
            if inspect.isawaitable(check):
                check = await check
            if check is False:
                print(json.dumps({"ok": False, "error": "plugin tool requirement check failed"}))
                return
        result = tool["handler"](tool_args)
        if inspect.isawaitable(result):
            result = await result
        print(json.dumps({"ok": True, "result": _jsonable(result)}))
        return
    results = []
    for callback in ctx.hooks.get(event, []):
        results.append(await _call(callback, kwargs))
    print(json.dumps({"results": results}))

try:
    asyncio.run(main())
except Exception as exc:
    print(json.dumps({"error": str(exc), "traceback": traceback.format_exc()}))
    sys.exit(2)
"#;

#[derive(Debug, Clone)]
struct ShellHookSpec {
    event: String,
    command: String,
    matcher: Option<String>,
    timeout_seconds: u64,
}

#[derive(Debug, Clone)]
struct PythonPluginHookSpec {
    plugin_id: String,
    plugin_name: String,
    path: PathBuf,
}

#[derive(Debug, Clone)]
pub(super) struct PythonPluginToolDefinition {
    pub(super) plugin_id: String,
    pub(super) plugin_name: String,
    pub(super) name: String,
    pub(super) toolset: String,
    pub(super) schema: Value,
    pub(super) description: String,
}

#[derive(Debug, Clone)]
pub(super) struct PythonPluginSkillDefinition {
    pub(super) plugin_id: String,
    pub(super) plugin_name: String,
    pub(super) name: String,
    pub(super) path: PathBuf,
    pub(super) description: String,
}

#[derive(Debug, Clone)]
pub(super) struct PythonPluginInjectedMessage {
    pub(super) role: String,
    pub(super) content: String,
}

#[derive(Debug, Clone)]
pub(super) struct PythonPluginCommandResult {
    pub(super) reply: String,
    pub(super) injected_messages: Vec<PythonPluginInjectedMessage>,
}

#[derive(Debug, Clone)]
struct CachedPythonPluginTools {
    captured_at: Instant,
    tools: Vec<PythonPluginToolDefinition>,
}

static PYTHON_PLUGIN_TOOL_CACHE: OnceLock<Mutex<HashMap<String, CachedPythonPluginTools>>> =
    OnceLock::new();

#[derive(Debug, Clone)]
struct CachedPythonPluginSkills {
    captured_at: Instant,
    skills: Vec<PythonPluginSkillDefinition>,
}

static PYTHON_PLUGIN_SKILL_CACHE: OnceLock<Mutex<HashMap<String, CachedPythonPluginSkills>>> =
    OnceLock::new();

#[derive(Debug)]
struct ShellHookDiagnosticRun {
    returncode: Option<i32>,
    timed_out: bool,
    stdout: String,
    stderr: String,
    parsed: Option<Value>,
    error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum PreGatewayDispatchDecision {
    Allow,
    Skip(String),
    Rewrite(String),
}

pub(super) async fn run_pre_tool_call_hooks(
    store: &AppStore,
    run_id: &str,
    tool_name: &str,
    payload: &Value,
) -> AppResult<()> {
    for spec in shell_hook_specs(store, "pre_tool_call")? {
        if !spec.matches_tool(tool_name) {
            continue;
        }
        if let Some(response) = run_shell_hook(&spec, run_id, tool_name, payload, None).await? {
            if let Some(message) = shell_hook_block_message(&response) {
                return Err(AppError::BadRequest(format!(
                    "blocked by shell hook: {message}"
                )));
            }
        }
    }
    let plugin_payload = json!({
        "tool_name": tool_name,
        "args": payload,
        "tool_input": payload,
        "task_id": run_id,
        "session_id": run_id,
    });
    for response in run_python_plugin_hooks(store, "pre_tool_call", &plugin_payload).await {
        if let Some(message) = shell_hook_block_message(&response) {
            return Err(AppError::BadRequest(format!(
                "blocked by plugin hook: {message}"
            )));
        }
    }
    Ok(())
}

pub(super) async fn run_post_tool_call_hooks(
    store: &AppStore,
    run_id: &str,
    tool_name: &str,
    payload: &Value,
    result: &Value,
) -> AppResult<()> {
    for spec in shell_hook_specs(store, "post_tool_call")? {
        if !spec.matches_tool(tool_name) {
            continue;
        }
        let _ = run_shell_hook(&spec, run_id, tool_name, payload, Some(result)).await;
    }
    let plugin_payload = json!({
        "tool_name": tool_name,
        "args": payload,
        "tool_input": payload,
        "result": result,
        "task_id": run_id,
        "session_id": run_id,
    });
    let _ = run_python_plugin_hooks(store, "post_tool_call", &plugin_payload).await;
    Ok(())
}

pub(super) async fn run_transform_terminal_output_hooks(
    store: &AppStore,
    run_id: &str,
    command: &str,
    output: &str,
    returncode: i32,
) -> String {
    let Ok(specs) = shell_hook_specs(store, "transform_terminal_output") else {
        return output.to_string();
    };
    let payload = json!({
        "command": command,
        "output": output,
        "returncode": returncode,
    });
    for spec in specs {
        let Ok(Some(response)) = run_shell_hook(&spec, run_id, "terminal", &payload, None).await
        else {
            continue;
        };
        if let Some(text) = response
            .get("output")
            .or_else(|| response.get("text"))
            .and_then(Value::as_str)
        {
            return text.to_string();
        }
    }
    output.to_string()
}

pub(super) async fn run_transform_tool_result_hooks(
    store: &AppStore,
    run_id: &str,
    tool_name: &str,
    payload: &Value,
    result_text: &str,
    ok: bool,
    error: Option<&str>,
) -> String {
    let Ok(specs) = shell_hook_specs(store, "transform_tool_result") else {
        return result_text.to_string();
    };
    let hook_payload = json!({
        "tool_name": tool_name,
        "args": payload,
        "tool_input": payload,
        "result": result_text,
        "text": result_text,
        "output": result_text,
        "ok": ok,
        "error": error,
    });
    for spec in specs {
        if !spec.matches_tool(tool_name) {
            continue;
        }
        let Ok(Some(response)) =
            run_shell_hook(&spec, run_id, tool_name, &hook_payload, None).await
        else {
            continue;
        };
        if let Some(text) = response
            .get("result")
            .or_else(|| response.get("text"))
            .or_else(|| response.get("output"))
            .and_then(Value::as_str)
        {
            return text.to_string();
        }
    }
    for response in run_python_plugin_hooks(store, "transform_tool_result", &hook_payload).await {
        if let Some(text) = response.as_str().map(ToOwned::to_owned).or_else(|| {
            response
                .get("result")
                .or_else(|| response.get("text"))
                .or_else(|| response.get("output"))
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        }) {
            return text;
        }
    }
    result_text.to_string()
}

pub(super) async fn run_pre_llm_call_hooks(
    store: &AppStore,
    run_id: &str,
    user_content: &str,
) -> Vec<String> {
    let Ok(specs) = shell_hook_specs(store, "pre_llm_call") else {
        return Vec::new();
    };
    let payload = json!({
        "messages": [{
            "role": "user",
            "content": user_content,
        }],
        "user_content": user_content,
    });
    let mut contexts = Vec::new();
    for spec in specs {
        let Ok(Some(response)) = run_shell_hook(&spec, run_id, "llm", &payload, None).await else {
            continue;
        };
        if let Some(context) = response
            .get("context")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            contexts.push(context.to_string());
        }
    }
    for response in run_python_plugin_hooks(store, "pre_llm_call", &payload).await {
        if let Some(context) = response
            .get("context")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            contexts.push(context.to_string());
        }
    }
    contexts
}

pub(super) fn inject_pre_llm_hook_context(user_content: &str, contexts: &[String]) -> String {
    let contexts = contexts
        .iter()
        .map(|context| context.trim())
        .filter(|context| !context.is_empty())
        .collect::<Vec<_>>();
    if contexts.is_empty() {
        return user_content.to_string();
    }
    format!("{}\n\n{}", contexts.join("\n\n"), user_content.trim_start())
}

pub(super) async fn run_transform_llm_output_hooks(
    store: &AppStore,
    run_id: &str,
    user_content: &str,
    response_text: &str,
    model: Option<&str>,
    provider_id: Option<&str>,
) -> String {
    let Ok(specs) = shell_hook_specs(store, "transform_llm_output") else {
        return response_text.to_string();
    };
    let payload = json!({
        "user_message": user_content,
        "response_text": response_text,
        "assistant_response": response_text,
        "model": model.unwrap_or_default(),
        "provider": provider_id.unwrap_or_default(),
    });
    for spec in specs {
        let Ok(Some(response)) = run_shell_hook(&spec, run_id, "llm", &payload, None).await else {
            continue;
        };
        if let Some(text) = response
            .get("response_text")
            .or_else(|| response.get("assistant_response"))
            .or_else(|| response.get("output"))
            .or_else(|| response.get("text"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            return text.to_string();
        }
    }
    for response in run_python_plugin_hooks(store, "transform_llm_output", &payload).await {
        if let Some(text) = response
            .as_str()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
            .or_else(|| {
                response
                    .get("response_text")
                    .or_else(|| response.get("assistant_response"))
                    .or_else(|| response.get("output"))
                    .or_else(|| response.get("text"))
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(ToOwned::to_owned)
            })
        {
            return text;
        }
    }
    response_text.to_string()
}

pub(super) async fn run_post_llm_call_hooks(
    store: &AppStore,
    run_id: &str,
    user_content: &str,
    response_text: &str,
    model: Option<&str>,
    provider_id: Option<&str>,
) {
    let Ok(specs) = shell_hook_specs(store, "post_llm_call") else {
        return;
    };
    let payload = json!({
        "user_message": user_content,
        "response_text": response_text,
        "assistant_response": response_text,
        "model": model.unwrap_or_default(),
        "provider": provider_id.unwrap_or_default(),
    });
    for spec in specs {
        let _ = run_shell_hook(&spec, run_id, "llm", &payload, None).await;
    }
    let _ = run_python_plugin_hooks(store, "post_llm_call", &payload).await;
}

pub(super) async fn run_pre_api_request_hooks(store: &AppStore, run_id: &str, payload: &Value) {
    let Ok(specs) = shell_hook_specs(store, "pre_api_request") else {
        return;
    };
    for spec in specs {
        let _ = run_shell_hook(&spec, run_id, "llm_api", payload, None).await;
    }
}

pub(super) async fn run_post_api_request_hooks(store: &AppStore, run_id: &str, payload: &Value) {
    let Ok(specs) = shell_hook_specs(store, "post_api_request") else {
        return;
    };
    for spec in specs {
        let _ = run_shell_hook(&spec, run_id, "llm_api", payload, None).await;
    }
}

pub(super) async fn run_pre_gateway_dispatch_hooks(
    store: &AppStore,
    platform: &str,
    inbound: &Value,
    text: &str,
) -> PreGatewayDispatchDecision {
    let Ok(specs) = shell_hook_specs(store, "pre_gateway_dispatch") else {
        return PreGatewayDispatchDecision::Allow;
    };
    if specs.is_empty() {
        return PreGatewayDispatchDecision::Allow;
    }
    let source = inbound.get("source").cloned().unwrap_or(Value::Null);
    let event_id = inbound
        .get("eventId")
        .or_else(|| inbound.get("event_id"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let payload = json!({
        "event": inbound,
        "inbound": inbound,
        "source": source,
        "text": text,
        "platform": platform,
        "event_id": event_id,
        "eventId": event_id,
    });
    for spec in specs {
        let Ok(Some(response)) = run_shell_hook(&spec, event_id, platform, &payload, None).await
        else {
            continue;
        };
        match response.get("action").and_then(Value::as_str) {
            Some("skip") => {
                let reason = response
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or("skipped by pre_gateway_dispatch hook")
                    .to_string();
                return PreGatewayDispatchDecision::Skip(reason);
            }
            Some("rewrite") => {
                if let Some(text) = response.get("text").and_then(Value::as_str) {
                    return PreGatewayDispatchDecision::Rewrite(text.to_string());
                }
            }
            Some("allow") => return PreGatewayDispatchDecision::Allow,
            _ => {}
        }
    }
    PreGatewayDispatchDecision::Allow
}

pub(super) async fn run_session_lifecycle_hooks(
    store: &AppStore,
    event: &str,
    run: &crate::models::AgentRunRecord,
    extra: Value,
) {
    let Ok(specs) = shell_hook_specs(store, event) else {
        return;
    };
    let payload = json!({
        "session_id": run.run_id,
        "run_id": run.run_id,
        "conversation_id": run.conversation_id,
        "persona_id": run.persona_id,
        "agent_id": run.agent_id,
        "status": run.state,
        "state": run.state,
        "user_message": run.user_request,
        "queue_item_id": run.queue_item_id,
        "updated_at": run.updated_at,
        "completed_at": run.completed_at,
        "error": run.error,
        "extra": extra,
    });
    for spec in specs {
        let _ = run_shell_hook(&spec, &run.run_id, "session", &payload, None).await;
    }
}

pub(super) async fn run_session_finished_hooks(
    store: &AppStore,
    run: &crate::models::AgentRunRecord,
    extra: Value,
) {
    run_session_lifecycle_hooks(store, "on_session_end", run, extra.clone()).await;
    run_session_lifecycle_hooks(store, "on_session_finalize", run, extra).await;
}

pub(super) fn spawn_session_finished_hooks(
    store: &AppStore,
    run: crate::models::AgentRunRecord,
    extra: Value,
) {
    let store = store.clone();
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        return;
    };
    handle.spawn(async move {
        run_session_finished_hooks(&store, &run, extra).await;
    });
}

pub(super) async fn run_session_reset_hooks(
    store: &AppStore,
    conversation: &crate::models::Conversation,
    extra: Value,
) {
    let Ok(specs) = shell_hook_specs(store, "on_session_reset") else {
        return;
    };
    let payload = json!({
        "session_id": conversation.id,
        "conversation_id": conversation.id,
        "persona_id": conversation.persona_id,
        "agent_id": conversation.agent_id,
        "title": conversation.title,
        "extra": extra,
    });
    for spec in specs {
        let _ = run_shell_hook(&spec, &conversation.id, "session", &payload, None).await;
    }
}

pub(super) fn spawn_session_reset_hooks(
    store: &AppStore,
    conversation: crate::models::Conversation,
    extra: Value,
) {
    let store = store.clone();
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        return;
    };
    handle.spawn(async move {
        run_session_reset_hooks(&store, &conversation, extra).await;
    });
}

pub(super) async fn run_subagent_stop_hooks(
    store: &AppStore,
    parent_run_id: &str,
    child_run: &AgentRunRecord,
    request: &DelegateTaskRequest,
    status: &str,
    summary: &str,
    transport: &str,
    extra: Value,
) {
    let Ok(specs) = shell_hook_specs(store, "subagent_stop") else {
        return;
    };
    if specs.is_empty() {
        return;
    }
    let payload = json!({
        "parent_session_id": parent_run_id,
        "parent_run_id": parent_run_id,
        "child_session_id": child_run.run_id,
        "child_run_id": child_run.run_id,
        "child_conversation_id": child_run.conversation_id,
        "child_role": request.role,
        "child_task": request.task,
        "child_summary": summary,
        "child_status": status,
        "status": status,
        "transport": transport,
        "toolsets": request.toolsets,
        "max_iterations": request.max_iterations,
        "maxIterations": request.max_iterations,
        "started_at": child_run.started_at,
        "completed_at": child_run.completed_at,
        "error": child_run.error,
        "extra": extra,
    });
    for spec in specs {
        let _ = run_shell_hook(&spec, parent_run_id, "subagent", &payload, None).await;
    }
}

pub(super) async fn run_pre_approval_request_hooks(
    store: &AppStore,
    run_id: &str,
    server_id: &str,
    tool_name: &str,
    payload: &Value,
    reason: &str,
) {
    run_approval_lifecycle_hooks(
        store,
        "pre_approval_request",
        run_id,
        server_id,
        tool_name,
        payload,
        json!({
            "reason": reason,
            "description": reason,
            "status": "pending",
        }),
    )
    .await;
}

pub(super) async fn run_post_approval_response_hooks(
    store: &AppStore,
    approval: &crate::models::ToolApprovalRequest,
) {
    let run_id = approval.run_id.as_deref().unwrap_or("approval");
    run_approval_lifecycle_hooks(
        store,
        "post_approval_response",
        run_id,
        &approval.server_id,
        &approval.tool_name,
        &approval.payload,
        json!({
            "approval_id": approval.id,
            "status": approval.status,
            "reason": approval.reason,
            "result": approval.result,
            "error": approval.error,
        }),
    )
    .await;
}

pub(super) fn spawn_post_approval_response_hooks(
    store: &AppStore,
    approval: crate::models::ToolApprovalRequest,
) {
    let store = store.clone();
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        return;
    };
    handle.spawn(async move {
        run_post_approval_response_hooks(&store, &approval).await;
    });
}

pub(super) fn handle_shell_hooks_control_command(
    store: &AppStore,
    argument_raw: &str,
) -> AppResult<String> {
    let mut parts = argument_raw.split_whitespace();
    let action = parts.next().unwrap_or("list").to_lowercase();
    match action.as_str() {
        "" | "list" | "status" => format_shell_hooks_status(store),
        "test" | "run" => {
            let event = parts.next().unwrap_or_default();
            if event.trim().is_empty() {
                return Ok("用法：/hooks test <event> [tool]".into());
            }
            let tool_name = parts.next();
            format_shell_hooks_test(store, event, tool_name)
        }
        "doctor" | "check" => format_shell_hooks_doctor(store),
        "revoke" | "untrust" | "remove" | "rm" => {
            let command = parts.collect::<Vec<_>>().join(" ");
            if command.trim().is_empty() {
                return Ok("用法：/hooks revoke <command>".into());
            }
            let removed = revoke_shell_hook_approval(store, None, command.trim())?;
            Ok(format!(
                "已撤销 shell hook 信任 {removed} 条。\n\n{}",
                format_shell_hooks_status(store)?
            ))
        }
        "reset" | "clear" => {
            save_shell_hook_allowlist(store, &json!({ "approvals": [] }))?;
            Ok(format!(
                "已清空 shell hook 信任。\n\n{}",
                format_shell_hooks_status(store)?
            ))
        }
        _ => Ok("用法：/hooks [list|test <event> [tool]|doctor|revoke <command>|reset]".into()),
    }
}

fn format_shell_hooks_test(
    store: &AppStore,
    event: &str,
    tool_name: Option<&str>,
) -> AppResult<String> {
    let chat = store.config()?.chat;
    let tool_name = tool_name
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| default_shell_hook_tool_name(event));
    let mut specs = Vec::new();
    if let Some(entries) = chat.hooks.get(event).and_then(Value::as_array) {
        for entry in entries {
            let Some(spec) = shell_hook_spec(event, entry) else {
                continue;
            };
            if spec.matches_tool(tool_name) {
                specs.push(spec);
            }
        }
    }
    if specs.is_empty() {
        return Ok(format!(
            "没有找到可测试的 shell hook：event={event} tool={tool_name}"
        ));
    }
    let payload = shell_hook_test_payload(event, tool_name);
    let mut lines = vec![format!(
        "测试 shell hooks：event={event} tool={tool_name} count={}",
        specs.len()
    )];
    for spec in specs {
        let result = run_shell_hook_diagnostic(&spec, "hooks-test", tool_name, &payload, None);
        lines.push(format!("- command={}", spec.command));
        if let Some(error) = result.error {
            lines.push(format!("  error={}", truncate_for_prompt(&error, 240)));
            continue;
        }
        lines.push(format!(
            "  exit={} timedOut={}",
            result
                .returncode
                .map(|code| code.to_string())
                .unwrap_or_else(|| "-".into()),
            result.timed_out
        ));
        let stdout = result.stdout.trim();
        if !stdout.is_empty() {
            lines.push(format!(
                "  stdout={}",
                truncate_for_prompt(&redact_sensitive_text(stdout), 400)
            ));
        }
        let stderr = result.stderr.trim();
        if !stderr.is_empty() {
            lines.push(format!(
                "  stderr={}",
                truncate_for_prompt(&redact_sensitive_text(stderr), 400)
            ));
        }
        if let Some(parsed) = result.parsed {
            lines.push(format!(
                "  parsed={}",
                truncate_for_prompt(&redact_sensitive_text(&parsed.to_string()), 400)
            ));
        } else {
            lines.push("  parsed=<none>".into());
        }
    }
    Ok(lines.join("\n"))
}

fn format_shell_hooks_status(store: &AppStore) -> AppResult<String> {
    let chat = store.config()?.chat;
    let mut configured = Vec::new();
    if let Some(object) = chat.hooks.as_object() {
        for (event, entries) in object {
            let Some(entries) = entries.as_array() else {
                continue;
            };
            for entry in entries {
                if let Some(spec) = shell_hook_spec(event, entry) {
                    configured.push(format!(
                        "- {} matcher={} command={}",
                        spec.event,
                        spec.matcher.as_deref().unwrap_or("*"),
                        spec.command
                    ));
                }
            }
        }
    }
    let allowlist = load_shell_hook_allowlist(store)?;
    let trusted = allowlist
        .get("approvals")
        .and_then(Value::as_array)
        .map(|approvals| {
            approvals
                .iter()
                .filter_map(|approval| {
                    let event = approval.get("event").and_then(Value::as_str)?;
                    let command = approval.get("command").and_then(Value::as_str)?;
                    let approved_at = approval
                        .get("approvedAt")
                        .or_else(|| approval.get("approved_at"))
                        .map(|value| value.to_string())
                        .unwrap_or_else(|| "-".into());
                    Some(format!(
                        "- {event} approvedAt={approved_at} command={command}"
                    ))
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Ok(format!(
        "Shell hooks：autoAccept={} envAccept={}\n\n配置项：\n{}\n\n已信任：\n{}",
        chat.hooks_auto_accept,
        env_flag("SYNTHCHAT_ACCEPT_HOOKS") || env_flag("HERMES_ACCEPT_HOOKS"),
        if configured.is_empty() {
            "- none".into()
        } else {
            configured.join("\n")
        },
        if trusted.is_empty() {
            "- none".into()
        } else {
            trusted.join("\n")
        }
    ))
}

fn format_shell_hooks_doctor(store: &AppStore) -> AppResult<String> {
    let chat = store.config()?.chat;
    let allowlist = load_shell_hook_allowlist(store)?;
    let mut rows = Vec::new();
    if let Some(object) = chat.hooks.as_object() {
        for (event, entries) in object {
            let Some(entries) = entries.as_array() else {
                continue;
            };
            for entry in entries {
                let Some(spec) = shell_hook_spec(event, entry) else {
                    continue;
                };
                let approval = shell_hook_allowlist_entry(&allowlist, &spec);
                let trusted = approval.is_some();
                let script = shell_hook_script_path(&spec.command);
                let current_mtime = script.as_deref().and_then(shell_hook_script_mtime_seconds);
                let approved_mtime = approval.and_then(shell_hook_approval_script_mtime);
                let drift = match (trusted, current_mtime, approved_mtime) {
                    (false, _, _) => "untrusted",
                    (true, None, _) => "missing",
                    (true, Some(current), Some(approved)) if current != approved => "changed",
                    (true, Some(_), Some(_)) => "ok",
                    (true, Some(_), None) => "unknown",
                };
                let executable = script
                    .as_deref()
                    .map(shell_hook_script_is_runnable)
                    .unwrap_or(false);
                rows.push(format!(
                    "- {} matcher={} trusted={} drift={} runnable={} script={} command={}",
                    spec.event,
                    spec.matcher.as_deref().unwrap_or("*"),
                    trusted,
                    drift,
                    executable,
                    script
                        .as_deref()
                        .map(Path::display)
                        .map(|value| value.to_string())
                        .unwrap_or_else(|| "-".into()),
                    spec.command
                ));
            }
        }
    }
    Ok(format!(
        "Shell hooks doctor：autoAccept={} envAccept={}\n{}",
        chat.hooks_auto_accept,
        env_flag("SYNTHCHAT_ACCEPT_HOOKS") || env_flag("HERMES_ACCEPT_HOOKS"),
        if rows.is_empty() {
            "- none".into()
        } else {
            rows.join("\n")
        }
    ))
}

fn revoke_shell_hook_approval(
    store: &AppStore,
    event: Option<&str>,
    command: &str,
) -> AppResult<usize> {
    let mut allowlist = load_shell_hook_allowlist(store)?;
    let Some(approvals) = allowlist.get_mut("approvals").and_then(Value::as_array_mut) else {
        return Ok(0);
    };
    let before = approvals.len();
    approvals.retain(|approval| {
        let matches_command = approval.get("command").and_then(Value::as_str) == Some(command);
        let matches_event = event
            .map(|event| approval.get("event").and_then(Value::as_str) == Some(event))
            .unwrap_or(true);
        !(matches_command && matches_event)
    });
    let removed = before.saturating_sub(approvals.len());
    if removed > 0 {
        save_shell_hook_allowlist(store, &allowlist)?;
    }
    Ok(removed)
}

async fn run_approval_lifecycle_hooks(
    store: &AppStore,
    event: &str,
    run_id: &str,
    server_id: &str,
    tool_name: &str,
    payload: &Value,
    extra: Value,
) {
    let Ok(specs) = shell_hook_specs(store, event) else {
        return;
    };
    if specs.is_empty() {
        return;
    }
    let hook_tool_name = format!("{server_id}.{tool_name}");
    let hook_payload = json!({
        "server_id": server_id,
        "tool_name": tool_name,
        "payload": payload,
    });
    for spec in specs {
        let lifecycle_payload = json!({
            "approval": hook_payload,
            "extra": extra,
        });
        let _ = run_shell_hook(
            &spec,
            run_id,
            &hook_tool_name,
            &lifecycle_payload,
            Some(&extra),
        )
        .await;
    }
}

fn env_flag(name: &str) -> bool {
    env::var(name)
        .ok()
        .map(|value| {
            let value = value.trim().to_ascii_lowercase();
            matches!(value.as_str(), "1" | "true" | "yes" | "on")
        })
        .unwrap_or(false)
}

fn shell_hook_specs(store: &AppStore, event: &str) -> AppResult<Vec<ShellHookSpec>> {
    let chat = store.config()?.chat;
    let Some(entries) = chat.hooks.get(event).and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    let auto_accept = shell_hooks_auto_accept_enabled(&chat);
    let mut specs = Vec::new();
    for entry in entries {
        let Some(spec) = shell_hook_spec(event, entry) else {
            continue;
        };
        if auto_accept {
            record_shell_hook_approval(store, &spec)?;
            specs.push(spec);
        } else if shell_hook_is_allowlisted(store, &spec)? {
            specs.push(spec);
        }
    }
    Ok(specs)
}

fn python_plugin_hook_specs(store: &AppStore, event: &str) -> AppResult<Vec<PythonPluginHookSpec>> {
    Ok(enabled_python_plugin_specs(store)?
        .into_iter()
        .filter(|plugin| plugin.provided_hooks.iter().any(|hook| hook == event))
        .filter_map(|plugin| {
            let path = PathBuf::from(&plugin.path);
            if path.join("__init__.py").is_file() {
                Some(PythonPluginHookSpec {
                    plugin_id: plugin.id,
                    plugin_name: plugin.name,
                    path,
                })
            } else {
                None
            }
        })
        .collect())
}

fn enabled_python_plugin_specs(store: &AppStore) -> AppResult<Vec<crate::models::PluginSummary>> {
    Ok(store
        .plugins()?
        .into_iter()
        .filter(|plugin| plugin.enabled)
        .filter(|plugin| !matches!(plugin.kind.as_str(), "exclusive" | "model-provider"))
        .filter(|plugin| {
            plugin
                .requires_env
                .iter()
                .all(|name| name.trim().is_empty() || env::var_os(name).is_some())
        })
        .collect())
}

async fn run_python_plugin_hooks(store: &AppStore, event: &str, kwargs: &Value) -> Vec<Value> {
    let Ok(specs) = python_plugin_hook_specs(store, event) else {
        return Vec::new();
    };
    let mut results = Vec::new();
    for spec in specs {
        match run_python_plugin_hook(&spec, event, kwargs).await {
            Ok(values) => results.extend(values),
            Err(error) => {
                eprintln!(
                    "SynthChat plugin hook '{}' failed for {}: {}",
                    event, spec.plugin_id, error
                );
            }
        }
    }
    results
}

pub(super) async fn run_python_plugin_command(
    store: &AppStore,
    command_name: &str,
    raw_args: &str,
) -> AppResult<Option<PythonPluginCommandResult>> {
    let command_name = command_name
        .trim()
        .trim_start_matches('/')
        .trim_start_matches('／');
    if command_name.is_empty() {
        return Ok(None);
    }
    for plugin in enabled_python_plugin_specs(store)? {
        let path = PathBuf::from(&plugin.path);
        if !path.join("__init__.py").is_file() {
            continue;
        }
        let spec = PythonPluginHookSpec {
            plugin_id: plugin.id,
            plugin_name: plugin.name,
            path,
        };
        let output = run_python_plugin_command_runner(&spec, command_name, raw_args).await?;
        if output
            .get("handled")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            let reply = output
                .get("result")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| {
                    output
                        .get("result")
                        .map(Value::to_string)
                        .unwrap_or_default()
                });
            let injected_messages = output
                .get("injected_messages")
                .and_then(Value::as_array)
                .map(|messages| {
                    messages
                        .iter()
                        .filter_map(|message| {
                            let content = message.get("content").and_then(Value::as_str)?.trim();
                            if content.is_empty() {
                                return None;
                            }
                            let role = message
                                .get("role")
                                .and_then(Value::as_str)
                                .unwrap_or("user")
                                .trim()
                                .to_lowercase();
                            Some(PythonPluginInjectedMessage {
                                role: if matches!(
                                    role.as_str(),
                                    "user" | "assistant" | "system" | "tool"
                                ) {
                                    role
                                } else {
                                    "user".into()
                                },
                                content: content.to_string(),
                            })
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            return Ok(Some(PythonPluginCommandResult {
                reply,
                injected_messages,
            }));
        }
    }
    Ok(None)
}

pub(super) fn list_python_plugin_tools(
    store: &AppStore,
) -> AppResult<Vec<PythonPluginToolDefinition>> {
    let mut definitions = Vec::new();
    for plugin in enabled_python_plugin_specs(store)? {
        let path = PathBuf::from(&plugin.path);
        if !path.join("__init__.py").is_file() {
            continue;
        }
        let spec = PythonPluginHookSpec {
            plugin_id: plugin.id,
            plugin_name: plugin.name,
            path,
        };
        match cached_python_plugin_tool_definitions(&spec) {
            Ok(tools) => definitions.extend(tools),
            Err(error) => {
                eprintln!(
                    "SynthChat plugin tool discovery failed for {}: {}",
                    spec.plugin_id, error
                );
            }
        }
    }
    Ok(definitions)
}

pub(super) fn list_python_plugin_skills(
    store: &AppStore,
) -> AppResult<Vec<PythonPluginSkillDefinition>> {
    let mut definitions = Vec::new();
    for plugin in enabled_python_plugin_specs(store)? {
        let path = PathBuf::from(&plugin.path);
        if !path.join("__init__.py").is_file() {
            continue;
        }
        let spec = PythonPluginHookSpec {
            plugin_id: plugin.id,
            plugin_name: plugin.name,
            path,
        };
        match cached_python_plugin_skill_definitions(&spec) {
            Ok(skills) => definitions.extend(skills),
            Err(error) => {
                eprintln!(
                    "SynthChat plugin skill discovery failed for {}: {}",
                    spec.plugin_id, error
                );
            }
        }
    }
    Ok(definitions)
}

fn cached_python_plugin_tool_definitions(
    spec: &PythonPluginHookSpec,
) -> AppResult<Vec<PythonPluginToolDefinition>> {
    let cache_key = python_plugin_tool_cache_key(spec);
    let cache = PYTHON_PLUGIN_TOOL_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    {
        let guard = cache
            .lock()
            .map_err(|_| AppError::BadRequest("python plugin tool cache lock poisoned".into()))?;
        if let Some(cached) = guard.get(&cache_key) {
            if cached.captured_at.elapsed() < PYTHON_PLUGIN_TOOL_CACHE_TTL {
                return Ok(cached.tools.clone());
            }
        }
    }
    let tools = run_python_plugin_tool_list_runner(spec)?;
    let mut guard = cache
        .lock()
        .map_err(|_| AppError::BadRequest("python plugin tool cache lock poisoned".into()))?;
    guard.insert(
        cache_key,
        CachedPythonPluginTools {
            captured_at: Instant::now(),
            tools: tools.clone(),
        },
    );
    Ok(tools)
}

fn cached_python_plugin_skill_definitions(
    spec: &PythonPluginHookSpec,
) -> AppResult<Vec<PythonPluginSkillDefinition>> {
    let cache_key = python_plugin_tool_cache_key(spec);
    let cache = PYTHON_PLUGIN_SKILL_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    {
        let guard = cache
            .lock()
            .map_err(|_| AppError::BadRequest("python plugin skill cache lock poisoned".into()))?;
        if let Some(cached) = guard.get(&cache_key) {
            if cached.captured_at.elapsed() < PYTHON_PLUGIN_TOOL_CACHE_TTL {
                return Ok(cached.skills.clone());
            }
        }
    }
    let skills = run_python_plugin_skill_list_runner(spec)?;
    let mut guard = cache
        .lock()
        .map_err(|_| AppError::BadRequest("python plugin skill cache lock poisoned".into()))?;
    guard.insert(
        cache_key,
        CachedPythonPluginSkills {
            captured_at: Instant::now(),
            skills: skills.clone(),
        },
    );
    Ok(skills)
}

fn python_plugin_tool_cache_key(spec: &PythonPluginHookSpec) -> String {
    let init_path = spec.path.join("__init__.py");
    let modified = init_path
        .metadata()
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    format!("{}|{}|{}", spec.plugin_id, spec.path.display(), modified)
}

pub(super) async fn run_python_plugin_tool(
    store: &AppStore,
    tool_name: &str,
    payload: &Value,
) -> AppResult<String> {
    let mut last_error = None;
    for plugin in enabled_python_plugin_specs(store)? {
        let path = PathBuf::from(&plugin.path);
        if !path.join("__init__.py").is_file() {
            continue;
        }
        let spec = PythonPluginHookSpec {
            plugin_id: plugin.id,
            plugin_name: plugin.name,
            path,
        };
        let output = run_python_plugin_tool_runner(&spec, tool_name, payload).await?;
        if output.get("ok").and_then(Value::as_bool).unwrap_or(false) {
            return Ok(output
                .get("result")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| {
                    output
                        .get("result")
                        .map(Value::to_string)
                        .unwrap_or_default()
                }));
        }
        let error = output
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("python plugin tool failed")
            .to_string();
        if error == "plugin did not register requested tool" {
            continue;
        }
        last_error = Some(error);
        break;
    }
    Err(AppError::BadRequest(last_error.unwrap_or_else(|| {
        format!("python plugin tool not found: {tool_name}")
    })))
}

async fn run_python_plugin_hook(
    spec: &PythonPluginHookSpec,
    event: &str,
    kwargs: &Value,
) -> AppResult<Vec<Value>> {
    let request = json!({
        "plugin_id": spec.plugin_id,
        "plugin_name": spec.plugin_name,
        "plugin_dir": spec.path,
        "event": event,
        "kwargs": kwargs,
    });
    let output = run_python_plugin_hook_runner(&request).await?;
    if let Some(error) = output.get("error").and_then(Value::as_str) {
        return Err(AppError::BadRequest(format!(
            "python plugin hook runner error: {error}"
        )));
    }
    Ok(output
        .get("results")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default())
}

async fn run_python_plugin_command_runner(
    spec: &PythonPluginHookSpec,
    command_name: &str,
    raw_args: &str,
) -> AppResult<Value> {
    let request = json!({
        "plugin_id": spec.plugin_id,
        "plugin_name": spec.plugin_name,
        "plugin_dir": spec.path,
        "command_name": command_name,
        "raw_args": raw_args,
    });
    let output = run_python_plugin_hook_runner(&request).await?;
    if let Some(error) = output.get("error").and_then(Value::as_str) {
        return Err(AppError::BadRequest(format!(
            "python plugin command runner error: {error}"
        )));
    }
    Ok(output)
}

fn run_python_plugin_tool_list_runner(
    spec: &PythonPluginHookSpec,
) -> AppResult<Vec<PythonPluginToolDefinition>> {
    let request = json!({
        "plugin_id": spec.plugin_id,
        "plugin_name": spec.plugin_name,
        "plugin_dir": spec.path,
        "list_tools": true,
    });
    let output = run_python_plugin_hook_runner_blocking(&request)?;
    if let Some(error) = output.get("error").and_then(Value::as_str) {
        return Err(AppError::BadRequest(format!(
            "python plugin tool discovery runner error: {error}"
        )));
    }
    Ok(output
        .get("tools")
        .and_then(Value::as_array)
        .map(|tools| {
            tools
                .iter()
                .filter_map(|tool| {
                    let name = tool.get("name").and_then(Value::as_str)?.trim();
                    if name.is_empty() {
                        return None;
                    }
                    Some(PythonPluginToolDefinition {
                        plugin_id: spec.plugin_id.clone(),
                        plugin_name: spec.plugin_name.clone(),
                        name: name.to_string(),
                        toolset: tool
                            .get("toolset")
                            .and_then(Value::as_str)
                            .unwrap_or("plugin")
                            .to_string(),
                        schema: tool.get("schema").cloned().unwrap_or_else(|| json!({})),
                        description: tool
                            .get("description")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default())
}

fn run_python_plugin_skill_list_runner(
    spec: &PythonPluginHookSpec,
) -> AppResult<Vec<PythonPluginSkillDefinition>> {
    let request = json!({
        "plugin_id": spec.plugin_id,
        "plugin_name": spec.plugin_name,
        "plugin_dir": spec.path,
        "list_skills": true,
    });
    let output = run_python_plugin_hook_runner_blocking(&request)?;
    if let Some(error) = output.get("error").and_then(Value::as_str) {
        return Err(AppError::BadRequest(format!(
            "python plugin skill discovery runner error: {error}"
        )));
    }
    Ok(output
        .get("skills")
        .and_then(Value::as_array)
        .map(|skills| {
            skills
                .iter()
                .filter_map(|skill| {
                    let name = skill.get("name").and_then(Value::as_str)?.trim();
                    let path = skill.get("path").and_then(Value::as_str)?.trim();
                    if name.is_empty() || path.is_empty() {
                        return None;
                    }
                    Some(PythonPluginSkillDefinition {
                        plugin_id: spec.plugin_id.clone(),
                        plugin_name: spec.plugin_name.clone(),
                        name: name.to_string(),
                        path: PathBuf::from(path),
                        description: skill
                            .get("description")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default())
}

async fn run_python_plugin_tool_runner(
    spec: &PythonPluginHookSpec,
    tool_name: &str,
    payload: &Value,
) -> AppResult<Value> {
    let request = json!({
        "plugin_id": spec.plugin_id,
        "plugin_name": spec.plugin_name,
        "plugin_dir": spec.path,
        "tool_name": tool_name,
        "tool_args": payload,
    });
    let output = run_python_plugin_hook_runner(&request).await?;
    if let Some(error) = output.get("error").and_then(Value::as_str) {
        return Err(AppError::BadRequest(format!(
            "python plugin tool runner error: {error}"
        )));
    }
    Ok(output)
}

fn run_python_plugin_hook_runner_blocking(request: &Value) -> AppResult<Value> {
    let python = env::var("HERMES_PYTHON")
        .or_else(|_| env::var("SYNTHCHAT_PYTHON"))
        .unwrap_or_else(|_| "python".into());
    let mut child = StdCommand::new(python)
        .arg("-c")
        .arg(PYTHON_PLUGIN_HOOK_RUNNER)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    if let Some(stdin) = child.stdin.as_mut() {
        stdin.write_all(serde_json::to_string(request)?.as_bytes())?;
    }
    drop(child.stdin.take());
    let deadline =
        std::time::Instant::now() + Duration::from_secs(PYTHON_PLUGIN_HOOK_TIMEOUT_SECONDS);
    loop {
        if child.try_wait()?.is_some() {
            let output = child.wait_with_output()?;
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
                let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
                return Err(AppError::BadRequest(format!(
                    "python plugin hook exited with {:?}: {}{}{}",
                    output.status.code(),
                    stdout,
                    if stdout.is_empty() || stderr.is_empty() {
                        ""
                    } else {
                        "\n"
                    },
                    stderr
                )));
            }
            let stdout = String::from_utf8_lossy(&output.stdout);
            return Ok(serde_json::from_str(stdout.trim())?);
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(AppError::BadRequest("python plugin hook timed out".into()));
        }
        thread::sleep(Duration::from_millis(20));
    }
}

async fn run_python_plugin_hook_runner(request: &Value) -> AppResult<Value> {
    let python = env::var("HERMES_PYTHON")
        .or_else(|_| env::var("SYNTHCHAT_PYTHON"))
        .unwrap_or_else(|_| "python".into());
    let mut child = Command::new(python)
        .arg("-c")
        .arg(PYTHON_PLUGIN_HOOK_RUNNER)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    if let Some(stdin) = child.stdin.as_mut() {
        stdin
            .write_all(serde_json::to_string(request)?.as_bytes())
            .await?;
    }
    drop(child.stdin.take());
    let output = tokio::time::timeout(
        Duration::from_secs(PYTHON_PLUGIN_HOOK_TIMEOUT_SECONDS),
        child.wait_with_output(),
    )
    .await
    .map_err(|_| AppError::BadRequest("python plugin hook timed out".into()))??;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        return Err(AppError::BadRequest(format!(
            "python plugin hook exited with {:?}: {}{}{}",
            output.status.code(),
            stdout,
            if stdout.is_empty() || stderr.is_empty() {
                ""
            } else {
                "\n"
            },
            stderr
        )));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(serde_json::from_str(stdout.trim())?)
}

fn shell_hooks_auto_accept_enabled(chat: &crate::models::ChatConfig) -> bool {
    chat.hooks_auto_accept || env_flag("SYNTHCHAT_ACCEPT_HOOKS") || env_flag("HERMES_ACCEPT_HOOKS")
}

fn shell_hook_allowlist_path(store: &AppStore) -> PathBuf {
    store.data_dir().join("shell-hooks-allowlist.json")
}

fn load_shell_hook_allowlist(store: &AppStore) -> AppResult<Value> {
    let path = shell_hook_allowlist_path(store);
    if !path.exists() {
        return Ok(json!({ "approvals": [] }));
    }
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

fn save_shell_hook_allowlist(store: &AppStore, allowlist: &Value) -> AppResult<()> {
    let path = shell_hook_allowlist_path(store);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_vec_pretty(allowlist)?)?;
    fs::rename(tmp, path)?;
    Ok(())
}

fn shell_hook_is_allowlisted(store: &AppStore, spec: &ShellHookSpec) -> AppResult<bool> {
    let allowlist = load_shell_hook_allowlist(store)?;
    Ok(shell_hook_allowlist_entry(&allowlist, spec).is_some())
}

fn shell_hook_allowlist_entry<'a>(allowlist: &'a Value, spec: &ShellHookSpec) -> Option<&'a Value> {
    allowlist
        .get("approvals")
        .and_then(Value::as_array)
        .and_then(|approvals| {
            approvals.iter().find(|approval| {
                approval.get("event").and_then(Value::as_str) == Some(spec.event.as_str())
                    && approval.get("command").and_then(Value::as_str)
                        == Some(spec.command.as_str())
            })
        })
}

fn record_shell_hook_approval(store: &AppStore, spec: &ShellHookSpec) -> AppResult<()> {
    let mut allowlist = load_shell_hook_allowlist(store)?;
    if allowlist
        .get("approvals")
        .and_then(Value::as_array)
        .map(|approvals| {
            approvals.iter().any(|approval| {
                approval.get("event").and_then(Value::as_str) == Some(spec.event.as_str())
                    && approval.get("command").and_then(Value::as_str)
                        == Some(spec.command.as_str())
            })
        })
        .unwrap_or(false)
    {
        return Ok(());
    }
    if !allowlist.is_object() {
        allowlist = json!({ "approvals": [] });
    }
    let approvals = allowlist
        .as_object_mut()
        .expect("allowlist is an object")
        .entry("approvals")
        .or_insert_with(|| json!([]));
    if !approvals.is_array() {
        *approvals = json!([]);
    }
    let approvals = approvals
        .as_array_mut()
        .expect("approvals was reset to an array");
    approvals.push(shell_hook_approval_entry(spec));
    save_shell_hook_allowlist(store, &allowlist)
}

fn shell_hook_approval_entry(spec: &ShellHookSpec) -> Value {
    let approved_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default();
    let script_mtime = shell_hook_script_path(&spec.command)
        .as_deref()
        .and_then(shell_hook_script_mtime_seconds);
    json!({
        "event": spec.event.as_str(),
        "command": spec.command.as_str(),
        "approvedAt": approved_at,
        "approved_at": approved_at,
        "scriptMtimeAtApproval": script_mtime,
        "script_mtime_at_approval": script_mtime,
    })
}

fn shell_hook_approval_script_mtime(approval: &Value) -> Option<u64> {
    approval
        .get("scriptMtimeAtApproval")
        .or_else(|| approval.get("script_mtime_at_approval"))
        .and_then(Value::as_u64)
}

fn shell_hook_script_mtime_seconds(path: &Path) -> Option<u64> {
    fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_secs())
}

fn shell_hook_script_is_runnable(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    if cfg!(windows) {
        return true;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        return fs::metadata(path)
            .map(|metadata| metadata.permissions().mode() & 0o111 != 0)
            .unwrap_or(false);
    }
    #[allow(unreachable_code)]
    true
}

fn shell_hook_script_path(command: &str) -> Option<PathBuf> {
    let argv = split_command_line(command)?;
    if argv.is_empty() {
        return None;
    }
    for (index, arg) in argv.iter().enumerate() {
        let lower = arg.to_ascii_lowercase();
        if matches!(lower.as_str(), "-file" | "--file" | "/file") {
            if let Some(next) = argv.get(index + 1) {
                return Some(expand_shell_hook_path(next));
            }
        }
    }
    let script_extensions = [
        ".ps1", ".bat", ".cmd", ".exe", ".sh", ".bash", ".zsh", ".fish", ".py", ".pyw", ".js",
        ".mjs", ".cjs", ".ts", ".rb", ".pl", ".lua",
    ];
    for arg in &argv {
        let lower = arg.to_ascii_lowercase();
        if script_extensions
            .iter()
            .any(|extension| lower.ends_with(extension))
        {
            return Some(expand_shell_hook_path(arg));
        }
    }
    argv.iter()
        .find(|arg| arg.contains('/') || arg.contains('\\') || arg.starts_with('~'))
        .map(|arg| expand_shell_hook_path(arg))
        .or_else(|| argv.first().map(|arg| expand_shell_hook_path(arg)))
}

fn expand_shell_hook_path(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\")) {
        if let Some(home) = env::var_os("USERPROFILE").or_else(|| env::var_os("HOME")) {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(path)
}

fn shell_hook_spec(event: &str, entry: &Value) -> Option<ShellHookSpec> {
    let command = entry.get("command").and_then(Value::as_str)?.trim();
    if command.is_empty() {
        return None;
    }
    let timeout_seconds = entry
        .get("timeout")
        .or_else(|| entry.get("timeoutSeconds"))
        .or_else(|| entry.get("timeout_seconds"))
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_TIMEOUT_SECONDS)
        .clamp(1, MAX_TIMEOUT_SECONDS);
    let matcher = entry
        .get("matcher")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    Some(ShellHookSpec {
        event: event.into(),
        command: command.to_string(),
        matcher,
        timeout_seconds,
    })
}

impl ShellHookSpec {
    fn matches_tool(&self, tool_name: &str) -> bool {
        let Some(matcher) = self.matcher.as_deref() else {
            return true;
        };
        wildcard_match(matcher, tool_name)
    }
}

async fn run_shell_hook(
    spec: &ShellHookSpec,
    run_id: &str,
    tool_name: &str,
    payload: &Value,
    result: Option<&Value>,
) -> AppResult<Option<Value>> {
    let argv = split_command_line(&spec.command).ok_or_else(|| {
        AppError::BadRequest(format!(
            "shell hook command cannot be parsed: {}",
            spec.command
        ))
    })?;
    let Some((program, args)) = argv.split_first() else {
        return Ok(None);
    };
    let stdin_json = serde_json::to_string(&shell_hook_stdin_json(
        spec, run_id, tool_name, payload, result,
    ))?;

    let mut child = Command::new(program);
    child
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let mut child = child.spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(stdin_json.as_bytes()).await?;
        stdin.shutdown().await?;
    }
    let output = match tokio::time::timeout(
        Duration::from_secs(spec.timeout_seconds),
        child.wait_with_output(),
    )
    .await
    {
        Ok(output) => output?,
        Err(_) => return Ok(None),
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stdout = stdout.trim();
    if stdout.is_empty() {
        return Ok(None);
    }
    match serde_json::from_str::<Value>(stdout) {
        Ok(Value::Object(_)) => serde_json::from_str::<Value>(stdout)
            .map(Some)
            .map_err(Into::into),
        Ok(_) => Ok(None),
        Err(error) => Err(AppError::BadRequest(format!(
            "shell hook stdout was not valid JSON: {} ({})",
            truncate_for_prompt(&redact_sensitive_text(stdout), 200),
            error
        ))),
    }
}

fn run_shell_hook_diagnostic(
    spec: &ShellHookSpec,
    run_id: &str,
    tool_name: &str,
    payload: &Value,
    result: Option<&Value>,
) -> ShellHookDiagnosticRun {
    let Some(argv) = split_command_line(&spec.command) else {
        return ShellHookDiagnosticRun::error(format!(
            "shell hook command cannot be parsed: {}",
            spec.command
        ));
    };
    let Some((program, args)) = argv.split_first() else {
        return ShellHookDiagnosticRun::error("shell hook command is empty".into());
    };
    let stdin_json = match serde_json::to_string(&shell_hook_stdin_json(
        spec, run_id, tool_name, payload, result,
    )) {
        Ok(value) => value,
        Err(error) => return ShellHookDiagnosticRun::error(error.to_string()),
    };

    let mut child = match std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => return ShellHookDiagnosticRun::error(error.to_string()),
    };
    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        if let Err(error) = stdin.write_all(stdin_json.as_bytes()) {
            let _ = child.kill();
            return ShellHookDiagnosticRun::error(error.to_string());
        }
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(spec.timeout_seconds);
    loop {
        match child.try_wait() {
            Ok(Some(_status)) => match child.wait_with_output() {
                Ok(output) => return ShellHookDiagnosticRun::from_output(output, false),
                Err(error) => return ShellHookDiagnosticRun::error(error.to_string()),
            },
            Ok(None) if std::time::Instant::now() >= deadline => {
                let _ = child.kill();
                return match child.wait_with_output() {
                    Ok(output) => ShellHookDiagnosticRun::from_output(output, true),
                    Err(error) => ShellHookDiagnosticRun::error(error.to_string()),
                };
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            Err(error) => {
                let _ = child.kill();
                return ShellHookDiagnosticRun::error(error.to_string());
            }
        }
    }
}

fn shell_hook_stdin_json(
    spec: &ShellHookSpec,
    run_id: &str,
    tool_name: &str,
    payload: &Value,
    result: Option<&Value>,
) -> Value {
    json!({
        "hook_event_name": spec.event,
        "tool_name": tool_name,
        "tool_input": payload,
        "session_id": run_id,
        "cwd": env::current_dir().unwrap_or_else(|_| PathBuf::from(".")).to_string_lossy(),
        "extra": {
            "run_id": run_id,
            "result": result,
        }
    })
}

fn default_shell_hook_tool_name(event: &str) -> &str {
    match event {
        "pre_tool_call"
        | "post_tool_call"
        | "transform_tool_result"
        | "transform_terminal_output" => "terminal",
        "pre_approval_request" | "post_approval_response" => "terminal",
        "subagent_stop" => "subagent",
        "pre_gateway_dispatch" => "gateway",
        _ => "llm",
    }
}

fn shell_hook_test_payload(event: &str, tool_name: &str) -> Value {
    match event {
        "pre_tool_call" | "post_tool_call" => json!({
            "tool_name": tool_name,
            "command": "echo hello",
        }),
        "transform_terminal_output" => json!({
            "command": "echo hello",
            "output": "hello",
            "returncode": 0,
        }),
        "transform_tool_result" => json!({
            "tool_name": tool_name,
            "args": {"command": "echo hello"},
            "tool_input": {"command": "echo hello"},
            "result": "hello",
            "text": "hello",
            "output": "hello",
            "ok": true,
            "error": null,
        }),
        "pre_llm_call" => json!({
            "user_content": "What is the weather?",
            "messages": [{"role": "user", "content": "What is the weather?"}],
        }),
        "transform_llm_output" | "post_llm_call" => json!({
            "user_message": "What is the weather?",
            "response_text": "It is sunny.",
            "assistant_response": "It is sunny.",
            "model": "test-model",
            "provider": "test-provider",
        }),
        "pre_approval_request" | "post_approval_response" => json!({
            "tool_name": tool_name,
            "command": "rm -rf temp",
            "reason": "synthetic approval test",
        }),
        "subagent_stop" => json!({
            "parent_session_id": "parent-run",
            "parent_run_id": "parent-run",
            "child_session_id": "child-run",
            "child_run_id": "child-run",
            "child_conversation_id": "child-conversation",
            "child_role": "subagent",
            "child_task": "inspect delegated work",
            "child_summary": "Synthetic summary for hooks test",
            "child_status": "completed",
            "status": "completed",
            "transport": "synthchat",
            "toolsets": ["file"],
            "max_iterations": 12,
            "maxIterations": 12,
        }),
        "pre_gateway_dispatch" => json!({
            "event": {
                "platform": "telegram",
                "eventId": "event-test",
                "source": {
                    "platform": "telegram",
                    "chatId": "chat-test",
                    "userId": "user-test",
                    "chatType": "dm"
                },
                "text": "hello"
            },
            "inbound": {
                "platform": "telegram",
                "eventId": "event-test",
                "text": "hello"
            },
            "source": {
                "platform": "telegram",
                "chatId": "chat-test",
                "userId": "user-test",
                "chatType": "dm"
            },
            "text": "hello",
            "platform": "telegram",
            "event_id": "event-test",
            "eventId": "event-test",
        }),
        _ => json!({
            "event": event,
            "tool_name": tool_name,
        }),
    }
}

impl ShellHookDiagnosticRun {
    fn error(error: String) -> Self {
        Self {
            returncode: None,
            timed_out: false,
            stdout: String::new(),
            stderr: String::new(),
            parsed: None,
            error: Some(error),
        }
    }

    fn from_output(output: std::process::Output, timed_out: bool) -> Self {
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let parsed = serde_json::from_str::<Value>(&stdout)
            .ok()
            .filter(Value::is_object);
        Self {
            returncode: exit_status_code(output.status),
            timed_out,
            stdout,
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            parsed,
            error: None,
        }
    }
}

fn exit_status_code(status: ExitStatus) -> Option<i32> {
    status.code()
}

fn shell_hook_block_message(response: &Value) -> Option<String> {
    let action = response.get("action").and_then(Value::as_str);
    let decision = response.get("decision").and_then(Value::as_str);
    if action != Some("block") && decision != Some("block") {
        return None;
    }
    response
        .get("message")
        .or_else(|| response.get("reason"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| Some("Blocked by shell hook.".into()))
}

fn split_command_line(command: &str) -> Option<Vec<String>> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut chars = command.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            if matches!(chars.peek(), Some(next) if next.is_whitespace() || matches!(next, '"' | '\'' | '\\'))
            {
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            } else {
                current.push(ch);
            }
            continue;
        }
        if let Some(active) = quote {
            if ch == active {
                quote = None;
            } else {
                current.push(ch);
            }
            continue;
        }
        if matches!(ch, '"' | '\'') {
            quote = Some(ch);
        } else if ch.is_whitespace() {
            if !current.is_empty() {
                args.push(std::mem::take(&mut current));
            }
        } else {
            current.push(ch);
        }
    }
    if quote.is_some() {
        return None;
    }
    if !current.is_empty() {
        args.push(current);
    }
    Some(args)
}

fn wildcard_match(pattern: &str, value: &str) -> bool {
    if pattern == "*" || pattern == value {
        return true;
    }
    if !pattern.contains('*') {
        return false;
    }
    let mut remainder = value;
    let anchored_start = !pattern.starts_with('*');
    let anchored_end = !pattern.ends_with('*');
    let parts = pattern
        .split('*')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    if let Some(first) = parts.first() {
        if anchored_start {
            let Some(after) = remainder.strip_prefix(first) else {
                return false;
            };
            remainder = after;
        } else if let Some(index) = remainder.find(first) {
            remainder = &remainder[index + first.len()..];
        } else {
            return false;
        }
    }
    for part in parts.iter().skip(1) {
        if let Some(index) = remainder.find(part) {
            remainder = &remainder[index + part.len()..];
        } else {
            return false;
        }
    }
    !anchored_end || parts.last().is_none_or(|last| value.ends_with(last))
}
