use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::{error::AppResult, models::PluginSummary, store::AppStore};

use super::{
    list_python_plugin_auxiliary_tasks, list_python_plugin_commands, list_python_plugin_skills,
    list_python_plugin_tools,
};

pub(super) fn plugin_runtime_tool(store: &AppStore, payload: &Value) -> AppResult<String> {
    let action = payload
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("status")
        .trim()
        .to_ascii_lowercase();
    if !matches!(
        action.as_str(),
        "status"
            | "sources"
            | "registries"
            | "commands"
            | "tools"
            | "hooks"
            | "auxiliary"
            | "diagnostics"
    ) {
        return Ok(serde_json::to_string_pretty(&json!({
            "schema": "hermes_plugin_runtime_desktop_v1",
            "status": "unsupported_action",
            "supportedActions": [
                "status",
                "sources",
                "registries",
                "commands",
                "tools",
                "hooks",
                "auxiliary",
                "diagnostics"
            ],
        }))?);
    }

    Ok(serde_json::to_string_pretty(&plugin_runtime_snapshot(
        store, &action,
    ))?)
}

pub(super) fn plugin_runtime_snapshot(store: &AppStore, action: &str) -> Value {
    let (plugins_ok, plugins_error, plugins) = match crate::plugins::list_plugins(store) {
        Ok(plugins) => (true, None, plugins),
        Err(error) => (false, Some(error.to_string()), Vec::new()),
    };
    let enabled_plugins = plugins
        .iter()
        .filter(|plugin| plugin.enabled)
        .cloned()
        .collect::<Vec<_>>();

    let tools = discover_tools(store);
    let skills = discover_skills(store);
    let commands = discover_commands(store);
    let auxiliary = discover_auxiliary_tasks(store);

    json!({
        "schema": "hermes_plugin_runtime_desktop_v1",
        "status": "ok",
        "action": action,
        "hermesReference": {
            "manager": "hermes_cli/plugins.py::PluginManager",
            "pluginContextSurface": [
                "register_tool",
                "register_command",
                "run_agent_tool",
                "register_context_engine",
                "register_memory_provider",
                "register_model_provider",
                "register_web_provider",
                "register_browser_provider",
                "register_platform",
                "register_auxiliary_task",
                "register_hook",
                "register_skill",
                "llm"
            ],
            "sources": [
                {"name": "bundled", "path": "<hermes-agent>/plugins", "autoLoadKinds": ["backend", "platform"]},
                {"name": "user", "path": "~/.hermes/plugins", "requiresEnabledList": true},
                {"name": "project", "path": "./.hermes/plugins", "gatedBy": "HERMES_ENABLE_PROJECT_PLUGINS"},
                {"name": "entry_points", "group": "hermes_agent.plugins", "requiresEnabledList": true}
            ],
            "loadRules": {
                "bundledBackendAndPlatformAutoLoad": true,
                "standaloneUserAndEntryPointRequirePluginsEnabled": true,
                "pluginsDisabledDenyListWins": true,
                "sourceCollision": "later loaded plugin source replaces earlier registration",
                "specialTopLevelCategories": ["memory", "context_engine", "platforms", "model-providers"]
            },
            "registries": [
                "_plugin_tool_names",
                "_plugin_platform_names",
                "_cli_commands",
                "_plugin_commands",
                "_plugin_skills",
                "_aux_tasks",
                "_context_engine",
                "_hooks"
            ]
        },
        "synthChatNativeAdaptation": {
            "pluginDiscovery": {
                "status": if plugins_ok { "ok" } else { "error" },
                "error": plugins_error,
                "total": plugins.len(),
                "enabled": enabled_plugins.len(),
                "bySource": count_by_key(&plugins, |plugin| plugin.source.as_str()),
                "byKind": count_by_key(&plugins, |plugin| plugin.kind.as_str()),
                "plugins": plugins.iter().map(plugin_snapshot).collect::<Vec<_>>()
            },
            "pythonBridgeRegistries": {
                "tools": discovery_status(&tools),
                "commands": discovery_status(&commands),
                "skills": discovery_status(&skills),
                "auxiliaryTasks": discovery_status(&auxiliary),
                "hookManifests": manifest_hook_status(&enabled_plugins)
            },
            "toolIntegration": {
                "plannerPromptAddsDynamicPluginTools": true,
                "dispatchRunsDynamicPluginTools": true,
                "bridgeApprovalContext": true,
                "pluginSlashCommands": true,
                "pluginSkills": true,
                "pluginAuxiliaryTasks": true
            },
            "currentRuntime": {
                "tools": tools.value,
                "commands": commands.value,
                "skills": skills.value,
                "auxiliaryTasks": auxiliary.value
            }
        },
        "boundary": {
            "executesPluginTools": true,
            "executesPluginCommands": true,
            "executesPluginHooks": true,
            "executesContextEngineCommands": true,
            "executesContextEngineTools": true,
            "importsHermesPluginManager": false,
            "embedsHermesPythonDaemon": false,
            "providerCategoriesHaveDedicatedStatusTools": [
                "memory_provider",
                "provider_plugins",
                "browser_plugins",
                "context_engine",
                "dashboard_plugins",
                "spotify_status"
            ],
            "boundary": "SynthChat adapts Hermes plugin discovery plus dynamic Python tool, command, skill, auxiliary-task, hook, and context-engine command/tool bridges in Rust/Tauri. It executes those bridges through bounded Python helper subprocesses with SynthChat approval/tool routing where required, but it still does not claim byte-for-byte PluginManager embedding or a long-lived Hermes Python daemon."
        }
    })
}

