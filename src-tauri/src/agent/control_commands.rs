use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::AppHandle;

use crate::{
    error::{AppError, AppResult},
    models::{
        AgentDefinition, AgentRunRecord, ChatMessage, Conversation, EnhancedSkillSummary,
        LlmProvider, Persona, ToolApprovalRequest, ToolDefinition, ToolTraceEntry,
    },
    store::AppStore,
};

use super::*;
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentControlCommandView {
    pub name: String,
    pub aliases: Vec<String>,
    pub description: String,
    pub category: String,
}

pub(super) struct AgentControlCommandSpec {
    pub(super) name: &'static str,
    pub(super) aliases: &'static [&'static str],
    pub(super) description: &'static str,
    pub(super) category: &'static str,
}

const AGENT_CONTROL_COMMANDS: &[AgentControlCommandSpec] = &[
    AgentControlCommandSpec {
        name: "help",
        aliases: &["agent-help"],
        description: "查看 agent 控制命令",
        category: "Info",
    },
    AgentControlCommandSpec {
        name: "doctor",
        aliases: &["status", "agent-status"],
        description: "查看当前 agent、模型、工具、队列和审批状态",
        category: "Info",
    },
    AgentControlCommandSpec {
        name: "profile",
        aliases: &["whoami"],
        description: "查看当前 profile、persona、agent 和会话",
        category: "Info",
    },
    AgentControlCommandSpec {
        name: "config",
        aliases: &["settings"],
        description: "查看 Agent/Chat 关键配置",
        category: "Info",
    },
    AgentControlCommandSpec {
        name: "queue",
        aliases: &["agent-queue"],
        description: "查看队列、加入 prompt，或执行当前会话队列",
        category: "Run",
    },
    AgentControlCommandSpec {
        name: "steer",
        aliases: &["inject"],
        description: "向当前运行中的 agent turn 注入指导",
        category: "Run",
    },
    AgentControlCommandSpec {
        name: "todo",
        aliases: &["agent-todo"],
        description: "查看当前会话 run 的 todo",
        category: "Run",
    },
    AgentControlCommandSpec {
        name: "search",
        aliases: &["session-search"],
        description: "搜索或浏览会话历史、run 和工具事件",
        category: "Run",
    },
    AgentControlCommandSpec {
        name: "agents",
        aliases: &["tasks"],
        description: "查看活跃 agent run 和队列概况",
        category: "Run",
    },
    AgentControlCommandSpec {
        name: "runs",
        aliases: &["run"],
        description: "查看当前会话最近 agent run",
        category: "Run",
    },
    AgentControlCommandSpec {
        name: "subagents",
        aliases: &["children"],
        description: "查看、暂停/恢复新建或中止子 agent",
        category: "Run",
    },
    AgentControlCommandSpec {
        name: "model",
        aliases: &["models"],
        description: "查看或切换当前 agent 的 LLM provider/model",
        category: "Config",
    },
    AgentControlCommandSpec {
        name: "tools",
        aliases: &[],
        description: "查看当前 agent 可用工具",
        category: "Tools",
    },
    AgentControlCommandSpec {
        name: "context",
        aliases: &[],
        description: "查看当前会话上下文与压缩状态",
        category: "Context",
    },
    AgentControlCommandSpec {
        name: "compact",
        aliases: &["context"],
        description:
            "手动压缩当前会话旧历史到 short context；支持 here N、--keep N 和 force/--force",
        category: "Context",
    },
    AgentControlCommandSpec {
        name: "history",
        aliases: &["hist"],
        description: "查看、删除或清空当前会话消息历史",
        category: "Context",
    },
    AgentControlCommandSpec {
        name: "reset",
        aliases: &[],
        description: "清空当前会话消息历史",
        category: "Context",
    },
    AgentControlCommandSpec {
        name: "version",
        aliases: &["about"],
        description: "查看 SynthChat 版本",
        category: "Info",
    },
    AgentControlCommandSpec {
        name: "usage",
        aliases: &["tokens"],
        description: "查看 LLM token 使用统计",
        category: "Context",
    },
    AgentControlCommandSpec {
        name: "insights",
        aliases: &["stats", "analytics"],
        description: "查看 Hermes 风格的会话、模型、工具和成本洞察",
        category: "Context",
    },
    AgentControlCommandSpec {
        name: "memory",
        aliases: &["mem"],
        description: "查看、搜索、写入、替换或删除当前 persona 的长期记忆",
        category: "Memory",
    },
    AgentControlCommandSpec {
        name: "skills",
        aliases: &["skill"],
        description: "查看、搜索、启用或禁用当前 agent 的 skills",
        category: "Skills",
    },
    AgentControlCommandSpec {
        name: "toolsets",
        aliases: &["tools"],
        description: "查看、启用、禁用或重置当前 agent 的工具集策略",
        category: "Tools",
    },
    AgentControlCommandSpec {
        name: "tool-registry",
        aliases: &["tool-defs", "tool-definitions"],
        description: "查看当前 agent 可见工具定义",
        category: "Tools",
    },
    AgentControlCommandSpec {
        name: "abort",
        aliases: &["stop"],
        description: "中止当前会话运行中的 agent run",
        category: "Run",
    },
    AgentControlCommandSpec {
        name: "approve",
        aliases: &[],
        description: "批准待审批工具调用",
        category: "Approval",
    },
    AgentControlCommandSpec {
        name: "always",
        aliases: &[],
        description: "批准并信任当前工具 server.tool",
        category: "Approval",
    },
    AgentControlCommandSpec {
        name: "trust-server",
        aliases: &[],
        description: "批准并信任当前服务器 server.*",
        category: "Approval",
    },
    AgentControlCommandSpec {
        name: "deny",
        aliases: &[],
        description: "拒绝待审批工具调用",
        category: "Approval",
    },
    AgentControlCommandSpec {
        name: "approvals",
        aliases: &["approval-policy"],
        description: "查看待审批工具调用或管理审批策略",
        category: "Approval",
    },
    AgentControlCommandSpec {
        name: "hooks",
        aliases: &["shell-hooks"],
        description: "查看或撤销 shell hook 持久信任",
        category: "Approval",
    },
    AgentControlCommandSpec {
        name: "cron",
        aliases: &["jobs"],
        description: "查看、创建、触发或管理计划任务",
        category: "Automation",
    },
    AgentControlCommandSpec {
        name: "background",
        aliases: &["bg", "btw"],
        description: "后台启动一个 agent turn，忙碌时自动排队",
        category: "Automation",
    },
    AgentControlCommandSpec {
        name: "platforms",
        aliases: &["platform", "adapters"],
        description: "查看或控制外部平台 adapter；支持 mattermost status/start/stop",
        category: "Automation",
    },
    AgentControlCommandSpec {
        name: "maintenance",
        aliases: &["cleanup"],
        description: "查看或执行历史资源清理",
        category: "Diagnostics",
    },
    AgentControlCommandSpec {
        name: "checkpoints",
        aliases: &["ckpt"],
        description: "查看当前会话 run 的 checkpoint",
        category: "Diagnostics",
    },
    AgentControlCommandSpec {
        name: "resume",
        aliases: &[],
        description: "从指定 checkpoint 恢复 agent run",
        category: "Diagnostics",
    },
    AgentControlCommandSpec {
        name: "export",
        aliases: &[],
        description: "导出当前会话 run 轨迹证据包",
        category: "Diagnostics",
    },
    AgentControlCommandSpec {
        name: "artifacts",
        aliases: &["artifact-index"],
        description: "查看当前会话或全局 agent 产物索引",
        category: "Diagnostics",
    },
    AgentControlCommandSpec {
        name: "diagnose",
        aliases: &[],
        description: "基于 run 轨迹生成失败或完成复盘",
        category: "Diagnostics",
    },
];

pub fn list_agent_control_commands() -> Vec<AgentControlCommandView> {
    AGENT_CONTROL_COMMANDS
        .iter()
        .map(|command| AgentControlCommandView {
            name: command.name.into(),
            aliases: command
                .aliases
                .iter()
                .map(|alias| (*alias).into())
                .collect(),
            description: command.description.into(),
            category: command.category.into(),
        })
        .collect()
}

pub(super) fn resolve_agent_control_command(
    input: &str,
) -> Option<&'static AgentControlCommandSpec> {
    let normalized = input
        .trim()
        .trim_start_matches('/')
        .trim_start_matches('／')
        .to_lowercase();
    if normalized.is_empty() {
        return None;
    }
    AGENT_CONTROL_COMMANDS.iter().find(|command| {
        command.name == normalized || command.aliases.contains(&normalized.as_str())
    })
}

pub(super) fn agent_control_help_text() -> String {
    let mut lines = vec!["Agent 控制命令：".to_string()];
    for command in AGENT_CONTROL_COMMANDS {
        let mut names = vec![format!("/{}", command.name)];
        names.extend(command.aliases.iter().map(|alias| format!("/{alias}")));
        lines.push(format!("- {}：{}", names.join(" 或 "), command.description));
    }
    lines.push(String::new());
    lines.push("这些命令会绕过 planner 直接执行控制操作。".into());
    lines.join("\n")
}

