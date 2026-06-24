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
use tauri::AppHandle;
use tokio::{io::AsyncWriteExt, process::Command};

use crate::{
    error::{AppError, AppResult},
    models::{
        new_id, now_iso, AgentDefinition, AgentRunRecord, ChatMessage, PluginAuxiliaryTaskSummary,
    },
    process_utils::CommandWindowExt,
    store::AppStore,
};

use super::{
    append_parent_phase_event, append_tool_approval_request, available_mcp_tool_definitions,
    decision_parser::PROVIDER_TOOL_CALL_META_KEY, delegation_request::DelegateTaskRequest,
    disk_cleanup_post_tool_call_hook, disk_cleanup_session_end_hook, emit_agent_run_record,
    execute_recovery_internal_tool, execute_recovery_mcp_tool, is_internal_tool, kanban_block_tool,
    kanban_comment_tool, kanban_complete_tool, kanban_create_tool, kanban_decompose_tool,
    kanban_heartbeat_tool, kanban_link_tool, kanban_list_tool, kanban_show_tool,
    kanban_specify_tool, kanban_unblock_tool, langfuse_record_hook, record_tool_event_for_run,
    record_tool_started_for_run, redact_sensitive_text, resolve_mcp_tool, tool_approval_reason,
    truncate_for_prompt, ToolExecutionContext,
};

const DEFAULT_TIMEOUT_SECONDS: u64 = 60;
const MAX_TIMEOUT_SECONDS: u64 = 300;
const PYTHON_PLUGIN_HOOK_TIMEOUT_SECONDS: u64 = 60;
const PYTHON_PLUGIN_TOOL_CACHE_TTL: Duration = Duration::from_secs(30);
const PYTHON_PLUGIN_BRIDGE_TOOLS: &[&str] = &[
    "kanban_create",
    "kanban_decompose",
    "kanban_specify",
    "kanban_list",
    "kanban_show",
    "kanban_complete",
    "kanban_block",
    "kanban_unblock",
    "kanban_heartbeat",
    "kanban_comment",
    "kanban_link",
];
const PYTHON_PLUGIN_HOOK_RUNNER: &str = r#"
import asyncio
import importlib.metadata
import importlib.util
import inspect
import json
import os
import sys
import traceback

