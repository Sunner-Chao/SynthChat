use std::{env, fs, path::PathBuf};

use serde_json::{json, Value};

use crate::{error::AppResult, store::AppStore};

use super::{
    list_agent_auxiliary_task_assignments, list_python_plugin_commands, list_python_plugin_tools,
};

pub(super) fn context_engine_tool(store: &AppStore, payload: &Value) -> AppResult<String> {
    let action = payload
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("status")
        .trim()
        .to_ascii_lowercase();
    if !matches!(
        action.as_str(),
        "status" | "discover" | "commands" | "diagnostics"
    ) {
        return Ok(serde_json::to_string_pretty(&json!({
            "schema": "hermes_context_engine_desktop_v1",
            "status": "unsupported_action",
            "supportedActions": ["status", "discover", "commands", "diagnostics"],
        }))?);
    }

    let snapshot = context_engine_snapshot(store, &action);
    Ok(serde_json::to_string_pretty(&snapshot)?)
}

pub(super) fn context_engine_snapshot(store: &AppStore, action: &str) -> Value {
    let config = store.config().ok();
    let chat = config
        .as_ref()
        .map(|config| config.chat.clone())
        .unwrap_or_default();
    let plugin_root = hermes_context_engine_plugins_dir();
    let discovered = discover_context_engine_plugin_dirs(&plugin_root);
    let active_engine_name = active_context_engine_name();
    let active_engine_is_default = active_engine_name.eq_ignore_ascii_case("compressor");
    let runtime_contract = context_engine_runtime_persistence_contract(
        store,
        &active_engine_name,
        active_engine_is_default,
    );
    let dynamic_commands = list_python_plugin_commands(store)
        .unwrap_or_default()
        .into_iter()
        .filter(|command| command.plugin_id.starts_with("context-engine/"))
        .map(|command| {
            json!({
                "pluginId": command.plugin_id,
                "pluginName": command.plugin_name,
                "name": command.name,
                "description": command.description,
                "argsHint": command.args_hint
            })
        })
        .collect::<Vec<_>>();
    let dynamic_tools = list_python_plugin_tools(store)
        .unwrap_or_default()
        .into_iter()
        .filter(|tool| tool.plugin_id.starts_with("context-engine/"))
        .map(|tool| {
            json!({
                "pluginId": tool.plugin_id,
                "pluginName": tool.plugin_name,
                "name": tool.name,
                "toolset": tool.toolset,
                "description": tool.description,
                "schema": tool.schema
            })
        })
        .collect::<Vec<_>>();
    let dynamic_command_count = dynamic_commands.len();
    let dynamic_tool_count = dynamic_tools.len();
    let compression_assignment = list_agent_auxiliary_task_assignments(store)
        .ok()
        .and_then(|assignments| {
            assignments
                .into_iter()
                .find(|assignment| assignment.key == "compression")
        })
        .map(|assignment| {
            json!({
                "key": assignment.key,
                "provider": assignment.provider,
                "model": assignment.model,
                "baseUrlConfigured": !assignment.base_url.trim().is_empty(),
                "apiKeyConfigured": !assignment.api_key.trim().is_empty(),
                "timeoutSeconds": assignment.timeout,
                "source": assignment.source,
                "pluginId": assignment.plugin_id,
            })
        });

    json!({
        "schema": "hermes_context_engine_desktop_v1",
        "status": "ok",
        "action": action,
        "hermesReference": {
            "abstractBase": "agent/context_engine.py",
            "pluginLoader": "plugins/context_engine/__init__.py",
            "defaultEngine": "compressor",
            "configKey": "context.engine",
            "oneActiveEngine": true,
            "selection": "Hermes selects exactly one context engine from config.yaml context.engine; missing config falls back to the built-in ContextCompressor.",
            "loaderPatterns": [
                "plugins/context_engine/<name>/__init__.py with register(ctx)",
                "ContextEngine subclass discovery fallback"
            ],
            "engineLifecycle": [
                "on_session_start",
                "update_from_response",
                "should_compress",
                "compress",
                "on_session_end",
                "on_session_reset"
            ],
            "optionalEngineTools": true
        },
        "discovery": {
            "pluginRoot": plugin_root.to_string_lossy().to_string(),
            "pluginRootExists": plugin_root.is_dir(),
            "bundledPluginEngineCount": discovered.len(),
            "bundledPluginEngines": discovered,
            "currentHermesTreeNote": "The referenced Hermes checkout currently contains only plugins/context_engine/__init__.py, so no child context-engine plugin directories are bundled."
        },
        "activeEngine": {
            "name": active_engine_name,
            "source": if active_engine_is_default { "native_synthchat_context_compression" } else { "plugins/context_engine" },
            "configuredByHermesContextEngineKey": !active_engine_is_default,
            "hermesDefaultEquivalent": active_engine_is_default,
            "oneActiveEngine": true
        },
        "synthChatNativeAdaptation": {
            "shortContextMode": chat.short_context_mode,
            "shortContextTokenBudget": chat.short_context_token_budget,
            "abortOnSummaryFailure": chat.short_context_abort_on_summary_failure,
            "legacySummaryProviderIdConfigured": !chat.short_context_summary_provider_id.trim().is_empty(),
            "legacySummaryModelConfigured": !chat.short_context_summary_model.trim().is_empty(),
            "compressionAssignment": compression_assignment,
            "nativeModules": [
                "agent/context_compression.rs",
                "agent/context_references.rs",
                "agent/prompt_builder.rs",
                "agent/control_commands.rs"
            ],
            "controlCommands": ["/context", "/compact"],
            "promptIntegration": true,
            "toolObservationBudgeting": true
        },
        "commandForwarding": {
            "hermesRegisterCommandForwarding": true,
            "hermesConflictChecks": [
                "built-in slash command conflicts are rejected",
                "regular plugin command conflicts are rejected"
            ],
            "synthChatNativeCommands": ["/context", "/compact"],
            "dynamicPythonContextEngineCommands": true,
            "dynamicCommands": dynamic_commands,
            "dynamicCommandCount": dynamic_command_count,
            "boundary": "Hermes context-engine plugins can register slash commands through _EngineCollector.register_command and the global Python plugin command registry. SynthChat now discovers and dispatches context-engine register(ctx) commands through the same Python plugin bridge used for normal plugin commands."
        },
        "dynamicEngineTools": {
            "dynamicEngineToolSchemas": true,
            "dynamicEngineToolDispatch": true,
            "tools": dynamic_tools,
            "toolCount": dynamic_tool_count,
            "source": "plugins/context_engine/<name>/register(ctx) or ContextEngine subclass get_tool_schemas()/handle_tool_call() through the SynthChat Python plugin bridge"
        },
        "desktopBoundary": {
            "embeddedPythonContextEngineLoader": true,
            "dynamicEngineToolSchemas": true,
            "dynamicEngineToolDispatch": true,
            "nativeCompressorEquivalent": true,
            "runtimePersistence": runtime_contract.clone(),
            "runtime_persistence": runtime_contract,
            "remainingBoundary": "SynthChat has native short-context compression and prompt/reference integration equivalent to Hermes' default compressor path, bridges third-party context-engine commands/tools, lets manual /compact plus preflight/post-turn automation call the selected engine's compress() through the bounded Python bridge, forwards update_model / update_from_response / should_compress lifecycle decisions, dispatches on_session_start / on_session_end / on_session_reset, and provides stable per-engine state directories through HERMES_HOME/context-engine-state plus SYNTHCHAT_CONTEXT_ENGINE_STATE_DIR / HERMES_CONTEXT_ENGINE_STATE_DIR. Remaining parity work is Hermes' long-lived in-process engine object identity rather than bounded helper subprocesses."
        }
    })
}

