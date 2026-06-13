use std::collections::HashSet;

use serde_json::json;

use crate::{
    error::{AppError, AppResult},
    models::{AgentAuxiliaryTaskAssignment, AgentAuxiliaryTaskSummary, PluginAuxiliaryTaskSummary},
    store::AppStore,
};

use super::list_python_plugin_auxiliary_tasks;

struct BuiltInAuxiliaryTask {
    key: &'static str,
    display_name: &'static str,
    description: &'static str,
    timeout: u64,
}

const BUILT_IN_AUXILIARY_TASKS: &[BuiltInAuxiliaryTask] = &[
    BuiltInAuxiliaryTask {
        key: "vision",
        display_name: "Vision",
        description: "image and screenshot understanding",
        timeout: 120,
    },
    BuiltInAuxiliaryTask {
        key: "web_extract",
        display_name: "Web extract",
        description: "page extraction and readability",
        timeout: 360,
    },
    BuiltInAuxiliaryTask {
        key: "compression",
        display_name: "Compression",
        description: "conversation summarization",
        timeout: 120,
    },
    BuiltInAuxiliaryTask {
        key: "approval",
        display_name: "Approval",
        description: "tool approval and policy decisions",
        timeout: 30,
    },
    BuiltInAuxiliaryTask {
        key: "goal_judge",
        display_name: "Goal judge",
        description: "persistent goal completion decisions",
        timeout: 30,
    },
    BuiltInAuxiliaryTask {
        key: "mcp",
        display_name: "MCP",
        description: "MCP discovery and tool metadata",
        timeout: 30,
    },
    BuiltInAuxiliaryTask {
        key: "title_generation",
        display_name: "Title generation",
        description: "conversation titles",
        timeout: 30,
    },
    BuiltInAuxiliaryTask {
        key: "skills_hub",
        display_name: "Skills hub",
        description: "skills search and install",
        timeout: 30,
    },
    BuiltInAuxiliaryTask {
        key: "triage_specifier",
        display_name: "Triage specifier",
        description: "kanban spec fleshing",
        timeout: 120,
    },
    BuiltInAuxiliaryTask {
        key: "kanban_decomposer",
        display_name: "Kanban decomposer",
        description: "task decomposition",
        timeout: 180,
    },
    BuiltInAuxiliaryTask {
        key: "profile_describer",
        display_name: "Profile describer",
        description: "automatic profile descriptions",
        timeout: 60,
    },
    BuiltInAuxiliaryTask {
        key: "curator",
        display_name: "Curator",
        description: "skill usage review pass",
        timeout: 600,
    },
];

pub(crate) fn list_agent_auxiliary_tasks(
    store: &AppStore,
) -> AppResult<Vec<AgentAuxiliaryTaskSummary>> {
    let mut tasks = BUILT_IN_AUXILIARY_TASKS
        .iter()
        .map(|task| AgentAuxiliaryTaskSummary {
            key: task.key.into(),
            display_name: task.display_name.into(),
            description: task.description.into(),
            source: "builtin".into(),
            plugin_id: String::new(),
            plugin_name: String::new(),
            defaults: builtin_auxiliary_task_defaults(task),
        })
        .collect::<Vec<_>>();

    let mut used = tasks
        .iter()
        .map(|task| task.key.clone())
        .collect::<HashSet<_>>();
    let mut plugin_tasks = list_python_plugin_auxiliary_tasks(store)?
        .into_iter()
        .filter_map(|task| plugin_auxiliary_task_to_agent_task(task, &mut used))
        .collect::<Vec<_>>();
    plugin_tasks.sort_by(|a, b| a.key.cmp(&b.key));
    tasks.extend(plugin_tasks);
    Ok(tasks)
}

fn builtin_auxiliary_task_defaults(task: &BuiltInAuxiliaryTask) -> serde_json::Value {
    let mut defaults = json!({
        "provider": "auto",
        "model": "",
        "base_url": "",
        "api_key": "",
        "timeout": task.timeout,
        "extra_body": {},
    });
    if task.key == "vision" {
        defaults["download_timeout"] = json!(30);
    }
    defaults
}

fn plugin_auxiliary_task_to_agent_task(
    task: PluginAuxiliaryTaskSummary,
    used: &mut HashSet<String>,
) -> Option<AgentAuxiliaryTaskSummary> {
    let key = task.key.trim();
    if key.is_empty() || !used.insert(key.to_string()) {
        return None;
    }
    Some(AgentAuxiliaryTaskSummary {
        key: key.to_string(),
        display_name: if task.display_name.trim().is_empty() {
            key.to_string()
        } else {
            task.display_name
        },
        description: task.description,
        source: "python-plugin".into(),
        plugin_id: task.plugin_id,
        plugin_name: task.plugin_name,
        defaults: task.defaults,
    })
}