class PluginContext:
    def __init__(self, plugin_dir="", bridge_tools=None):
        self.plugin_dir = plugin_dir
        self.hooks = {}
        self.commands = {}
        self.tools = {}
        self.global_tools = None
        self.bridge_tools = set(str(name) for name in (bridge_tools or []))
        self.allow_external_dispatch = False
        self.skills = {}
        self.auxiliary_tasks = {}
        self.injected_messages = []
        self.context_engine = None

    def register_hook(self, hook_name, callback):
        self.hooks.setdefault(str(hook_name), []).append(callback)

    def register_context_engine(self, engine):
        self.context_engine = engine
        return None

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

    def register_auxiliary_task(self, key, *, display_name, description, defaults=None):
        clean = str(key or "").strip()
        if not clean:
            raise ValueError("plugin auxiliary task key must be non-empty")
        if not all(ch.isalnum() or ch == "_" for ch in clean):
            raise ValueError("plugin auxiliary task key must contain only alphanumeric characters and underscores")
        builtin = {
            "vision",
            "compression",
            "web_extract",
            "approval",
            "goal_judge",
            "mcp",
            "title_generation",
            "skills_hub",
            "triage_specifier",
            "kanban_decomposer",
            "profile_describer",
            "curator",
        }
        if clean in builtin:
            raise ValueError("plugin auxiliary task key is reserved for a built-in task: " + clean)
        merged_defaults = {
            "provider": "auto",
            "model": "",
            "base_url": "",
            "api_key": "",
            "timeout": 60,
            "extra_body": {},
        }
        if isinstance(defaults, dict):
            merged_defaults.update(defaults)
        self.auxiliary_tasks[clean] = {
            "key": clean,
            "display_name": str(display_name or clean),
            "description": str(description or ""),
            "defaults": merged_defaults,
        }
        return None

    def dispatch_tool(self, tool_name, args=None, **kwargs):
        clean_tool = str(tool_name or "").strip()
        registry = self.global_tools if isinstance(self.global_tools, dict) else self.tools
        tool = registry.get(clean_tool)
        if not tool:
            if clean_tool in self.bridge_tools or self.allow_external_dispatch:
                payload = args or {}
                return json.dumps({
                    "__synthchat_dispatch_tool__": {
                        "tool_name": clean_tool,
                        "args": _jsonable(payload),
                        "kwargs": _jsonable(kwargs),
                    }
                })
            raise ValueError("plugin tool not found: " + clean_tool)
        for name in tool.get("requires_env") or []:
            if isinstance(name, dict):
                name = name.get("name") or name.get("key") or name.get("var") or name.get("env")
            if name and not os.environ.get(str(name)):
                raise RuntimeError("missing required env: " + str(name))
        check_fn = tool.get("check_fn")
        if callable(check_fn):
            check = check_fn()
            if inspect.isawaitable(check):
                raise RuntimeError("async check_fn is not supported by dispatch_tool")
            if check is False:
                raise RuntimeError("plugin tool requirement check failed")
        payload = args or {}
        result = tool["handler"](payload, **kwargs)
        if inspect.isawaitable(result):
            raise RuntimeError("async tool handler is not supported by dispatch_tool")
        return json.dumps(_jsonable(result))

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
    plugin_specs = request.get("plugins") or []
    plugin_dir = request.get("plugin_dir") or ""
    plugin_id = request.get("plugin_id") or "plugin"
    plugin_source = request.get("plugin_source") or ""
    entry_point = request.get("entry_point") or ""
    event = request.get("event") or ""
    command_name = request.get("command_name") or ""
    raw_args = request.get("raw_args") or ""
    tool_name = request.get("tool_name") or ""
    tool_args = request.get("tool_args") or {}
    context_engine_action = request.get("context_engine_action") or ""
    context_engine_messages = request.get("context_engine_messages") or []
    context_engine_current_tokens = request.get("context_engine_current_tokens")
    context_engine_focus_topic = request.get("context_engine_focus_topic")
    context_engine_usage = request.get("context_engine_usage") or {}
    context_engine_model = request.get("context_engine_model") or {}
    context_engine_lifecycle_event = request.get("context_engine_lifecycle_event") or ""
    context_engine_session_id = request.get("context_engine_session_id") or ""
    context_engine_lifecycle_extra = request.get("context_engine_lifecycle_extra") or {}
    list_tools = bool(request.get("list_tools"))
    list_skills = bool(request.get("list_skills"))
    list_commands = bool(request.get("list_commands"))
    list_auxiliary_tasks = bool(request.get("list_auxiliary_tasks"))
    kwargs = request.get("kwargs") or {}

    def _safe_env_component(value):
        clean = "".join(ch if ch.isalnum() or ch in ("-", "_", ".") else "_" for ch in str(value or "").strip())
        return clean or "context_engine"

    def _prepare_context_engine_env(spec_info):
        spec_plugin_id = spec_info.get("plugin_id") or plugin_id
        spec_plugin_name = spec_info.get("plugin_name") or spec_plugin_id
        spec_plugin_source = spec_info.get("plugin_source") or plugin_source
        spec_plugin_dir = spec_info.get("plugin_dir") or plugin_dir
        is_context_engine = (
            spec_plugin_source == "context_engine"
            or str(spec_plugin_id).startswith("context-engine/")
        )
        if not is_context_engine:
            return
        engine_name = _safe_env_component(spec_plugin_name)
        explicit_root = (
            os.environ.get("SYNTHCHAT_CONTEXT_ENGINE_STATE_ROOT")
            or os.environ.get("HERMES_CONTEXT_ENGINE_STATE_ROOT")
        )
        hermes_home = os.environ.get("HERMES_HOME")
        if explicit_root:
            state_root = explicit_root
        elif hermes_home:
            state_root = os.path.join(hermes_home, "context-engine-state")
        else:
            state_root = os.path.join(spec_plugin_dir or ".", ".synthchat-state")
            hermes_home = os.path.join(state_root, ".hermes")
            os.environ.setdefault("HERMES_HOME", hermes_home)
        state_dir = os.path.join(state_root, engine_name)
        os.makedirs(state_dir, exist_ok=True)
        os.environ["SYNTHCHAT_CONTEXT_ENGINE_NAME"] = engine_name
        os.environ["HERMES_CONTEXT_ENGINE_NAME"] = engine_name
        os.environ["SYNTHCHAT_CONTEXT_ENGINE_STATE_DIR"] = state_dir
        os.environ["HERMES_CONTEXT_ENGINE_STATE_DIR"] = state_dir
        os.environ.setdefault("SYNTHCHAT_HERMES_HOME", os.environ.get("HERMES_HOME", ""))

    def _load_module(spec_info):
        _prepare_context_engine_env(spec_info)
        spec_plugin_dir = spec_info.get("plugin_dir") or ""
        spec_plugin_id = spec_info.get("plugin_id") or "plugin"
        spec_plugin_source = spec_info.get("plugin_source") or ""
        spec_entry_point = spec_info.get("entry_point") or ""
        if spec_plugin_source == "entrypoint":
            module = None
            eps = importlib.metadata.entry_points()
            if hasattr(eps, "select"):
                group_eps = eps.select(group="hermes_agent.plugins")
            elif isinstance(eps, dict):
                group_eps = eps.get("hermes_agent.plugins", [])
            else:
                group_eps = [ep for ep in eps if getattr(ep, "group", "") == "hermes_agent.plugins"]
            for ep in group_eps:
                if ep.name == spec_plugin_id or ep.value == spec_entry_point:
                    module = ep.load()
                    break
            if module is None:
                raise RuntimeError("entry point plugin not found: " + spec_plugin_id)
            return module
        init_file = os.path.join(spec_plugin_dir, "__init__.py")
        if not os.path.isfile(init_file):
            return None
        parent_dir = os.path.dirname(spec_plugin_dir)
        if parent_dir and parent_dir not in sys.path:
            sys.path.insert(0, parent_dir)
        module_name = "synthchat_hermes_plugin_" + "".join(
            ch if ch.isalnum() else "_" for ch in spec_plugin_id
        )
        spec = importlib.util.spec_from_file_location(
            module_name,
            init_file,
            submodule_search_locations=[spec_plugin_dir],
        )
        if spec is None or spec.loader is None:
            raise RuntimeError("cannot load plugin module")
        module = importlib.util.module_from_spec(spec)
        sys.modules[module_name] = module
        spec.loader.exec_module(module)
        return module

    bridge_tools = request.get("bridge_tools") or []
    allow_external_dispatch = bool(request.get("allow_external_dispatch"))

    def _load_context(spec_info):
        module = _load_module(spec_info)
        if module is None:
            return None
        ctx = PluginContext(spec_info.get("plugin_dir") or "", bridge_tools)
        ctx.allow_external_dispatch = allow_external_dispatch
        register = getattr(module, "register", None)
        if callable(register):
            register(ctx)
        if ctx.context_engine is None:
            for value in vars(module).values():
                if not inspect.isclass(value):
                    continue
                if value.__module__ != getattr(module, "__name__", ""):
                    continue
                if hasattr(value, "get_tool_schemas") and hasattr(value, "handle_tool_call"):
                    try:
                        ctx.context_engine = value()
                        break
                    except Exception:
                        pass
        return ctx

    if plugin_specs and command_name:
        contexts = []
        global_tools = {}
        for spec_info in plugin_specs:
            ctx = _load_context(spec_info)
            if ctx is None:
                continue
            contexts.append(ctx)
            for name, tool in ctx.tools.items():
                global_tools.setdefault(name, tool)
        for ctx in contexts:
            ctx.global_tools = global_tools
        clean = str(command_name).lower().strip().lstrip("/").replace(" ", "-")
        for ctx in contexts:
            command = ctx.commands.get(clean)
            if not command:
                continue
            result = command["handler"](raw_args)
            if inspect.isawaitable(result):
                result = await result
            injected = []
            for item in contexts:
                injected.extend(item.injected_messages)
            print(json.dumps({
                "handled": True,
                "result": _jsonable(result),
                "injected_messages": injected,
            }))
            return
        print(json.dumps({"handled": False}))
        return

    _prepare_context_engine_env({
        "plugin_id": plugin_id,
        "plugin_name": request.get("plugin_name") or plugin_id,
        "plugin_source": plugin_source,
        "plugin_dir": plugin_dir,
    })

    if plugin_source == "entrypoint":
        module = None
        eps = importlib.metadata.entry_points()
        if hasattr(eps, "select"):
            group_eps = eps.select(group="hermes_agent.plugins")
        elif isinstance(eps, dict):
            group_eps = eps.get("hermes_agent.plugins", [])
        else:
            group_eps = [ep for ep in eps if getattr(ep, "group", "") == "hermes_agent.plugins"]
        for ep in group_eps:
            if ep.name == plugin_id or ep.value == entry_point:
                module = ep.load()
                break
        if module is None:
            raise RuntimeError("entry point plugin not found: " + plugin_id)
    else:
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
    ctx = PluginContext(plugin_dir, bridge_tools)
    ctx.allow_external_dispatch = allow_external_dispatch
    register = getattr(module, "register", None)
    if callable(register):
        register(ctx)
    if ctx.context_engine is None:
        for value in vars(module).values():
            if not inspect.isclass(value):
                continue
            if value.__module__ != getattr(module, "__name__", ""):
                continue
            if hasattr(value, "compress") or (hasattr(value, "get_tool_schemas") and hasattr(value, "handle_tool_call")):
                try:
                    ctx.context_engine = value()
                    break
                except Exception:
                    pass
    if context_engine_action:
        engine = getattr(ctx, "context_engine", None)
        if engine is None:
            print(json.dumps({"ok": False, "error": "context engine was not registered"}))
            return
        if context_engine_action == "compress":
            if not hasattr(engine, "compress"):
                print(json.dumps({"ok": False, "error": "context engine does not implement compress"}))
                return
            result = engine.compress(
                context_engine_messages,
                current_tokens=context_engine_current_tokens,
                focus_topic=context_engine_focus_topic,
            )
            if inspect.isawaitable(result):
                result = await result
            print(json.dumps({"ok": True, "messages": _jsonable(result)}))
            return
        if context_engine_action == "status":
            status = engine.get_status() if hasattr(engine, "get_status") else {}
            if inspect.isawaitable(status):
                status = await status
            print(json.dumps({"ok": True, "status": _jsonable(status)}))
            return
        if context_engine_action == "update_from_response":
            method = getattr(engine, "update_from_response", None)
            if method is None:
                print(json.dumps({"ok": True, "implemented": False}))
                return
            result = method(context_engine_usage)
            if inspect.isawaitable(result):
                result = await result
            print(json.dumps({"ok": True, "implemented": True, "result": _jsonable(result)}))
            return
        if context_engine_action == "update_model":
            method = getattr(engine, "update_model", None)
            if method is None:
                print(json.dumps({"ok": True, "implemented": False}))
                return
            model_payload = context_engine_model or {}
            result = method(
                str(model_payload.get("model") or ""),
                int(model_payload.get("context_length") or 0),
                base_url=str(model_payload.get("base_url") or ""),
                api_key=str(model_payload.get("api_key") or ""),
                provider=str(model_payload.get("provider") or ""),
                api_mode=str(model_payload.get("api_mode") or ""),
            )
            if inspect.isawaitable(result):
                result = await result
            print(json.dumps({"ok": True, "implemented": True, "result": _jsonable(result)}))
            return
        if context_engine_action == "lifecycle":
            event_name = str(context_engine_lifecycle_event or "").strip()
            method = getattr(engine, event_name, None)
            if method is None:
                print(json.dumps({"ok": True, "implemented": False}))
                return
            if event_name == "on_session_start":
                result = method(context_engine_session_id, **context_engine_lifecycle_extra)
            elif event_name == "on_session_end":
                result = method(context_engine_session_id, context_engine_messages)
            elif event_name == "on_session_reset":
                result = method()
            else:
                print(json.dumps({"ok": False, "error": "unsupported context engine lifecycle event: " + event_name}))
                return
            if inspect.isawaitable(result):
                result = await result
            print(json.dumps({"ok": True, "implemented": True, "result": _jsonable(result)}))
            return
        if context_engine_action == "should_compress_preflight":
            method = getattr(engine, "should_compress_preflight", None)
            if method is None:
                print(json.dumps({"ok": True, "implemented": False}))
                return
            try:
                result = method(context_engine_messages)
            except TypeError:
                result = method(messages=context_engine_messages, current_tokens=context_engine_current_tokens)
            if inspect.isawaitable(result):
                result = await result
            print(json.dumps({"ok": True, "implemented": True, "decision": bool(result)}))
            return
        if context_engine_action == "should_compress":
            method = getattr(engine, "should_compress", None)
            if method is None:
                print(json.dumps({"ok": True, "implemented": False}))
                return
            try:
                result = method(context_engine_current_tokens)
            except TypeError:
                result = method(prompt_tokens=context_engine_current_tokens)
            if inspect.isawaitable(result):
                result = await result
            print(json.dumps({"ok": True, "implemented": True, "decision": bool(result)}))
            return
        print(json.dumps({"ok": False, "error": "unsupported context engine action: " + str(context_engine_action)}))
        return
    if list_skills:
        print(json.dumps({"skills": list(ctx.skills.values())}))
        return
    if list_commands:
        commands = []
        for name, command in ctx.commands.items():
            commands.append({
                "name": name,
                "description": _jsonable(command.get("description") or "Plugin command"),
                "args_hint": _jsonable(command.get("args_hint") or ""),
            })
        print(json.dumps({"commands": commands}))
        return
    if list_auxiliary_tasks:
        print(json.dumps({"auxiliary_tasks": list(ctx.auxiliary_tasks.values())}))
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
        engine = getattr(ctx, "context_engine", None)
        if engine is not None and hasattr(engine, "get_tool_schemas"):
            schemas = engine.get_tool_schemas()
            if inspect.isawaitable(schemas):
                schemas = await schemas
            for schema in schemas or []:
                if not isinstance(schema, dict):
                    continue
                name = str(schema.get("name") or schema.get("function", {}).get("name") or "").strip()
                if not name:
                    continue
                function_schema = schema.get("function") if isinstance(schema.get("function"), dict) else schema
                tools.append({
                    "name": name,
                    "toolset": "context_engine",
                    "schema": _jsonable(function_schema),
                    "description": _jsonable(function_schema.get("description") or ""),
                    "emoji": "",
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
        if tool:
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
        engine = getattr(ctx, "context_engine", None)
        if engine is not None and hasattr(engine, "get_tool_schemas") and hasattr(engine, "handle_tool_call"):
            schemas = engine.get_tool_schemas()
            if inspect.isawaitable(schemas):
                schemas = await schemas
            engine_tool_names = {
                str(schema.get("name") or schema.get("function", {}).get("name") or "").strip()
                for schema in (schemas or [])
                if isinstance(schema, dict)
            }
            if clean_tool in engine_tool_names:
                result = engine.handle_tool_call(clean_tool, tool_args)
                if inspect.isawaitable(result):
                    result = await result
                print(json.dumps({"ok": True, "result": _jsonable(result)}))
                return
        print(json.dumps({"ok": False, "error": "plugin did not register requested tool"}))
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
    source: String,
    entry_point: String,
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
pub(super) struct PythonPluginCommandDefinition {
    pub(super) plugin_id: String,
    pub(super) plugin_name: String,
    pub(super) name: String,
    pub(super) description: String,
    pub(super) args_hint: String,
}

#[derive(Debug, Clone)]
pub(super) struct ContextEngineCompressedMessage {
    pub(super) role: String,
    pub(super) content: String,
}

#[derive(Clone, Copy)]
pub(super) struct PythonPluginBridgeContext<'a> {
    pub(super) agent: &'a AgentDefinition,
    pub(super) conversation_id: &'a str,
    pub(super) run_id: &'a str,
    pub(super) tool_context: ToolExecutionContext,
    pub(super) app: Option<&'a AppHandle>,
    pub(super) allow_mutating_tools: bool,
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

#[derive(Debug, Clone)]
struct CachedPythonPluginAuxiliaryTasks {
    captured_at: Instant,
    tasks: Vec<PluginAuxiliaryTaskSummary>,
}

static PYTHON_PLUGIN_AUXILIARY_TASK_CACHE: OnceLock<
    Mutex<HashMap<String, CachedPythonPluginAuxiliaryTasks>>,
> = OnceLock::new();

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
    let plugin_payload = json!({
        "tool_name": tool_name,
        "args": payload,
        "tool_input": payload,
        "task_id": run_id,
        "session_id": run_id,
    });
    let _ = langfuse_record_hook(store, "pre_tool_call", run_id, &plugin_payload, None);
    if security_guidance_block_mode_enabled() {
        let findings = security_guidance_scan_tool_args(tool_name, payload);
        if !findings.is_empty() {
            return Err(AppError::BadRequest(format!(
                "security-guidance refused this write: {}\n\nTo override, unset SECURITY_GUIDANCE_BLOCK and retry.",
                security_guidance_warning_block(&findings)
            )));
        }
    }
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
    let _ = disk_cleanup_post_tool_call_hook(store, run_id, tool_name, payload, result);
    let _ = langfuse_record_hook(store, "post_tool_call", run_id, payload, Some(result));
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
    let result_text = if ok && error.is_none() {
        security_guidance_transform_tool_result(tool_name, payload, result_text)
    } else {
        result_text.to_string()
    };
    let Ok(specs) = shell_hook_specs(store, "transform_tool_result") else {
        return result_text;
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
    result_text
}

#[derive(Debug, Clone)]
struct SecurityGuidanceFinding {
    rule_name: &'static str,
    reminder: &'static str,
}

const SECURITY_GUIDANCE_MAX_SCAN_BYTES: usize = 256 * 1024;

fn security_guidance_disabled() -> bool {
    env_bool("SECURITY_GUIDANCE_DISABLE")
}

fn security_guidance_block_mode_enabled() -> bool {
    !security_guidance_disabled() && env_bool("SECURITY_GUIDANCE_BLOCK")
}

fn env_bool(name: &str) -> bool {
    env::var(name)
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

fn security_guidance_transform_tool_result(
    tool_name: &str,
    payload: &Value,
    result_text: &str,
) -> String {
    if security_guidance_disabled() || security_guidance_block_mode_enabled() {
        return result_text.to_string();
    }
    let findings = security_guidance_scan_tool_args(tool_name, payload);
    if findings.is_empty() || security_guidance_result_is_simple_error(result_text) {
        return result_text.to_string();
    }
    format!(
        "{result_text}\n\n{}",
        security_guidance_warning_block(&findings)
    )
}

fn security_guidance_result_is_simple_error(result_text: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(result_text) else {
        return false;
    };
    value
        .as_object()
        .map(|object| object.contains_key("error") && object.len() <= 2)
        .unwrap_or(false)
}

fn security_guidance_scan_tool_args(
    tool_name: &str,
    payload: &Value,
) -> Vec<SecurityGuidanceFinding> {
    if security_guidance_disabled() {
        return Vec::new();
    }
    let mut findings = Vec::new();
    for (path, content) in security_guidance_extract_written_content(tool_name, payload) {
        findings.extend(security_guidance_scan_content(&path, &content));
    }
    findings.sort_by_key(|finding| finding.rule_name);
    findings.dedup_by_key(|finding| finding.rule_name);
    findings
}

fn security_guidance_extract_written_content(
    tool_name: &str,
    payload: &Value,
) -> Vec<(String, String)> {
    let mut out = Vec::new();
    match tool_name {
        "write_file" => {
            if let Some(content) = string_arg_from_value(payload, &["content", "text"]) {
                out.push((
                    string_arg_from_value(payload, &["path", "filePath"]).unwrap_or_default(),
                    content,
                ));
            }
        }
        "patch" => {
            let path = string_arg_from_value(payload, &["path", "filePath"]).unwrap_or_default();
            for key in [
                "patch",
                "new_string",
                "newString",
                "replacement",
                "content",
                "diff",
            ] {
                if let Some(content) = string_arg_from_value(payload, &[key]) {
                    out.push((path.clone(), content));
                }
            }
        }
        "skill_manage" => {
            let action = string_arg_from_value(payload, &["action"])
                .unwrap_or_default()
                .to_ascii_lowercase();
            if matches!(
                action.as_str(),
                "write_file" | "patch" | "edit" | "create" | "install_content"
            ) {
                let path = string_arg_from_value(payload, &["filePath", "file_path", "path"])
                    .unwrap_or_default();
                for key in [
                    "fileContent",
                    "file_content",
                    "content",
                    "newString",
                    "new_string",
                    "patch",
                ] {
                    if let Some(content) = string_arg_from_value(payload, &[key]) {
                        out.push((path.clone(), content));
                    }
                }
            }
        }
        _ => {}
    }
    out
}

fn string_arg_from_value(payload: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| payload.get(*key).and_then(Value::as_str))
        .map(str::to_string)
}

fn security_guidance_scan_content(path: &str, content: &str) -> Vec<SecurityGuidanceFinding> {
    if content.is_empty()
        || content
            .as_bytes()
            .len()
            .saturating_sub(SECURITY_GUIDANCE_MAX_SCAN_BYTES)
            > 0
    {
        return Vec::new();
    }
    let lower = content.to_ascii_lowercase();
    let path_lower = path.to_ascii_lowercase();
    let mut findings = Vec::new();
    push_security_guidance_finding(
        &mut findings,
        "python_eval_injection",
        "Avoid eval/exec on untrusted input. Prefer parsing, whitelisted dispatch, or ast.literal_eval for Python literals.",
        lower.contains("eval(") || lower.contains("exec("),
    );
    push_security_guidance_finding(
        &mut findings,
        "python_pickle_load",
        "pickle.load/pickle.loads can execute code during deserialization. Use a safer format for untrusted data.",
        lower.contains("pickle.load(") || lower.contains("pickle.loads("),
    );
    push_security_guidance_finding(
        &mut findings,
        "python_yaml_load",
        "yaml.load can construct arbitrary Python objects. Use yaml.safe_load or SafeLoader unless the input is fully trusted.",
        lower.contains("yaml.load(") && !lower.contains("safe_load") && !lower.contains("safeloader"),
    );
    push_security_guidance_finding(
        &mut findings,
        "python_os_system",
        "os.system and shell=True are command-injection prone. Prefer subprocess with an argument list and validated inputs.",
        lower.contains("os.system(") || lower.contains("shell=true"),
    );
    push_security_guidance_finding(
        &mut findings,
        "requests_verify_false",
        "verify=False disables TLS certificate validation. Keep verification enabled or pin a trusted CA bundle.",
        lower.contains("verify=false"),
    );
    push_security_guidance_finding(
        &mut findings,
        "javascript_dangerously_set_inner_html",
        "dangerouslySetInnerHTML can introduce XSS. Sanitize HTML with a trusted sanitizer or render structured content instead.",
        content.contains("dangerouslySetInnerHTML"),
    );
    push_security_guidance_finding(
        &mut findings,
        "crypto_ecb_mode",
        "ECB mode leaks plaintext structure. Use an authenticated mode such as AES-GCM or ChaCha20-Poly1305.",
        lower.contains("ecb")
            && (lower.contains("aes") || lower.contains("cipher") || lower.contains("crypto")),
    );
    push_security_guidance_finding(
        &mut findings,
        "xxe_xml_parser",
        "XML parsers can be XXE-prone when external entities are enabled. Disable DTD/entity resolution or use a hardened parser.",
        lower.contains("resolve_entities=true")
            || lower.contains("noent")
            || lower.contains("external_general_entities"),
    );
    push_security_guidance_finding(
        &mut findings,
        "github_actions_injection",
        "GitHub Actions expressions sourced from github.event can be attacker-controlled. Pass them through env vars and quote safely.",
        (path_lower.contains(".github/workflows") || path_lower.ends_with(".yml") || path_lower.ends_with(".yaml"))
            && lower.contains("${{ github.event"),
    );
    push_security_guidance_finding(
        &mut findings,
        "torch_load_without_weights_only",
        "torch.load can execute pickle payloads. Use weights_only=True for model weights when possible.",
        lower.contains("torch.load(") && !lower.contains("weights_only=true"),
    );
    findings
}

fn push_security_guidance_finding(
    findings: &mut Vec<SecurityGuidanceFinding>,
    rule_name: &'static str,
    reminder: &'static str,
    matched: bool,
) {
    if matched {
        findings.push(SecurityGuidanceFinding {
            rule_name,
            reminder,
        });
    }
}

fn security_guidance_warning_block(findings: &[SecurityGuidanceFinding]) -> String {
    let names = findings
        .iter()
        .map(|finding| finding.rule_name)
        .collect::<Vec<_>>()
        .join(", ");
    let mut lines = vec![
        "---".to_string(),
        format!(
            "Security guidance - {} pattern{} matched ({names})",
            findings.len(),
            if findings.len() == 1 { "" } else { "s" }
        ),
        String::new(),
    ];
    for finding in findings {
        lines.push(format!("- {}: {}", finding.rule_name, finding.reminder));
    }
    lines.push(String::new());
    lines.push(
        "Pattern matches can be false positives. If the construct is safe in this context, briefly document why; otherwise fix the code before moving on.".into(),
    );
    lines.join("\n")
}

pub(super) async fn run_pre_llm_call_hooks(
    store: &AppStore,
    run_id: &str,
    user_content: &str,
) -> Vec<String> {
    let payload = json!({
        "messages": [{
            "role": "user",
            "content": user_content,
        }],
        "user_content": user_content,
    });
    let _ = langfuse_record_hook(store, "pre_llm_call", run_id, &payload, None);
    let Ok(specs) = shell_hook_specs(store, "pre_llm_call") else {
        return Vec::new();
    };
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
    let payload = json!({
        "user_message": user_content,
        "response_text": response_text,
        "assistant_response": response_text,
        "model": model.unwrap_or_default(),
        "provider": provider_id.unwrap_or_default(),
    });
    let _ = langfuse_record_hook(store, "post_llm_call", run_id, &payload, None);
    let Ok(specs) = shell_hook_specs(store, "post_llm_call") else {
        return;
    };
    for spec in specs {
        let _ = run_shell_hook(&spec, run_id, "llm", &payload, None).await;
    }
    let _ = run_python_plugin_hooks(store, "post_llm_call", &payload).await;
}

pub(super) async fn run_pre_api_request_hooks(store: &AppStore, run_id: &str, payload: &Value) {
    let _ = langfuse_record_hook(store, "pre_api_request", run_id, payload, None);
    let Ok(specs) = shell_hook_specs(store, "pre_api_request") else {
        return;
    };
    for spec in specs {
        let _ = run_shell_hook(&spec, run_id, "llm_api", payload, None).await;
    }
}

pub(super) async fn run_post_api_request_hooks(store: &AppStore, run_id: &str, payload: &Value) {
    let _ = langfuse_record_hook(store, "post_api_request", run_id, payload, None);
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
        if let Some(callback) = response
            .get("postDeliveryCallback")
            .or_else(|| response.get("post_delivery_callback"))
            .or_else(|| response.get("registerPostDeliveryCallback"))
            .or_else(|| response.get("register_post_delivery_callback"))
        {
            if let Some(session_key) = hermes_post_delivery_session_key(platform, inbound) {
                let generation = response
                    .get("generation")
                    .or_else(|| response.get("hermesRunGeneration"))
                    .or_else(|| response.get("hermes_run_generation"))
                    .and_then(Value::as_i64);
                let _ = register_hermes_post_delivery_callback(
                    store,
                    &session_key,
                    generation,
                    callback.clone(),
                );
            }
        }
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

fn hermes_post_delivery_session_key(platform: &str, inbound: &Value) -> Option<String> {
    for key in ["sessionKey", "session_key", "sessionId", "session_id"] {
        if let Some(value) = inbound.get(key).and_then(Value::as_str) {
            let value = value.trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    let source = inbound.get("source").unwrap_or(&Value::Null);
    for key in [
        "sessionKey",
        "session_key",
        "chat_id",
        "chatId",
        "channel_id",
        "room_id",
    ] {
        if let Some(value) = source.get(key).and_then(Value::as_str) {
            let value = value.trim();
            if !value.is_empty() {
                let thread_id = source
                    .get("thread_id")
                    .or_else(|| source.get("threadId"))
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty());
                return Some(match thread_id {
                    Some(thread_id) => format!("{platform}:{value}:{thread_id}"),
                    None => format!("{platform}:{value}"),
                });
            }
        }
    }
    None
}

pub(super) async fn run_session_lifecycle_hooks(
    store: &AppStore,
    event: &str,
    run: &crate::models::AgentRunRecord,
    extra: Value,
) {
    run_selected_context_engine_lifecycle_for_run(store, event, run, &extra);
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

fn selected_context_engine_name_for_hooks() -> Option<String> {
    env::var("SYNTHCHAT_CONTEXT_ENGINE")
        .or_else(|_| env::var("HERMES_CONTEXT_ENGINE"))
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty() && !value.eq_ignore_ascii_case("compressor"))
}

fn run_selected_context_engine_lifecycle_for_run(
    store: &AppStore,
    event: &str,
    run: &crate::models::AgentRunRecord,
    extra: &Value,
) {
    if !matches!(event, "on_session_start" | "on_session_end") {
        return;
    }
    let Some(engine_name) = selected_context_engine_name_for_hooks() else {
        return;
    };
    let messages = if event == "on_session_end" {
        store
            .messages(&run.conversation_id, None)
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let lifecycle_extra = json!({
        "run_id": run.run_id,
        "conversation_id": run.conversation_id,
        "persona_id": run.persona_id,
        "agent_id": run.agent_id,
        "status": run.state,
        "state": run.state,
        "source": extra.get("source").cloned().unwrap_or(Value::Null),
        "extra": extra,
    });
    match run_context_engine_lifecycle(
        &engine_name,
        event,
        &run.conversation_id,
        &messages,
        &lifecycle_extra,
    ) {
        Ok(true) => {
            let _ = append_parent_phase_event(
                store,
                &run.run_id,
                "context_engine_lifecycle",
                json!({
                    "contextEngine": engine_name,
                    "event": event,
                    "conversationId": run.conversation_id,
                    "messageCount": messages.len(),
                }),
            );
        }
        Ok(false) => {}
        Err(error) => {
            eprintln!("SynthChat context engine '{engine_name}' lifecycle {event} failed: {error}");
        }
    }
}

fn hermes_post_delivery_callbacks_path(store: &AppStore) -> PathBuf {
    store
        .data_dir()
        .join("platforms")
        .join("post_delivery_callbacks.json")
}

fn load_hermes_post_delivery_registry(store: &AppStore) -> Value {
    let path = hermes_post_delivery_callbacks_path(store);
    fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| {
            json!({
                "schema": "hermes_post_delivery_callback_registry_desktop_v1",
                "desktopAdaptation": true,
                "callbacks": {},
                "history": []
            })
        })
}

fn save_hermes_post_delivery_registry(store: &AppStore, registry: &Value) -> AppResult<()> {
    let path = hermes_post_delivery_callbacks_path(store);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_vec_pretty(registry)?)?;
    fs::rename(tmp, path)?;
    Ok(())
}

pub(super) fn register_hermes_post_delivery_callback(
    store: &AppStore,
    session_key: &str,
    generation: Option<i64>,
    callback: Value,
) -> AppResult<Value> {
    let session_key = session_key.trim();
    if session_key.is_empty() {
        return Err(AppError::BadRequest(
            "post-delivery callback session_key is required".into(),
        ));
    }
    let command = callback
        .get("command")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| AppError::BadRequest("post-delivery callback.command is required".into()))?
        .to_string();
    let callback_id = callback
        .get("id")
        .or_else(|| callback.get("callbackId"))
        .or_else(|| callback.get("callback_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| new_id("post-delivery-callback"));
    let timeout_seconds = callback
        .get("timeout")
        .or_else(|| callback.get("timeoutSeconds"))
        .or_else(|| callback.get("timeout_seconds"))
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_TIMEOUT_SECONDS)
        .clamp(1, MAX_TIMEOUT_SECONDS);
    let mut entry = json!({
        "id": callback_id,
        "callbackId": callback_id,
        "command": command,
        "timeoutSeconds": timeout_seconds,
        "timeout_seconds": timeout_seconds,
        "payload": callback.get("payload").cloned().unwrap_or(Value::Null),
        "registeredAt": now_iso(),
        "registered_at": now_iso(),
        "schema": "hermes_post_delivery_callback_intent_desktop_v1",
        "desktopAdaptation": true
    });
    if let Some(object) = entry.as_object_mut() {
        if let Some(extra) = callback.get("extra") {
            object.insert("extra".into(), extra.clone());
        }
    }

    let mut registry = load_hermes_post_delivery_registry(store);
    registry["schema"] = json!("hermes_post_delivery_callback_registry_desktop_v1");
    registry["desktopAdaptation"] = json!(true);
    registry["updatedAt"] = json!(now_iso());
    if !registry
        .get("callbacks")
        .map(Value::is_object)
        .unwrap_or(false)
    {
        registry["callbacks"] = json!({});
    }
    if !registry
        .get("history")
        .map(Value::is_array)
        .unwrap_or(false)
    {
        registry["history"] = json!([]);
    }
    let callbacks = registry["callbacks"].as_object_mut().unwrap();
    let existing = callbacks.get(session_key).cloned();
    let mut stale = false;
    let mut callback_entries = Vec::new();
    if let Some(existing) = existing {
        let existing_generation = existing.get("generation").and_then(Value::as_i64);
        if let (Some(existing_generation), Some(generation)) = (existing_generation, generation) {
            if generation < existing_generation {
                stale = true;
            }
        }
        if !stale
            && (existing_generation.is_none()
                || generation.is_none()
                || existing_generation == generation)
        {
            callback_entries.extend(
                existing
                    .get("callbacks")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default(),
            );
        }
    }
    if stale {
        return Ok(json!({
            "schema": "hermes_post_delivery_callback_registry_desktop_v1",
            "registered": false,
            "staleGeneration": true,
            "sessionKey": session_key,
            "generation": generation,
            "note": "Stale-generation registrations do not overwrite a fresher Hermes post-delivery callback slot."
        }));
    }
    callback_entries.push(entry.clone());
    callbacks.insert(
        session_key.to_string(),
        json!({
            "sessionKey": session_key,
            "session_key": session_key,
            "generation": generation,
            "callbacks": callback_entries,
            "updatedAt": now_iso(),
            "updated_at": now_iso(),
        }),
    );
    save_hermes_post_delivery_registry(store, &registry)?;
    Ok(json!({
        "schema": "hermes_post_delivery_callback_registry_desktop_v1",
        "registered": true,
        "staleGeneration": false,
        "sessionKey": session_key,
        "generation": generation,
        "callback": entry
    }))
}

fn pop_hermes_post_delivery_callbacks(
    store: &AppStore,
    session_key: &str,
    generation: Option<i64>,
) -> AppResult<Vec<Value>> {
    let mut registry = load_hermes_post_delivery_registry(store);
    let Some(callbacks) = registry.get_mut("callbacks").and_then(Value::as_object_mut) else {
        return Ok(Vec::new());
    };
    let Some(entry) = callbacks.get(session_key).cloned() else {
        return Ok(Vec::new());
    };
    let entry_generation = entry.get("generation").and_then(Value::as_i64);
    if generation.is_some() && entry_generation != generation {
        return Ok(Vec::new());
    }
    callbacks.remove(session_key);
    registry["updatedAt"] = json!(now_iso());
    save_hermes_post_delivery_registry(store, &registry)?;
    Ok(entry
        .get("callbacks")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default())
}

fn append_hermes_post_delivery_history(store: &AppStore, records: Vec<Value>) -> AppResult<()> {
    if records.is_empty() {
        return Ok(());
    }
    let mut registry = load_hermes_post_delivery_registry(store);
    if !registry
        .get("history")
        .map(Value::is_array)
        .unwrap_or(false)
    {
        registry["history"] = json!([]);
    }
    if let Some(history) = registry["history"].as_array_mut() {
        history.extend(records);
        if history.len() > 100 {
            let drop_count = history.len() - 100;
            history.drain(0..drop_count);
        }
    }
    registry["updatedAt"] = json!(now_iso());
    save_hermes_post_delivery_registry(store, &registry)
}

fn hermes_post_delivery_generation_for_run(run: &crate::models::AgentRunRecord) -> Option<i64> {
    run.phase_events.iter().rev().find_map(|event| {
        event
            .detail
            .get("hermesRunGeneration")
            .or_else(|| event.detail.get("hermes_run_generation"))
            .and_then(Value::as_i64)
    })
}

pub(super) async fn run_hermes_post_delivery_callbacks(
    store: &AppStore,
    run: &crate::models::AgentRunRecord,
    extra: &Value,
) -> AppResult<Value> {
    let generation = hermes_post_delivery_generation_for_run(run);
    let callbacks = pop_hermes_post_delivery_callbacks(store, &run.conversation_id, generation)?;
    let mut records = Vec::new();
    for callback in callbacks {
        let command = callback
            .get("command")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let timeout_seconds = callback
            .get("timeoutSeconds")
            .or_else(|| callback.get("timeout_seconds"))
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_TIMEOUT_SECONDS)
            .clamp(1, MAX_TIMEOUT_SECONDS);
        let spec = ShellHookSpec {
            event: "post_delivery_callback".into(),
            command,
            matcher: None,
            timeout_seconds,
        };
        let payload = json!({
            "schema": "hermes_post_delivery_callback_fire_desktop_v1",
            "sessionKey": run.conversation_id,
            "session_key": run.conversation_id,
            "runId": run.run_id,
            "run_id": run.run_id,
            "generation": generation,
            "callback": callback,
            "postDelivery": extra.get("postDelivery").cloned().unwrap_or(Value::Null),
            "post_delivery": extra.get("post_delivery").cloned().unwrap_or_else(|| extra.get("postDelivery").cloned().unwrap_or(Value::Null)),
            "extra": extra,
            "desktopAdaptation": true
        });
        let mut record = json!({
            "schema": "hermes_post_delivery_callback_fire_record_desktop_v1",
            "sessionKey": run.conversation_id,
            "runId": run.run_id,
            "callbackId": callback.get("callbackId").or_else(|| callback.get("id")).cloned().unwrap_or(Value::Null),
            "generation": generation,
            "command": spec.command,
            "firedAt": now_iso(),
            "status": "started"
        });
        match run_shell_hook(&spec, &run.run_id, "post_delivery_callback", &payload, None).await {
            Ok(result) => {
                record["status"] = json!("completed");
                record["result"] = result.unwrap_or(Value::Null);
            }
            Err(error) => {
                record["status"] = json!("failed");
                record["error"] = json!(error.to_string());
            }
        }
        records.push(record);
    }
    let count = records.len();
    append_hermes_post_delivery_history(store, records.clone())?;
    Ok(json!({
        "schema": "hermes_post_delivery_callback_registry_desktop_v1",
        "fired": count,
        "generation": generation,
        "records": records
    }))
}

pub(super) async fn run_session_finished_hooks(
    store: &AppStore,
    run: &crate::models::AgentRunRecord,
    extra: Value,
) {
    let _ = run_hermes_post_delivery_callbacks(store, run, &extra).await;
    let _ = disk_cleanup_session_end_hook(store, run);
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
    run_selected_context_engine_reset(conversation, &extra);
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

fn run_selected_context_engine_reset(conversation: &crate::models::Conversation, extra: &Value) {
    let Some(engine_name) = selected_context_engine_name_for_hooks() else {
        return;
    };
    let lifecycle_extra = json!({
        "conversation_id": conversation.id,
        "persona_id": conversation.persona_id,
        "agent_id": conversation.agent_id,
        "title": conversation.title,
        "source": extra.get("source").cloned().unwrap_or(Value::Null),
        "extra": extra,
    });
    match run_context_engine_lifecycle(
        &engine_name,
        "on_session_reset",
        &conversation.id,
        &[],
        &lifecycle_extra,
    ) {
        Ok(true) => {}
        Ok(false) => {}
        Err(error) => {
            eprintln!("SynthChat context engine '{engine_name}' lifecycle on_session_reset failed: {error}");
        }
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
        .filter_map(|plugin| python_plugin_spec_from_summary(plugin))
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

fn python_plugin_command_tool_specs(store: &AppStore) -> AppResult<Vec<PythonPluginHookSpec>> {
    let mut specs = enabled_python_plugin_specs(store)?
        .into_iter()
        .filter_map(python_plugin_spec_from_summary)
        .collect::<Vec<_>>();
    specs.extend(context_engine_python_plugin_specs());
    Ok(specs)
}

fn context_engine_python_plugin_specs() -> Vec<PythonPluginHookSpec> {
    let root = hermes_context_engine_plugins_dir();
    let Ok(entries) = fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut specs = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if !path.is_dir() || name.starts_with('_') || name.starts_with('.') {
                return None;
            }
            if !path.join("__init__.py").is_file() {
                return None;
            }
            Some(PythonPluginHookSpec {
                plugin_id: format!("context-engine/{name}"),
                plugin_name: name,
                path,
                source: "context_engine".into(),
                entry_point: String::new(),
            })
        })
        .collect::<Vec<_>>();
    specs.sort_by(|left, right| left.plugin_id.cmp(&right.plugin_id));
    specs
}

fn hermes_context_engine_plugins_dir() -> PathBuf {
    if let Some(root) = env::var_os("HERMES_AGENT_REPO")
        .or_else(|| env::var_os("HERMES_REPO"))
        .filter(|value| !value.is_empty())
    {
        return PathBuf::from(root).join("plugins").join("context_engine");
    }
    PathBuf::from(r"D:\pro_sunner\demo_vscode\hermes-agent")
        .join("plugins")
        .join("context_engine")
}

fn python_plugin_spec_from_summary(
    plugin: crate::models::PluginSummary,
) -> Option<PythonPluginHookSpec> {
    let source = plugin.source.clone();
    if source == "entrypoint" {
        let entry_point = if plugin.entry_point.trim().is_empty() {
            plugin.path.clone()
        } else {
            plugin.entry_point.clone()
        };
        if entry_point.trim().is_empty() {
            return None;
        }
        return Some(PythonPluginHookSpec {
            plugin_id: plugin.id,
            plugin_name: plugin.name,
            path: PathBuf::from(&entry_point),
            source,
            entry_point,
        });
    }
    let path = PathBuf::from(&plugin.path);
    if !path.join("__init__.py").is_file() {
        return None;
    }
    Some(PythonPluginHookSpec {
        plugin_id: plugin.id,
        plugin_name: plugin.name,
        path,
        source,
        entry_point: plugin.entry_point,
    })
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
    let specs = python_plugin_command_tool_specs(store)?;
    if specs.is_empty() {
        return Ok(None);
    }
    let output = run_python_plugin_command_runner(&specs, command_name, raw_args).await?;
    if output
        .get("handled")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        let result_value = resolve_python_plugin_bridge_result(
            store,
            output.get("result").cloned().unwrap_or(Value::Null),
            None,
        )
        .await?;
        let reply = result_value
            .as_str()
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| result_value.to_string());
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
    Ok(None)
}

async fn resolve_python_plugin_bridge_result(
    store: &AppStore,
    value: Value,
    bridge_context: Option<&PythonPluginBridgeContext<'_>>,
) -> AppResult<Value> {
    let candidate = value
        .as_str()
        .and_then(|text| serde_json::from_str::<Value>(text).ok())
        .unwrap_or_else(|| value.clone());
    let Some(dispatch) = candidate.get("__synthchat_dispatch_tool__") else {
        return Ok(value);
    };
    let tool_name = dispatch
        .get("tool_name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let payload = dispatch.get("args").cloned().unwrap_or_else(|| json!({}));
    let allow_mutating_tools = bridge_context
        .map(|context| context.allow_mutating_tools)
        .unwrap_or(false);
    if python_plugin_bridge_tool_mutates(tool_name, &payload) && !allow_mutating_tools {
        return Err(AppError::BadRequest(format!(
            "python plugin bridge tool requires agent run context: {tool_name}"
        )));
    }
    if let Some(context) = bridge_context {
        if is_internal_tool(tool_name) {
            if let Some(reason) = tool_approval_reason(
                store,
                "__internal",
                tool_name,
                &payload,
                python_plugin_bridge_tool_mutates(tool_name, &payload),
            )? {
                return request_python_plugin_bridge_approval(
                    store,
                    context,
                    "__internal",
                    tool_name,
                    payload.clone(),
                    reason,
                );
            }
            let (text, event) = Box::pin(execute_recovery_internal_tool(
                store,
                context.agent,
                context.conversation_id,
                context.run_id,
                tool_name,
                payload.clone(),
                context.tool_context,
                context.app,
            ))
            .await?;
            record_tool_event_for_run(
                store,
                context.app,
                context.conversation_id,
                context.run_id,
                event,
            )?;
            return Ok(Value::String(text));
        }
        let tools = available_mcp_tool_definitions(store, context.agent)?;
        if let Some(definition) = resolve_mcp_tool(&tools, tool_name) {
            if definition.source == "python-plugin" {
                return Err(AppError::BadRequest(format!(
                    "python plugin bridge cannot recursively dispatch python plugin tool: {tool_name}"
                )));
            }
            if let Some(reason) = tool_approval_reason(
                store,
                &definition.server_id,
                &definition.tool_name,
                &payload,
                definition.requires_approval,
            )? {
                return request_python_plugin_bridge_approval(
                    store,
                    context,
                    &definition.server_id,
                    &definition.tool_name,
                    payload.clone(),
                    reason,
                );
            }
            let (text, event) = Box::pin(execute_recovery_mcp_tool(
                store,
                context.run_id,
                &definition,
                payload.clone(),
                Some(context),
            ))
            .await?;
            record_tool_event_for_run(
                store,
                context.app,
                context.conversation_id,
                context.run_id,
                event,
            )?;
            return Ok(Value::String(text));
        }
    }
    let result = match tool_name {
        "kanban_create" => kanban_create_tool(store, &payload)?,
        "kanban_list" => kanban_list_tool(store, &payload)?,
        "kanban_show" => kanban_show_tool(store, &payload)?,
        "kanban_decompose" => kanban_decompose_tool(store, &payload).await?,
        "kanban_specify" => kanban_specify_tool(store, &payload).await?,
        "kanban_complete" => kanban_complete_tool(store, &payload)?,
        "kanban_block" => kanban_block_tool(store, &payload)?,
        "kanban_unblock" => kanban_unblock_tool(store, &payload)?,
        "kanban_heartbeat" => kanban_heartbeat_tool(store, &payload)?,
        "kanban_comment" => kanban_comment_tool(store, &payload)?,
        "kanban_link" => kanban_link_tool(store, &payload)?,
        _ => {
            return Err(AppError::BadRequest(format!(
                "python plugin bridge tool is not available: {tool_name}"
            )));
        }
    };
    Ok(Value::String(result))
}

fn request_python_plugin_bridge_approval(
    store: &AppStore,
    context: &PythonPluginBridgeContext<'_>,
    server_id: &str,
    tool_name: &str,
    payload: Value,
    reason: String,
) -> AppResult<Value> {
    let run = store.agent_run(context.run_id)?;
    let approval_payload = python_plugin_bridge_approval_payload(payload);
    record_tool_started_for_run(
        store,
        context.app,
        context.run_id,
        server_id,
        tool_name,
        &approval_payload,
        0,
    )?;
    let approval = append_tool_approval_request(
        store,
        context.conversation_id,
        &run.persona_id,
        &run.agent_id,
        context.run_id,
        server_id,
        tool_name,
        approval_payload,
        reason,
        context.tool_context,
    )?;
    let mut updated_run = store.agent_run(context.run_id)?;
    updated_run.state = "pendingApproval".into();
    updated_run.updated_at = crate::models::now_iso();
    let saved_run = store.save_agent_run(updated_run)?;
    let assistant = store.append_message(ChatMessage::new(
        context.conversation_id.to_string(),
        "assistant",
        format!(
            "插件工具调用正在等待审批：{} · {}",
            approval.server_id, approval.tool_name
        ),
        "desktop-agent",
    ))?;
    emit_agent_run_record(context.app, &saved_run, Some(&assistant));
    Err(AppError::BadRequest(format!(
        "python plugin bridge tool requires approval: {} · {}",
        approval.server_id, approval.tool_name
    )))
}

fn python_plugin_bridge_approval_payload(mut payload: Value) -> Value {
    if let Some(object) = payload.as_object_mut() {
        object
            .entry(PROVIDER_TOOL_CALL_META_KEY.to_string())
            .or_insert_with(|| json!({"id": new_id("call")}));
    }
    payload
}

fn python_plugin_bridge_tool_mutates(tool_name: &str, payload: &Value) -> bool {
    match tool_name {
        "kanban_create" | "kanban_complete" | "kanban_block" | "kanban_unblock"
        | "kanban_heartbeat" | "kanban_comment" | "kanban_link" | "kanban_specify" => true,
        "kanban_decompose" => payload
            .get("create")
            .or_else(|| payload.get("createTasks"))
            .or_else(|| payload.get("create_tasks"))
            .and_then(Value::as_bool)
            .unwrap_or(false),
        _ => false,
    }
}

pub(super) fn list_python_plugin_tools(
    store: &AppStore,
) -> AppResult<Vec<PythonPluginToolDefinition>> {
    let mut definitions = Vec::new();
    for spec in python_plugin_command_tool_specs(store)? {
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
        let Some(spec) = python_plugin_spec_from_summary(plugin) else {
            continue;
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

pub(super) fn list_python_plugin_commands(
    store: &AppStore,
) -> AppResult<Vec<PythonPluginCommandDefinition>> {
    let mut definitions = Vec::new();
    for spec in python_plugin_command_tool_specs(store)? {
        match run_python_plugin_command_list_runner(&spec) {
            Ok(commands) => definitions.extend(commands),
            Err(error) => {
                eprintln!(
                    "SynthChat plugin command discovery failed for {}: {}",
                    spec.plugin_id, error
                );
            }
        }
    }
    definitions.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.plugin_id.cmp(&right.plugin_id))
    });
    Ok(definitions)
}

pub(crate) fn list_python_plugin_auxiliary_tasks(
    store: &AppStore,
) -> AppResult<Vec<PluginAuxiliaryTaskSummary>> {
    let mut definitions = Vec::new();
    for plugin in enabled_python_plugin_specs(store)? {
        let Some(spec) = python_plugin_spec_from_summary(plugin) else {
            continue;
        };
        match cached_python_plugin_auxiliary_task_definitions(&spec) {
            Ok(tasks) => definitions.extend(tasks),
            Err(error) => {
                eprintln!(
                    "SynthChat plugin auxiliary task discovery failed for {}: {}",
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

fn cached_python_plugin_auxiliary_task_definitions(
    spec: &PythonPluginHookSpec,
) -> AppResult<Vec<PluginAuxiliaryTaskSummary>> {
    let cache_key = python_plugin_tool_cache_key(spec);
    let cache = PYTHON_PLUGIN_AUXILIARY_TASK_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    {
        let guard = cache.lock().map_err(|_| {
            AppError::BadRequest("python plugin auxiliary task cache lock poisoned".into())
        })?;
        if let Some(cached) = guard.get(&cache_key) {
            if cached.captured_at.elapsed() < PYTHON_PLUGIN_TOOL_CACHE_TTL {
                return Ok(cached.tasks.clone());
            }
        }
    }
    let tasks = run_python_plugin_auxiliary_task_list_runner(spec)?;
    let mut guard = cache.lock().map_err(|_| {
        AppError::BadRequest("python plugin auxiliary task cache lock poisoned".into())
    })?;
    guard.insert(
        cache_key,
        CachedPythonPluginAuxiliaryTasks {
            captured_at: Instant::now(),
            tasks: tasks.clone(),
        },
    );
    Ok(tasks)
}

fn python_plugin_tool_cache_key(spec: &PythonPluginHookSpec) -> String {
    if spec.source == "entrypoint" {
        return format!("{}|entrypoint|{}", spec.plugin_id, spec.entry_point.trim());
    }
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
    bridge_context: Option<&PythonPluginBridgeContext<'_>>,
) -> AppResult<String> {
    let mut last_error = None;
    for spec in python_plugin_command_tool_specs(store)? {
        let output =
            run_python_plugin_tool_runner(&spec, tool_name, payload, bridge_context.is_some())
                .await?;
        if output.get("ok").and_then(Value::as_bool).unwrap_or(false) {
            let result_value = resolve_python_plugin_bridge_result(
                store,
                output.get("result").cloned().unwrap_or(Value::Null),
                bridge_context,
            )
            .await?;
            return Ok(result_value
                .as_str()
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| result_value.to_string()));
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

pub(super) fn run_context_engine_compress(
    engine_name: &str,
    messages: &[ChatMessage],
    current_tokens: usize,
    focus_topic: Option<&str>,
) -> AppResult<Vec<ContextEngineCompressedMessage>> {
    let clean = engine_name.trim();
    if clean.is_empty() || clean.eq_ignore_ascii_case("compressor") {
        return Err(AppError::BadRequest(
            "context engine name must be a non-default plugin".into(),
        ));
    }
    let spec = context_engine_python_plugin_specs()
        .into_iter()
        .find(|spec| spec.plugin_name.eq_ignore_ascii_case(clean))
        .ok_or_else(|| AppError::NotFound(format!("context engine {clean}")))?;
    let request_messages = messages
        .iter()
        .filter(|message| {
            matches!(
                message.role.as_str(),
                "system" | "user" | "assistant" | "tool"
            )
        })
        .map(|message| {
            json!({
                "role": message.role,
                "content": message.content,
            })
        })
        .collect::<Vec<_>>();
    let request = json!({
        "plugin_id": spec.plugin_id,
        "plugin_name": spec.plugin_name,
        "plugin_source": spec.source,
        "entry_point": spec.entry_point,
        "plugin_dir": spec.path,
        "context_engine_action": "compress",
        "context_engine_messages": request_messages,
        "context_engine_current_tokens": current_tokens,
        "context_engine_focus_topic": focus_topic.unwrap_or_default(),
    });
    let output = run_python_plugin_hook_runner_blocking(&request)?;
    if !output.get("ok").and_then(Value::as_bool).unwrap_or(false) {
        let error = output
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("context engine compress failed");
        return Err(AppError::BadRequest(format!(
            "context engine compress failed for {clean}: {error}"
        )));
    }
    let raw_messages = output
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            AppError::BadRequest("context engine compress returned no messages".into())
        })?;
    let compressed = raw_messages
        .iter()
        .filter_map(|message| {
            let role = message
                .get("role")
                .and_then(Value::as_str)
                .unwrap_or("assistant")
                .trim()
                .to_ascii_lowercase();
            let content = message.get("content").and_then(Value::as_str)?.trim();
            if content.is_empty() {
                return None;
            }
            Some(ContextEngineCompressedMessage {
                role: if matches!(role.as_str(), "system" | "user" | "assistant" | "tool") {
                    role
                } else {
                    "assistant".into()
                },
                content: content.to_string(),
            })
        })
        .collect::<Vec<_>>();
    if compressed.is_empty() {
        return Err(AppError::BadRequest(
            "context engine compress returned empty content".into(),
        ));
    }
    Ok(compressed)
}

pub(super) fn run_context_engine_should_compress(
    engine_name: &str,
    messages: &[ChatMessage],
    current_tokens: usize,
    preflight: bool,
) -> AppResult<Option<bool>> {
    let clean = engine_name.trim();
    if clean.is_empty() || clean.eq_ignore_ascii_case("compressor") {
        return Err(AppError::BadRequest(
            "context engine name must be a non-default plugin".into(),
        ));
    }
    let spec = context_engine_python_plugin_specs()
        .into_iter()
        .find(|spec| spec.plugin_name.eq_ignore_ascii_case(clean))
        .ok_or_else(|| AppError::NotFound(format!("context engine {clean}")))?;
    let request_messages = messages
        .iter()
        .filter(|message| {
            matches!(
                message.role.as_str(),
                "system" | "user" | "assistant" | "tool"
            )
        })
        .map(|message| {
            json!({
                "role": message.role,
                "content": message.content,
            })
        })
        .collect::<Vec<_>>();
    let request = json!({
        "plugin_id": spec.plugin_id,
        "plugin_name": spec.plugin_name,
        "plugin_source": spec.source,
        "entry_point": spec.entry_point,
        "plugin_dir": spec.path,
        "context_engine_action": if preflight {
            "should_compress_preflight"
        } else {
            "should_compress"
        },
        "context_engine_messages": request_messages,
        "context_engine_current_tokens": current_tokens,
    });
    let output = run_python_plugin_hook_runner_blocking(&request)?;
    if !output.get("ok").and_then(Value::as_bool).unwrap_or(false) {
        let error = output
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("context engine compression decision failed");
        return Err(AppError::BadRequest(format!(
            "context engine compression decision failed for {clean}: {error}"
        )));
    }
    if !output
        .get("implemented")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Ok(None);
    }
    Ok(Some(
        output
            .get("decision")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    ))
}

pub(super) fn run_context_engine_update_from_response(
    engine_name: &str,
    usage: &Value,
) -> AppResult<bool> {
    let clean = engine_name.trim();
    if clean.is_empty() || clean.eq_ignore_ascii_case("compressor") {
        return Err(AppError::BadRequest(
            "context engine name must be a non-default plugin".into(),
        ));
    }
    let spec = context_engine_python_plugin_specs()
        .into_iter()
        .find(|spec| spec.plugin_name.eq_ignore_ascii_case(clean))
        .ok_or_else(|| AppError::NotFound(format!("context engine {clean}")))?;
    let request = json!({
        "plugin_id": spec.plugin_id,
        "plugin_name": spec.plugin_name,
        "plugin_source": spec.source,
        "entry_point": spec.entry_point,
        "plugin_dir": spec.path,
        "context_engine_action": "update_from_response",
        "context_engine_usage": usage,
    });
    let output = run_python_plugin_hook_runner_blocking(&request)?;
    if !output.get("ok").and_then(Value::as_bool).unwrap_or(false) {
        let error = output
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("context engine update_from_response failed");
        return Err(AppError::BadRequest(format!(
            "context engine update_from_response failed for {clean}: {error}"
        )));
    }
    Ok(output
        .get("implemented")
        .and_then(Value::as_bool)
        .unwrap_or(false))
}

pub(super) fn run_context_engine_update_model(
    engine_name: &str,
    model_context: &Value,
) -> AppResult<bool> {
    let clean = engine_name.trim();
    if clean.is_empty() || clean.eq_ignore_ascii_case("compressor") {
        return Err(AppError::BadRequest(
            "context engine name must be a non-default plugin".into(),
        ));
    }
    let spec = context_engine_python_plugin_specs()
        .into_iter()
        .find(|spec| spec.plugin_name.eq_ignore_ascii_case(clean))
        .ok_or_else(|| AppError::NotFound(format!("context engine {clean}")))?;
    let request = json!({
        "plugin_id": spec.plugin_id,
        "plugin_name": spec.plugin_name,
        "plugin_source": spec.source,
        "entry_point": spec.entry_point,
        "plugin_dir": spec.path,
        "context_engine_action": "update_model",
        "context_engine_model": model_context,
    });
    let output = run_python_plugin_hook_runner_blocking(&request)?;
    if !output.get("ok").and_then(Value::as_bool).unwrap_or(false) {
        let error = output
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("context engine update_model failed");
        return Err(AppError::BadRequest(format!(
            "context engine update_model failed for {clean}: {error}"
        )));
    }
    Ok(output
        .get("implemented")
        .and_then(Value::as_bool)
        .unwrap_or(false))
}

pub(super) fn run_context_engine_lifecycle(
    engine_name: &str,
    event: &str,
    session_id: &str,
    messages: &[ChatMessage],
    extra: &Value,
) -> AppResult<bool> {
    let clean = engine_name.trim();
    if clean.is_empty() || clean.eq_ignore_ascii_case("compressor") {
        return Err(AppError::BadRequest(
            "context engine name must be a non-default plugin".into(),
        ));
    }
    let clean_event = event.trim();
    if !matches!(
        clean_event,
        "on_session_start" | "on_session_end" | "on_session_reset"
    ) {
        return Err(AppError::BadRequest(format!(
            "unsupported context engine lifecycle event: {clean_event}"
        )));
    }
    let spec = context_engine_python_plugin_specs()
        .into_iter()
        .find(|spec| spec.plugin_name.eq_ignore_ascii_case(clean))
        .ok_or_else(|| AppError::NotFound(format!("context engine {clean}")))?;
    let request_messages = messages
        .iter()
        .filter(|message| {
            matches!(
                message.role.as_str(),
                "system" | "user" | "assistant" | "tool"
            )
        })
        .map(|message| {
            json!({
                "role": message.role,
                "content": message.content,
            })
        })
        .collect::<Vec<_>>();
    let request = json!({
        "plugin_id": spec.plugin_id,
        "plugin_name": spec.plugin_name,
        "plugin_source": spec.source,
        "entry_point": spec.entry_point,
        "plugin_dir": spec.path,
        "context_engine_action": "lifecycle",
        "context_engine_lifecycle_event": clean_event,
        "context_engine_session_id": session_id,
        "context_engine_messages": request_messages,
        "context_engine_lifecycle_extra": extra,
    });
    let output = run_python_plugin_hook_runner_blocking(&request)?;
    if !output.get("ok").and_then(Value::as_bool).unwrap_or(false) {
        let error = output
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("context engine lifecycle failed");
        return Err(AppError::BadRequest(format!(
            "context engine lifecycle {clean_event} failed for {clean}: {error}"
        )));
    }
    Ok(output
        .get("implemented")
        .and_then(Value::as_bool)
        .unwrap_or(false))
}

async fn run_python_plugin_hook(
    spec: &PythonPluginHookSpec,
    event: &str,
    kwargs: &Value,
) -> AppResult<Vec<Value>> {
    let request = json!({
        "plugin_id": spec.plugin_id,
        "plugin_name": spec.plugin_name,
        "plugin_source": spec.source,
        "entry_point": spec.entry_point,
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
    specs: &[PythonPluginHookSpec],
    command_name: &str,
    raw_args: &str,
) -> AppResult<Value> {
    let plugins = specs
        .iter()
        .map(python_plugin_spec_request_value)
        .collect::<Vec<_>>();
    let request = json!({
        "plugins": plugins,
        "command_name": command_name,
        "raw_args": raw_args,
        "bridge_tools": PYTHON_PLUGIN_BRIDGE_TOOLS,
    });
    let output = run_python_plugin_hook_runner(&request).await?;
    if let Some(error) = output.get("error").and_then(Value::as_str) {
        return Err(AppError::BadRequest(format!(
            "python plugin command runner error: {error}"
        )));
    }
    Ok(output)
}

fn python_plugin_spec_request_value(spec: &PythonPluginHookSpec) -> Value {
    json!({
        "plugin_id": spec.plugin_id,
        "plugin_name": spec.plugin_name,
        "plugin_source": spec.source,
        "entry_point": spec.entry_point,
        "plugin_dir": spec.path,
    })
}

fn run_python_plugin_tool_list_runner(
    spec: &PythonPluginHookSpec,
) -> AppResult<Vec<PythonPluginToolDefinition>> {
    let request = json!({
        "plugin_id": spec.plugin_id,
        "plugin_name": spec.plugin_name,
        "plugin_source": spec.source,
        "entry_point": spec.entry_point,
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
        "plugin_source": spec.source,
        "entry_point": spec.entry_point,
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

fn run_python_plugin_command_list_runner(
    spec: &PythonPluginHookSpec,
) -> AppResult<Vec<PythonPluginCommandDefinition>> {
    let request = json!({
        "plugin_id": spec.plugin_id,
        "plugin_name": spec.plugin_name,
        "plugin_source": spec.source,
        "entry_point": spec.entry_point,
        "plugin_dir": spec.path,
        "list_commands": true,
    });
    let output = run_python_plugin_hook_runner_blocking(&request)?;
    if let Some(error) = output.get("error").and_then(Value::as_str) {
        return Err(AppError::BadRequest(format!(
            "python plugin command discovery runner error: {error}"
        )));
    }
    Ok(output
        .get("commands")
        .and_then(Value::as_array)
        .map(|commands| {
            commands
                .iter()
                .filter_map(|command| {
                    let name = command.get("name").and_then(Value::as_str)?.trim();
                    if name.is_empty() {
                        return None;
                    }
                    Some(PythonPluginCommandDefinition {
                        plugin_id: spec.plugin_id.clone(),
                        plugin_name: spec.plugin_name.clone(),
                        name: name.to_string(),
                        description: command
                            .get("description")
                            .and_then(Value::as_str)
                            .unwrap_or("Plugin command")
                            .to_string(),
                        args_hint: command
                            .get("args_hint")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default())
}

fn run_python_plugin_auxiliary_task_list_runner(
    spec: &PythonPluginHookSpec,
) -> AppResult<Vec<PluginAuxiliaryTaskSummary>> {
    let request = json!({
        "plugin_id": spec.plugin_id,
        "plugin_name": spec.plugin_name,
        "plugin_source": spec.source,
        "entry_point": spec.entry_point,
        "plugin_dir": spec.path,
        "list_auxiliary_tasks": true,
    });
    let output = run_python_plugin_hook_runner_blocking(&request)?;
    if let Some(error) = output.get("error").and_then(Value::as_str) {
        return Err(AppError::BadRequest(format!(
            "python plugin auxiliary task discovery runner error: {error}"
        )));
    }
    Ok(output
        .get("auxiliary_tasks")
        .and_then(Value::as_array)
        .map(|tasks| {
            tasks
                .iter()
                .filter_map(|task| {
                    let key = task.get("key").and_then(Value::as_str)?.trim();
                    if key.is_empty() {
                        return None;
                    }
                    Some(PluginAuxiliaryTaskSummary {
                        plugin_id: spec.plugin_id.clone(),
                        plugin_name: spec.plugin_name.clone(),
                        key: key.to_string(),
                        display_name: task
                            .get("display_name")
                            .and_then(Value::as_str)
                            .unwrap_or(key)
                            .to_string(),
                        description: task
                            .get("description")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                        defaults: task.get("defaults").cloned().unwrap_or_else(|| json!({})),
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
    allow_external_dispatch: bool,
) -> AppResult<Value> {
    let request = json!({
        "plugin_id": spec.plugin_id,
        "plugin_name": spec.plugin_name,
        "plugin_source": spec.source,
        "entry_point": spec.entry_point,
        "plugin_dir": spec.path,
        "tool_name": tool_name,
        "tool_args": payload,
        "bridge_tools": PYTHON_PLUGIN_BRIDGE_TOOLS,
        "allow_external_dispatch": allow_external_dispatch,
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
        .hide_window()
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
        .hide_window()
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
    child.hide_window();
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

    let mut command = std::process::Command::new(program);
    command.hide_window();
    let mut child = match command
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