pub(super) async fn handle_agent_control_command(
    store: &AppStore,
    conversation: &Conversation,
    persona: &Persona,
    content: &str,
    app: Option<&AppHandle>,
) -> AppResult<Option<ChatMessage>> {
    let trimmed = content.trim();
    if !(trimmed.starts_with('/') || trimmed.starts_with('／')) {
        return Ok(None);
    }
    let raw_body = trimmed
        .strip_prefix('/')
        .or_else(|| trimmed.strip_prefix('／'))
        .unwrap_or("");
    let mut raw_parts = raw_body.splitn(2, char::is_whitespace);
    let command_input = raw_parts.next().unwrap_or("").to_lowercase();
    let argument_raw = raw_parts.next().unwrap_or("").trim();
    let argument = argument_raw.to_lowercase();
    let Some(command_spec) = resolve_agent_control_command(&command_input) else {
        if let Some(result) = run_python_plugin_command(store, &command_input, argument_raw).await?
        {
            for injected in result.injected_messages {
                store.append_message(ChatMessage::new(
                    conversation.id.clone(),
                    &injected.role,
                    injected.content,
                    "python-plugin",
                ))?;
            }
            return Ok(Some(control_message(conversation, result.reply)));
        }
        return Ok(None);
    };
    let command = command_spec.name;

    let reply = match command {
        "help" => agent_control_help_text(),
        "doctor" => handle_agent_status_control_command(store, conversation, persona)?,
        "profile" => handle_profile_control_command(store, conversation, persona)?,
        "config" => handle_config_control_command(store)?,
        "approve" | "always" | "trust-server" => {
            let Some(approval) = select_pending_approval(store, &conversation.id, &argument)?
            else {
                return Ok(Some(control_message(
                    conversation,
                    "当前会话没有待审批工具调用。",
                )));
            };
            let saved = match command {
                "always" => {
                    approve_tool_call_always_and_resume(store, approval.id.clone(), None, app)
                        .await?
                }
                "trust-server" => {
                    approve_tool_call_server_and_resume(store, approval.id.clone(), None, app)
                        .await?
                }
                _ => approve_tool_call_and_resume(store, approval.id.clone(), None, app).await?,
            };
            match command {
                "always" => format!(
                    "已批准并信任工具调用：{}.{}。审批状态：{}。",
                    saved.server_id, saved.tool_name, saved.status
                ),
                "trust-server" => format!(
                    "已批准并信任服务器：{}.*。审批状态：{}。",
                    saved.server_id, saved.status
                ),
                _ => format!(
                    "已批准工具调用：{}.{}。审批状态：{}。",
                    saved.server_id, saved.tool_name, saved.status
                ),
            }
        }
        "deny" => {
            let Some(approval) = select_pending_approval(store, &conversation.id, &argument)?
            else {
                return Ok(Some(control_message(
                    conversation,
                    "当前会话没有待拒绝工具调用。",
                )));
            };
            let saved = deny_tool_call_and_update_run(
                store,
                approval.id.clone(),
                Some("Denied by control command.".into()),
                app,
            )?;
            format!(
                "已拒绝工具调用：{}.{}。审批状态：{}。",
                saved.server_id, saved.tool_name, saved.status
            )
        }
        "approvals" => handle_approvals_control_command(store, conversation, argument_raw)?,
        "hooks" => handle_shell_hooks_control_command(store, argument_raw)?,
        "export" => {
            let Some(run) = select_agent_run_for_conversation(store, &conversation.id, &argument)?
            else {
                return Ok(Some(control_message(
                    conversation,
                    "当前会话没有可导出的 agent run。",
                )));
            };
            let bundle = export_agent_run_bundle(store, run.run_id.clone())?;
            format!("agent run 轨迹证据包：{}\n{}", run.run_id, bundle)
        }
        "artifacts" => handle_artifacts_control_command(store, conversation, argument_raw)?,
        "diagnose" => {
            let Some(run) = select_agent_run_for_conversation(store, &conversation.id, &argument)?
            else {
                return Ok(Some(control_message(
                    conversation,
                    "当前会话没有可诊断的 agent run。",
                )));
            };
            diagnose_agent_run(store, run.run_id, app).await?.content
        }
        "abort" => {
            if let Some(active) = store.active_agent_run_for_conversation(&conversation.id)? {
                let saved = abort_agent_run(
                    store,
                    active.run_id,
                    Some("Agent run stopped by control command.".into()),
                    app,
                )?;
                format!(
                    "已中止当前 agent run：{}。状态：{}。",
                    saved.run_id, saved.state
                )
            } else {
                "当前会话没有运行中的 agent run。".into()
            }
        }
        "queue" => {
            handle_queue_control_command(store, conversation, persona, argument_raw, app).await?
        }
        "cron" => {
            let payload = cron_control_payload(argument_raw);
            cronjob_tool(store, &conversation.id, &payload)?
        }
        "background" => {
            if argument_raw.trim().is_empty() {
                "用法：/background <prompt>".into()
            } else if let Some(app_handle) = app.cloned() {
                spawn_background_chat_turn_for_job(
                    app_handle,
                    conversation.id.clone(),
                    persona.id.clone(),
                    argument_raw.trim().to_string(),
                    None,
                );
                "后台任务已启动；结果会写回当前会话。".into()
            } else {
                "当前运行环境不支持后台任务。".into()
            }
        }
        "platforms" => handle_platforms_control_command(store, argument_raw, app).await?,
        "maintenance" => handle_maintenance_control_command(store, &argument)?,
        "agents" => format_agents_control_status(store)?,
        "runs" => format_agent_runs_control_status(store, conversation, &argument)?,
        "model" => handle_model_control_command(store, conversation, persona, argument_raw)?,
        "tools" => handle_tool_registry_control_command(store, conversation, argument_raw)?,
        "context" => handle_context_status_control_command(store, conversation, persona)?,
        "compact" => {
            let agent = store.agent(Some(&conversation.agent_id))?;
            handle_compact_control_command(store, conversation, persona, &agent, argument_raw)
                .await?
        }
        "history" => handle_history_control_command(store, conversation, argument_raw)?,
        "reset" => handle_history_control_command(store, conversation, "clear")?,
        "version" => format!(
            "SynthChat v{}",
            option_env!("CARGO_PKG_VERSION").unwrap_or("1.0.0")
        ),
        "usage" => handle_usage_control_command(store)?,
        "insights" => handle_insights_control_command(store, argument_raw)?,
        "memory" => handle_memory_control_command(store, persona, argument_raw)?,
        "skills" => handle_skills_control_command(store, conversation, argument_raw)?,
        "toolsets" => handle_toolsets_control_command(store, conversation, argument_raw)?,
        "tool-registry" => handle_tool_registry_control_command(store, conversation, argument_raw)?,
        "todo" => format_todo_control_status(store, conversation, &argument)?,
        "search" => {
            execute_session_search(
                store,
                conversation,
                &json!({
                    "query": argument_raw,
                    "limit": 12
                }),
            )?
            .0
        }
        "checkpoints" => format_checkpoints_control_status(store, conversation, &argument)?,
        "resume" => {
            let (run_selector, checkpoint_selector) = parse_resume_control_args(argument_raw);
            let Some(run) =
                select_agent_run_for_conversation(store, &conversation.id, run_selector)?
            else {
                return Ok(Some(control_message(
                    conversation,
                    "当前会话没有可恢复的 agent run。",
                )));
            };
            let saved = resume_agent_run(
                store,
                run.run_id,
                checkpoint_selector
                    .filter(|selector| !selector.trim().is_empty())
                    .map(str::to_string),
                app,
            )
            .await?;
            format!(
                "已恢复 agent run：{}。状态：{}。",
                saved.run_id, saved.state
            )
        }
        "subagents" => handle_subagents_control_command(store, argument_raw, app)?,
        "steer" => handle_steer_control_command(store, conversation, argument_raw, app)?,
        _ => handle_agent_status_control_command(store, conversation, persona)?,
    };

    Ok(Some(control_message(conversation, reply)))
}

pub(super) fn handle_steer_control_command(
    store: &AppStore,
    conversation: &Conversation,
    argument_raw: &str,
    app: Option<&AppHandle>,
) -> AppResult<String> {
    let steer_text = argument_raw.trim();
    if steer_text.is_empty() {
        return Ok("用法：/steer <guidance>".into());
    }
    let Some(active) = store.active_agent_run_for_conversation(&conversation.id)? else {
        return Ok("当前会话没有运行中的 agent turn。".into());
    };
    let saved = store.append_agent_run_steer(&active.run_id, steer_text.to_string())?;
    emit_agent_run_record(app, &saved, None);
    Ok(format!("已将指导注入当前 agent run：{}。", saved.run_id))
}

pub(super) fn control_message(
    conversation: &Conversation,
    content: impl Into<String>,
) -> ChatMessage {
    ChatMessage::new(
        conversation.id.clone(),
        "assistant",
        content.into(),
        "desktop-control",
    )
}

pub(super) fn handle_subagents_control_command(
    store: &AppStore,
    argument_raw: &str,
    app: Option<&AppHandle>,
) -> AppResult<String> {
    let mut parts = argument_raw.split_whitespace();
    let mode = parts.next().unwrap_or("active").to_lowercase();
    if matches!(mode.as_str(), "pause" | "paused") {
        let was_paused = set_delegation_spawn_paused(true);
        return Ok(format!(
            "已暂停新的 delegate_task 子智能体创建。之前状态：{}。",
            if was_paused { "paused" } else { "running" }
        ));
    }
    if matches!(mode.as_str(), "resume" | "unpause") {
        let was_paused = set_delegation_spawn_paused(false);
        return Ok(format!(
            "已恢复新的 delegate_task 子智能体创建。之前状态：{}。",
            if was_paused { "paused" } else { "running" }
        ));
    }
    if matches!(mode.as_str(), "abort" | "stop" | "cancel" | "interrupt") {
        let prefix = parts.next().unwrap_or("").trim();
        if prefix.is_empty() {
            return Ok("用法：/subagents abort <runId前缀>".into());
        }
        let Some(run) = select_subagent_run_by_prefix(store, prefix)? else {
            return Ok(format!("未找到匹配的子智能体 run：{prefix}"));
        };
        let saved = abort_agent_run(
            store,
            run.run_id.clone(),
            Some("Subagent interrupted by control command.".into()),
            app,
        )?;
        return Ok(format!(
            "已中止子智能体 run：{}。状态：{}。parent={}",
            saved.run_id,
            saved.state,
            saved.parent_run_id.as_deref().unwrap_or("-")
        ));
    }

    let limit = parts
        .next()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(12)
        .clamp(1, 50);
    let mut child_runs = store
        .agent_runs()?
        .into_iter()
        .filter(|run| run.parent_run_id.is_some())
        .collect::<Vec<_>>();
    child_runs.sort_by(|left, right| {
        agent_run_activity_sort_key(right).cmp(&agent_run_activity_sort_key(left))
    });
    let active_count = child_runs
        .iter()
        .filter(|run| is_active_run_state(&run.state))
        .count();
    let completed_count = child_runs
        .iter()
        .filter(|run| run.state == "completed")
        .count();
    let failed_count = child_runs
        .iter()
        .filter(|run| run.state == "failed")
        .count();
    let selected = match mode.as_str() {
        "all" | "recent" => child_runs.iter().take(limit).collect::<Vec<_>>(),
        "active" | "" => child_runs
            .iter()
            .filter(|run| is_active_run_state(&run.state))
            .take(limit)
            .collect::<Vec<_>>(),
        _ => {
            return Ok("用法：/subagents [active|recent|all|pause|resume] [limit]，或 /subagents abort <runId前缀>".into());
        }
    };
    let selected = if selected.is_empty() && matches!(mode.as_str(), "active" | "") {
        child_runs.iter().take(limit).collect::<Vec<_>>()
    } else {
        selected
    };
    let mut lines = vec![format!(
        "Subagent 概况：total={} active={} completed={} failed={} spawnPaused={} mode={} limit={}",
        child_runs.len(),
        active_count,
        completed_count,
        failed_count,
        delegation_spawn_paused(),
        mode,
        limit
    )];
    if selected.is_empty() {
        lines.push("暂无子智能体运行。".into());
    } else {
        lines.push("子智能体 runs：".into());
        lines.extend(selected.into_iter().map(|run| {
            let toolsets = if run.subagent_toolsets.is_empty() {
                "default".into()
            } else {
                run.subagent_toolsets.join(",")
            };
            let task = run
                .subagent_task
                .as_deref()
                .or_else(|| {
                    (!run.user_request.trim().is_empty()).then_some(run.user_request.as_str())
                })
                .unwrap_or("子任务执行");
            format!(
                "- {} [{}] parent={} role={} index={} maxIterations={} activity={} toolsets={} task={}",
                run.run_id,
                run.state,
                run.parent_run_id.as_deref().unwrap_or("-"),
                run.subagent_role.as_deref().unwrap_or("leaf"),
                run.subagent_index
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "-".into()),
                run.subagent_max_iterations
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "-".into()),
                format_agent_run_activity(run),
                toolsets,
                truncate_for_prompt(task, 140)
            )
        }));
    }
    Ok(lines.join("\n"))
}

pub(super) fn select_subagent_run_by_prefix(
    store: &AppStore,
    prefix: &str,
) -> AppResult<Option<AgentRunRecord>> {
    let prefix = prefix.trim();
    if prefix.is_empty() {
        return Ok(None);
    }
    Ok(store
        .agent_runs()?
        .into_iter()
        .find(|run| run.parent_run_id.is_some() && run.run_id.starts_with(prefix)))
}

pub(super) fn is_active_run_state(state: &str) -> bool {
    matches!(state, "started" | "running" | "pendingApproval")
}

pub(super) fn handle_toolsets_control_command(
    store: &AppStore,
    conversation: &Conversation,
    argument_raw: &str,
) -> AppResult<String> {
    let mut agent = store.agent(Some(&conversation.agent_id))?;
    let (_, all_names, _) = agent_toolset_inventory(store, &agent)?;
    let mut parts = argument_raw.split_whitespace();
    let action = parts.next().unwrap_or("").trim().to_lowercase();
    if !matches!(action.as_str(), "" | "list" | "status" | "show") {
        let names = parts
            .map(normalize_toolset_name)
            .filter(|name| !name.is_empty())
            .collect::<Vec<_>>();
        match action.as_str() {
            "reset" | "clear" => {
                agent.enabled_toolsets.clear();
                agent.disabled_toolsets.clear();
                store.save_agent(agent.clone())?;
            }
            "enable" => {
                if names.is_empty() {
                    return Ok("用法：/toolsets enable <name...>".into());
                }
                ensure_known_toolsets(&names, &all_names)?;
                for name in names {
                    agent
                        .disabled_toolsets
                        .retain(|item| normalize_toolset_name(item) != name);
                    if !agent.enabled_toolsets.is_empty()
                        && !agent
                            .enabled_toolsets
                            .iter()
                            .any(|item| normalize_toolset_name(item) == name)
                    {
                        agent.enabled_toolsets.push(name);
                    }
                }
                store.save_agent(agent.clone())?;
            }
            "disable" => {
                if names.is_empty() {
                    return Ok("用法：/toolsets disable <name...>".into());
                }
                ensure_known_toolsets(&names, &all_names)?;
                for name in names {
                    agent
                        .enabled_toolsets
                        .retain(|item| normalize_toolset_name(item) != name);
                    if !agent
                        .disabled_toolsets
                        .iter()
                        .any(|item| normalize_toolset_name(item) == name)
                    {
                        agent.disabled_toolsets.push(name);
                    }
                }
                store.save_agent(agent.clone())?;
            }
            "only" | "set" => {
                if names.is_empty() {
                    return Ok("用法：/toolsets only <name...>".into());
                }
                ensure_known_toolsets(&names, &all_names)?;
                agent.enabled_toolsets = names;
                agent.disabled_toolsets.clear();
                store.save_agent(agent.clone())?;
            }
            _ => {
                return Ok(
                    "用法：/toolsets [list|enable <name...>|disable <name...>|only <name...>|reset]"
                        .into(),
                );
            }
        }
    }

    let (counts, _, tool_count) = agent_toolset_inventory(store, &agent)?;
    Ok(format_toolsets_control_reply(&agent, tool_count, &counts))
}