fn context_engine_runtime_persistence_contract(
    store: &AppStore,
    active_engine_name: &str,
    active_engine_is_default: bool,
) -> Value {
    let state_root = context_engine_state_root(store);
    let state_dir = state_root.join(sanitize_context_engine_state_name(active_engine_name));
    json!({
        "schema": "hermes_context_engine_runtime_persistence_desktop_v1",
        "hermesReferences": [
            "agent/context_engine.py",
            "agent/conversation_compression.py::compress_conversation_context",
            "agent/agent_runtime_helpers.py::restore_primary_runtime",
            "plugins/context_engine/__init__.py::load_context_engine"
        ],
        "hermes_references": [
            "agent/context_engine.py",
            "agent/conversation_compression.py::compress_conversation_context",
            "agent/agent_runtime_helpers.py::restore_primary_runtime",
            "plugins/context_engine/__init__.py::load_context_engine"
        ],
        "activeEngine": active_engine_name,
        "active_engine": active_engine_name,
        "defaultEngine": active_engine_is_default,
        "default_engine": active_engine_is_default,
        "stateRoot": state_root.to_string_lossy().to_string(),
        "state_root": state_root.to_string_lossy().to_string(),
        "stateDir": state_dir.to_string_lossy().to_string(),
        "state_dir": state_dir.to_string_lossy().to_string(),
        "stateDirExists": state_dir.is_dir(),
        "state_dir_exists": state_dir.is_dir(),
        "environmentExports": {
            "HERMES_HOME": context_engine_hermes_home(store).to_string_lossy().to_string(),
            "SYNTHCHAT_CONTEXT_ENGINE_NAME": active_engine_name,
            "HERMES_CONTEXT_ENGINE_NAME": active_engine_name,
            "SYNTHCHAT_CONTEXT_ENGINE_STATE_DIR": state_dir.to_string_lossy().to_string(),
            "HERMES_CONTEXT_ENGINE_STATE_DIR": state_dir.to_string_lossy().to_string()
        },
        "environment_exports": {
            "HERMES_HOME": context_engine_hermes_home(store).to_string_lossy().to_string(),
            "SYNTHCHAT_CONTEXT_ENGINE_NAME": active_engine_name,
            "HERMES_CONTEXT_ENGINE_NAME": active_engine_name,
            "SYNTHCHAT_CONTEXT_ENGINE_STATE_DIR": state_dir.to_string_lossy().to_string(),
            "HERMES_CONTEXT_ENGINE_STATE_DIR": state_dir.to_string_lossy().to_string()
        },
        "boundedHelperSubprocess": true,
        "bounded_helper_subprocess": true,
        "longLivedInProcessPythonObject": false,
        "long_lived_in_process_python_object": false,
        "statusActionAvailable": true,
        "status_action_available": true,
        "stateReloadAcrossInvocations": true,
        "state_reload_across_invocations": true,
        "compressionLockParity": {
            "hermesSessionDbLock": true,
            "synthChatRunSerializesCompression": true,
            "externalProcessLock": false,
            "boundary": "Hermes guards overlapping compression through a SessionDB lock keyed by the old session_id. SynthChat serializes native run compression through its agent-run/session execution path and stable AppStore writes, but bounded helper subprocesses do not share a long-lived Python SessionDB lock object."
        },
        "compression_lock_parity": {
            "hermes_session_db_lock": true,
            "synthchat_run_serializes_compression": true,
            "external_process_lock": false,
            "boundary": "Hermes guards overlapping compression through a SessionDB lock keyed by the old session_id. SynthChat serializes native run compression through its agent-run/session execution path and stable AppStore writes, but bounded helper subprocesses do not share a long-lived Python SessionDB lock object."
        },
        "sessionRotationLifecycle": {
            "hermesBoundaryReasonCompression": true,
            "synthChatSessionLifecycleHooks": true,
            "oldSessionIdForwardedOnCompression": false,
            "boundary": "Hermes calls on_session_start(new_session_id, boundary_reason='compression', old_session_id=...) after DB session rotation. SynthChat forwards session start/end/reset lifecycle events and persists state dirs; it does not currently rotate to a Hermes Python session id inside a long-lived engine object."
        },
        "session_rotation_lifecycle": {
            "hermes_boundary_reason_compression": true,
            "synthchat_session_lifecycle_hooks": true,
            "old_session_id_forwarded_on_compression": false,
            "boundary": "Hermes calls on_session_start(new_session_id, boundary_reason='compression', old_session_id=...) after DB session rotation. SynthChat forwards session start/end/reset lifecycle events and persists state dirs; it does not currently rotate to a Hermes Python session id inside a long-lived engine object."
        },
        "modelRuntimeRestore": {
            "hermesUpdateModelHook": true,
            "synthChatUsageForwarding": true,
            "synthChatProviderRebuildNative": true,
            "updateModelForwardedToDynamicEngine": true,
            "apiKeyForwarded": false,
            "boundary": "Hermes restores the primary runtime and calls context_compressor.update_model(...) on the live object. SynthChat forwards non-secret model/provider/context-length/base-url metadata to the selected dynamic engine through the bounded helper before update_from_response; it still does not keep a live Python object identity across turns."
        },
        "model_runtime_restore": {
            "hermes_update_model_hook": true,
            "synthchat_usage_forwarding": true,
            "synthchat_provider_rebuild_native": true,
            "update_model_forwarded_to_dynamic_engine": true,
            "api_key_forwarded": false,
            "boundary": "Hermes restores the primary runtime and calls context_compressor.update_model(...) on the live object. SynthChat forwards non-secret model/provider/context-length/base-url metadata to the selected dynamic engine through the bounded helper before update_from_response; it still does not keep a live Python object identity across turns."
        },
        "remainingBoundary": "The remaining strict parity gap is not discovery, command/tool dispatch, compression, lifecycle calls, model metadata forwarding, or state persistence. It is Hermes' single long-lived in-process Python ContextEngine object identity with direct session-lock/session-rotation ownership.",
        "remaining_boundary": "The remaining strict parity gap is not discovery, command/tool dispatch, compression, lifecycle calls, model metadata forwarding, or state persistence. It is Hermes' single long-lived in-process Python ContextEngine object identity with direct session-lock/session-rotation ownership."
    })
}