#[derive(Clone)]
struct DiscoverySnapshot {
    ok: bool,
    error: Option<String>,
    value: Value,
    count: usize,
}

fn discover_tools(store: &AppStore) -> DiscoverySnapshot {
    match list_python_plugin_tools(store) {
        Ok(tools) => {
            let count = tools.len();
            DiscoverySnapshot {
                ok: true,
                error: None,
                value: json!(tools
                    .into_iter()
                    .map(|tool| json!({
                        "pluginId": tool.plugin_id,
                        "pluginName": tool.plugin_name,
                        "name": tool.name,
                        "toolset": tool.toolset,
                        "description": tool.description,
                        "schema": tool.schema
                    }))
                    .collect::<Vec<_>>()),
                count,
            }
        }
        Err(error) => discovery_error(error.to_string()),
    }
}

fn discover_skills(store: &AppStore) -> DiscoverySnapshot {
    match list_python_plugin_skills(store) {
        Ok(skills) => {
            let count = skills.len();
            DiscoverySnapshot {
                ok: true,
                error: None,
                value: json!(skills
                    .into_iter()
                    .map(|skill| json!({
                        "pluginId": skill.plugin_id,
                        "pluginName": skill.plugin_name,
                        "name": skill.name,
                        "path": skill.path.to_string_lossy().to_string(),
                        "description": skill.description
                    }))
                    .collect::<Vec<_>>()),
                count,
            }
        }
        Err(error) => discovery_error(error.to_string()),
    }
}

fn discover_commands(store: &AppStore) -> DiscoverySnapshot {
    match list_python_plugin_commands(store) {
        Ok(commands) => {
            let count = commands.len();
            DiscoverySnapshot {
                ok: true,
                error: None,
                value: json!(commands
                    .into_iter()
                    .map(|command| json!({
                        "pluginId": command.plugin_id,
                        "pluginName": command.plugin_name,
                        "name": command.name,
                        "description": command.description,
                        "argsHint": command.args_hint
                    }))
                    .collect::<Vec<_>>()),
                count,
            }
        }
        Err(error) => discovery_error(error.to_string()),
    }
}

fn discover_auxiliary_tasks(store: &AppStore) -> DiscoverySnapshot {
    match list_python_plugin_auxiliary_tasks(store) {
        Ok(tasks) => {
            let count = tasks.len();
            DiscoverySnapshot {
                ok: true,
                error: None,
                value: json!(tasks
                    .into_iter()
                    .map(|task| json!({
                        "pluginId": task.plugin_id,
                        "pluginName": task.plugin_name,
                        "key": task.key,
                        "displayName": task.display_name,
                        "description": task.description,
                        "defaults": task.defaults
                    }))
                    .collect::<Vec<_>>()),
                count,
            }
        }
        Err(error) => discovery_error(error.to_string()),
    }
}

fn discovery_error(error: String) -> DiscoverySnapshot {
    DiscoverySnapshot {
        ok: false,
        error: Some(error),
        value: json!([]),
        count: 0,
    }
}

fn discovery_status(snapshot: &DiscoverySnapshot) -> Value {
    json!({
        "ok": snapshot.ok,
        "error": snapshot.error,
        "count": snapshot.count,
        "discoveryRunnerExecuted": true,
        "pluginToolsOrHooksExecuted": false
    })
}

fn plugin_snapshot(plugin: &PluginSummary) -> Value {
    json!({
        "id": plugin.id,
        "name": plugin.name,
        "kind": plugin.kind,
        "source": plugin.source,
        "enabled": plugin.enabled,
        "envConfigured": plugin.env_configured,
        "missingEnv": plugin.missing_env,
        "providedTools": plugin.provided_tools,
        "providedCapabilities": plugin.provided_capabilities,
        "providedHooks": plugin.provided_hooks,
        "path": plugin.path,
        "manifestPath": plugin.manifest_path,
        "entryPoint": plugin.entry_point
    })
}

fn manifest_hook_status(plugins: &[PluginSummary]) -> Value {
    let hooks = plugins
        .iter()
        .flat_map(|plugin| {
            plugin.provided_hooks.iter().map(|hook| {
                json!({
                    "pluginId": plugin.id,
                    "pluginName": plugin.name,
                    "hook": hook
                })
            })
        })
        .collect::<Vec<_>>();
    json!({
        "count": hooks.len(),
        "hooks": hooks,
        "manifestOnly": true,
        "runtimeCallbacksExecuted": false
    })
}

fn count_by_key<F>(plugins: &[PluginSummary], key: F) -> BTreeMap<String, usize>
where
    F: Fn(&PluginSummary) -> &str,
{
    let mut counts = BTreeMap::new();
    for plugin in plugins {
        let clean = key(plugin).trim();
        let name = if clean.is_empty() { "unknown" } else { clean };
        *counts.entry(name.to_string()).or_insert(0) += 1;
    }
    counts
}
