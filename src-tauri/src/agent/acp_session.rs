use std::{
    collections::{HashMap, HashSet},
    path::Path,
    sync::{Mutex, OnceLock},
};

use serde_json::{json, Value};

use crate::{
    error::{AppError, AppResult},
    models::{
        AgentDefinition, AgentRunRecord, ChatMessage, Conversation, LlmProvider, McpServer,
        ShortContextState, ToolDefinition,
    },
    store::AppStore,
};

use super::{
    acp_history::acp_session_history_updates_for_store,
    acp_queue::acp_queue_update_notification,
    acp_server::{AcpListSessionsResponse, AcpSessionInfo},
    shell_hooks::{list_python_plugin_commands, spawn_session_finished_hooks},
};

static ACP_SESSION_RUNTIME_CONFIG: OnceLock<Mutex<HashMap<String, AcpSessionRuntimeConfig>>> =
    OnceLock::new();
const ACP_RUNTIME_CONFIG_METADATA_KEY: &str = "acpRuntimeConfig";
const ACP_INTERRUPTED_PROMPT_METADATA_KEY: &str = "acpInterruptedPromptText";
const ACP_LIST_SESSIONS_PAGE_SIZE: usize = 50;
pub(super) const ACP_MODE_DEFAULT: &str = "default";
pub(super) const ACP_MODE_ACCEPT_EDITS: &str = "accept_edits";
pub(super) const ACP_MODE_DONT_ASK: &str = "dont_ask";

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct AcpSessionRuntimeConfig {
    #[serde(default)]
    pub(super) provider: Option<String>,
    #[serde(default)]
    pub(super) model: Option<String>,
    #[serde(default)]
    pub(super) mode: Option<String>,
    #[serde(default)]
    pub(super) config_options: HashMap<String, Value>,
    #[serde(default)]
    pub(super) mcp_servers: Vec<Value>,
}

pub(super) fn acp_update_session_runtime_config(
    store: &AppStore,
    session_id: &str,
    update: impl FnOnce(&mut AcpSessionRuntimeConfig),
) -> AppResult<()> {
    let configs = ACP_SESSION_RUNTIME_CONFIG.get_or_init(|| Mutex::new(HashMap::new()));
    let mut configs = configs
        .lock()
        .map_err(|_| AppError::BadRequest("ACP session runtime config lock poisoned".into()))?;
    let runtime = configs.entry(session_id.to_string()).or_default();
    update(runtime);
    store.set_conversation_metadata_value(
        session_id,
        ACP_RUNTIME_CONFIG_METADATA_KEY,
        serde_json::to_value(runtime.clone())?,
    )?;
    Ok(())
}

fn acp_session_runtime_config(session_id: &str) -> Option<AcpSessionRuntimeConfig> {
    ACP_SESSION_RUNTIME_CONFIG
        .get()
        .and_then(|configs| configs.lock().ok()?.get(session_id).cloned())
}

pub(super) fn acp_session_runtime_config_for_store(
    store: &AppStore,
    session_id: &str,
) -> AppResult<Option<AcpSessionRuntimeConfig>> {
    if let Some(runtime) = acp_session_runtime_config(session_id) {
        return Ok(Some(runtime));
    }
    let conversation = match store.conversation(session_id) {
        Ok(conversation) => conversation,
        Err(_) => return Ok(None),
    };
    let Some(value) = conversation.metadata.get(ACP_RUNTIME_CONFIG_METADATA_KEY) else {
        return Ok(None);
    };
    let runtime: AcpSessionRuntimeConfig = serde_json::from_value(value.clone())?;
    let configs = ACP_SESSION_RUNTIME_CONFIG.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(mut configs) = configs.lock() {
        configs.insert(session_id.to_string(), runtime.clone());
    }
    Ok(Some(runtime))
}