fn context_engine_hermes_home(store: &AppStore) -> PathBuf {
    env::var_os("HERMES_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| store.data_dir().join(".hermes"))
}

fn context_engine_state_root(store: &AppStore) -> PathBuf {
    env::var_os("SYNTHCHAT_CONTEXT_ENGINE_STATE_ROOT")
        .or_else(|| env::var_os("HERMES_CONTEXT_ENGINE_STATE_ROOT"))
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| context_engine_hermes_home(store).join("context-engine-state"))
}

fn sanitize_context_engine_state_name(name: &str) -> String {
    let cleaned = name
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    if cleaned.trim_matches('_').is_empty() {
        "context_engine".into()
    } else {
        cleaned
    }
}

fn active_context_engine_name() -> String {
    env::var("SYNTHCHAT_CONTEXT_ENGINE")
        .or_else(|_| env::var("HERMES_CONTEXT_ENGINE"))
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "compressor".into())
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

fn discover_context_engine_plugin_dirs(plugin_root: &PathBuf) -> Vec<Value> {
    let Ok(entries) = fs::read_dir(plugin_root) else {
        return Vec::new();
    };
    let mut engines = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if !path.is_dir() || name.starts_with('_') || name.starts_with('.') {
                return None;
            }
            let init_path = path.join("__init__.py");
            if !init_path.is_file() {
                return None;
            }
            let manifest_path = path.join("plugin.yaml");
            Some(json!({
                "name": name,
                "path": path.to_string_lossy().to_string(),
                "initPath": init_path.to_string_lossy().to_string(),
                "manifestPath": manifest_path.to_string_lossy().to_string(),
                "manifestExists": manifest_path.is_file(),
                "availabilityCheckedByImport": false,
                "available": null,
                "boundary": "SynthChat lists Hermes context-engine plugin directories without importing Python or calling is_available()."
            }))
        })
        .collect::<Vec<_>>();
    engines.sort_by(|left, right| {
        left.get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .cmp(
                right
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            )
    });
    engines
}