pub(super) fn handle_tool_registry_control_command(
    store: &AppStore,
    conversation: &Conversation,
    query_raw: &str,
) -> AppResult<String> {
    let agent = store.agent(Some(&conversation.agent_id))?;
    let query = query_raw.trim().to_lowercase();
    let tools =
        visible_tool_definitions_for_agent(store, &agent, ToolExecutionContext::Interactive)?
            .into_iter()
            .filter(|tool| tool_matches_query(tool, &query))
            .collect::<Vec<_>>();
    if tools.is_empty() {
        return if query.is_empty() {
            Ok("当前 agent 没有可见工具。可检查 MCP、toolsets 或工具注册表。".into())
        } else {
            Ok(format!("没有匹配 `{}` 的当前 agent 可见工具。", query))
        };
    }

    let total = tools.len();
    let rows = tools
        .iter()
        .take(20)
        .map(|tool| {
            let approval = if tool.requires_approval {
                "approval"
            } else {
                "auto"
            };
            let toolsets = tool_toolsets(tool)
                .into_iter()
                .filter(|name| !name.starts_with("server:") && !name.starts_with("tool:"))
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>()
                .join(",");
            format!(
                "- {} [{}] {}.{} approval={} toolsets={} :: {}",
                tool.display_name,
                tool.source,
                tool.server_id,
                tool.tool_name,
                approval,
                toolsets,
                truncate_for_prompt(&tool.description.replace('\n', " "), 140)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let suffix = if total > 20 {
        format!("\n... 还有 {} 个匹配工具未显示。", total - 20)
    } else {
        String::new()
    };
    Ok(format!(
        "当前 agent 可见工具：{} 个匹配\n{}{}",
        total, rows, suffix
    ))
}

pub(super) fn tool_matches_query(tool: &ToolDefinition, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    [
        tool.name.as_str(),
        tool.display_name.as_str(),
        tool.description.as_str(),
        tool.source.as_str(),
        tool.server_id.as_str(),
        tool.tool_name.as_str(),
    ]
    .iter()
    .any(|value| value.to_lowercase().contains(query))
}

pub(super) fn agent_toolset_inventory(
    store: &AppStore,
    agent: &AgentDefinition,
) -> AppResult<(BTreeMap<String, usize>, BTreeSet<String>, usize)> {
    let mut tools = internal_tool_prompt_lines()
        .into_iter()
        .map(|(name, line)| ToolDefinition {
            name: name.into(),
            display_name: name.into(),
            description: line.trim_start_matches("- ").to_string(),
            source: "internal".into(),
            server_id: "__internal".into(),
            tool_name: name.into(),
            input_schema: json!({}),
            requires_approval: false,
        })
        .collect::<Vec<_>>();
    tools.extend(available_mcp_tool_definitions(store, agent)?);

    let mut counts = BTreeMap::<String, usize>::new();
    let mut all_names = BTreeSet::<String>::new();
    for tool in &tools {
        for toolset in tool_toolsets(tool) {
            all_names.insert(toolset.clone());
            if !toolset.starts_with("server:") && !toolset.starts_with("tool:") {
                *counts.entry(toolset).or_insert(0) += 1;
            }
        }
    }
    Ok((counts, all_names, tools.len()))
}

pub(super) fn ensure_known_toolsets(
    names: &[String],
    all_names: &BTreeSet<String>,
) -> AppResult<()> {
    let unknown = names
        .iter()
        .filter(|name| name.as_str() != "all" && !all_names.contains(name.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    if unknown.is_empty() {
        Ok(())
    } else {
        Err(AppError::BadRequest(format!(
            "未知 toolset：{}。先用 /toolsets list 查看可用项。",
            unknown.join(", ")
        )))
    }
}

pub(super) fn format_toolsets_control_reply(
    agent: &AgentDefinition,
    tool_count: usize,
    counts: &BTreeMap<String, usize>,
) -> String {
    let rows = counts
        .iter()
        .map(|(name, count)| format!("- {}: {}", name, count))
        .collect::<Vec<_>>()
        .join("\n");
    let enabled = if agent.enabled_toolsets.is_empty() {
        "all".to_string()
    } else {
        agent.enabled_toolsets.join(", ")
    };
    let disabled = if agent.disabled_toolsets.is_empty() {
        "-".to_string()
    } else {
        agent.disabled_toolsets.join(", ")
    };
    format!(
        "当前 Agent Toolsets：\n- agent: {} ({})\n- tools: {}\n- enabledToolsets: {}\n- disabledToolsets: {}\n\n可用 toolset 计数：\n{}",
        agent.name,
        agent.id,
        tool_count,
        enabled,
        disabled,
        if rows.is_empty() {
            "- none".into()
        } else {
            rows
        }
    )
}

pub(super) fn handle_model_control_command(
    store: &AppStore,
    conversation: &Conversation,
    persona: &Persona,
    argument_raw: &str,
) -> AppResult<String> {
    let mut agent = store.agent(Some(&conversation.agent_id))?;
    let providers = store.providers()?;
    let argument = argument_raw.trim();
    if argument.is_empty() || matches!(argument.to_lowercase().as_str(), "list" | "status" | "show")
    {
        return format_model_control_reply(&agent, persona, &providers, None);
    }
    if matches!(argument.to_lowercase().as_str(), "reset" | "clear") {
        agent.llm_provider.clear();
        agent.llm_model.clear();
        let saved = store.save_agent(agent)?;
        return format_model_control_reply(
            &saved,
            persona,
            &providers,
            Some("已清除当前 agent 的模型覆盖。"),
        );
    }

    let mut provider_selector: Option<String> = None;
    let mut model_parts = Vec::new();
    let mut tokens = argument.split_whitespace();
    while let Some(token) = tokens.next() {
        if let Some(value) = token.strip_prefix("--provider=") {
            provider_selector = Some(value.to_string());
            continue;
        }
        match token {
            "--provider" | "-p" => {
                let Some(value) = tokens.next() else {
                    return Ok("用法：/model [model] [--provider <provider>]".into());
                };
                provider_selector = Some(value.to_string());
            }
            "--global" => {}
            _ => model_parts.push(token),
        }
    }
    let model = model_parts.join(" ");
    if provider_selector.is_none() && model.trim().is_empty() {
        return Ok("用法：/model [model] [--provider <provider>] 或 /model reset".into());
    }

    let mut resolved_alias: Option<String> = None;
    if let Some(selector) = provider_selector.as_deref() {
        let provider = select_llm_provider(&providers, selector)?;
        if !provider.enabled {
            return Err(AppError::BadRequest(format!(
                "llm provider {} is disabled",
                provider.id
            )));
        }
        agent.llm_provider = provider.id.clone();
        if model.trim().is_empty() {
            agent.llm_model.clear();
        }
    } else if !model.trim().is_empty() {
        if let Ok(provider) = select_llm_provider(&providers, model.trim()) {
            if !provider.enabled {
                return Err(AppError::BadRequest(format!(
                    "llm provider {} is disabled",
                    provider.id
                )));
            }
            agent.llm_provider = provider.id.clone();
            agent.llm_model.clear();
            let saved = store.save_agent(agent)?;
            return format_model_control_reply(
                &saved,
                persona,
                &providers,
                Some("已切换当前 agent 的 LLM provider。"),
            );
        }
    }
    if !model.trim().is_empty() {
        let active_provider = if !agent.llm_provider.trim().is_empty() {
            select_llm_provider(&providers, &agent.llm_provider)?
        } else if let Some(provider_id) = selected_provider_id(persona, &agent) {
            select_llm_provider(&providers, provider_id)?
        } else {
            providers
                .iter()
                .find(|provider| provider.enabled)
                .or_else(|| providers.first())
                .ok_or_else(|| AppError::NotFound("llm provider".into()))?
        };
        if let Some(alias) = resolve_model_alias(model.trim(), active_provider, &providers) {
            let alias_matches_explicit_provider = provider_selector.is_none()
                || alias
                    .provider_id
                    .as_deref()
                    .map(|provider_id| provider_id == active_provider.id)
                    .unwrap_or(true);
            if !alias_matches_explicit_provider {
                agent.llm_model = model.trim().to_string();
            } else {
                if provider_selector.is_none() {
                    if let Some(provider_id) = alias.provider_id.as_deref() {
                        agent.llm_provider = provider_id.to_string();
                    }
                }
                agent.llm_model = alias.model;
                resolved_alias = Some(alias.alias);
            }
        } else {
            agent.llm_model = model.trim().to_string();
        }
    }
    let saved = store.save_agent(agent)?;
    let prefix = resolved_alias
        .as_deref()
        .map(|alias| format!("已更新当前 agent 的模型设置。resolvedAlias: {alias}"))
        .unwrap_or_else(|| "已更新当前 agent 的模型设置。".into());
    format_model_control_reply(&saved, persona, &providers, Some(&prefix))
}

pub(super) fn selected_provider_id<'a>(
    persona: &'a Persona,
    agent: &'a AgentDefinition,
) -> Option<&'a str> {
    if !persona.llm_provider.trim().is_empty() {
        Some(persona.llm_provider.as_str())
    } else if !agent.llm_provider.trim().is_empty() {
        Some(agent.llm_provider.as_str())
    } else {
        None
    }
}

pub(super) fn effective_llm_persona(persona: &Persona, agent: &AgentDefinition) -> Persona {
    let mut effective = persona.clone();
    if effective.llm_provider.trim().is_empty() && !agent.llm_provider.trim().is_empty() {
        effective.llm_provider = agent.llm_provider.clone();
    }
    if effective.llm_model.trim().is_empty() && !agent.llm_model.trim().is_empty() {
        effective.llm_model = agent.llm_model.clone();
    }
    effective
}

pub(super) fn select_llm_provider<'a>(
    providers: &'a [LlmProvider],
    selector: &str,
) -> AppResult<&'a LlmProvider> {
    let needle = selector.trim().to_lowercase();
    if needle.is_empty() {
        return Err(AppError::BadRequest("provider selector is empty".into()));
    }
    providers
        .iter()
        .find(|provider| {
            provider.id.to_lowercase() == needle
                || provider.name.to_lowercase() == needle
                || provider
                    .preset
                    .as_deref()
                    .unwrap_or_default()
                    .to_lowercase()
                    == needle
        })
        .or_else(|| {
            providers.iter().find(|provider| {
                provider.id.to_lowercase().starts_with(&needle)
                    || provider.name.to_lowercase().starts_with(&needle)
            })
        })
        .ok_or_else(|| AppError::NotFound(format!("llm provider {selector}")))
}

#[derive(Debug, Clone)]
struct ModelAliasResolution {
    alias: String,
    model: String,
    provider_id: Option<String>,
}

pub(super) fn resolve_model_alias(
    raw_model: &str,
    current_provider: &LlmProvider,
    providers: &[LlmProvider],
) -> Option<ModelAliasResolution> {
    let key = normalize_model_alias_key(raw_model);
    let (provider_hint, model) = match key.as_str() {
        "4o" | "gpt4o" | "gpt-4o" => ("openai", "gpt-4o"),
        "4omini" | "4o-mini" | "gpt4omini" | "gpt-4o-mini" => ("openai", "gpt-4o-mini"),
        "41" | "gpt41" | "gpt-4.1" => ("openai", "gpt-4.1"),
        "41mini" | "gpt41mini" | "gpt-4.1-mini" => ("openai", "gpt-4.1-mini"),
        "sonnet" | "claude-sonnet" | "sonnet-4" | "sonnet4" => ("anthropic", "claude-sonnet-4-5"),
        "opus" | "claude-opus" | "opus-4" | "opus4" => ("anthropic", "claude-opus-4-5"),
        "haiku" | "claude-haiku" | "haiku-4" | "haiku4" => ("anthropic", "claude-haiku-4-5"),
        "flash" | "gemini-flash" => ("gemini", "gemini-2.0-flash"),
        "pro" | "gemini-pro" => ("gemini", "gemini-2.5-pro"),
        "deepseek" | "deepseek-chat" => ("deepseek", "deepseek-chat"),
        "deepseek-reasoner" | "deepseek-r1" | "r1" => ("deepseek", "deepseek-reasoner"),
        "qwen" | "qwen-plus" => ("qwen", "qwen-plus"),
        "qwen-max" => ("qwen", "qwen-max"),
        _ => return None,
    };
    let provider_id = if provider_matches_alias_hint(current_provider, provider_hint) {
        Some(current_provider.id.clone())
    } else {
        providers
            .iter()
            .find(|provider| {
                provider.enabled && provider_matches_alias_hint(provider, provider_hint)
            })
            .map(|provider| provider.id.clone())
    };
    Some(ModelAliasResolution {
        alias: raw_model.trim().to_string(),
        model: model.to_string(),
        provider_id,
    })
}

pub(super) fn normalize_model_alias_key(value: &str) -> String {
    value
        .trim()
        .to_ascii_lowercase()
        .chars()
        .filter(|ch| !ch.is_whitespace() && *ch != '_' && *ch != '/')
        .collect()
}

pub(super) fn provider_matches_alias_hint(provider: &LlmProvider, hint: &str) -> bool {
    let hint = hint.to_ascii_lowercase();
    let fields = [
        provider.id.as_str(),
        provider.name.as_str(),
        provider.provider_type.as_str(),
        provider.preset.as_deref().unwrap_or_default(),
        provider.base_url.as_str(),
    ];
    fields
        .iter()
        .any(|field| field.to_ascii_lowercase().contains(&hint))
}

pub(super) fn format_model_control_reply(
    agent: &AgentDefinition,
    persona: &Persona,
    providers: &[LlmProvider],
    prefix: Option<&str>,
) -> AppResult<String> {
    let provider = if let Some(provider_id) = selected_provider_id(persona, agent) {
        select_llm_provider(providers, provider_id)?
    } else {
        providers
            .iter()
            .find(|provider| provider.enabled)
            .or_else(|| providers.first())
            .ok_or_else(|| AppError::NotFound("llm provider".into()))?
    };
    let effective_persona = effective_llm_persona(persona, agent);
    let effective_model = if !effective_persona.llm_model.trim().is_empty() {
        effective_persona.llm_model.trim()
    } else {
        provider.model.trim()
    };
    let persona_note =
        if !persona.llm_provider.trim().is_empty() || !persona.llm_model.trim().is_empty() {
            "\n- note: 当前 persona 有 LLM 覆盖，优先级高于 agent。"
        } else {
            ""
        };
    let provider_rows = providers
        .iter()
        .take(10)
        .map(|provider| {
            format!(
                "- {} ({}) [{}] model={} {}",
                provider.name,
                provider.id,
                provider.provider_type,
                if provider.model.trim().is_empty() {
                    "-"
                } else {
                    provider.model.trim()
                },
                if provider.enabled {
                    "enabled"
                } else {
                    "disabled"
                }
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let fallback_chain = providers
        .iter()
        .filter(|provider| provider.enabled)
        .map(|provider| provider.id.as_str())
        .collect::<Vec<_>>()
        .join(" -> ");
    let prefix = prefix.map(|value| format!("{value}\n")).unwrap_or_default();
    Ok(format!(
        "{}当前模型设置：\n- agent: {} ({})\n- agentProviderOverride: {}\n- agentModelOverride: {}\n- activeProvider: {} ({})\n- effectiveModel: {}\n- fallbackProviderChain: {}{}\n\n可用 providers：\n{}",
        prefix,
        agent.name,
        agent.id,
        if agent.llm_provider.trim().is_empty() {
            "-"
        } else {
            agent.llm_provider.trim()
        },
        if agent.llm_model.trim().is_empty() {
            "-"
        } else {
            agent.llm_model.trim()
        },
        provider.name,
        provider.id,
        if effective_model.is_empty() {
            "-"
        } else {
            effective_model
        },
        if fallback_chain.is_empty() {
            "-"
        } else {
            fallback_chain.as_str()
        },
        persona_note,
        if provider_rows.is_empty() {
            "- none".into()
        } else {
            provider_rows
        }
    ))
}

pub(super) fn handle_history_control_command(
    store: &AppStore,
    conversation: &Conversation,
    argument_raw: &str,
) -> AppResult<String> {
    let mut parts = argument_raw.split_whitespace();
    let first = parts.next().unwrap_or("").trim();
    let action = first.to_lowercase();

    if matches!(action.as_str(), "clear" | "reset" | "purge") {
        let removed = store.clear_conversation_history(&conversation.id)?;
        spawn_session_reset_hooks(
            store,
            conversation.clone(),
            json!({
                "source": "history_control",
                "action": action,
                "removed_messages": removed,
            }),
        );
        return Ok(format!("已清空当前会话历史：删除 {removed} 条消息。"));
    }

    if matches!(action.as_str(), "drop" | "remove" | "delete" | "del" | "rm") {
        let selector = parts.next().unwrap_or("").trim();
        if selector.is_empty() {
            return Ok("用法：/history drop <数量|messageId前缀>".into());
        }
        let messages = store.messages(&conversation.id, None)?;
        if messages.is_empty() {
            return Ok("当前会话还没有消息历史。".into());
        }
        let ids = if let Ok(count) = selector.parse::<usize>() {
            let count = count.clamp(1, 50).min(messages.len());
            messages
                .iter()
                .rev()
                .take(count)
                .map(|message| message.id.clone())
                .collect::<Vec<_>>()
        } else {
            let matches = messages
                .iter()
                .filter(|message| message.id == selector || message.id.starts_with(selector))
                .map(|message| message.id.clone())
                .collect::<Vec<_>>();
            if matches.len() > 1 {
                return Ok(format!(
                    "messageId 前缀不唯一：{}",
                    matches
                        .iter()
                        .take(8)
                        .map(String::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            matches
        };
        if ids.is_empty() {
            return Ok(format!("未找到匹配的消息：{selector}"));
        }
        let removed = store.remove_messages(&conversation.id, &ids)?;
        return Ok(format!(
            "已从当前会话历史删除 {removed} 条消息：{}",
            ids.join(", ")
        ));
    }

    let limit = if matches!(action.as_str(), "list" | "show" | "status" | "recent") {
        parts
            .next()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(12)
    } else if first.is_empty() {
        12
    } else {
        first.parse::<usize>().unwrap_or(12)
    }
    .clamp(1, 50);

    let messages = store.messages(&conversation.id, Some(limit))?;
    if messages.is_empty() {
        return Ok("当前会话还没有消息历史。".into());
    }

    let total = store.messages(&conversation.id, None)?.len();
    let rows = messages
        .iter()
        .map(|message| {
            format!(
                "- {} {} {}: {}",
                message.id,
                message.created_at,
                message.role,
                truncate_for_prompt(&message.content.replace('\n', " "), 180)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    Ok(format!(
        "当前会话共有 {total} 条消息，最近 {} 条：\n{}",
        messages.len(),
        rows
    ))
}

pub(super) fn handle_usage_control_command(store: &AppStore) -> AppResult<String> {
    let usage = store.token_usage()?;
    let prompt = usage
        .get("promptTokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let completion = usage
        .get("completionTokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let total = usage
        .get("totalTokens")
        .and_then(Value::as_u64)
        .unwrap_or(prompt + completion);
    let calls = usage.get("callCount").and_then(Value::as_u64).unwrap_or(0);
    let average = if calls == 0 { 0 } else { total / calls };
    let cache_read = usage
        .get("cacheReadTokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let cache_write = usage
        .get("cacheWriteTokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let reasoning = usage
        .get("reasoningTokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let cost = usage
        .get("estimatedCostUsd")
        .and_then(Value::as_f64)
        .map(|value| format!("{value:.6}"))
        .unwrap_or_else(|| "unknown".into());
    let provider_lines = usage
        .get("byProvider")
        .and_then(Value::as_object)
        .map(|providers| {
            providers
                .iter()
                .take(8)
                .map(|(name, item)| {
                    format!(
                        "- {name}: calls={}, totalTokens={}, estimatedCostUsd={:.6}",
                        item.get("callCount").and_then(Value::as_u64).unwrap_or(0),
                        item.get("totalTokens").and_then(Value::as_u64).unwrap_or(0),
                        item.get("estimatedCostUsd")
                            .and_then(Value::as_f64)
                            .unwrap_or(0.0)
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .filter(|text| !text.trim().is_empty())
        .unwrap_or_else(|| "- none".into());
    let rate_limit = usage
        .get("lastRateLimit")
        .map(format_rate_limit_usage)
        .filter(|text| !text.trim().is_empty())
        .unwrap_or_else(|| "No rate limit headers captured yet.".into());
    Ok(format!(
        "Token 使用统计：\n- promptTokens: {prompt}\n- completionTokens: {completion}\n- cacheReadTokens: {cache_read}\n- cacheWriteTokens: {cache_write}\n- reasoningTokens: {reasoning}\n- totalTokens: {total}\n- callCount: {calls}\n- averageTokensPerCall: {average}\n- estimatedCostUsd: {cost}\n\nProvider breakdown:\n{provider_lines}\n\nRate limits:\n{rate_limit}"
    ))
}

pub(super) fn handle_insights_control_command(
    store: &AppStore,
    argument_raw: &str,
) -> AppResult<String> {
    let days = parse_insights_days(argument_raw)
        .unwrap_or(30)
        .clamp(1, 365);
    let cutoff = Utc::now() - ChronoDuration::days(days as i64);
    let conversations = store.conversations()?;
    let runs = store
        .agent_runs()?
        .into_iter()
        .filter(|run| iso_after_cutoff(&run.started_at, cutoff))
        .collect::<Vec<_>>();
    let tool_traces = store
        .tool_traces()?
        .into_iter()
        .filter(|trace| iso_after_cutoff(&trace.created_at, cutoff))
        .collect::<Vec<_>>();
    let usage = store.token_usage()?;

    let mut total_messages = 0usize;
    let mut user_messages = 0usize;
    let mut assistant_messages = 0usize;
    let mut tool_messages = 0usize;
    let mut active_conversations = 0usize;
    for conversation in &conversations {
        let messages = store.messages(&conversation.id, None)?;
        let recent = messages
            .iter()
            .filter(|message| iso_after_cutoff(&message.created_at, cutoff))
            .collect::<Vec<_>>();
        if !recent.is_empty() {
            active_conversations += 1;
        }
        for message in recent {
            total_messages += 1;
            match message.role.as_str() {
                "user" => user_messages += 1,
                "assistant" => assistant_messages += 1,
                "tool" => tool_messages += 1,
                _ => {}
            }
        }
    }

    let completed_runs = runs.iter().filter(|run| run.state == "completed").count();
    let failed_runs = runs
        .iter()
        .filter(|run| run.state == "failed" || run.error.is_some())
        .count();
    let pending_runs = runs
        .iter()
        .filter(|run| {
            matches!(
                run.state.as_str(),
                "running" | "pendingApproval" | "started"
            )
        })
        .count();
    let subagent_runs = runs
        .iter()
        .filter(|run| run.parent_run_id.is_some())
        .count();
    let total_tool_events = runs.iter().map(|run| run.tool_events.len()).sum::<usize>();
    let total_phase_events = runs.iter().map(|run| run.phase_events.len()).sum::<usize>();

    let prompt = usage
        .get("promptTokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let completion = usage
        .get("completionTokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let total_tokens = usage
        .get("totalTokens")
        .and_then(Value::as_u64)
        .unwrap_or(prompt + completion);
    let call_count = usage.get("callCount").and_then(Value::as_u64).unwrap_or(0);
    let estimated_cost = usage
        .get("estimatedCostUsd")
        .and_then(Value::as_f64)
        .unwrap_or(0.0);

    let providers = format_usage_breakdown(usage.get("byProvider"), "provider");
    let models = format_usage_breakdown(usage.get("byModel"), "model");
    let tools = format_tool_breakdown(&tool_traces);
    let skills = format_skill_breakdown(&tool_traces);
    let failures = format_recent_run_failures(&runs);
    let activity = format_run_activity(&runs);

    Ok(format!(
        "Agent Insights（最近 {days} 天）：\n\nOverview:\n- conversations: {} total / {active_conversations} active\n- runs: {} total / {completed_runs} completed / {failed_runs} failed / {pending_runs} active\n- subagentRuns: {subagent_runs}\n- messages: {total_messages} total / {user_messages} user / {assistant_messages} assistant / {tool_messages} tool\n- toolEvents: {total_tool_events}; phaseEvents: {total_phase_events}; toolTraces: {}\n\nLLM Usage:\n- promptTokens: {prompt}\n- completionTokens: {completion}\n- totalTokens: {total_tokens}\n- callCount: {call_count}\n- estimatedCostUsd: {:.6}\n\nTop Providers:\n{providers}\n\nTop Models:\n{models}\n\nTop Tools:\n{tools}\n\nSkill Activity:\n{skills}\n\nRun Activity:\n{activity}\n\nRecent Failures:\n{failures}",
        conversations.len(),
        runs.len(),
        tool_traces.len(),
        estimated_cost,
    ))
}

pub(super) fn parse_insights_days(argument_raw: &str) -> Option<u64> {
    let parts = argument_raw.split_whitespace().collect::<Vec<_>>();
    if parts.is_empty() {
        return None;
    }
    for (index, part) in parts.iter().enumerate() {
        if matches!(*part, "--days" | "-d") {
            return parts.get(index + 1).and_then(|value| value.parse().ok());
        }
        if let Some(value) = part.strip_prefix("--days=") {
            return value.parse().ok();
        }
    }
    parts.first().and_then(|value| value.parse().ok())
}

pub(super) fn iso_after_cutoff(value: &str, cutoff: DateTime<Utc>) -> bool {
    DateTime::parse_from_rfc3339(value)
        .map(|timestamp| timestamp.with_timezone(&Utc) >= cutoff)
        .unwrap_or(true)
}

pub(super) fn format_usage_breakdown(value: Option<&Value>, label: &str) -> String {
    let Some(items) = value.and_then(Value::as_object) else {
        return "- none".into();
    };
    let mut rows = items
        .iter()
        .map(|(name, item)| {
            (
                name.as_str(),
                item.get("totalTokens").and_then(Value::as_u64).unwrap_or(0),
                item.get("callCount").and_then(Value::as_u64).unwrap_or(0),
                item.get("estimatedCostUsd")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0),
            )
        })
        .collect::<Vec<_>>();
    rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| b.2.cmp(&a.2)));
    if rows.is_empty() {
        return "- none".into();
    }
    rows.into_iter()
        .take(8)
        .map(|(name, tokens, calls, cost)| {
            format!(
                "- {label} {name}: calls={calls}, totalTokens={tokens}, estimatedCostUsd={cost:.6}"
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub(super) fn format_tool_breakdown(tool_traces: &[ToolTraceEntry]) -> String {
    if tool_traces.is_empty() {
        return "- none".into();
    }
    let mut counts: BTreeMap<String, (usize, usize, u128)> = BTreeMap::new();
    for trace in tool_traces {
        let key = if trace.server_id == "__internal" {
            trace.tool_name.clone()
        } else {
            format!("{}.{}", trace.server_id, trace.tool_name)
        };
        let entry = counts.entry(key).or_insert((0, 0, 0));
        entry.0 += 1;
        if !trace.ok {
            entry.1 += 1;
        }
        entry.2 += trace.elapsed_ms;
    }
    let mut rows = counts.into_iter().collect::<Vec<_>>();
    rows.sort_by(|a, b| (b.1).0.cmp(&(a.1).0));
    rows.into_iter()
        .take(10)
        .map(|(tool, (calls, failures, elapsed))| {
            let avg = if calls == 0 {
                0
            } else {
                elapsed / calls as u128
            };
            format!("- {tool}: calls={calls}, failures={failures}, avgMs={avg}")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub(super) fn format_skill_breakdown(tool_traces: &[ToolTraceEntry]) -> String {
    let mut counts: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for trace in tool_traces {
        if !matches!(trace.tool_name.as_str(), "skill_view" | "skill_manage") {
            continue;
        }
        let Some(name) = trace
            .payload
            .get("name")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
        else {
            continue;
        };
        let entry = counts.entry(name.to_string()).or_insert((0, 0));
        if trace.tool_name == "skill_view" {
            entry.0 += 1;
        } else {
            entry.1 += 1;
        }
    }
    if counts.is_empty() {
        return "- none".into();
    }
    let mut rows = counts.into_iter().collect::<Vec<_>>();
    rows.sort_by(|a, b| ((b.1).0 + (b.1).1).cmp(&((a.1).0 + (a.1).1)));
    rows.into_iter()
        .take(8)
        .map(|(skill, (views, manages))| format!("- {skill}: views={views}, manages={manages}"))
        .collect::<Vec<_>>()
        .join("\n")
}

pub(super) fn format_recent_run_failures(runs: &[AgentRunRecord]) -> String {
    let mut rows = runs
        .iter()
        .filter_map(|run| {
            run.error.as_ref().map(|error| {
                format!(
                    "- {} {}: {}",
                    run.started_at,
                    run.run_id,
                    truncate_for_prompt(error, 160)
                )
            })
        })
        .collect::<Vec<_>>();
    rows.reverse();
    if rows.is_empty() {
        "- none".into()
    } else {
        rows.into_iter().take(8).collect::<Vec<_>>().join("\n")
    }
}

pub(super) fn format_run_activity(runs: &[AgentRunRecord]) -> String {
    let mut buckets: BTreeMap<String, usize> = BTreeMap::new();
    for run in runs {
        let day = run
            .started_at
            .split('T')
            .next()
            .filter(|value| !value.is_empty())
            .unwrap_or("unknown");
        *buckets.entry(day.to_string()).or_insert(0) += 1;
    }
    if buckets.is_empty() {
        return "- none".into();
    }
    buckets
        .into_iter()
        .rev()
        .take(7)
        .map(|(day, count)| format!("- {day}: {count} runs"))
        .collect::<Vec<_>>()
        .join("\n")
}

pub(super) fn handle_approvals_control_command(
    store: &AppStore,
    conversation: &Conversation,
    argument_raw: &str,
) -> AppResult<String> {
    let mut parts = argument_raw.split_whitespace();
    let action = parts.next().unwrap_or("pending").to_lowercase();
    match action.as_str() {
        "" | "pending" | "list" | "status" => {
            format_pending_approvals_reply(store, conversation)
        }
        "policy" | "mode" => {
            if let Some(mode) = parts.next() {
                let mut config = store.config()?;
                config.chat.tool_approval_mode = normalize_approval_mode(mode)?;
                store.set_config(config)?;
            }
            format_approval_policy_reply(store)
        }
        "cron-mode" | "cron" => {
            if let Some(mode) = parts.next() {
                let mut config = store.config()?;
                config.chat.cron_approval_mode = normalize_cron_approval_mode(mode);
                store.set_config(config)?;
            }
            format_approval_policy_reply(store)
        }
        "trust" | "always" => {
            let Some(pattern) = parts.next() else {
                return Ok("用法：/approvals trust <server.tool|server.*|*>".into());
            };
            store.trust_tool_pattern(pattern.to_string())?;
            format_approval_policy_reply(store)
        }
        "trust-command" | "trust-cmd" | "allow-command" | "allow-cmd" => {
            let pattern = parts.collect::<Vec<_>>().join(" ");
            if pattern.trim().is_empty() {
                return Ok("用法：/approvals trust-command <command pattern>".into());
            }
            store.trust_command_pattern(pattern)?;
            format_approval_policy_reply(store)
        }
        "untrust" | "remove" | "rm" => {
            let Some(pattern) = parts.next() else {
                return Ok("用法：/approvals untrust <server.tool|server.*|*>".into());
            };
            store.untrust_tool_pattern(pattern)?;
            format_approval_policy_reply(store)
        }
        "untrust-command" | "untrust-cmd" | "remove-command" | "remove-cmd" => {
            let pattern = parts.collect::<Vec<_>>().join(" ");
            if pattern.trim().is_empty() {
                return Ok("用法：/approvals untrust-command <command pattern>".into());
            }
            store.untrust_command_pattern(&pattern)?;
            format_approval_policy_reply(store)
        }
        "trusted" | "trusts" => format_approval_policy_reply(store),
        "reset-trust" | "clear-trust" => {
            let mut config = store.config()?;
            config.chat.trusted_tool_patterns.clear();
            store.set_config(config)?;
            format_approval_policy_reply(store)
        }
        "reset-command-trust" | "clear-command-trust" => {
            let mut config = store.config()?;
            config.chat.trusted_command_patterns.clear();
            store.set_config(config)?;
            format_approval_policy_reply(store)
        }
        _ => Ok("用法：/approvals [pending|mode <risky|smart|always|never>|cron-mode <deny|approve>|trust <server.tool|server.*|*>|untrust <pattern>|trust-command <command pattern>|untrust-command <pattern>|trusted|reset-trust|reset-command-trust]".into()),
    }
}

pub(super) fn format_pending_approvals_reply(
    store: &AppStore,
    conversation: &Conversation,
) -> AppResult<String> {
    let approvals = store
        .tool_approvals()?
        .into_iter()
        .filter(|approval| {
            approval.conversation_id.as_deref() == Some(conversation.id.as_str())
                && approval.status == "pending"
        })
        .take(12)
        .collect::<Vec<_>>();
    if approvals.is_empty() {
        let config = store.config()?;
        return Ok(format!(
            "当前会话没有待审批工具调用。\n{}",
            approval_policy_summary(
                &config.chat.tool_approval_mode,
                &config.chat.cron_approval_mode,
                &config.chat.trusted_tool_patterns,
                &config.chat.trusted_command_patterns
            )
        ));
    }
    let rows = approvals
        .iter()
        .map(|approval| {
            format!(
                "- {} {}.{} run={} reason={}",
                approval.id,
                approval.server_id,
                approval.tool_name,
                approval.run_id.as_deref().unwrap_or("-"),
                truncate_for_prompt(&approval.reason.replace('\n', " "), 120)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    Ok(format!("当前会话待审批工具调用：\n{rows}"))
}

pub(super) fn format_approval_policy_reply(store: &AppStore) -> AppResult<String> {
    let config = store.config()?;
    Ok(approval_policy_summary(
        &config.chat.tool_approval_mode,
        &config.chat.cron_approval_mode,
        &config.chat.trusted_tool_patterns,
        &config.chat.trusted_command_patterns,
    ))
}

pub(super) fn approval_policy_summary(
    mode: &str,
    cron_mode: &str,
    trusted_patterns: &[String],
    trusted_command_patterns: &[String],
) -> String {
    let trusted = if trusted_patterns.is_empty() {
        "- none".into()
    } else {
        trusted_patterns
            .iter()
            .map(|pattern| format!("- {pattern}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let trusted_commands = if trusted_command_patterns.is_empty() {
        "- none".into()
    } else {
        trusted_command_patterns
            .iter()
            .map(|pattern| format!("- {pattern}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    format!(
        "工具审批策略：\n- mode: {mode}\n- cronMode: {}\n- hardline: 灾难性命令和敏感路径写入始终阻断\n- trustedToolPatterns:\n{trusted}\n- trustedCommandPatterns:\n{trusted_commands}",
        normalize_cron_approval_mode(cron_mode)
    )
}

pub(super) fn normalize_approval_mode(mode: &str) -> AppResult<String> {
    match mode.trim().to_lowercase().as_str() {
        "risky" | "risk" | "auto" => Ok("risky".into()),
        "smart" | "llm" | "guardian" => Ok("smart".into()),
        "always" | "all" => Ok("always".into()),
        "never" | "allow" | "auto_allow" | "off" => Ok("never".into()),
        other => Err(AppError::BadRequest(format!(
            "未知审批模式：{other}。可用：risky, always, never。"
        ))),
    }
}

pub(super) fn handle_profile_control_command(
    store: &AppStore,
    conversation: &Conversation,
    persona: &Persona,
) -> AppResult<String> {
    let profile = store.profile()?;
    let agent = store.agent(Some(&conversation.agent_id))?;
    let avatar = profile
        .avatar_path
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("-");
    Ok(format!(
        "当前 Profile：\n- user: {}\n- avatarPath: {}\n- persona: {} ({})\n- agent: {} ({})\n- conversation: {}",
        profile.name, avatar, persona.name, persona.id, agent.name, agent.id, conversation.id
    ))
}

pub(super) fn handle_config_control_command(store: &AppStore) -> AppResult<String> {
    let config = store.config()?;
    let chat = config.chat;
    Ok(format!(
        "Agent/Chat 配置：\n- agentEngine: {}\n- busyInputMode: {}\n- autoTitle: {}\n- toolUseEnforcement: {}\n- toolApprovalMode: {}\n- toolParallel: {} (limit {})\n- queueWaitSeconds: {}\n- maxContextRounds: {}\n- shortContext: {} / {} tokens\n- intentAnalyzerMode: {}\n- toolRouterMode: {}\n- trustedToolPatterns: {}\n- trustedCommandPatterns: {}\n- skillHotReload: {} ({}s)\n- retention: {} ({} days)\n- storageLimits: messagesPerConversation={} agentRuns={} toolTraces={}",
        chat.agent_engine,
        chat.busy_input_mode,
        if chat.auto_title_enabled {
            "enabled"
        } else {
            "disabled"
        },
        chat.tool_use_enforcement,
        chat.tool_approval_mode,
        if chat.tool_parallel_enabled {
            "enabled"
        } else {
            "disabled"
        },
        chat.tool_parallel_limit,
        chat.queue_wait_seconds,
        chat.max_context_rounds,
        chat.short_context_mode,
        chat.short_context_token_budget,
        chat.intent_analyzer_mode,
        chat.tool_router_mode,
        chat.trusted_tool_patterns.len(),
        chat.trusted_command_patterns.len(),
        if chat.skill_hot_reload_enabled {
            "enabled"
        } else {
            "disabled"
        },
        chat.skill_hot_reload_interval_seconds,
        if chat.history_cleanup_enabled {
            "enabled"
        } else {
            "disabled"
        },
        chat.history_retention_days,
        chat.max_stored_messages_per_conversation,
        chat.max_stored_agent_runs,
        chat.max_stored_tool_traces
    ))
}

pub(super) fn handle_context_status_control_command(
    store: &AppStore,
    conversation: &Conversation,
    persona: &Persona,
) -> AppResult<String> {
    let messages = store.messages(&conversation.id, None)?;
    let mut roles = BTreeMap::<String, usize>::new();
    for message in &messages {
        *roles.entry(message.role.clone()).or_insert(0) += 1;
    }
    let agent = store.agent(Some(&conversation.agent_id)).ok();
    let short_context = store.short_context(&conversation.id)?;
    let config = store.config()?.chat;
    let context_budget = config.short_context_token_budget.max(0) as usize;
    let threshold_tokens = if context_budget > 0 {
        context_budget.saturating_mul(80) / 100
    } else {
        0
    };
    let transcript = messages
        .iter()
        .map(|message| format!("{}: {}", message.role, message.content))
        .collect::<Vec<_>>()
        .join("\n");
    let approx_tokens =
        estimate_tokens(&format!("{}\n{}", short_context.summary.trim(), transcript));
    let persona_label = if persona.name.trim().is_empty() {
        persona.id.as_str()
    } else {
        persona.name.as_str()
    };
    let model = agent
        .as_ref()
        .map(|agent| agent.llm_model.trim())
        .filter(|value| !value.is_empty())
        .unwrap_or("auto");
    let provider = agent
        .as_ref()
        .map(|agent| agent.llm_provider.trim())
        .filter(|value| !value.is_empty())
        .unwrap_or("auto");
    let compression_state = if short_context.last_compress_aborted {
        format!(
            "aborted{}",
            short_context
                .last_summary_error
                .as_deref()
                .filter(|value| !value.trim().is_empty())
                .map(|error| format!(" ({})", truncate_for_prompt(error, 120)))
                .unwrap_or_default()
        )
    } else if short_context.summary.trim().is_empty() {
        "not compacted".into()
    } else {
        format!(
            "active summary ({} chars, {} messages)",
            short_context.summary.len(),
            short_context.summary_messages
        )
    };
    let context_usage_line = if context_budget > 0 {
        let pct = (approx_tokens as f64 / context_budget as f64) * 100.0;
        format!(
            "Context usage: ~{} / {} tokens ({pct:.1}%)",
            approx_tokens, context_budget
        )
    } else {
        format!("Context usage: ~{} tokens", approx_tokens)
    };
    let compression_guidance = if threshold_tokens > 0 {
        if approx_tokens >= threshold_tokens {
            let threshold_pct = if context_budget > 0 {
                format!(", {}%", (threshold_tokens * 100) / context_budget)
            } else {
                String::new()
            };
            format!(
                "Compression: due now (threshold ~{}{threshold_pct}). Run /compact.",
                threshold_tokens
            )
        } else {
            let remaining = threshold_tokens.saturating_sub(approx_tokens);
            let threshold_pct = if context_budget > 0 {
                format!(", {}%", (threshold_tokens * 100) / context_budget)
            } else {
                String::new()
            };
            format!(
                "Compression: ~{} tokens until threshold (~{}{threshold_pct}).",
                remaining, threshold_tokens
            )
        }
    } else {
        "Compression threshold: unavailable".into()
    };
    Ok(format!(
        "Context 状态：\n- conversation: {} ({})\n- persona: {} ({})\n- messages: {} user={} assistant={} tool={} system={}\n- model: {}\n- provider: {}\n- shortContextMode: {}\n- shortContextBudget: {} tokens\n- {}\n- {}\n- compression: {}\n- ineffectiveCompressionCount: {}\n\nTip: run /compact to compress manually before the threshold.",
        conversation.title,
        conversation.id,
        persona_label,
        persona.id,
        messages.len(),
        roles.get("user").copied().unwrap_or(0),
        roles.get("assistant").copied().unwrap_or(0),
        roles.get("tool").copied().unwrap_or(0),
        roles.get("system").copied().unwrap_or(0),
        model,
        provider,
        config.short_context_mode,
        config.short_context_token_budget,
        context_usage_line,
        compression_guidance,
        compression_state,
        short_context.ineffective_compression_count,
    ))
}

pub(super) fn handle_maintenance_control_command(
    store: &AppStore,
    argument: &str,
) -> AppResult<String> {
    match argument.trim() {
        "" | "run" | "cleanup" | "clean" | "prune" | "gc" => {
            let report = store.cleanup_historical_resources()?;
            Ok(format_cleanup_report(&report))
        }
        "status" | "show" | "list" => format_maintenance_status(store),
        _ => Ok("用法：/maintenance [status|run]，也可用 /cleanup 直接执行清理。".into()),
    }
}

pub(super) async fn handle_platforms_control_command(
    store: &AppStore,
    argument_raw: &str,
    app: Option<&AppHandle>,
) -> AppResult<String> {
    let parts = argument_raw
        .split_whitespace()
        .map(|part| part.to_ascii_lowercase())
        .collect::<Vec<_>>();
    let platform = parts.first().map(String::as_str).unwrap_or("status");
    let action = parts.get(1).map(String::as_str).unwrap_or("status");
    let (platform, action) = if matches!(platform, "status" | "list" | "show") {
        ("all", platform)
    } else {
        (platform, action)
    };
    if platform == "all" {
        return match action {
            "status" | "list" | "show" => {
                let state = platform_adapter_status(store, None)?;
                Ok(format_platform_adapter_statuses(&state))
            }
            _ => {
                Ok("用法：/platforms [status] 或 /platforms <platform> [status|start|stop]".into())
            }
        };
    }
    let state = match action {
        "status" | "list" | "show" => platform_adapter_status(store, Some(platform))?,
        "start" | "run" => {
            let Some(app_handle) = app.cloned() else {
                return Ok("当前运行环境不支持启动平台 adapter。".into());
            };
            start_platform_adapter(store, app_handle, platform).await?
        }
        "stop" | "halt" => stop_platform_adapter(store, platform)?,
        _ => {
            return Ok(
                "用法：/platforms [status] 或 /platforms <platform> [status|start|stop]".into(),
            );
        }
    };
    Ok(format_platform_adapter_state(&state))
}

fn format_platform_adapter_statuses(state: &Value) -> String {
    let adapters = state
        .get("adapters")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if adapters.is_empty() {
        return "平台 adapters：无状态。".into();
    }
    let mut lines = vec!["平台 adapters：".to_string()];
    for adapter in adapters {
        lines.push(format!(
            "- {}",
            format_platform_adapter_state_line(&adapter)
        ));
    }
    lines.join("\n")
}

fn format_platform_adapter_state_line(state: &Value) -> String {
    let platform = state
        .get("platform")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let status = state
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let runtime = state
        .get("runtime")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let configured = state
        .get("configured")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let transport = state
        .get("transport")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    format!(
        "{platform}: status={status}, mode={}, configured={configured}, transport={transport}",
        if runtime { "runtime" } else { "send-only" }
    )
}

fn format_platform_adapter_state(state: &Value) -> String {
    let platform = state
        .get("platform")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let status = state
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let received = state
        .get("receivedCount")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    let triggered = state
        .get("triggeredCount")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    let updated_at = state.get("updatedAt").and_then(Value::as_str).unwrap_or("");
    let last_error = state
        .get("lastError")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("none");
    format!(
        "{platform} adapter：\n- status: {status}\n- received: {received}\n- triggered: {triggered}\n- updatedAt: {updated_at}\n- lastError: {last_error}"
    )
}

pub(super) fn format_maintenance_status(store: &AppStore) -> AppResult<String> {
    let config = store.config()?.chat;
    let conversations = store.conversations()?;
    let mut message_count = 0usize;
    for conversation in &conversations {
        message_count += store.messages(&conversation.id, None)?.len();
    }
    let runs = store.agent_runs()?;
    let tool_traces = store.tool_traces()?;
    let planner_traces = store.planner_traces()?;
    let router_traces = store.tool_router_traces()?;
    let snapshots = store.state_snapshots()?;
    let workspace_snapshots = store.workspace_snapshots()?;
    Ok(format!(
        "历史资源维护状态：\n- cleanup: {} (retention {} days)\n- conversations: {}\n- messages: {}\n- agentRuns: {}\n- plannerTraces: {}\n- toolRouterTraces: {}\n- toolTraces: {}\n- stateSnapshots: {}\n- workspaceSnapshots: {}\n执行清理：/maintenance run 或 /cleanup",
        if config.history_cleanup_enabled {
            "enabled"
        } else {
            "disabled"
        },
        config.history_retention_days,
        conversations.len(),
        message_count,
        runs.len(),
        planner_traces.len(),
        router_traces.len(),
        tool_traces.len(),
        snapshots.len(),
        workspace_snapshots.len()
    ))
}

pub(super) fn format_cleanup_report(report: &Value) -> String {
    if report
        .get("skipped")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        let reason = report
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or("no cleanup was needed");
        return format!("历史资源清理已跳过：{reason}");
    }
    format!(
        "历史资源清理完成：\n- conversations: {}\n- messages: {}\n- runs: {}\n- plannerTraces: {}\n- toolRouterTraces: {}\n- toolTraces: {}\n- stateSnapshots: {}\n- workspaceSnapshots: {}\n- todos: {}\n- queueItems: {}\n- approvals: {}",
        report_u64(report, "removedConversations"),
        report_u64(report, "removedMessages"),
        report_u64(report, "removedRuns"),
        report_u64(report, "removedPlannerTraces"),
        report_u64(report, "removedToolRouterTraces"),
        report_u64(report, "removedToolTraces"),
        report_u64(report, "removedStateSnapshots"),
        report_u64(report, "removedWorkspaceSnapshots"),
        report_u64(report, "removedTodos"),
        report_u64(report, "removedQueueItems"),
        report_u64(report, "removedApprovals")
    )
}

pub(super) fn report_u64(report: &Value, key: &str) -> u64 {
    report.get(key).and_then(Value::as_u64).unwrap_or(0)
}

pub(super) fn handle_memory_control_command(
    store: &AppStore,
    persona: &Persona,
    argument_raw: &str,
) -> AppResult<String> {
    let mut payload = parse_memory_control_payload(argument_raw);
    let action = payload
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("read")
        .to_string();
    if action == "status" {
        return format_memory_status_reply(store, persona);
    }
    if matches!(action.as_str(), "replace" | "update" | "remove" | "delete") {
        if let Some(selector) = payload
            .get("id")
            .or_else(|| payload.get("memoryId"))
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
        {
            let resolved = resolve_memory_id_for_persona(store, persona, selector)?;
            payload["id"] = json!(resolved);
        }
    }
    let (text, _raw, _ok) = execute_manage_memory(store, persona, &payload)?;
    Ok(text)
}

pub(super) fn parse_memory_control_payload(argument_raw: &str) -> Value {
    let argument = argument_raw.trim();
    if argument.is_empty() {
        return json!({"action": "read"});
    }
    let mut parts = argument.split_whitespace();
    let first = parts.next().unwrap_or("read");
    let action = match first.to_lowercase().as_str() {
        "list" | "read" | "show" => "read",
        "status" | "info" => "status",
        "search" | "find" | "recall" => "read_query",
        "add" | "remember" => "add",
        "replace" | "update" | "set" => "replace",
        "remove" | "delete" | "rm" | "forget" => "remove",
        _ => "read_query",
    };
    let rest = parts.collect::<Vec<_>>();
    let (importance, rest) = extract_memory_importance(rest);
    match action {
        "status" => json!({"action": "status"}),
        "add" => {
            let mut payload = json!({"action": "add", "summary": rest.join(" ")});
            if let Some(value) = importance {
                payload["importance"] = json!(value);
            }
            payload
        }
        "replace" => {
            let id = rest.first().copied().unwrap_or_default();
            let summary = if rest.len() > 1 {
                rest[1..].join(" ")
            } else {
                String::new()
            };
            let mut payload = json!({"action": "replace", "id": id, "summary": summary});
            if let Some(value) = importance {
                payload["importance"] = json!(value);
            }
            payload
        }
        "remove" => json!({"action": "remove", "id": rest.first().copied().unwrap_or_default()}),
        "read" if !rest.is_empty() => json!({"action": "read", "query": rest.join(" ")}),
        "read_query" => {
            let first = argument.split_whitespace().next().unwrap_or_default();
            let query = if matches!(
                first,
                "search" | "find" | "recall" | "read" | "list" | "show"
            ) {
                rest.join(" ")
            } else {
                argument.to_string()
            };
            json!({"action": "read", "query": query})
        }
        _ => json!({"action": "read"}),
    }
}

pub(super) fn extract_memory_importance(parts: Vec<&str>) -> (Option<u8>, Vec<&str>) {
    let mut importance = None;
    let mut rest = Vec::new();
    let mut idx = 0usize;
    while idx < parts.len() {
        if matches!(parts[idx], "--importance" | "-i") {
            if let Some(value) = parts
                .get(idx + 1)
                .and_then(|value| value.parse::<u8>().ok())
            {
                importance = Some(value.clamp(1, 5));
                idx += 2;
                continue;
            }
        }
        rest.push(parts[idx]);
        idx += 1;
    }
    (importance, rest)
}

pub(super) fn format_memory_status_reply(store: &AppStore, persona: &Persona) -> AppResult<String> {
    let memories = store.memories(Some(&persona.id))?;
    let safe_count = memories
        .iter()
        .filter(|memory| crate::store::scan_memory_content(&memory.summary).is_none())
        .count();
    let blocked_count = memories.len().saturating_sub(safe_count);
    let enabled = persona
        .memory
        .get("enabled")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let include_in_prompt = persona
        .memory
        .get("includeInPrompt")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let max_memories = persona
        .memory
        .get("maxMemories")
        .and_then(Value::as_u64)
        .unwrap_or(50);
    let trigger_rounds = persona
        .memory
        .get("triggerRounds")
        .and_then(Value::as_u64)
        .unwrap_or(10);
    let prompt_count = if enabled && include_in_prompt {
        safe_count.min(max_memories.max(1) as usize)
    } else {
        0
    };
    Ok(format!(
        "Memory Status：{}\n- enabled: {}\n- includeInPrompt: {}\n- triggerRounds: {}\n- maxMemories: {}\n- total: {}\n- promptSafe: {}\n- blockedBySecurityScan: {}\n- promptInjected: {}",
        persona.name,
        enabled,
        include_in_prompt,
        trigger_rounds,
        max_memories,
        memories.len(),
        safe_count,
        blocked_count,
        prompt_count
    ))
}

pub(super) fn resolve_memory_id_for_persona(
    store: &AppStore,
    persona: &Persona,
    selector: &str,
) -> AppResult<String> {
    let selector = selector.trim();
    let memories = store.memories(Some(&persona.id))?;
    if memories.iter().any(|memory| memory.id == selector) {
        return Ok(selector.to_string());
    }
    let matches = memories
        .iter()
        .filter(|memory| memory.id.starts_with(selector))
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [memory] => Ok(memory.id.clone()),
        [] => Err(AppError::NotFound(format!("memory {selector}"))),
        _ => Err(AppError::BadRequest(format!(
            "memory selector is ambiguous: {selector}"
        ))),
    }
}

pub(super) fn handle_skills_control_command(
    store: &AppStore,
    conversation: &Conversation,
    argument_raw: &str,
) -> AppResult<String> {
    let mut agent = store.agent(Some(&conversation.agent_id))?;
    let mut parts = argument_raw.split_whitespace();
    let action = parts.next().unwrap_or("list").to_lowercase();
    match action.as_str() {
        "" | "list" | "show" => {
            let query = parts.collect::<Vec<_>>().join(" ");
            format_skills_control_reply(store, &agent, &query, false)
        }
        "enabled" => format_skills_control_reply(store, &agent, "", true),
        "search" | "find" => {
            let query = parts.collect::<Vec<_>>().join(" ");
            format_skills_control_reply(store, &agent, &query, false)
        }
        "inspect" | "info" | "view" => {
            let Some(selector) = parts.next() else {
                return Ok("用法：/skills inspect <skill-id>".into());
            };
            let skills = crate::skills::list_skills_for_agent(store, &agent.id)?;
            let ids = resolve_skill_selectors(&skills, &[selector])?;
            let skill = skills
                .iter()
                .find(|skill| skill.id == ids[0])
                .ok_or_else(|| AppError::NotFound(format!("skill {}", ids[0])))?;
            Ok(format_skill_inspect_reply(skill))
        }
        "reload" | "refresh" => {
            crate::skills::install_builtin_skills(store)?;
            format_skills_control_reply(store, &agent, "", false)
        }
        "reset" | "clear" => {
            agent.enabled_skills.clear();
            agent.skills_enabled = true;
            let saved = store.save_agent(agent)?;
            format_skills_control_reply(store, &saved, "", false)
        }
        "enable" | "add" => {
            let selectors = parts.collect::<Vec<_>>();
            if selectors.is_empty() {
                return Ok("用法：/skills enable <skill-id...>".into());
            }
            let skills = crate::skills::list_skills(store)?;
            let ids = resolve_skill_selectors(&skills, &selectors)?;
            agent.skills_enabled = true;
            for id in ids {
                if !agent.enabled_skills.iter().any(|item| item == &id) {
                    agent.enabled_skills.push(id);
                }
            }
            let saved = store.save_agent(agent)?;
            format_skills_control_reply(store, &saved, "", false)
        }
        "disable" | "remove" | "rm" => {
            let selectors = parts.collect::<Vec<_>>();
            if selectors.is_empty() {
                return Ok("用法：/skills disable <skill-id...>".into());
            }
            let skills = crate::skills::list_skills(store)?;
            let ids = resolve_skill_selectors(&skills, &selectors)?;
            agent
                .enabled_skills
                .retain(|skill_id| !ids.iter().any(|id| id == skill_id));
            let saved = store.save_agent(agent)?;
            format_skills_control_reply(store, &saved, "", false)
        }
        _ => Ok(
            "用法：/skills [list [query]|enabled|search <query>|inspect <id>|enable <id...>|disable <id...>|reset|reload]"
                .into(),
        ),
    }
}

pub(super) fn format_skills_control_reply(
    store: &AppStore,
    agent: &AgentDefinition,
    query: &str,
    enabled_only: bool,
) -> AppResult<String> {
    let mut skills = crate::skills::list_skills_for_agent(store, &agent.id)?;
    let query = query.trim().to_lowercase();
    if !query.is_empty() {
        skills.retain(|skill| skill_matches_query(skill, &query));
    }
    if enabled_only {
        skills.retain(|skill| skill.enabled);
    }
    let total = skills.len();
    let enabled_count = skills.iter().filter(|skill| skill.enabled).count();
    if skills.is_empty() {
        return Ok("当前没有匹配 skills。可尝试 /skills reload。".into());
    }
    skills.sort_by(|left, right| {
        right
            .enabled
            .cmp(&left.enabled)
            .then_with(|| left.id.cmp(&right.id))
    });
    let rows = skills
        .iter()
        .take(20)
        .map(|skill| {
            format!(
                "- {} [{}] enabled={} source={} path={} :: {}",
                skill.id,
                skill.name,
                skill.enabled,
                skill.source,
                skill.path,
                truncate_for_prompt(&skill.description.replace('\n', " "), 160)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let suffix = if total > 20 {
        format!("\n... 还有 {} 个 skill 未显示。", total - 20)
    } else {
        String::new()
    };
    Ok(format!(
        "当前 Agent Skills：\n- agent: {} ({})\n- skillsEnabled: {}\n- enabled: {} / {}\n{}\n{}",
        agent.name, agent.id, agent.skills_enabled, enabled_count, total, rows, suffix
    ))
}

pub(super) fn skill_matches_query(skill: &EnhancedSkillSummary, query: &str) -> bool {
    [
        skill.id.as_str(),
        skill.name.as_str(),
        skill.description.as_str(),
        skill.source.as_str(),
        skill.author.as_str(),
    ]
    .iter()
    .any(|value| value.to_lowercase().contains(query))
}

pub(super) fn format_skill_inspect_reply(skill: &EnhancedSkillSummary) -> String {
    format!(
        "Skill：{} ({})\n- enabled: {}\n- source: {}\n- bundled: {}\n- core: {}\n- path: {}\n- version: {}\n- author: {}\n- description: {}",
        skill.name,
        skill.id,
        skill.enabled,
        skill.source,
        skill.is_bundled,
        skill.is_core,
        skill.path,
        skill.version,
        skill.author,
        truncate_for_prompt(&skill.description.replace('\n', " "), 800)
    )
}

pub(super) fn resolve_skill_selectors(
    skills: &[EnhancedSkillSummary],
    selectors: &[&str],
) -> AppResult<Vec<String>> {
    let mut ids = Vec::new();
    for selector in selectors {
        let selector = selector.trim();
        if selector.is_empty() {
            continue;
        }
        let needle = selector.to_lowercase();
        if let Some(skill) = skills
            .iter()
            .find(|skill| skill.id.to_lowercase() == needle || skill.name.to_lowercase() == needle)
        {
            ids.push(skill.id.clone());
            continue;
        }
        let matches = skills
            .iter()
            .filter(|skill| {
                skill.id.to_lowercase().starts_with(&needle)
                    || skill.name.to_lowercase().contains(&needle)
            })
            .collect::<Vec<_>>();
        match matches.as_slice() {
            [skill] => ids.push(skill.id.clone()),
            [] => return Err(AppError::NotFound(format!("skill {selector}"))),
            _ => {
                return Err(AppError::BadRequest(format!(
                    "skill selector is ambiguous: {selector}"
                )));
            }
        }
    }
    Ok(ids)
}

pub(super) fn handle_agent_status_control_command(
    store: &AppStore,
    conversation: &Conversation,
    persona: &Persona,
) -> AppResult<String> {
    let agent = store.agent(Some(&conversation.agent_id))?;
    let active = store.active_agent_run_for_conversation(&conversation.id)?;
    let queue = store.agent_queue()?;
    let pending_queue = queue
        .iter()
        .filter(|item| item.conversation_id == conversation.id && item.status == "pending")
        .count();
    let pending_approvals = store
        .tool_approvals()?
        .into_iter()
        .filter(|approval| {
            approval.conversation_id.as_deref() == Some(conversation.id.as_str())
                && approval.status == "pending"
        })
        .count();
    let runs = store.agent_runs()?;
    let conversation_runs = runs
        .iter()
        .filter(|run| run.conversation_id == conversation.id)
        .count();
    let jobs = store.scheduled_agent_jobs()?;
    let enabled_jobs = jobs.iter().filter(|job| job.enabled).count();
    Ok(format!(
        "Agent 状态：{}\n- conversation: {} ({})\n- persona: {} ({})\n- agent: {} ({})\n- allowShell: {}\n- runs: {}\n- queuePending: {}\n- pendingApprovals: {}\n- scheduledJobs: {} enabled / {} total",
        active
            .as_ref()
            .map(|run| format!("{} ({})", run.run_id, run.state))
            .unwrap_or_else(|| "idle".into()),
        conversation.title,
        conversation.id,
        persona.name,
        persona.id,
        agent.name,
        agent.id,
        agent.allow_shell,
        conversation_runs,
        pending_queue,
        pending_approvals,
        enabled_jobs,
        jobs.len()
    ))
}

pub(super) fn select_pending_approval(
    store: &AppStore,
    conversation_id: &str,
    selector: &str,
) -> AppResult<Option<ToolApprovalRequest>> {
    let selector = selector.trim();
    let mut approvals = store
        .tool_approvals()?
        .into_iter()
        .filter(|approval| {
            approval.status == "pending"
                && approval.conversation_id.as_deref() == Some(conversation_id)
        })
        .collect::<Vec<_>>();
    approvals.sort_by(|left, right| right.created_at.cmp(&left.created_at));
    if selector.is_empty() {
        return Ok(approvals.into_iter().next());
    }
    Ok(approvals.into_iter().find(|approval| {
        approval.id == selector
            || approval.id.starts_with(selector)
            || format!("{}.{}", approval.server_id, approval.tool_name).starts_with(selector)
    }))
}

pub(super) fn select_agent_run_for_conversation(
    store: &AppStore,
    conversation_id: &str,
    selector: &str,
) -> AppResult<Option<AgentRunRecord>> {
    let selector = selector.trim();
    let mut runs = store
        .agent_runs()?
        .into_iter()
        .filter(|run| run.conversation_id == conversation_id)
        .collect::<Vec<_>>();
    runs.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
    if selector.is_empty() {
        return Ok(runs.into_iter().next());
    }
    Ok(runs
        .into_iter()
        .find(|run| run.run_id == selector || run.run_id.starts_with(selector)))
}

pub(super) fn handle_artifacts_control_command(
    store: &AppStore,
    conversation: &Conversation,
    argument_raw: &str,
) -> AppResult<String> {
    let mut scope_all = false;
    let mut limit = store.config()?.chat.artifact_scan_limit.max(1).min(200);
    for part in argument_raw.split_whitespace() {
        if part.eq_ignore_ascii_case("all") || part.eq_ignore_ascii_case("global") {
            scope_all = true;
        } else if let Ok(value) = part.parse::<usize>() {
            limit = value.max(1).min(200);
        }
    }
    let artifacts = list_agent_artifact_index(
        store,
        if scope_all {
            None
        } else {
            Some(conversation.id.as_str())
        },
        limit,
    )?;
    if artifacts.is_empty() {
        return Ok(if scope_all {
            "当前没有 agent 产物。".into()
        } else {
            "当前会话没有 agent 产物。使用 /artifacts all 查看全局产物索引。".into()
        });
    }
    let mut lines = vec![format!(
        "Agent 产物索引（{}，最多 {} 条）：",
        if scope_all { "全局" } else { "当前会话" },
        limit
    )];
    for artifact in artifacts {
        let run_id = artifact.get("runId").and_then(Value::as_str).unwrap_or("-");
        let file_name = artifact
            .get("fileName")
            .and_then(Value::as_str)
            .unwrap_or("-");
        let size = artifact
            .get("sizeBytes")
            .and_then(Value::as_u64)
            .unwrap_or_default();
        let path = artifact
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let preview = artifact
            .get("contentPreview")
            .and_then(Value::as_str)
            .map(|value| truncate_for_prompt(&value.replace('\n', " "), 120))
            .unwrap_or_default();
        let mut line = format!(
            "- run={} file={} size={} path={}",
            run_id, file_name, size, path
        );
        if !preview.trim().is_empty() {
            line.push_str(&format!(" preview={}", preview));
        }
        lines.push(line);
    }
    Ok(lines.join("\n"))
}

async fn handle_queue_control_command(
    store: &AppStore,
    conversation: &Conversation,
    persona: &Persona,
    argument_raw: &str,
    app: Option<&AppHandle>,
) -> AppResult<String> {
    let trimmed = argument_raw.trim();
    let mut parts = trimmed.split_whitespace();
    let action = parts.next().unwrap_or("").to_lowercase();
    let rest = parts.collect::<Vec<_>>().join(" ");
    if matches!(action.as_str(), "drain" | "run" | "start") && rest.trim().is_empty() {
        let drained = drain_agent_queue_for_conversation(store, &conversation.id, app).await?;
        return Ok(format!("已执行当前会话队列：{} item(s)。", drained));
    }
    if matches!(action.as_str(), "cancel" | "stop" | "rm" | "remove") {
        let selector = rest.split_whitespace().next().unwrap_or("").trim();
        return cancel_agent_queue_item_for_conversation(store, conversation, selector, app);
    }
    if matches!(action.as_str(), "clear" | "clean" | "prune") && rest.trim().is_empty() {
        return clear_finished_agent_queue_items_for_conversation(store, conversation, app);
    }
    if !trimmed.is_empty() && !matches!(action.as_str(), "list" | "show" | "status" | "ls") {
        return enqueue_control_prompt(store, conversation, persona, trimmed, app);
    }
    let mut queue = store
        .agent_queue()?
        .into_iter()
        .filter(|item| item.conversation_id == conversation.id)
        .collect::<Vec<_>>();
    queue.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
    if queue.is_empty() {
        return Ok("当前会话队列为空。".into());
    }
    let rows = queue
        .into_iter()
        .take(20)
        .map(|item| {
            format!(
                "- {} [{}] {}",
                item.id,
                item.status,
                truncate_for_prompt(&item.content.replace('\n', " "), 120)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    Ok(format!("当前会话队列：\n{rows}"))
}

pub(super) fn cancel_agent_queue_item_for_conversation(
    store: &AppStore,
    conversation: &Conversation,
    selector: &str,
    app: Option<&AppHandle>,
) -> AppResult<String> {
    let selector = selector.trim();
    if selector.is_empty() {
        return Ok("请提供要取消的 queue id 前缀。".into());
    }
    let Some(item) = store.agent_queue()?.into_iter().find(|item| {
        item.conversation_id == conversation.id
            && matches!(item.status.as_str(), "pending" | "running")
            && item.id.starts_with(selector)
    }) else {
        return Ok("未找到匹配的当前会话 pending/running 队列项。".into());
    };
    let canceled = store.cancel_agent_queue_item(&item.id)?;
    emit_agent_queue_event(app, "canceled", Some(&canceled), Some(&conversation.id));
    Ok(format!("已取消 agent 队列项：{}。", canceled.id))
}

pub(super) fn clear_finished_agent_queue_items_for_conversation(
    store: &AppStore,
    conversation: &Conversation,
    app: Option<&AppHandle>,
) -> AppResult<String> {
    let before = store
        .agent_queue()?
        .into_iter()
        .filter(|item| item.conversation_id == conversation.id)
        .count();
    let remaining = store.clear_finished_agent_queue_items_for_conversation(&conversation.id)?;
    emit_agent_queue_event(app, "cleared", None, Some(&conversation.id));
    Ok(format!(
        "已清理终态 agent 队列项。当前会话队列：{} -> {}。",
        before,
        remaining.len()
    ))
}

pub(super) fn enqueue_control_prompt(
    store: &AppStore,
    conversation: &Conversation,
    persona: &Persona,
    prompt: &str,
    app: Option<&AppHandle>,
) -> AppResult<String> {
    let (_, queued) = enqueue_prompt_for_conversation(store, conversation, persona, prompt)?;
    emit_agent_queue_event(app, "queued", Some(&queued), Some(&conversation.id));
    Ok(format!("已加入 agent 队列：{}。", queued.id))
}

pub(super) fn enqueue_prompt_for_conversation(
    store: &AppStore,
    conversation: &Conversation,
    persona: &Persona,
    prompt: &str,
) -> AppResult<(ChatMessage, crate::models::AgentQueuedRequest)> {
    let user_message = ChatMessage::new(
        conversation.id.clone(),
        "user",
        prompt.trim().to_string(),
        "desktop-control-queue",
    );
    let saved = store.append_message(user_message)?;
    let queued =
        store.enqueue_agent_request(conversation.id.clone(), persona.id.clone(), &saved)?;
    Ok((saved, queued))
}

async fn drain_agent_queue_for_conversation(
    store: &AppStore,
    conversation_id: &str,
    app: Option<&AppHandle>,
) -> AppResult<usize> {
    let mut count = 0usize;
    while let Some(item) = store.claim_next_agent_request(conversation_id)? {
        emit_agent_queue_event(app, "claimed", Some(&item), Some(conversation_id));
        let request = SendChatRequest {
            conversation_id: Some(item.conversation_id.clone()),
            persona_id: Some(item.persona_id.clone()),
            agent_id: None,
            content: item.content.clone(),
            provider_data: None,
            queue_item_id: Some(item.id.clone()),
        };
        let status = match Box::pin(run_chat_turn_with_app(
            store,
            request,
            ToolExecutionContext::Interactive,
            app,
        ))
        .await
        {
            Ok(_) => "completed",
            Err(error) => {
                let failed = store
                    .complete_agent_queue_item(&item.id, "failed", Some(error.to_string()))?
                    .unwrap_or_else(|| {
                        let mut fallback = item.clone();
                        fallback.status = "failed".into();
                        fallback.error = Some(error.to_string());
                        fallback.updated_at = now_iso();
                        fallback.completed_at = Some(now_iso());
                        fallback
                    });
                emit_agent_queue_event(app, &failed.status, Some(&failed), Some(conversation_id));
                return Err(error);
            }
        };
        let completed = store
            .complete_agent_queue_item(&item.id, status, None)?
            .unwrap_or_else(|| {
                let mut fallback = item;
                fallback.status = status.into();
                fallback.updated_at = now_iso();
                fallback.completed_at = Some(now_iso());
                fallback
            });
        emit_agent_queue_event(
            app,
            &completed.status,
            Some(&completed),
            Some(conversation_id),
        );
        count += 1;
    }
    Ok(count)
}

pub(super) fn cron_control_payload(argument_raw: &str) -> Value {
    let argument = argument_raw.trim();
    let mut parts = argument.split_whitespace();
    let action = parts.next().unwrap_or("list");
    if action.eq_ignore_ascii_case("create") {
        let create_body = argument.get(action.len()..).unwrap_or("").trim();
        if let Some((schedule, prompt)) = create_body.split_once('|') {
            return json!({
                "action": "create",
                "schedule": schedule.trim(),
                "prompt": prompt.trim(),
                "limit": 20
            });
        }
    }
    let job_id = parts.next().unwrap_or("");
    json!({
        "action": action,
        "jobId": job_id,
        "limit": 20
    })
}

pub(super) fn format_agents_control_status(store: &AppStore) -> AppResult<String> {
    let mut runs = store.agent_runs()?;
    runs.sort_by(|left, right| {
        agent_run_activity_sort_key(right).cmp(&agent_run_activity_sort_key(left))
    });
    if runs.is_empty() {
        return Ok("当前没有 agent run。".into());
    }
    let rows = runs
        .into_iter()
        .take(20)
        .map(|run| {
            format!(
                "- {} [{}] conversation={} tools={} checkpoints={} activity={} request={}",
                run.run_id,
                run.state,
                run.conversation_id,
                run.tool_events.len(),
                run.checkpoints.len(),
                format_agent_run_activity(&run),
                truncate_for_prompt(&run.user_request.replace('\n', " "), 100)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    Ok(format!("Agent runs：\n{rows}"))
}

pub(super) fn format_agent_runs_control_status(
    store: &AppStore,
    conversation: &Conversation,
    argument: &str,
) -> AppResult<String> {
    let limit = argument
        .trim()
        .parse::<usize>()
        .ok()
        .map(|value| value.clamp(1, 30))
        .unwrap_or(8);
    let mut runs = store
        .agent_runs()?
        .into_iter()
        .filter(|run| run.conversation_id == conversation.id)
        .collect::<Vec<_>>();
    runs.sort_by(|left, right| {
        agent_run_activity_sort_key(right).cmp(&agent_run_activity_sort_key(left))
    });
    runs.truncate(limit);
    if runs.is_empty() {
        return Ok("当前会话还没有 agent run。".into());
    }
    let rows = runs
        .iter()
        .map(|run| {
            format!(
                "- {} [{}] updated={} tools={} checkpoints={} activity={} request={}",
                run.run_id,
                run.state,
                run.updated_at,
                run.tool_events.len(),
                run.checkpoints.len(),
                format_agent_run_activity(run),
                truncate_for_prompt(&run.user_request.replace('\n', " "), 120)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    Ok(format!(
        "当前会话最近 {} 个 agent run：\n{rows}",
        runs.len()
    ))
}

pub(super) fn format_agent_run_activity(run: &AgentRunRecord) -> String {
    let at = run
        .last_activity_at
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(&run.updated_at);
    let desc = run
        .last_activity_desc
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("updated");
    match DateTime::parse_from_rfc3339(at) {
        Ok(parsed) => {
            let idle_seconds = Utc::now()
                .signed_duration_since(parsed.with_timezone(&Utc))
                .num_seconds()
                .max(0);
            format!(
                "{} at={} idle={}s",
                truncate_for_prompt(&desc.replace('\n', " "), 80),
                at,
                idle_seconds
            )
        }
        Err(_) => format!(
            "{} at={}",
            truncate_for_prompt(&desc.replace('\n', " "), 80),
            at
        ),
    }
}

fn agent_run_activity_sort_key(run: &AgentRunRecord) -> &str {
    run.last_activity_at.as_deref().unwrap_or(&run.updated_at)
}

pub(super) fn format_todo_control_status(
    store: &AppStore,
    conversation: &Conversation,
    selector: &str,
) -> AppResult<String> {
    let Some(run) = select_agent_run_for_conversation(store, &conversation.id, selector)? else {
        return Ok("当前会话没有 agent run。".into());
    };
    let todos = store.agent_todos_for_run(&run.run_id)?;
    if todos.is_empty() {
        return Ok(format!("agent run {} 暂无 todo。", run.run_id));
    }
    let rows = todos
        .into_iter()
        .map(|todo| format!("- [{}] {}", todo.status, todo.content))
        .collect::<Vec<_>>()
        .join("\n");
    Ok(format!("agent run {} Todo：\n{}", run.run_id, rows))
}

pub(super) fn format_checkpoints_control_status(
    store: &AppStore,
    conversation: &Conversation,
    selector: &str,
) -> AppResult<String> {
    let Some(run) = select_agent_run_for_conversation(store, &conversation.id, selector)? else {
        return Ok("当前会话没有 agent run。".into());
    };
    if run.checkpoints.is_empty() {
        return Ok(format!("agent run {} 暂无 checkpoint。", run.run_id));
    }
    let rows = run
        .checkpoints
        .iter()
        .take(20)
        .map(|checkpoint| {
            format!(
                "- {} #{} [{}] {}",
                checkpoint.checkpoint_id,
                checkpoint.iteration,
                checkpoint.state,
                truncate_for_prompt(&checkpoint.summary.replace('\n', " "), 140)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    Ok(format!("agent run {} Checkpoints：\n{}", run.run_id, rows))
}

pub(super) fn parse_resume_control_args(argument_raw: &str) -> (&str, Option<&str>) {
    let mut parts = argument_raw.split_whitespace();
    let run_selector = parts.next().unwrap_or("");
    let checkpoint_selector = parts.next();
    (run_selector, checkpoint_selector)
}