pub(super) fn acp_list_sessions_for_store(
    store: &AppStore,
    cwd: Option<&str>,
    cursor: Option<&str>,
) -> AppResult<AcpListSessionsResponse> {
    let cwd_filter = cwd
        .map(normalize_acp_session_cwd_for_compare)
        .filter(|value| !value.is_empty());
    let agents = store
        .agents()?
        .into_iter()
        .map(|agent| (agent.id.clone(), agent))
        .collect::<HashMap<_, _>>();
    let runs = store.agent_runs()?;
    let mut sessions = Vec::new();

    for conversation in store.conversations()? {
        let messages = store.messages(&conversation.id, None)?;
        if messages.is_empty() {
            continue;
        }
        let agent = agents
            .get(&conversation.agent_id)
            .or_else(|| agents.get("default"));
        let runtime = acp_session_runtime_config_for_store(store, &conversation.id)?;
        let session_cwd = runtime
            .as_ref()
            .and_then(|runtime| runtime.config_options.get("cwd"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .or_else(|| {
                agent
                    .map(|agent| agent.workspace_dir.trim())
                    .filter(|value| !value.is_empty())
            })
            .unwrap_or(".");
        let model = acp_session_model_id(runtime.as_ref(), agent);
        let session_cwd = session_cwd.to_string();
        if cwd_filter
            .as_ref()
            .is_some_and(|wanted| normalize_acp_session_cwd_for_compare(&session_cwd) != *wanted)
        {
            continue;
        }
        let latest_run = latest_run_record_for_session(&runs, &conversation.id);
        sessions.push(acp_session_info_from_conversation(
            &conversation,
            &messages,
            latest_run,
            &session_cwd,
            &model,
        ));
    }

    sessions.sort_by(|a, b| {
        b.updated_at
            .cmp(&a.updated_at)
            .then_with(|| a.session_id.cmp(&b.session_id))
    });

    if let Some(cursor) = cursor.map(str::trim).filter(|value| !value.is_empty()) {
        if let Some(idx) = sessions
            .iter()
            .position(|session| session.session_id == cursor)
        {
            sessions = sessions.split_off(idx + 1);
        } else {
            sessions.clear();
        }
    }

    let has_more = sessions.len() > ACP_LIST_SESSIONS_PAGE_SIZE;
    sessions.truncate(ACP_LIST_SESSIONS_PAGE_SIZE);
    let next_cursor = if has_more {
        sessions.last().map(|session| session.session_id.clone())
    } else {
        None
    };

    Ok(AcpListSessionsResponse {
        sessions,
        next_cursor,
    })
}

pub(super) fn acp_session_info_update_for_store(
    store: &AppStore,
    session_id: &str,
) -> AppResult<Option<Value>> {
    let conversation = match store.conversation(session_id) {
        Ok(conversation) => conversation,
        Err(_) => return Ok(None),
    };
    let messages = store.messages(session_id, None)?;
    let runs = store.agent_runs()?;
    let latest_run = latest_run_record_for_session(&runs, session_id);
    let agent = store.agent(Some(&conversation.agent_id)).ok();
    let runtime = acp_session_runtime_config_for_store(store, session_id)?;
    let cwd = runtime
        .as_ref()
        .and_then(|runtime| runtime.config_options.get("cwd"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or_else(|| {
            agent
                .as_ref()
                .map(|agent| agent.workspace_dir.trim())
                .filter(|value| !value.is_empty())
        })
        .unwrap_or(".");
    let model = acp_session_model_id(runtime.as_ref(), agent.as_ref());
    let info =
        acp_session_info_from_conversation(&conversation, &messages, latest_run, cwd, &model);
    Ok(Some(json!({
        "sessionUpdate": "session_info_update",
        "sessionId": info.session_id,
        "cwd": info.cwd,
        "title": info.title,
        "updatedAt": info.updated_at,
        "model": info.model,
        "historyLen": info.history_len
    })))
}

pub(super) fn acp_session_history_notifications(
    store: &AppStore,
    session_id: &str,
) -> AppResult<Vec<Value>> {
    let updates = match acp_session_history_updates_for_store(store, session_id) {
        Ok(updates) => updates,
        Err(_) => return Ok(Vec::new()),
    };
    Ok(updates
        .into_iter()
        .map(|update| acp_session_update_notification(session_id, update))
        .collect())
}

pub(super) fn acp_session_status_notifications_for_result(
    store: &AppStore,
    result: &Value,
) -> AppResult<Vec<Value>> {
    let session_id = result
        .get("sessionId")
        .and_then(Value::as_str)
        .unwrap_or("");
    if session_id.is_empty() {
        return Ok(Vec::new());
    }
    acp_session_status_notifications(store, session_id)
}

pub(super) fn acp_session_status_notifications_for_params(
    store: &AppStore,
    params: &Value,
) -> AppResult<Vec<Value>> {
    let session_id = acp_session_id_from_params(params);
    if session_id.is_empty() || store.conversation(&session_id).is_err() {
        return Ok(Vec::new());
    }
    acp_session_status_notifications(store, &session_id)
}

pub(super) fn acp_session_status_notifications(
    store: &AppStore,
    session_id: &str,
) -> AppResult<Vec<Value>> {
    let mut notifications = Vec::new();
    notifications.extend(acp_session_info_notification(store, session_id)?);
    notifications.extend(acp_available_commands_notification(store, session_id));
    notifications.extend(acp_usage_notification(store, session_id)?);
    Ok(notifications)
}

pub(super) fn acp_session_info_notification(
    store: &AppStore,
    session_id: &str,
) -> AppResult<Vec<Value>> {
    let Some(update) = acp_session_info_update_for_store(store, session_id)? else {
        return Ok(Vec::new());
    };
    Ok(vec![acp_session_update_notification(session_id, update)])
}

fn acp_available_commands_notification(store: &AppStore, session_id: &str) -> Vec<Value> {
    vec![acp_session_update_notification(
        session_id,
        acp_available_commands_update_for_store(store),
    )]
}

pub(super) fn acp_available_commands_update() -> Value {
    acp_available_commands_update_from_specs(acp_advertised_command_specs_owned())
}

pub(super) fn acp_available_commands_update_for_store(store: &AppStore) -> Value {
    let mut specs = acp_advertised_command_specs_owned();
    let mut used = specs
        .iter()
        .map(|(name, _, _)| (*name).to_string())
        .collect::<HashSet<_>>();

    if let Ok(mut plugin_commands) = list_python_plugin_commands(store) {
        plugin_commands.sort_by(|left, right| left.name.cmp(&right.name));
        for command in plugin_commands {
            let name = sanitize_acp_dynamic_command_name(&command.name);
            if name.is_empty() || !used.insert(name.clone()) {
                continue;
            }
            let description = if command.description.trim().is_empty() {
                format!("Plugin command from {}", command.plugin_name)
            } else {
                command.description
            };
            let hint = (!command.args_hint.trim().is_empty()).then(|| command.args_hint);
            specs.push((
                name,
                truncate_acp_command_description(&description),
                hint.map(|hint| hint.trim().to_string()),
            ));
        }
    }

    if let Ok(mut skills) = crate::skills::list_skills(store) {
        skills.sort_by(|left, right| {
            left.name
                .cmp(&right.name)
                .then_with(|| left.id.cmp(&right.id))
        });
        for skill in skills {
            let name = sanitize_acp_dynamic_command_name(&skill.name);
            if name.is_empty() || !used.insert(name.clone()) {
                continue;
            }
            let description = if skill.description.trim().is_empty() {
                format!("Invoke skill {}", skill.name)
            } else {
                skill.description
            };
            specs.push((
                name,
                truncate_acp_command_description(&description),
                Some("instruction for the skill".into()),
            ));
        }
    }

    if let Ok(mut bundles) = crate::skills::list_skill_bundles(store) {
        bundles.sort_by(|left, right| left.id.cmp(&right.id));
        for bundle in bundles {
            let name = sanitize_acp_dynamic_command_name(&bundle.id);
            if name.is_empty() || !used.insert(name.clone()) {
                continue;
            }
            let description = if bundle.description.trim().is_empty() {
                format!("Invoke skill bundle {}", bundle.name)
            } else {
                bundle.description
            };
            specs.push((
                name,
                truncate_acp_command_description(&description),
                Some("instruction for the skill bundle".into()),
            ));
        }
    }

    acp_available_commands_update_from_specs(specs)
}

fn acp_available_commands_update_from_specs(specs: Vec<(String, String, Option<String>)>) -> Value {
    let available_commands = specs
        .into_iter()
        .map(|(name, description, hint)| {
            let mut value = json!({
                "name": name,
                "description": description
            });
            if let Some(hint) = hint {
                value["input"] = json!({
                    "root": {
                        "hint": hint
                    }
                });
            }
            value
        })
        .collect::<Vec<_>>();
    json!({
        "sessionUpdate": "available_commands_update",
        "availableCommands": available_commands
    })
}

fn acp_advertised_command_specs_owned() -> Vec<(String, String, Option<String>)> {
    acp_advertised_command_specs()
        .into_iter()
        .map(|(name, description, hint)| {
            (
                name.to_string(),
                description.to_string(),
                hint.map(str::to_string),
            )
        })
        .collect()
}

fn sanitize_acp_dynamic_command_name(value: &str) -> String {
    let mut normalized = String::new();
    let mut previous_hyphen = false;
    for ch in value
        .trim()
        .trim_start_matches('/')
        .trim_start_matches('／')
        .to_lowercase()
        .chars()
    {
        let mapped = if ch.is_ascii_alphanumeric() {
            Some(ch)
        } else if ch == ' ' || ch == '_' || ch == '-' || ch == '/' {
            Some('-')
        } else {
            None
        };
        let Some(ch) = mapped else {
            continue;
        };
        if ch == '-' {
            if normalized.is_empty() || previous_hyphen {
                continue;
            }
            previous_hyphen = true;
        } else {
            previous_hyphen = false;
        }
        normalized.push(ch);
    }
    normalized.trim_matches('-').to_string()
}

fn truncate_acp_command_description(value: &str) -> String {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() <= 100 {
        return normalized;
    }
    let mut truncated = normalized.chars().take(97).collect::<String>();
    truncated.push_str("...");
    truncated
}

pub(super) fn acp_advertised_command_specs(
) -> Vec<(&'static str, &'static str, Option<&'static str>)> {
    vec![
        ("help", "List available commands", None),
        (
            "model",
            "Show current model and provider, or switch models",
            Some("model name to switch to"),
        ),
        ("tools", "List available tools with descriptions", None),
        ("context", "Show conversation message counts by role", None),
        ("reset", "Clear conversation history", None),
        ("compact", "Compress conversation context", None),
        (
            "steer",
            "Inject guidance into the currently running agent turn",
            Some("guidance for the active turn"),
        ),
        (
            "queue",
            "Queue a prompt to run after the current turn finishes",
            Some("prompt to run next"),
        ),
        ("version", "Show SynthChat version", None),
    ]
}

pub(super) fn acp_usage_notification(store: &AppStore, session_id: &str) -> AppResult<Vec<Value>> {
    let Some(update) = acp_usage_update_for_store(store, session_id)? else {
        return Ok(Vec::new());
    };
    Ok(vec![acp_session_update_notification(session_id, update)])
}

pub(super) fn acp_usage_update_for_store(
    store: &AppStore,
    session_id: &str,
) -> AppResult<Option<Value>> {
    let size = store.config()?.chat.short_context_token_budget as u64;
    if size == 0 {
        return Ok(None);
    }
    let short_context = store.short_context(session_id).ok();
    let session_used = short_context
        .as_ref()
        .map(|context| context.last_real_prompt_tokens as u64)
        .unwrap_or(0);
    let used = if session_used > 0 {
        session_used
    } else {
        acp_estimate_session_context_tokens(store, session_id, short_context.as_ref())? as u64
    };
    Ok(Some(json!({
        "sessionUpdate": "usage_update",
        "size": size,
        "used": used
    })))
}

pub(super) fn acp_estimate_session_context_tokens(
    store: &AppStore,
    session_id: &str,
    short_context: Option<&ShortContextState>,
) -> AppResult<usize> {
    let messages = store.messages(session_id, None)?;
    let transcript = messages
        .iter()
        .map(|message| format!("{}: {}", message.role, message.content))
        .collect::<Vec<_>>()
        .join("\n");
    let summary = short_context
        .map(|context| context.summary.as_str())
        .unwrap_or("");
    Ok(super::estimate_tokens(&format!("{summary}\n{transcript}")))
}

fn acp_session_update_notification(session_id: &str, update: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": {
            "sessionId": session_id,
            "update": update
        }
    })
}

pub(super) fn acp_session_id_from_params(params: &Value) -> String {
    params
        .get("sessionId")
        .or_else(|| params.get("session_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("")
        .to_string()
}

pub(super) fn acp_mcp_servers_from_params(params: &Value) -> Option<Vec<Value>> {
    let items = params
        .get("mcpServers")
        .or_else(|| params.get("mcp_servers"))?
        .as_array()?;
    Some(
        items
            .iter()
            .filter_map(acp_normalize_mcp_server_param)
            .collect(),
    )
}

fn acp_normalize_mcp_server_param(server: &Value) -> Option<Value> {
    if !server.is_object() {
        return None;
    }
    let name = acp_session_string_text(server, &["name", "id"]);
    if name.is_empty() {
        return None;
    }
    let command = acp_session_string_text(server, &["command"]);
    let url = acp_session_string_text(server, &["url"]);
    if command.is_empty() && url.is_empty() {
        return None;
    }
    let mut normalized = json!({ "name": name });
    if !command.is_empty() {
        normalized["command"] = Value::String(command);
        normalized["args"] = server
            .get("args")
            .and_then(Value::as_array)
            .map(|items| {
                Value::Array(
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .map(|value| Value::String(value.to_string()))
                        .collect(),
                )
            })
            .unwrap_or_else(|| json!([]));
        normalized["env"] = acp_named_values_array(server.get("env"));
    } else {
        normalized["url"] = Value::String(url);
        normalized["headers"] = acp_named_values_array(server.get("headers"));
    }
    Some(normalized)
}

fn acp_named_values_array(value: Option<&Value>) -> Value {
    let Some(value) = value else {
        return json!([]);
    };
    if let Some(object) = value.as_object() {
        return Value::Array(
            object
                .iter()
                .map(|(name, value)| {
                    json!({
                        "name": name,
                        "value": value.as_str().unwrap_or("").to_string()
                    })
                })
                .collect(),
        );
    }
    Value::Array(
        value
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|item| {
                let name = acp_session_string_text(item, &["name"]);
                if name.is_empty() {
                    return None;
                }
                Some(json!({
                    "name": name,
                    "value": acp_session_string_text(item, &["value"])
                }))
            })
            .collect(),
    )
}

pub(super) fn acp_register_session_mcp_servers(
    store: &AppStore,
    session_id: &str,
) -> AppResult<()> {
    let prefix = acp_session_mcp_server_id_prefix(session_id);
    let runtime = acp_session_runtime_config_for_store(store, session_id)?;
    let synth_servers = runtime
        .as_ref()
        .map(|runtime| {
            runtime
                .mcp_servers
                .iter()
                .filter_map(|server| acp_mcp_server_to_synth_server(session_id, server))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let mut servers = store.static_list("mcpServers")?;
    servers.retain(|server| {
        server
            .get("id")
            .and_then(Value::as_str)
            .is_none_or(|id| !id.starts_with(&prefix))
    });
    servers.extend(
        synth_servers
            .iter()
            .map(|server| serde_json::to_value(server).unwrap_or(Value::Null)),
    );
    store.set_mcp_servers(servers)?;

    let mut definitions = store.tool_definitions()?;
    definitions.retain(|definition| !definition.server_id.starts_with(&prefix));
    for server in &synth_servers {
        definitions.extend(acp_mcp_utility_tool_definitions(server));
    }
    store.set_tool_definitions(definitions)?;
    Ok(())
}

pub(super) fn acp_server_new_session(store: &AppStore, params: &Value) -> AppResult<Value> {
    let cwd = params
        .get("cwd")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(".");
    let persona = store.persona(None)?;
    let conversation =
        store.create_conversation(Some(acp_new_session_title_from_cwd(cwd)), Some(persona.id))?;
    acp_update_session_runtime_config(store, &conversation.id, |runtime| {
        runtime
            .config_options
            .insert("cwd".into(), Value::String(cwd.to_string()));
        if let Some(mcp_servers) = acp_mcp_servers_from_params(params) {
            runtime.mcp_servers = mcp_servers;
        }
    })?;
    acp_register_session_mcp_servers(store, &conversation.id)?;
    acp_server_session_response(store, &conversation.id)
}

pub(super) fn acp_server_load_session(
    store: &AppStore,
    params: &Value,
) -> AppResult<(Value, Vec<Value>)> {
    let session_id = acp_session_id_from_params(params);
    if session_id.is_empty() || store.conversation(&session_id).is_err() {
        return Ok((Value::Null, Vec::new()));
    }
    acp_apply_session_params_to_runtime(store, &session_id, params)?;
    let mut notifications = acp_session_history_notifications(store, &session_id)?;
    notifications.extend(acp_session_status_notifications(store, &session_id)?);
    Ok((
        acp_server_session_response(store, &session_id)?,
        notifications,
    ))
}

pub(super) fn acp_server_resume_session(
    store: &AppStore,
    params: &Value,
) -> AppResult<(Value, Vec<Value>)> {
    let mut session_id = acp_session_id_from_params(params);
    if session_id.is_empty() || store.conversation(&session_id).is_err() {
        let persona = store.persona(None)?;
        let conversation =
            store.create_conversation(Some("ACP Session".into()), Some(persona.id))?;
        session_id = conversation.id;
    }
    acp_apply_session_params_to_runtime(store, &session_id, params)?;
    let mut notifications = acp_session_history_notifications(store, &session_id)?;
    notifications.extend(acp_session_status_notifications(store, &session_id)?);
    Ok((
        acp_server_session_response(store, &session_id)?,
        notifications,
    ))
}

pub(super) fn acp_server_cancel_session(
    store: &AppStore,
    params: &Value,
) -> AppResult<(Value, Vec<Value>)> {
    let session_id = acp_session_id_from_params(params);
    let mut cancelled_queue_items = Vec::new();
    if !session_id.is_empty() {
        if let Some(active) = store.active_agent_run_for_conversation(&session_id)? {
            if !active.user_request.trim().is_empty() {
                let _ = store.set_conversation_metadata_value(
                    &session_id,
                    ACP_INTERRUPTED_PROMPT_METADATA_KEY,
                    Value::String(active.user_request.clone()),
                )?;
            }
            let aborted = store.abort_agent_run(
                &active.run_id,
                Some("ACP session cancelled by client.".into()),
            )?;
            spawn_session_finished_hooks(
                store,
                aborted,
                json!({
                    "source": "acp_session_cancel",
                    "reason": "ACP session cancelled by client.",
                }),
            );
        }
        for item in store.agent_queue()?.into_iter().filter(|item| {
            item.conversation_id == session_id
                && matches!(item.status.as_str(), "pending" | "running")
        }) {
            cancelled_queue_items.push(store.cancel_agent_queue_item(&item.id)?);
        }
    }
    let notifications = cancelled_queue_items
        .iter()
        .map(|item| acp_queue_update_notification(&session_id, item, None, 0, None))
        .collect();
    Ok((Value::Null, notifications))
}

pub(super) fn acp_take_interrupted_prompt_text(
    store: &AppStore,
    session_id: &str,
) -> AppResult<Option<String>> {
    let conversation = match store.conversation(session_id) {
        Ok(conversation) => conversation,
        Err(_) => return Ok(None),
    };
    let text = conversation
        .metadata
        .get(ACP_INTERRUPTED_PROMPT_METADATA_KEY)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    if text.is_some() {
        store.set_conversation_metadata_value(
            session_id,
            ACP_INTERRUPTED_PROMPT_METADATA_KEY,
            Value::Null,
        )?;
    }
    Ok(text)
}

pub(super) fn acp_server_set_session_model(store: &AppStore, params: &Value) -> AppResult<Value> {
    let session_id = acp_session_id_from_params(params);
    let model_id = acp_session_string_text(params, &["modelId", "model_id", "model"]);
    if session_id.is_empty() || model_id.is_empty() || store.conversation(&session_id).is_err() {
        return Ok(Value::Null);
    }
    let (provider, model) = acp_model_selection_from_id(&model_id);
    acp_update_session_runtime_config(store, &session_id, |runtime| {
        if provider.is_some() {
            runtime.provider = provider;
        }
        runtime.model = Some(model);
    })?;
    Ok(json!({}))
}

pub(super) fn acp_server_set_session_mode(store: &AppStore, params: &Value) -> AppResult<Value> {
    let session_id = acp_session_id_from_params(params);
    if session_id.is_empty() || store.conversation(&session_id).is_err() {
        return Ok(Value::Null);
    }
    let mode = acp_normalize_session_mode(&acp_session_string_text(
        params,
        &["modeId", "mode_id", "mode"],
    ));
    acp_update_session_runtime_config(store, &session_id, |runtime| {
        runtime.mode = Some(mode);
    })?;
    Ok(json!({}))
}

pub(super) fn acp_server_set_session_config_option(
    store: &AppStore,
    params: &Value,
) -> AppResult<Value> {
    let session_id = acp_session_id_from_params(params);
    if session_id.is_empty() || store.conversation(&session_id).is_err() {
        return Ok(Value::Null);
    }
    let config_id = acp_normalize_config_option_id(&acp_session_string_text(
        params,
        &[
            "configId",
            "config_id",
            "optionId",
            "option_id",
            "name",
            "id",
        ],
    ));
    let value = params.get("value").cloned().unwrap_or(Value::Null);
    acp_update_session_runtime_config(store, &session_id, |runtime| {
        if matches!(config_id.as_str(), "approval_mode" | "edit_approval_policy") {
            runtime.mode = Some(acp_mode_from_approval_config_value(&value));
        } else if !config_id.is_empty() {
            runtime.config_options.insert(config_id, value);
        }
    })?;
    Ok(json!({"configOptions": []}))
}

pub(super) fn acp_server_fork_session(store: &AppStore, params: &Value) -> AppResult<Value> {
    let session_id = acp_session_id_from_params(params);
    if session_id.is_empty() {
        return Ok(json!({"sessionId": ""}));
    }
    let original = match store.conversation(&session_id) {
        Ok(conversation) => conversation,
        Err(_) => return Ok(json!({"sessionId": ""})),
    };
    let fork = store.create_conversation(
        Some(acp_fork_session_title(&original.title)),
        original.persona_id.clone(),
    )?;
    let original_runtime = acp_session_runtime_config_for_store(store, &session_id)?;
    for message in store.messages(&original.id, None)? {
        let mut copied = ChatMessage::new(
            fork.id.clone(),
            &message.role,
            message.content.clone(),
            &message.source,
        );
        copied.created_at = message.created_at;
        copied.account_id = message.account_id;
        copied.provider_data = message.provider_data;
        store.append_message(copied)?;
    }
    acp_update_session_runtime_config(store, &fork.id, |runtime| {
        if let Some(original_runtime) = original_runtime {
            *runtime = original_runtime;
        }
        let cwd = acp_session_string_text(params, &["cwd"]);
        if !cwd.is_empty() {
            runtime
                .config_options
                .insert("cwd".into(), Value::String(cwd));
        }
        if let Some(mcp_servers) = acp_mcp_servers_from_params(params) {
            runtime.mcp_servers = mcp_servers;
        }
    })?;
    acp_register_session_mcp_servers(store, &fork.id)?;
    acp_server_session_response(store, &fork.id)
}

fn acp_apply_session_params_to_runtime(
    store: &AppStore,
    session_id: &str,
    params: &Value,
) -> AppResult<()> {
    let cwd = acp_session_string_text(params, &["cwd"]);
    let mcp_servers = acp_mcp_servers_from_params(params);
    if cwd.is_empty() && mcp_servers.is_none() {
        return Ok(());
    }
    acp_update_session_runtime_config(store, session_id, |runtime| {
        if !cwd.is_empty() {
            runtime
                .config_options
                .insert("cwd".into(), Value::String(cwd));
        }
        if let Some(mcp_servers) = mcp_servers {
            runtime.mcp_servers = mcp_servers;
        }
    })?;
    acp_register_session_mcp_servers(store, session_id)
}

pub(super) fn acp_model_selection_from_id(model_id: &str) -> (Option<String>, String) {
    let model_id = model_id.trim();
    let Some((provider, model)) = model_id.split_once(':') else {
        return (None, model_id.to_string());
    };
    let provider = provider.trim();
    let model = model.trim();
    if provider.is_empty() || model.is_empty() {
        (None, model_id.to_string())
    } else {
        (Some(provider.to_string()), model.to_string())
    }
}

fn acp_normalize_session_mode(mode: &str) -> String {
    match mode.trim() {
        ACP_MODE_ACCEPT_EDITS => ACP_MODE_ACCEPT_EDITS.into(),
        ACP_MODE_DONT_ASK => ACP_MODE_DONT_ASK.into(),
        _ => ACP_MODE_DEFAULT.into(),
    }
}

fn acp_mode_from_approval_value(value: &Value) -> String {
    match value.as_str().unwrap_or("").trim() {
        "auto" | "always" | "allow" | "auto_allow" | "on" | "accept_edits" => {
            ACP_MODE_ACCEPT_EDITS.into()
        }
        "never" | "dont_ask" | "bypass" | "off" => ACP_MODE_DONT_ASK.into(),
        _ => ACP_MODE_DEFAULT.into(),
    }
}

fn acp_mode_from_approval_config_value(value: &Value) -> String {
    match value.as_str().unwrap_or("").trim() {
        "workspace_session" => ACP_MODE_ACCEPT_EDITS.into(),
        "session" => ACP_MODE_DONT_ASK.into(),
        other => acp_mode_from_approval_value(&Value::String(other.to_string())),
    }
}

fn acp_normalize_config_option_id(config_id: &str) -> String {
    match config_id.trim() {
        "approvalMode" => "approval_mode".into(),
        "editApprovalPolicy" => "edit_approval_policy".into(),
        other => other.to_string(),
    }
}

fn acp_mcp_server_to_synth_server(session_id: &str, server: &Value) -> Option<McpServer> {
    let command = acp_session_string_text(server, &["command"]);
    let url = acp_session_string_text(server, &["url"]);
    let name = acp_session_string_text(server, &["name", "id"]);
    if name.is_empty() || (command.is_empty() && url.is_empty()) {
        return None;
    }
    let id = format!(
        "{}{}",
        acp_session_mcp_server_id_prefix(session_id),
        acp_sanitize_mcp_name_component(&name)
    );
    let env = acp_named_values_map(server.get("env"));
    let headers = acp_named_values_map(server.get("headers"));
    Some(McpServer {
        id,
        name,
        transport: Some(if url.is_empty() { "stdio" } else { "http" }.into()),
        command,
        args: server
            .get("args")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default(),
        env: if env.is_empty() { None } else { Some(env) },
        url: if url.is_empty() { None } else { Some(url) },
        headers: if headers.is_empty() {
            None
        } else {
            Some(headers)
        },
        protocol: "jsonRpc".into(),
        enabled: true,
        timeout_seconds: 30,
        supports_parallel_tool_calls: false,
    })
}

fn acp_named_values_map(value: Option<&Value>) -> HashMap<String, String> {
    let Some(value) = value else {
        return HashMap::new();
    };
    if let Some(object) = value.as_object() {
        return object
            .iter()
            .map(|(name, value)| (name.clone(), value.as_str().unwrap_or("").to_string()))
            .collect();
    }
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let name = acp_session_string_text(item, &["name"]);
            if name.is_empty() {
                return None;
            }
            Some((name, acp_session_string_text(item, &["value"])))
        })
        .collect()
}

fn acp_mcp_utility_tool_definitions(server: &McpServer) -> Vec<ToolDefinition> {
    let safe_server = acp_sanitize_mcp_name_component(&server.id);
    vec![
        ToolDefinition {
            name: format!("mcp_{safe_server}_list_resources"),
            display_name: "list_resources".into(),
            description: format!("List available resources from MCP server '{}'", server.id),
            source: "mcp_utility".into(),
            server_id: server.id.clone(),
            tool_name: "__mcp_list_resources".into(),
            input_schema: json!({"type": "object", "properties": {}}),
            requires_approval: false,
        },
        ToolDefinition {
            name: format!("mcp_{safe_server}_read_resource"),
            display_name: "read_resource".into(),
            description: format!("Read a resource by URI from MCP server '{}'", server.id),
            source: "mcp_utility".into(),
            server_id: server.id.clone(),
            tool_name: "__mcp_read_resource".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "uri": {"type": "string", "description": "URI of the resource to read"}
                },
                "required": ["uri"]
            }),
            requires_approval: false,
        },
        ToolDefinition {
            name: format!("mcp_{safe_server}_list_prompts"),
            display_name: "list_prompts".into(),
            description: format!("List available prompts from MCP server '{}'", server.id),
            source: "mcp_utility".into(),
            server_id: server.id.clone(),
            tool_name: "__mcp_list_prompts".into(),
            input_schema: json!({"type": "object", "properties": {}}),
            requires_approval: false,
        },
        ToolDefinition {
            name: format!("mcp_{safe_server}_get_prompt"),
            display_name: "get_prompt".into(),
            description: format!("Get a prompt by name from MCP server '{}'", server.id),
            source: "mcp_utility".into(),
            server_id: server.id.clone(),
            tool_name: "__mcp_get_prompt".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string", "description": "Name of the prompt to retrieve"},
                    "arguments": {"type": "object", "description": "Optional arguments to pass to the prompt", "additionalProperties": true}
                },
                "required": ["name"]
            }),
            requires_approval: false,
        },
    ]
}