pub(crate) fn agent_auxiliary_task_defaults(
    store: &AppStore,
    key: &str,
) -> AppResult<serde_json::Value> {
    let clean = key.trim();
    if clean.is_empty() {
        return Err(AppError::BadRequest(
            "auxiliary task key must be non-empty".into(),
        ));
    }
    Ok(list_agent_auxiliary_tasks(store)?
        .into_iter()
        .find(|task| task.key == clean)
        .map(|task| task.defaults)
        .unwrap_or_else(|| json!({})))
}

pub(crate) fn list_agent_auxiliary_task_assignments(
    store: &AppStore,
) -> AppResult<Vec<AgentAuxiliaryTaskAssignment>> {
    let config = store.config()?;
    let overrides = config
        .chat
        .auxiliary_task_assignments
        .as_object()
        .cloned()
        .unwrap_or_default();
    list_agent_auxiliary_tasks(store)?
        .into_iter()
        .map(|task| {
            let mut merged = task.defaults.as_object().cloned().unwrap_or_default();
            if let Some(user) = overrides.get(&task.key).and_then(|value| value.as_object()) {
                for (key, value) in user {
                    merged.insert(key.clone(), value.clone());
                }
            }
            Ok(AgentAuxiliaryTaskAssignment {
                provider: string_config_field(&merged, "provider", "auto"),
                model: string_config_field(&merged, "model", ""),
                base_url: string_config_field(&merged, "base_url", ""),
                api_key: string_config_field(&merged, "api_key", ""),
                timeout: merged
                    .get("timeout")
                    .and_then(|value| value.as_u64())
                    .unwrap_or(60),
                extra_body: merged
                    .get("extra_body")
                    .cloned()
                    .unwrap_or_else(|| json!({})),
                key: task.key,
                display_name: task.display_name,
                description: task.description,
                source: task.source,
                plugin_id: task.plugin_id,
                plugin_name: task.plugin_name,
            })
        })
        .collect()
}

pub(crate) fn save_agent_auxiliary_task_assignment(
    store: &AppStore,
    key: &str,
    provider: &str,
    model: &str,
    base_url: &str,
    api_key: &str,
    timeout: Option<u64>,
    extra_body: Option<serde_json::Value>,
) -> AppResult<Vec<AgentAuxiliaryTaskAssignment>> {
    let clean = key.trim();
    if clean.is_empty() {
        return Err(AppError::BadRequest(
            "auxiliary task key must be non-empty".into(),
        ));
    }
    if list_agent_auxiliary_tasks(store)?
        .iter()
        .all(|task| task.key != clean)
    {
        return Err(AppError::BadRequest(format!(
            "unknown auxiliary task: {clean}"
        )));
    }

    let mut config = store.config()?;
    let mut assignments = config
        .chat
        .auxiliary_task_assignments
        .as_object()
        .cloned()
        .unwrap_or_default();
    let mut entry = assignments
        .get(clean)
        .and_then(|value| value.as_object())
        .cloned()
        .unwrap_or_default();
    entry.insert(
        "provider".into(),
        json!(normalized_provider_choice(provider)),
    );
    entry.insert("model".into(), json!(model.trim()));
    entry.insert("base_url".into(), json!(base_url.trim()));
    entry.insert("api_key".into(), json!(api_key.trim()));
    if let Some(timeout) = timeout {
        entry.insert("timeout".into(), json!(timeout.max(1)));
    }
    if let Some(extra_body) = extra_body {
        entry.insert(
            "extra_body".into(),
            if extra_body.is_object() {
                extra_body
            } else {
                json!({})
            },
        );
    }
    assignments.insert(clean.into(), serde_json::Value::Object(entry));
    config.chat.auxiliary_task_assignments = serde_json::Value::Object(assignments);
    store.set_config(config)?;
    list_agent_auxiliary_task_assignments(store)
}

pub(crate) fn reset_agent_auxiliary_task_assignments(
    store: &AppStore,
) -> AppResult<Vec<AgentAuxiliaryTaskAssignment>> {
    let mut config = store.config()?;
    config.chat.auxiliary_task_assignments = json!({});
    store.set_config(config)?;
    list_agent_auxiliary_task_assignments(store)
}

fn string_config_field(
    object: &serde_json::Map<String, serde_json::Value>,
    key: &str,
    default: &str,
) -> String {
    object
        .get(key)
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(default)
        .to_string()
}

fn normalized_provider_choice(provider: &str) -> String {
    provider
        .trim()
        .to_string()
        .chars()
        .collect::<String>()
        .if_empty("auto")
}

trait EmptyStringFallback {
    fn if_empty(self, fallback: &str) -> String;
}

impl EmptyStringFallback for String {
    fn if_empty(self, fallback: &str) -> String {
        if self.is_empty() {
            fallback.into()
        } else {
            self
        }
    }
}