pub(super) fn acp_session_mcp_server_id_prefix(session_id: &str) -> String {
    format!("acp_{}_", acp_sanitize_mcp_name_component(session_id))
}

fn acp_sanitize_mcp_name_component(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

pub(super) fn acp_new_session_title_from_cwd(cwd: &str) -> String {
    let title = Path::new(cwd)
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("ACP Session");
    format!("ACP: {title}")
}

pub(super) fn acp_fork_session_title(title: &str) -> String {
    let title = title.trim();
    if title.is_empty() {
        "ACP Fork".into()
    } else {
        format!("{title} (fork)")
    }
}

pub(super) fn acp_server_session_response(store: &AppStore, session_id: &str) -> AppResult<Value> {
    let conversation = store.conversation(session_id)?;
    let agent = store.agent(Some(&conversation.agent_id)).ok();
    let runtime = acp_session_runtime_config_for_store(store, session_id)?;
    let current_model = acp_session_current_model_id(runtime.as_ref(), agent.as_ref());
    let available_models = acp_session_available_models_for_store(
        store,
        runtime.as_ref(),
        agent.as_ref(),
        &current_model,
    )?;
    let current_mode = runtime
        .as_ref()
        .and_then(|runtime| runtime.mode.as_deref())
        .unwrap_or(ACP_MODE_DEFAULT);
    let mcp_servers = runtime
        .as_ref()
        .map(|runtime| runtime.mcp_servers.clone())
        .unwrap_or_default();
    Ok(json!({
        "sessionId": conversation.id,
        "models": {
            "currentModel": current_model,
            "currentModelId": current_model,
            "current_model_id": current_model,
            "availableModels": available_models.clone(),
            "available_models": available_models
        },
        "modes": {
            "currentModeId": current_mode,
            "availableModes": [
                {
                    "id": ACP_MODE_DEFAULT,
                    "name": "Default",
                    "description": "Ask before edits."
                },
                {
                    "id": ACP_MODE_ACCEPT_EDITS,
                    "name": "Accept Edits",
                    "description": "Auto-allow ordinary workspace edits for this session."
                },
                {
                    "id": ACP_MODE_DONT_ASK,
                    "name": "Don't Ask",
                    "description": "Auto-allow eligible commands and edits for this session."
                }
            ]
        },
        "mcpServers": mcp_servers
    }))
}

pub(super) fn acp_session_current_model_id(
    runtime: Option<&AcpSessionRuntimeConfig>,
    agent: Option<&AgentDefinition>,
) -> String {
    acp_session_model_id(runtime, agent)
}

pub(super) fn acp_session_available_models_for_store(
    store: &AppStore,
    runtime: Option<&AcpSessionRuntimeConfig>,
    agent: Option<&AgentDefinition>,
    current_model: &str,
) -> AppResult<Value> {
    let providers = store.providers().unwrap_or_default();
    let mut models = Vec::<Value>::new();
    let mut seen = HashSet::<String>::new();
    let runtime_provider = runtime.and_then(|runtime| runtime.provider.as_deref());
    let agent_provider = agent.map(|agent| agent.llm_provider.as_str());
    let current_provider = runtime_provider
        .or(agent_provider)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    for provider in providers {
        if !provider.enabled {
            continue;
        }
        let model = provider.model.trim();
        if model.is_empty() {
            continue;
        }
        let model_id = acp_model_choice_id(Some(provider.id.as_str()), model);
        if !seen.insert(model_id.clone()) {
            continue;
        }
        let is_current = model_id == current_model
            || (Some(provider.id.as_str()) == current_provider
                && runtime
                    .and_then(|runtime| runtime.model.as_deref())
                    .or_else(|| agent.map(|agent| agent.llm_model.as_str()))
                    .map(str::trim)
                    == Some(model));
        let mut description = format!("Provider: {}", acp_provider_display_name(&provider));
        if is_current {
            description.push_str(" • current");
        }
        models.push(json!({
            "modelId": model_id,
            "model_id": model_id,
            "name": model,
            "description": description
        }));
    }
    if !current_model.trim().is_empty() && !seen.contains(current_model) {
        let name = current_model
            .split_once(':')
            .map(|(_, model)| model)
            .unwrap_or(current_model)
            .trim();
        let provider = current_model
            .split_once(':')
            .map(|(provider, _)| provider)
            .or(current_provider)
            .unwrap_or("auto");
        models.insert(
            0,
            json!({
                "modelId": current_model,
                "model_id": current_model,
                "name": name,
                "description": format!("Provider: {provider} • current")
            }),
        );
    }
    Ok(Value::Array(models))
}

fn acp_model_choice_id(provider: Option<&str>, model: &str) -> String {
    let model = model.trim();
    let provider = provider.map(str::trim).filter(|value| !value.is_empty());
    match provider {
        Some(provider) if !model.is_empty() => format!("{provider}:{model}"),
        _ => model.to_string(),
    }
}

fn acp_provider_display_name(provider: &LlmProvider) -> String {
    let name = provider.name.trim();
    if name.is_empty() {
        provider.id.clone()
    } else {
        name.to_string()
    }
}

fn acp_session_string_text(value: &Value, keys: &[&str]) -> String {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .unwrap_or("")
        .to_string()
}

fn acp_session_model_id(
    runtime: Option<&AcpSessionRuntimeConfig>,
    agent: Option<&AgentDefinition>,
) -> String {
    let model = runtime
        .and_then(|runtime| runtime.model.as_deref())
        .or_else(|| agent.map(|agent| agent.llm_model.as_str()))
        .unwrap_or("");
    let provider = runtime
        .and_then(|runtime| runtime.provider.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let model = model.trim();
    match provider {
        Some(provider) if !model.is_empty() => format!("{provider}:{model}"),
        _ => model.to_string(),
    }
}

pub(super) fn acp_session_info_from_conversation(
    conversation: &Conversation,
    messages: &[ChatMessage],
    latest_run: Option<&AgentRunRecord>,
    cwd: &str,
    model: &str,
) -> AcpSessionInfo {
    let preview = messages
        .iter()
        .find(|message| message.role == "user" && !message.content.trim().is_empty())
        .map(|message| message.content.trim())
        .unwrap_or("");
    AcpSessionInfo {
        session_id: conversation.id.clone(),
        cwd: cwd.to_string(),
        title: acp_session_title(&conversation.title, preview, cwd),
        updated_at: latest_session_updated_at(conversation, latest_run),
        model: model.to_string(),
        history_len: messages.len(),
    }
}

pub(super) fn latest_run_record_for_session<'a>(
    runs: &'a [AgentRunRecord],
    conversation_id: &str,
) -> Option<&'a AgentRunRecord> {
    runs.iter()
        .filter(|run| run.conversation_id == conversation_id)
        .max_by(|a, b| a.updated_at.cmp(&b.updated_at))
}

fn latest_session_updated_at(
    conversation: &Conversation,
    latest_run: Option<&AgentRunRecord>,
) -> String {
    latest_run
        .map(|run| run.updated_at.as_str())
        .filter(|updated| *updated > conversation.updated_at.as_str())
        .unwrap_or(conversation.updated_at.as_str())
        .to_string()
}

fn acp_session_title(title: &str, preview: &str, cwd: &str) -> String {
    let title = title.trim();
    if !title.is_empty() && title != "新会话" {
        return title.chars().take(80).collect();
    }
    let preview = preview.trim();
    if !preview.is_empty() {
        return preview.chars().take(80).collect();
    }
    Path::new(cwd)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("ACP Session")
        .to_string()
}

pub(super) fn normalize_acp_session_cwd_for_compare(cwd: &str) -> String {
    let mut value = cwd.trim().trim_start_matches("file://").replace('\\', "/");
    if value.len() >= 7
        && value.as_bytes()[0] == b'/'
        && value[1..5].eq_ignore_ascii_case("mnt/")
        && value.as_bytes()[6] == b'/'
    {
        value = format!("{}:/{}", &value[5..6], &value[7..]);
    }
    while value.len() > 1 && value.ends_with('/') {
        value.pop();
    }
    value.to_ascii_lowercase()
}
