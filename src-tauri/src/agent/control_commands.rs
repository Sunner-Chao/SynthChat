use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    time::{Duration as StdDuration, Instant},
};

use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, CONTENT_TYPE, USER_AGENT};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};
use uuid::Uuid;

use crate::{
    error::{AppError, AppResult},
    hermes_auth::{
        complete_anthropic_oauth_login, complete_codex_device_code_login,
        complete_google_gemini_oauth_login, complete_minimax_oauth_login,
        complete_nous_device_code_login, complete_spotify_oauth_login, complete_xai_oauth_login,
        hermes_auth_store_credential_status, hermes_auth_store_status,
        hermes_external_credential_status, list_hermes_credential_pool,
        refresh_anthropic_oauth_credentials, refresh_codex_oauth_credentials,
        refresh_google_gemini_oauth_credentials, refresh_minimax_oauth_credentials,
        refresh_nous_oauth_credentials, refresh_qwen_cli_oauth_credentials,
        refresh_spotify_oauth_credentials, refresh_xai_oauth_credentials,
        remove_hermes_credential_pool_entry, reset_hermes_credential_pool_statuses,
        resolve_bitwarden_secret, resolve_hermes_runtime_credential, start_anthropic_oauth_login,
        start_codex_device_code_login, start_google_gemini_oauth_login, start_minimax_oauth_login,
        start_nous_device_code_login, start_spotify_oauth_login, start_xai_oauth_login,
    },
    models::{
        AgentDefinition, AgentRunRecord, ChatMessage, Conversation, EnhancedSkillSummary,
        LlmProvider, Persona, SkillBundle, ToolApprovalRequest, ToolDefinition, ToolTraceEntry,
    },
    store::AppStore,
};

use super::communication::{channel_directory_status_snapshot, send_message_external_targets};
use super::shell_hooks::list_python_plugin_commands;
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
        name: "commands",
        aliases: &["cmds"],
        description: "分页浏览所有控制命令和 plugin slash commands",
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
        description: "查看或管理 Hermes-style desktop profiles",
        category: "Info",
    },
    AgentControlCommandSpec {
        name: "config",
        aliases: &["settings"],
        description: "查看 Agent/Chat 关键配置",
        category: "Info",
    },
    AgentControlCommandSpec {
        name: "auth",
        aliases: &["login"],
        description: "查看 Hermes 风格 provider auth/credential 状态",
        category: "Info",
    },
    AgentControlCommandSpec {
        name: "qqbot",
        aliases: &["qq-bot"],
        description: "查看或执行 QQBot onboard/crypto/状态命令",
        category: "Config",
    },
    AgentControlCommandSpec {
        name: "queue",
        aliases: &["agent-queue", "q"],
        description: "查看队列、加入 prompt，或执行当前会话队列",
        category: "Run",
    },
    AgentControlCommandSpec {
        name: "new",
        aliases: &[],
        description: "创建一个新的空会话",
        category: "Run",
    },
    AgentControlCommandSpec {
        name: "start",
        aliases: &[],
        description: "响应外部平台 start/ping 命令并保持当前会话",
        category: "Run",
    },
    AgentControlCommandSpec {
        name: "topic",
        aliases: &[],
        description: "查看或设置当前会话的外部平台 topic/thread session",
        category: "Session",
    },
    AgentControlCommandSpec {
        name: "redraw",
        aliases: &[],
        description: "请求桌面端刷新当前聊天 UI",
        category: "Session",
    },
    AgentControlCommandSpec {
        name: "handoff",
        aliases: &[],
        description: "查看可 handoff 的外部平台 home target",
        category: "Session",
    },
    AgentControlCommandSpec {
        name: "retry",
        aliases: &[],
        description: "将当前会话最后一条用户消息重新加入 agent 队列",
        category: "Run",
    },
    AgentControlCommandSpec {
        name: "undo",
        aliases: &[],
        description: "回退最近 N 个用户轮次并重新排队目标 prompt",
        category: "Run",
    },
    AgentControlCommandSpec {
        name: "branch",
        aliases: &["fork"],
        description: "从当前会话复制历史并创建一个新的分支会话",
        category: "Run",
    },
    AgentControlCommandSpec {
        name: "steer",
        aliases: &["inject"],
        description: "向当前运行中的 agent turn 注入指导",
        category: "Run",
    },
    AgentControlCommandSpec {
        name: "busy",
        aliases: &[],
        description: "设置忙碌时新输入的处理方式：queue、steer 或 interrupt",
        category: "Run",
    },
    AgentControlCommandSpec {
        name: "goal",
        aliases: &[],
        description: "设置或管理 Hermes 风格 standing goal",
        category: "Run",
    },
    AgentControlCommandSpec {
        name: "subgoal",
        aliases: &[],
        description: "为当前 standing goal 添加、移除或清空附加标准",
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
        name: "sessions",
        aliases: &["conversations"],
        description: "浏览最近会话并查看 conversation id",
        category: "Run",
    },
    AgentControlCommandSpec {
        name: "sethome",
        aliases: &["set-home"],
        description: "设置 gateway/cron 默认投递 home target",
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
        description: "查看或切换当前角色的模型 ID",
        category: "Config",
    },
    AgentControlCommandSpec {
        name: "voice",
        aliases: &[],
        description: "查看或切换当前 persona 的语音回复模式",
        category: "Config",
    },
    AgentControlCommandSpec {
        name: "reasoning",
        aliases: &["think"],
        description: "查看或切换 reasoning replay 设置",
        category: "Config",
    },
    AgentControlCommandSpec {
        name: "personality",
        aliases: &["persona"],
        description: "查看或切换当前会话使用的 persona",
        category: "Config",
    },
    AgentControlCommandSpec {
        name: "fast",
        aliases: &[],
        description: "切换 LLM fast/priority request mode",
        category: "Config",
    },
    AgentControlCommandSpec {
        name: "footer",
        aliases: &[],
        description: "切换 gateway/cron final reply runtime footer",
        category: "Config",
    },
    AgentControlCommandSpec {
        name: "statusbar",
        aliases: &["sb"],
        description: "切换桌面端上下文/模型状态栏显示",
        category: "Config",
    },
    AgentControlCommandSpec {
        name: "verbose",
        aliases: &[],
        description: "切换工具进度展示级别：off/new/all/verbose",
        category: "Config",
    },
    AgentControlCommandSpec {
        name: "skin",
        aliases: &[],
        description: "查看或切换桌面端显示 skin/theme 名称",
        category: "Config",
    },
    AgentControlCommandSpec {
        name: "indicator",
        aliases: &[],
        description: "选择忙碌指示器样式：kaomoji/emoji/unicode/ascii",
        category: "Config",
    },
    AgentControlCommandSpec {
        name: "codex-runtime",
        aliases: &["codex_runtime"],
        description: "切换 Codex app-server runtime：auto/codex_app_server",
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
        aliases: &["context", "compress"],
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
        name: "title",
        aliases: &[],
        description: "查看或设置当前会话标题",
        category: "Context",
    },
    AgentControlCommandSpec {
        name: "save",
        aliases: &[],
        description: "将当前会话导出为 Markdown 文件",
        category: "Context",
    },
    AgentControlCommandSpec {
        name: "reset",
        aliases: &[],
        description: "清空当前会话消息历史",
        category: "Context",
    },
    AgentControlCommandSpec {
        name: "clear",
        aliases: &["cls"],
        description: "清空当前会话消息历史（Hermes /clear 兼容入口）",
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
        name: "copy",
        aliases: &[],
        description: "取出当前会话最近一条 assistant 回复",
        category: "Info",
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
        name: "reload-skills",
        aliases: &["reload_skills"],
        description: "重新安装/扫描内置 skills 并刷新 skill 列表",
        category: "Skills",
    },
    AgentControlCommandSpec {
        name: "plugins",
        aliases: &["plugin"],
        description: "查看、启用或禁用 Hermes/SynthChat plugins",
        category: "Skills",
    },
    AgentControlCommandSpec {
        name: "bundles",
        aliases: &["bundle"],
        description: "查看或安装 skill bundles",
        category: "Skills",
    },
    AgentControlCommandSpec {
        name: "curator",
        aliases: &[],
        description: "查看或运行 Hermes 风格 skill curator，支持 pause/resume/pin/archive/restore",
        category: "Skills",
    },
    AgentControlCommandSpec {
        name: "kanban",
        aliases: &["kb"],
        description: "管理 Hermes 风格 agent kanban 任务板",
        category: "Tools",
    },
    AgentControlCommandSpec {
        name: "toolsets",
        aliases: &["tools"],
        description: "查看、启用、禁用或重置当前 agent 的工具集策略",
        category: "Tools",
    },
    AgentControlCommandSpec {
        name: "platform-tools",
        aliases: &["platform_tools", "hermes-tools"],
        description: "Hermes tools enable/disable/list --platform 兼容配置",
        category: "Tools",
    },
    AgentControlCommandSpec {
        name: "reload-mcp",
        aliases: &["reload_mcp"],
        description: "重新读取磁盘状态中的 MCP server 配置",
        category: "Tools",
    },
    AgentControlCommandSpec {
        name: "reload",
        aliases: &[],
        description: "重新读取磁盘状态、配置和静态资源列表",
        category: "Tools",
    },
    AgentControlCommandSpec {
        name: "tool-registry",
        aliases: &["tool-defs", "tool-definitions"],
        description: "查看当前 agent 可见工具定义",
        category: "Tools",
    },
    AgentControlCommandSpec {
        name: "browser",
        aliases: &[],
        description: "查看浏览器 provider/CDP 工具配置状态",
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
        name: "yolo",
        aliases: &[],
        description: "切换默认允许工具调用的 YOLO 模式（hardline 仍阻断）",
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
        name: "restart",
        aliases: &[],
        description: "重载本地状态并提示 gateway/adapter restart 入口",
        category: "Automation",
    },
    AgentControlCommandSpec {
        name: "paste",
        aliases: &[],
        description: "说明桌面端粘贴图片/文件附件入口",
        category: "Info",
    },
    AgentControlCommandSpec {
        name: "image",
        aliases: &[],
        description: "说明或生成本地图片附件引用格式",
        category: "Info",
    },
    AgentControlCommandSpec {
        name: "gquota",
        aliases: &[],
        description: "查看 Gemini/Google quota 支持状态",
        category: "Info",
    },
    AgentControlCommandSpec {
        name: "update",
        aliases: &[],
        description: "查看 SynthChat 更新/技能更新入口",
        category: "Info",
    },
    AgentControlCommandSpec {
        name: "quit",
        aliases: &["exit"],
        description: "桌面端退出说明",
        category: "Exit",
    },
    AgentControlCommandSpec {
        name: "maintenance",
        aliases: &["cleanup"],
        description: "查看或执行历史资源清理",
        category: "Diagnostics",
    },
    AgentControlCommandSpec {
        name: "checkpoints",
        aliases: &["ckpt", "rollback"],
        description: "查看当前会话 run 的 checkpoint",
        category: "Diagnostics",
    },
    AgentControlCommandSpec {
        name: "snapshot",
        aliases: &["snap"],
        description: "创建、查看、恢复或裁剪 state snapshot",
        category: "Diagnostics",
    },
    AgentControlCommandSpec {
        name: "backup",
        aliases: &["backup-home"],
        description: "生成 Hermes backup CLI 托管进程计划",
        category: "Diagnostics",
    },
    AgentControlCommandSpec {
        name: "import",
        aliases: &["restore-backup"],
        description: "生成 Hermes import/restore CLI 托管进程计划",
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
    AgentControlCommandSpec {
        name: "debug",
        aliases: &["debug-report"],
        description: "生成本地 debug report 和 run 轨迹证据包",
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
    let normalized = normalize_agent_control_command_input(input)?;
    AGENT_CONTROL_COMMANDS.iter().find(|command| {
        command.name == normalized || command.aliases.contains(&normalized.as_str())
    })
}

pub(super) fn normalize_agent_control_command_input(input: &str) -> Option<String> {
    let token = input
        .trim()
        .trim_start_matches('/')
        .trim_start_matches('／')
        .split_whitespace()
        .next()
        .unwrap_or("");
    if token.is_empty() || token.contains('/') || token.contains('\\') {
        return None;
    }
    let without_bot_suffix = token.split('@').next().unwrap_or("");
    if without_bot_suffix.is_empty() {
        return None;
    }
    Some(without_bot_suffix.to_lowercase())
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

pub(super) fn agent_control_help_text_for_store(store: &AppStore) -> String {
    let mut text = agent_control_help_text();
    let Ok(mut plugin_commands) = list_python_plugin_commands(store) else {
        return text;
    };
    plugin_commands.retain(|command| resolve_agent_control_command(&command.name).is_none());
    if plugin_commands.is_empty() {
        return text;
    }
    text.push_str("\n\nPlugin commands：");
    for command in plugin_commands.into_iter().take(20) {
        let args = if command.args_hint.trim().is_empty() {
            String::new()
        } else {
            format!(" {}", command.args_hint.trim())
        };
        text.push_str(&format!(
            "\n- /{}{} [{}]：{}",
            command.name,
            args,
            command.plugin_name,
            truncate_for_prompt(&command.description.replace('\n', " "), 120)
        ));
    }
    text
}

pub(super) fn handle_commands_control_command(
    store: &AppStore,
    argument_raw: &str,
) -> AppResult<String> {
    const PAGE_SIZE: usize = 20;
    let requested_page = argument_raw
        .split_whitespace()
        .next()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|page| *page > 0)
        .unwrap_or(1);
    let mut entries = AGENT_CONTROL_COMMANDS
        .iter()
        .map(|command| {
            let aliases = if command.aliases.is_empty() {
                String::new()
            } else {
                format!(
                    " aliases: {}",
                    command
                        .aliases
                        .iter()
                        .map(|alias| format!("/{alias}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            };
            format!(
                "/{} [{}]{} - {}",
                command.name, command.category, aliases, command.description
            )
        })
        .collect::<Vec<_>>();
    if let Ok(mut plugin_commands) = list_python_plugin_commands(store) {
        plugin_commands.sort_by(|left, right| left.name.cmp(&right.name));
        for command in plugin_commands {
            if resolve_agent_control_command(&command.name).is_some() {
                continue;
            }
            let args = if command.args_hint.trim().is_empty() {
                String::new()
            } else {
                format!(" {}", command.args_hint.trim())
            };
            entries.push(format!(
                "/{}{} [Plugin:{}] - {}",
                command.name,
                args,
                command.plugin_name,
                truncate_for_prompt(&command.description.replace('\n', " "), 120)
            ));
        }
    }
    entries.sort();
    let total_pages = entries.len().max(1).div_ceil(PAGE_SIZE);
    let page = requested_page.min(total_pages).max(1);
    let start = (page - 1) * PAGE_SIZE;
    let end = (start + PAGE_SIZE).min(entries.len());
    let mut lines = vec![format!(
        "Commands page {page}/{total_pages} ({} total):",
        entries.len()
    )];
    if entries.is_empty() {
        lines.push("No commands available.".into());
    } else {
        lines.extend(entries[start..end].iter().cloned());
    }
    if page < total_pages {
        lines.push(format!("Use /commands {} for next page.", page + 1));
    }
    Ok(lines.join("\n"))
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
    let command_input =
        normalize_agent_control_command_input(raw_parts.next().unwrap_or("")).unwrap_or_default();
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
    let requested_command = command_input.clone();
    let command = command_spec.name;

    let reply = match command {
        "help" => agent_control_help_text_for_store(store),
        "commands" => handle_commands_control_command(store, argument_raw)?,
        "doctor" => handle_agent_status_control_command(store, conversation, persona)?,
        "profile" => handle_profile_control_command(store, conversation, persona, argument_raw)?,
        "config" => handle_config_control_command(store)?,
        "auth" => handle_auth_control_command(store, argument_raw).await?,
        "qqbot" => handle_qqbot_control_command(store, argument_raw).await?,
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
        "yolo" => handle_yolo_control_command(store, argument_raw)?,
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
        "debug" => handle_debug_control_command(store, conversation, &argument)?,
        "abort" => {
            handle_abort_control_command(store, conversation, app, requested_command == "stop")?
        }
        "queue" => {
            handle_queue_control_command(store, conversation, persona, argument_raw, app).await?
        }
        "new" => handle_new_control_command(store, persona, argument_raw)?,
        "start" => "start acknowledged. 当前会话保持不变，可直接发送任务。".into(),
        "topic" => handle_topic_control_command(store, conversation, argument_raw)?,
        "redraw" => handle_redraw_control_command(conversation, app)?,
        "handoff" => handle_handoff_control_command(store, argument_raw)?,
        "retry" => handle_retry_control_command(store, conversation, persona, app)?,
        "undo" => handle_undo_control_command(store, conversation, persona, argument_raw, app)?,
        "branch" => handle_branch_control_command(store, conversation, persona, argument_raw)?,
        "goal" => handle_goal_control_command(store, conversation, argument_raw, app)?,
        "subgoal" => handle_subgoal_control_command(store, conversation, argument_raw, app)?,
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
        "restart" => handle_restart_control_command(store, argument_raw)?,
        "maintenance" => handle_maintenance_control_command(store, &argument)?,
        "agents" => format_agents_control_status(store)?,
        "runs" => format_agent_runs_control_status(store, conversation, &argument)?,
        "sessions" => format_sessions_control_status(store, argument_raw)?,
        "sethome" => handle_sethome_control_command(store, argument_raw)?,
        "model" => handle_model_control_command(store, conversation, persona, argument_raw)?,
        "voice" => handle_voice_control_command(store, persona, argument_raw)?,
        "reasoning" => handle_reasoning_control_command(store, argument_raw)?,
        "personality" => handle_personality_control_command(store, conversation, argument_raw)?,
        "fast" => handle_fast_control_command(store, argument_raw)?,
        "footer" => handle_footer_control_command(store, argument_raw)?,
        "statusbar" => handle_statusbar_control_command(store, argument_raw)?,
        "verbose" => handle_verbose_control_command(store, argument_raw)?,
        "skin" => handle_skin_control_command(store, argument_raw)?,
        "indicator" => handle_indicator_control_command(store, argument_raw)?,
        "codex-runtime" => handle_codex_runtime_control_command(store, argument_raw)?,
        "tools" => handle_tool_registry_control_command(store, conversation, argument_raw)?,
        "context" => handle_context_status_control_command(store, conversation, persona)?,
        "compact" => {
            let agent = store.agent(Some(&conversation.agent_id))?;
            handle_compact_control_command(store, conversation, persona, &agent, argument_raw)
                .await?
        }
        "history" => handle_history_control_command(store, conversation, argument_raw)?,
        "title" => handle_title_control_command(store, conversation, argument_raw)?,
        "save" => handle_save_control_command(store, conversation, argument_raw)?,
        "reset" => handle_history_control_command(store, conversation, "clear")?,
        "clear" => handle_history_control_command(store, conversation, "clear")?,
        "version" => format!(
            "SynthChat v{}",
            option_env!("CARGO_PKG_VERSION").unwrap_or("1.0.0")
        ),
        "usage" => handle_usage_control_command_with_account(store, argument_raw).await?,
        "gquota" => handle_gquota_control_command(store)?,
        "update" => handle_update_control_command(store, argument_raw)?,
        "quit" => handle_quit_control_command(argument_raw),
        "paste" => handle_paste_control_command(),
        "image" => handle_image_control_command(argument_raw),
        "insights" => handle_insights_control_command(store, argument_raw)?,
        "copy" => handle_copy_control_command(store, conversation, argument_raw)?,
        "memory" => handle_memory_control_command(store, persona, argument_raw)?,
        "skills" => handle_skills_control_command(store, conversation, argument_raw)?,
        "reload-skills" => handle_skills_control_command(store, conversation, "reload")?,
        "plugins" => handle_plugins_control_command(store, argument_raw)?,
        "bundles" => handle_bundles_control_command(store, conversation, argument_raw)?,
        "curator" => handle_curator_control_command(store, argument_raw)?,
        "kanban" => handle_kanban_control_command(store, argument_raw).await?,
        "toolsets" => handle_toolsets_control_command(store, conversation, argument_raw)?,
        "platform-tools" => handle_platform_tools_control_command(store, argument_raw)?,
        "reload-mcp" => handle_reload_mcp_control_command(store)?,
        "reload" => handle_reload_control_command(store)?,
        "tool-registry" => handle_tool_registry_control_command(store, conversation, argument_raw)?,
        "browser" => handle_browser_control_command(store, argument_raw)?,
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
        "snapshot" => handle_snapshot_control_command(store, argument_raw)?,
        "backup" => handle_backup_control_command(argument_raw)?,
        "import" => handle_import_control_command(argument_raw)?,
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
        "busy" => handle_busy_control_command(store, argument_raw)?,
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

pub(super) fn handle_abort_control_command(
    store: &AppStore,
    conversation: &Conversation,
    app: Option<&AppHandle>,
    stop_managed_processes_for_conversation: bool,
) -> AppResult<String> {
    let mut lines = Vec::new();
    if let Some(active) = store.active_agent_run_for_conversation(&conversation.id)? {
        let saved = abort_agent_run(
            store,
            active.run_id,
            Some("Agent run stopped by control command.".into()),
            app,
        )?;
        lines.push(format!(
            "已中止当前 agent run：{}。状态：{}。",
            saved.run_id, saved.state
        ));
    } else {
        lines.push("当前会话没有运行中的 agent run。".into());
    }

    if stop_managed_processes_for_conversation {
        mark_hermes_session_suspended(store, conversation, "control_stop")?;
        let stopped =
            store.stop_managed_processes(None, Some(&conversation.id), None, None, None, false)?;
        let count = stopped.get("count").and_then(Value::as_u64).unwrap_or(0);
        let error_count = stopped
            .get("errors")
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or(0);
        lines.push(format!(
            "Hermes /stop compat：已停止当前会话 managed processes：{}；错误：{}。",
            count, error_count
        ));
    }

    Ok(lines.join("\n"))
}

fn mark_hermes_session_suspended(
    store: &AppStore,
    conversation: &Conversation,
    reason: &str,
) -> AppResult<()> {
    let snapshot = json!({
        "schema": "hermes_gateway_session_lifecycle_desktop_v1",
        "sessionKey": conversation.id,
        "sessionId": conversation.id,
        "suspended": true,
        "resumePending": false,
        "resumeReason": Value::Null,
        "isFreshReset": false,
        "wasAutoReset": false,
        "autoResetReason": Value::Null,
        "reason": reason,
        "updatedAt": now_iso(),
        "source": "control-command",
        "desktopAdaptation": true,
        "note": "SynthChat maps Hermes SessionEntry.suspended to conversation metadata so /stop can be observed as a forced session break without embedding the Python gateway session store.",
    });
    store
        .set_conversation_metadata_value(&conversation.id, "hermesSessionLifecycle", snapshot)
        .map(|_| ())
}

pub(super) fn handle_busy_control_command(
    store: &AppStore,
    argument_raw: &str,
) -> AppResult<String> {
    let action = argument_raw
        .split_whitespace()
        .next()
        .unwrap_or("status")
        .to_lowercase();
    if matches!(action.as_str(), "" | "status" | "show") {
        let config = store.config()?;
        return Ok(format_busy_input_mode_reply(&config.chat.busy_input_mode));
    }
    let normalized = normalize_busy_input_mode(&action);
    let recognized = matches!(
        action.as_str(),
        "queue" | "q" | "steer" | "inject" | "plan" | "interrupt" | "abort" | "replace"
    );
    if !recognized {
        return Ok("用法：/busy [queue|steer|interrupt|status]".into());
    }
    let mut config = store.config()?;
    config.chat.busy_input_mode = normalized;
    store.set_config(config)?;
    let config = store.config()?;
    Ok(format_busy_input_mode_reply(&config.chat.busy_input_mode))
}

fn format_busy_input_mode_reply(mode: &str) -> String {
    let normalized = normalize_busy_input_mode(mode);
    let description = match normalized.as_str() {
        "steer" => "新输入会注入当前运行中的 agent turn。",
        "interrupt" => "新输入会中止当前运行，并作为新的请求继续。",
        _ => "新输入会加入当前会话队列，等待运行结束后处理。",
    };
    format!("busyInputMode: {normalized}\n{description}")
}

pub(super) fn handle_topic_control_command(
    store: &AppStore,
    conversation: &Conversation,
    argument_raw: &str,
) -> AppResult<String> {
    let argument = argument_raw.trim();
    let current = conversation
        .metadata
        .get("telegramTopicSessionId")
        .and_then(Value::as_str)
        .unwrap_or("");
    if argument.is_empty() || matches!(argument.to_lowercase().as_str(), "status" | "show") {
        let status = if current.is_empty() { "off" } else { current };
        return Ok(format!(
            "Topic session：{status}\n当前会话 metadata key：telegramTopicSessionId"
        ));
    }
    if matches!(
        argument.to_lowercase().as_str(),
        "off" | "disable" | "disabled"
    ) {
        store.set_conversation_metadata_value(
            &conversation.id,
            "telegramTopicSessionId",
            Value::Null,
        )?;
        return Ok("Topic session：off".into());
    }
    store.set_conversation_metadata_value(
        &conversation.id,
        "telegramTopicSessionId",
        json!(argument),
    )?;
    Ok(format!("Topic session：{argument}"))
}

pub(super) fn handle_redraw_control_command(
    conversation: &Conversation,
    app: Option<&AppHandle>,
) -> AppResult<String> {
    if let Some(app) = app {
        let _ = app.emit(
            "synthchat-redraw-request",
            json!({
                "conversationId": conversation.id,
                "requestedAt": now_iso()
            }),
        );
        Ok("已请求桌面端刷新当前会话 UI。".into())
    } else {
        Ok("当前运行环境没有可用 AppHandle；桌面端会在下一次状态更新时刷新。".into())
    }
}

pub(super) fn handle_handoff_control_command(
    store: &AppStore,
    argument_raw: &str,
) -> AppResult<String> {
    let platform = argument_raw.trim().to_ascii_lowercase();
    let targets = send_message_external_targets(store)?;
    if platform.is_empty() || matches!(platform.as_str(), "status" | "show") {
        return format_sethome_status(store).map(|status| {
            format!("Handoff targets：\n{status}\n使用 /sethome <platform:target> 配置默认目标。")
        });
    }
    let target = targets.into_iter().find(|target| {
        target
            .get("platform")
            .and_then(Value::as_str)
            .map(|name| name.eq_ignore_ascii_case(&platform))
            .unwrap_or(false)
    });
    let Some(target) = target else {
        return Ok(format!(
            "未找到 handoff platform：{platform}。先用 /sethome 或配置页设置 external home target。"
        ));
    };
    let home = target
        .get("homeTarget")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if home.is_empty() {
        return Ok(format!(
            "{platform} 尚未配置 home target。先用 /sethome {platform}:<target>。"
        ));
    }
    Ok(format!(
        "Handoff target ready：{platform}:{home}\n桌面端不会自动迁移当前窗口；后续 gateway/cron 投递会使用该 home target。"
    ))
}

pub(super) fn handle_restart_control_command(
    store: &AppStore,
    argument_raw: &str,
) -> AppResult<String> {
    store.reload_from_disk()?;
    let suffix = if argument_raw.trim().is_empty() {
        ""
    } else {
        "\n外部 adapter restart 请使用：/platforms <platform> restart（若该 adapter 支持）。"
    };
    Ok(format!("已重载本地状态、配置和静态资源。{suffix}"))
}

pub(super) fn handle_gquota_control_command(store: &AppStore) -> AppResult<String> {
    let providers = store.providers()?;
    let gemini = providers
        .iter()
        .filter(|provider| {
            let haystack = format!(
                "{} {} {} {}",
                provider.id, provider.name, provider.provider_type, provider.model
            )
            .to_ascii_lowercase();
            haystack.contains("gemini") || haystack.contains("google")
        })
        .map(|provider| {
            format!(
                "- {} ({}) model={} enabled={}",
                provider.name, provider.id, provider.model, provider.enabled
            )
        })
        .collect::<Vec<_>>();
    if gemini.is_empty() {
        return Ok("Gemini quota：未发现 Gemini/Google LLM provider；当前未接入 Google Code Assist quota API。".into());
    }
    Ok(format!(
        "Gemini quota：当前未接入 Google Code Assist quota API。\n已配置 Gemini/Google provider：\n{}",
        gemini.join("\n")
    ))
}

pub(super) fn handle_update_control_command(
    store: &AppStore,
    argument_raw: &str,
) -> AppResult<String> {
    let skills = store.skills()?;
    let enabled = skills.iter().filter(|skill| skill.enabled).count();
    let action = argument_raw.trim();
    if matches!(action, "skills" | "skill") {
        return Ok("技能更新入口：/skills reload，或配置页使用 remote skill update。".into());
    }
    Ok(format!(
        "SynthChat v{}\n桌面端当前没有安全的自更新执行器；技能库：{} enabled / {} total。可用入口：/skills reload、配置页 remote skill update。",
        option_env!("CARGO_PKG_VERSION").unwrap_or("1.0.0"),
        enabled,
        skills.len()
    ))
}

pub(super) fn handle_backup_control_command(argument_raw: &str) -> AppResult<String> {
    let args = parse_backup_control_args(argument_raw)?;
    let mut command_args = vec!["backup".to_string()];
    if args.quick {
        command_args.push("--quick".into());
    }
    if let Some(label) = args.label {
        command_args.push("--label".into());
        command_args.push(label);
    }
    if let Some(output) = args.output {
        command_args.push("--output".into());
        command_args.push(output);
    }
    Ok(format_hermes_backup_cli_plan(
        "backup",
        &command_args,
        "Hermes backup",
        "hermes backup creates a zip archive of HERMES_HOME; SynthChat returns a managed-process plan so backup execution stays visible in desktop approvals/logs.",
    ))
}

pub(super) fn handle_import_control_command(argument_raw: &str) -> AppResult<String> {
    let args = parse_import_control_args(argument_raw)?;
    let mut command_args = vec!["import".to_string(), args.zipfile];
    if args.force {
        command_args.push("--force".into());
    }
    Ok(format_hermes_backup_cli_plan(
        "import",
        &command_args,
        "Hermes backup import",
        "hermes import restores a backup zip into HERMES_HOME and may overwrite files; SynthChat only returns a managed-process plan so restore execution requires explicit process/tool approval.",
    ))
}

#[derive(Debug, Default)]
struct BackupControlArgs {
    output: Option<String>,
    quick: bool,
    label: Option<String>,
}

#[derive(Debug)]
struct ImportControlArgs {
    zipfile: String,
    force: bool,
}

fn parse_backup_control_args(argument_raw: &str) -> AppResult<BackupControlArgs> {
    let mut args = BackupControlArgs::default();
    let tokens = shell_words(argument_raw);
    let mut index = 0;
    while index < tokens.len() {
        let token = tokens[index].clone();
        match token.as_str() {
            "-q" | "--quick" => {
                args.quick = true;
                index += 1;
            }
            "-o" | "--output" => {
                index += 1;
                let Some(value) = tokens.get(index).cloned() else {
                    return Err(AppError::BadRequest(
                        "用法：/backup [--quick] [--label <label>] [--output <path>]".into(),
                    ));
                };
                args.output = Some(value);
                index += 1;
            }
            "-l" | "--label" => {
                index += 1;
                let Some(value) = tokens.get(index).cloned() else {
                    return Err(AppError::BadRequest(
                        "用法：/backup [--quick] [--label <label>] [--output <path>]".into(),
                    ));
                };
                args.label = Some(value);
                index += 1;
            }
            _ if token.starts_with("--output=") => {
                args.output = Some(token["--output=".len()..].to_string());
                index += 1;
            }
            _ if token.starts_with("--label=") => {
                args.label = Some(token["--label=".len()..].to_string());
                index += 1;
            }
            _ if !token.starts_with('-') && args.output.is_none() => {
                args.output = Some(token);
                index += 1;
            }
            _ => {
                return Err(AppError::BadRequest(format!(
                    "unknown /backup argument: {token}"
                )));
            }
        }
    }
    Ok(args)
}

fn parse_import_control_args(argument_raw: &str) -> AppResult<ImportControlArgs> {
    let tokens = shell_words(argument_raw);
    let mut zipfile = None;
    let mut force = false;
    for token in tokens {
        match token.as_str() {
            "-f" | "--force" => force = true,
            _ if token.starts_with('-') => {
                return Err(AppError::BadRequest(format!(
                    "unknown /import argument: {token}"
                )));
            }
            _ if zipfile.is_none() => zipfile = Some(token),
            _ => {
                return Err(AppError::BadRequest(
                    "用法：/import <backup.zip> [--force]".into(),
                ));
            }
        }
    }
    let zipfile = zipfile
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| AppError::BadRequest("用法：/import <backup.zip> [--force]".into()))?;
    Ok(ImportControlArgs { zipfile, force })
}

fn format_hermes_backup_cli_plan(
    action: &str,
    command_args: &[String],
    label: &str,
    boundary: &str,
) -> String {
    let command = format_hermes_cli_command(command_args);
    let task_id = format!("hermes-{}-{}", action, stable_task_suffix(command_args));
    let plan = json!({
        "schema": "hermes_backup_cli_plan_desktop_v1",
        "status": "external_cli_plan",
        "ok": true,
        "action": action,
        "command": command,
        "commandText": command,
        "command_text": command,
        "args": command_args,
        "requiresApproval": true,
        "requires_approval": true,
        "managedProcessStartPayload": {
            "action": "start",
            "label": label,
            "command": command,
            "taskId": task_id,
            "notifyOnComplete": true,
            "watchPatterns": ["Backup", "Import", "restored", "error", "complete"]
        },
        "managed_process_start_payload": {
            "action": "start",
            "label": label,
            "command": command,
            "taskId": task_id,
            "task_id": task_id,
            "notifyOnComplete": true,
            "notify_on_complete": true,
            "watchPatterns": ["Backup", "Import", "restored", "error", "complete"],
            "watch_patterns": ["Backup", "Import", "restored", "error", "complete"]
        },
        "boundary": boundary
    });
    serde_json::to_string_pretty(&plan).unwrap_or_else(|_| plan.to_string())
}

fn format_hermes_cli_command(args: &[String]) -> String {
    std::iter::once("hermes".to_string())
        .chain(args.iter().map(|arg| shell_quote(arg)))
        .collect::<Vec<_>>()
        .join(" ")
}

fn stable_task_suffix(args: &[String]) -> String {
    let joined = args.join("-");
    let mut out = String::new();
    let mut previous_dash = false;
    for ch in joined.chars().flat_map(char::to_lowercase) {
        let mapped = if ch.is_ascii_alphanumeric() { ch } else { '-' };
        if mapped == '-' {
            if previous_dash {
                continue;
            }
            previous_dash = true;
        } else {
            previous_dash = false;
        }
        out.push(mapped);
        if out.len() >= 48 {
            break;
        }
    }
    out.trim_matches('-').to_string()
}

fn shell_quote(value: &str) -> String {
    if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '/' | ':' | '\\'))
    {
        return value.to_string();
    }
    format!("\"{}\"", value.replace('"', "\\\""))
}

fn shell_words(argument_raw: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut chars = argument_raw.chars().peekable();
    let mut quote = None;
    while let Some(ch) = chars.next() {
        if let Some(active) = quote {
            if ch == active {
                quote = None;
            } else if ch == '\\' && active == '"' {
                if matches!(chars.peek(), Some('"') | Some('\\')) {
                    if let Some(next) = chars.next() {
                        current.push(next);
                    }
                } else {
                    current.push(ch);
                }
            } else {
                current.push(ch);
            }
            continue;
        }
        match ch {
            '\'' | '"' => quote = Some(ch),
            ch if ch.is_whitespace() => {
                if !current.is_empty() {
                    words.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(ch),
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

pub(super) fn handle_quit_control_command(argument_raw: &str) -> String {
    if argument_raw
        .split_whitespace()
        .any(|part| part == "--delete")
    {
        "桌面端 /quit 不会直接关闭应用；如需删除当前会话历史，请使用 /history clear。".into()
    } else {
        "桌面端 /quit 不会直接关闭应用；请使用窗口关闭按钮结束应用。".into()
    }
}

pub(super) fn handle_paste_control_command() -> String {
    "桌面端已支持在输入框粘贴剪贴板图片/文件；粘贴后会作为附件随下一条消息发送。".into()
}

pub(super) fn handle_image_control_command(argument_raw: &str) -> String {
    let path = argument_raw.trim();
    if path.is_empty() {
        return "用法：/image <local-path>；桌面端也可点击图片附件按钮选择文件。".into();
    }
    let mime = image_mime_from_control_path(path);
    let attachment = json!({
        "type": "attachment",
        "id": new_id("image"),
        "fileName": std::path::Path::new(path)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("image"),
        "mimeType": mime,
        "fileSize": 0,
        "path": path,
        "recommendedTool": "vision_analyze"
    });
    format!("可将以下附件引用随下一条消息发送：\n{}", attachment)
}

fn image_mime_from_control_path(path: &str) -> &'static str {
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        "image/jpeg"
    } else if lower.ends_with(".webp") {
        "image/webp"
    } else if lower.ends_with(".gif") {
        "image/gif"
    } else if lower.ends_with(".bmp") {
        "image/bmp"
    } else if lower.ends_with(".svg") {
        "image/svg+xml"
    } else {
        "image/png"
    }
}

pub(super) fn handle_voice_control_command(
    store: &AppStore,
    persona: &Persona,
    argument_raw: &str,
) -> AppResult<String> {
    let action = argument_raw
        .split_whitespace()
        .next()
        .unwrap_or("status")
        .to_lowercase();
    if matches!(action.as_str(), "" | "status" | "show") {
        return format_voice_control_reply(store, persona);
    }
    let enabled = match action.as_str() {
        "on" | "enable" | "enabled" | "true" | "yes" | "tts" => true,
        "off" | "disable" | "disabled" | "false" | "no" => false,
        _ => return Ok("用法：/voice [on|off|tts|status]".into()),
    };
    let mut saved = persona.clone();
    if !saved.voice_reply.is_object() {
        saved.voice_reply = json!({});
    }
    if let Some(object) = saved.voice_reply.as_object_mut() {
        object.insert("enabled".into(), json!(enabled));
    }
    let saved = store.save_persona(saved)?;
    format_voice_control_reply(store, &saved)
}

fn format_voice_control_reply(store: &AppStore, persona: &Persona) -> AppResult<String> {
    let enabled = persona
        .voice_reply
        .get("enabled")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let engine = persona
        .voice_reply
        .get("engine")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let status = serde_json::from_str::<Value>(&voice_status_tool(store, &json!({}))?)
        .unwrap_or_else(|_| json!({}));
    Ok(format!(
        "Voice mode：\n- persona: {} ({})\n- enabled: {}\n- engine: {}\n\nRuntime readiness:\n- available: {}\n- audioAvailable: {}\n- sttAvailable: {}\n- ttsAvailable: {}\n- captureBackend: {}\n- playback: {}\n- sttProvider: {} ({})\n- ttsProvider: {} ({})",
        persona.name,
        persona.id,
        enabled,
        engine,
        status.get("available").and_then(Value::as_bool).unwrap_or(false),
        status.get("audioAvailable").and_then(Value::as_bool).unwrap_or(false),
        status.get("sttAvailable").and_then(Value::as_bool).unwrap_or(false),
        status.get("ttsAvailable").and_then(Value::as_bool).unwrap_or(false),
        status
            .get("audioCapture")
            .and_then(|value| value.get("backend"))
            .and_then(Value::as_str)
            .unwrap_or("none"),
        status
            .get("playback")
            .and_then(|value| value.get("command"))
            .and_then(Value::as_str)
            .unwrap_or("none"),
        status
            .get("sttProvider")
            .and_then(|value| value.get("providerId"))
            .and_then(Value::as_str)
            .unwrap_or("-"),
        status
            .get("sttProvider")
            .and_then(|value| value.get("reason"))
            .and_then(Value::as_str)
            .unwrap_or("ready"),
        status
            .get("ttsProvider")
            .and_then(|value| value.get("providerId"))
            .and_then(Value::as_str)
            .unwrap_or("-"),
        status
            .get("ttsProvider")
            .and_then(|value| value.get("reason"))
            .and_then(Value::as_str)
            .unwrap_or("ready")
    ))
}

pub(super) fn handle_personality_control_command(
    store: &AppStore,
    conversation: &Conversation,
    argument_raw: &str,
) -> AppResult<String> {
    let selector = argument_raw.trim();
    let personas = store.personas()?;
    let current_id = conversation.persona_id.as_deref().unwrap_or("default");
    if selector.is_empty()
        || matches!(
            selector.to_lowercase().as_str(),
            "status" | "show" | "list" | "ls"
        )
    {
        return Ok(format_personality_control_status(
            &personas,
            current_id,
            conversation,
        ));
    }
    let Some(persona) = resolve_persona_selector(&personas, selector)? else {
        return Ok(format!(
            "未找到 persona：{selector}\n用法：/personality [name|id]"
        ));
    };
    let saved = store.set_conversation_persona(&conversation.id, persona.id.clone())?;
    store.set_conversation_metadata_value(
        &conversation.id,
        "personalitySwitchSource",
        json!("control-command"),
    )?;
    Ok(format!(
        "已切换当前会话 persona：{} ({})\nagent: {}\nconversation: {}",
        persona.name, persona.id, saved.agent_id, saved.id
    ))
}

fn format_personality_control_status(
    personas: &[Persona],
    current_id: &str,
    conversation: &Conversation,
) -> String {
    let rows = personas
        .iter()
        .take(16)
        .map(|persona| {
            let marker = if persona.id == current_id { "*" } else { " " };
            format!(
                "{marker} {} ({}) agent={}",
                persona.name, persona.id, persona.agent_id
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let suffix = if personas.len() > 16 {
        format!("\n... 还有 {} 个未显示", personas.len() - 16)
    } else {
        String::new()
    };
    format!(
        "Personality：\n- conversation: {} ({})\n- currentPersona: {}\n{}\n{}{}",
        conversation.title,
        conversation.id,
        current_id,
        if rows.is_empty() {
            "无可用 persona"
        } else {
            "可用 persona："
        },
        rows,
        suffix
    )
}

fn resolve_persona_selector<'a>(
    personas: &'a [Persona],
    selector: &str,
) -> AppResult<Option<&'a Persona>> {
    let query = selector.trim().to_lowercase();
    if query.is_empty() {
        return Ok(None);
    }
    let mut matches = personas
        .iter()
        .filter(|persona| {
            persona.id.to_lowercase() == query || persona.name.to_lowercase() == query
        })
        .collect::<Vec<_>>();
    if matches.is_empty() {
        matches = personas
            .iter()
            .filter(|persona| {
                persona.id.to_lowercase().contains(&query)
                    || persona.name.to_lowercase().contains(&query)
            })
            .collect::<Vec<_>>();
    }
    match matches.len() {
        0 => Ok(None),
        1 => Ok(matches.first().copied()),
        _ => Err(AppError::BadRequest(format!(
            "persona selector {selector} 匹配多个 persona：{}",
            matches
                .iter()
                .map(|persona| format!("{} ({})", persona.name, persona.id))
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

pub(super) fn handle_fast_control_command(
    store: &AppStore,
    argument_raw: &str,
) -> AppResult<String> {
    let action = argument_raw
        .split_whitespace()
        .next()
        .unwrap_or("status")
        .to_lowercase();
    match action.as_str() {
        "" | "status" | "show" => {
            let config = store.config()?;
            Ok(format_fast_control_reply(config.chat.fast_mode_enabled))
        }
        "fast" | "on" | "enable" | "enabled" | "true" | "yes" => set_fast_mode_enabled(store, true),
        "normal" | "off" | "disable" | "disabled" | "false" | "no" => {
            set_fast_mode_enabled(store, false)
        }
        _ => Ok("用法：/fast [status|fast|normal|on|off]".into()),
    }
}

fn set_fast_mode_enabled(store: &AppStore, enabled: bool) -> AppResult<String> {
    let mut config = store.config()?;
    config.chat.fast_mode_enabled = enabled;
    store.set_config(config)?;
    Ok(format_fast_control_reply(enabled))
}

fn format_fast_control_reply(enabled: bool) -> String {
    let mode = if enabled { "fast" } else { "normal" };
    format!(
        "Fast mode：{mode}\n- OpenAI/Responses: service_tier=priority when enabled\n- Anthropic: speed=fast with fast-mode beta header when enabled"
    )
}

pub(super) fn handle_footer_control_command(
    store: &AppStore,
    argument_raw: &str,
) -> AppResult<String> {
    let action = argument_raw
        .split_whitespace()
        .next()
        .unwrap_or("status")
        .to_lowercase();
    match action.as_str() {
        "" | "status" | "show" => {
            let config = store.config()?;
            Ok(format_footer_control_reply(
                config.chat.runtime_footer_enabled,
            ))
        }
        "on" | "enable" | "enabled" | "true" | "yes" => set_runtime_footer_enabled(store, true),
        "off" | "disable" | "disabled" | "false" | "no" => set_runtime_footer_enabled(store, false),
        _ => Ok("用法：/footer [on|off|status]".into()),
    }
}

fn set_runtime_footer_enabled(store: &AppStore, enabled: bool) -> AppResult<String> {
    let mut config = store.config()?;
    config.chat.runtime_footer_enabled = enabled;
    store.set_config(config)?;
    Ok(format_footer_control_reply(enabled))
}

fn format_footer_control_reply(enabled: bool) -> String {
    let mode = if enabled { "enabled" } else { "disabled" };
    let snapshot = json!({
        "schema": "hermes_runtime_footer_desktop_v1",
        "enabled": enabled,
        "effectiveConfig": {
            "enabled": enabled,
            "fields": ["model", "context_pct", "cwd"],
            "separator": " · ",
            "scope": "global_desktop",
            "platformOverrides": false
        },
        "deliverySurface": {
            "finalReplyOnly": true,
            "streamingTrailingFooter": true,
            "scheduledDelivery": true,
            "gatewayDelivery": true,
            "toolProgressUpdates": false,
            "partialStreamingDeltas": false
        },
        "runtimeFields": {
            "model": "short provider/model suffix when available",
            "contextPct": "computed when context length is known",
            "cwd": "home-relative cwd when available"
        },
        "desktopBoundary": "SynthChat stores the footer toggle in chat.runtime_footer_enabled and applies it to scheduled/gateway final delivery replies; Hermes YAML per-platform footer overrides are represented as diagnostics until a Python gateway daemon is embedded."
    });
    format!(
        "Runtime footer：{mode}\n当前作用范围：scheduled/gateway final delivery replies。\n{}",
        serde_json::to_string_pretty(&snapshot).unwrap_or_else(|_| snapshot.to_string())
    )
}

pub(super) fn handle_statusbar_control_command(
    store: &AppStore,
    argument_raw: &str,
) -> AppResult<String> {
    let action = argument_raw
        .split_whitespace()
        .next()
        .unwrap_or("status")
        .to_lowercase();
    match action.as_str() {
        "" | "status" | "show" => {
            let config = store.config()?;
            Ok(format_statusbar_control_reply(
                config.chat.statusbar_enabled,
                &config.chat.tool_progress_display,
            ))
        }
        "on" | "enable" | "enabled" | "true" | "yes" => set_statusbar_enabled(store, true),
        "off" | "disable" | "disabled" | "false" | "no" => set_statusbar_enabled(store, false),
        "toggle" => {
            let enabled = !store.config()?.chat.statusbar_enabled;
            set_statusbar_enabled(store, enabled)
        }
        _ => Ok("用法：/statusbar [on|off|toggle|status]".into()),
    }
}

fn set_statusbar_enabled(store: &AppStore, enabled: bool) -> AppResult<String> {
    let mut config = store.config()?;
    config.chat.statusbar_enabled = enabled;
    let tool_progress_display = config.chat.tool_progress_display.clone();
    store.set_config(config)?;
    Ok(format_statusbar_control_reply(
        enabled,
        &tool_progress_display,
    ))
}

fn format_statusbar_control_reply(enabled: bool, tool_progress_display: &str) -> String {
    let mode = if enabled { "enabled" } else { "disabled" };
    let snapshot = json!({
        "schema": "hermes_display_statusbar_desktop_v1",
        "enabled": enabled,
        "effectiveConfig": {
            "statusbarEnabled": enabled,
            "toolProgress": tool_progress_display,
            "showReasoning": false,
            "toolPreviewLength": if enabled { 40 } else { 0 },
            "streaming": null,
            "interimAssistantMessages": true,
            "longRunningNotifications": true,
            "busyAckDetail": true,
            "cleanupProgress": false
        },
        "sessionContext": {
            "storage": "explicit Rust run/conversation context",
            "legacyHermesEnvNames": [
                "HERMES_SESSION_PLATFORM",
                "HERMES_SESSION_CHAT_ID",
                "HERMES_SESSION_CHAT_NAME",
                "HERMES_SESSION_THREAD_ID",
                "HERMES_SESSION_USER_ID",
                "HERMES_SESSION_USER_NAME",
                "HERMES_SESSION_KEY",
                "HERMES_SESSION_ID",
                "HERMES_SESSION_MESSAGE_ID"
            ],
            "concurrencySafe": true,
            "processGlobalEnvFallback": false
        },
        "platformDefaults": {
            "desktop": {
                "toolProgress": tool_progress_display,
                "statusbar": enabled
            },
            "gateway": "Hermes per-platform display tiers are surfaced through platform capability/status diagnostics; SynthChat uses desktop UI state plus explicit delivery metadata."
        }
    });
    format!(
        "Statusbar：{mode}\n当前作用范围：桌面端上下文/模型状态展示配置。\n{}",
        serde_json::to_string_pretty(&snapshot).unwrap_or_else(|_| snapshot.to_string())
    )
}

pub(super) fn handle_verbose_control_command(
    store: &AppStore,
    argument_raw: &str,
) -> AppResult<String> {
    let action = argument_raw
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_lowercase();
    if matches!(action.as_str(), "" | "cycle") {
        let current = store.config()?.chat.tool_progress_display;
        let next = match current.as_str() {
            "off" => "new",
            "new" => "all",
            "all" => "verbose",
            _ => "off",
        };
        return set_tool_progress_display(store, next);
    }
    match action.as_str() {
        "status" | "show" => {
            let config = store.config()?;
            Ok(format_verbose_control_reply(
                &config.chat.tool_progress_display,
            ))
        }
        "off" | "new" | "all" | "verbose" => set_tool_progress_display(store, &action),
        _ => Ok("用法：/verbose [off|new|all|verbose|status]；不带参数时循环切换。".into()),
    }
}

fn set_tool_progress_display(store: &AppStore, mode: &str) -> AppResult<String> {
    let mut config = store.config()?;
    config.chat.tool_progress_display = mode.to_string();
    store.set_config(config)?;
    Ok(format_verbose_control_reply(mode))
}

fn format_verbose_control_reply(mode: &str) -> String {
    format!("Tool progress display：{mode}\n当前作用范围：桌面端工具/ACP 进度展示策略。")
}

pub(super) fn handle_skin_control_command(
    store: &AppStore,
    argument_raw: &str,
) -> AppResult<String> {
    let skin = argument_raw.trim();
    if skin.is_empty() || matches!(skin.to_lowercase().as_str(), "status" | "show") {
        let config = store.config()?;
        return Ok(format!("Skin：{}", config.chat.display_skin));
    }
    if skin.chars().any(char::is_whitespace) {
        return Ok("用法：/skin [name]；name 不能包含空白字符。".into());
    }
    let mut config = store.config()?;
    config.chat.display_skin = skin.to_string();
    store.set_config(config)?;
    Ok(format!("Skin：{skin}"))
}

pub(super) fn handle_indicator_control_command(
    store: &AppStore,
    argument_raw: &str,
) -> AppResult<String> {
    let action = argument_raw
        .split_whitespace()
        .next()
        .unwrap_or("status")
        .to_lowercase();
    match action.as_str() {
        "" | "status" | "show" => {
            let config = store.config()?;
            Ok(format!("Indicator：{}", config.chat.busy_indicator_style))
        }
        "kaomoji" | "emoji" | "unicode" | "ascii" => {
            let mut config = store.config()?;
            config.chat.busy_indicator_style = action.clone();
            store.set_config(config)?;
            Ok(format!("Indicator：{action}"))
        }
        _ => Ok("用法：/indicator [kaomoji|emoji|unicode|ascii|status]".into()),
    }
}

pub(super) fn handle_codex_runtime_control_command(
    store: &AppStore,
    argument_raw: &str,
) -> AppResult<String> {
    let action = argument_raw
        .split_whitespace()
        .next()
        .unwrap_or("status")
        .to_lowercase();
    match action.as_str() {
        "" | "status" | "show" => {
            let config = store.config()?;
            Ok(format_codex_runtime_control_reply(
                &config.chat.codex_runtime,
            ))
        }
        "auto" | "codex_app_server" | "codex-app-server" | "app-server" => {
            let mode = if action == "auto" {
                "auto"
            } else {
                "codex_app_server"
            };
            let mut config = store.config()?;
            config.chat.codex_runtime = mode.into();
            store.set_config(config)?;
            Ok(format_codex_runtime_control_reply(mode))
        }
        _ => Ok("用法：/codex-runtime [auto|codex_app_server|status]".into()),
    }
}

fn format_codex_runtime_control_reply(mode: &str) -> String {
    format!("Codex runtime：{mode}\n当前作用范围：OpenAI/Codex runtime preference 配置。")
}

pub(super) fn handle_reasoning_control_command(
    store: &AppStore,
    argument_raw: &str,
) -> AppResult<String> {
    let mut parts = argument_raw.split_whitespace();
    let action = parts.next().unwrap_or("status").to_lowercase();
    match action.as_str() {
        "" | "status" | "show" => {
            let config = store.config()?;
            Ok(format_reasoning_control_reply(
                config.chat.responses_reasoning_replay_enabled,
            ))
        }
        "on" | "enable" | "enabled" | "show-replay" | "replay-on" => {
            set_reasoning_replay_enabled(store, true)
        }
        "off" | "disable" | "disabled" | "hide" | "hide-replay" | "replay-off" => {
            set_reasoning_replay_enabled(store, false)
        }
        "replay" => {
            let Some(value) = parts.next() else {
                let config = store.config()?;
                return Ok(format_reasoning_control_reply(
                    config.chat.responses_reasoning_replay_enabled,
                ));
            };
            match value.to_lowercase().as_str() {
                "on" | "enable" | "enabled" | "true" | "yes" => {
                    set_reasoning_replay_enabled(store, true)
                }
                "off" | "disable" | "disabled" | "false" | "no" => {
                    set_reasoning_replay_enabled(store, false)
                }
                _ => Ok("用法：/reasoning [status|on|off|replay on|replay off]".into()),
            }
        }
        "effort" | "level" | "none" | "minimal" | "low" | "medium" | "high" | "xhigh" => Ok(
            "SynthChat 当前尚未把 Hermes reasoning_effort 接入 LLM 请求；/reasoning 目前可控制 Responses reasoning replay：/reasoning on|off。"
                .into(),
        ),
        _ => Ok("用法：/reasoning [status|on|off|replay on|replay off]".into()),
    }
}

fn set_reasoning_replay_enabled(store: &AppStore, enabled: bool) -> AppResult<String> {
    let mut config = store.config()?;
    config.chat.responses_reasoning_replay_enabled = enabled;
    store.set_config(config)?;
    let config = store.config()?;
    Ok(format_reasoning_control_reply(
        config.chat.responses_reasoning_replay_enabled,
    ))
}

fn format_reasoning_control_reply(responses_reasoning_replay_enabled: bool) -> String {
    let replay = if responses_reasoning_replay_enabled {
        "enabled"
    } else {
        "disabled"
    };
    format!(
        "Reasoning：\n- responsesReasoningReplay: {replay}\n- effect: controls whether stored Responses reasoning items are replayed on subsequent compatible requests"
    )
}

pub(super) fn handle_copy_control_command(
    store: &AppStore,
    conversation: &Conversation,
    argument_raw: &str,
) -> AppResult<String> {
    let ordinal = argument_raw
        .split_whitespace()
        .next()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(1);
    let messages = store.messages(&conversation.id, None)?;
    let assistant_messages = messages
        .iter()
        .filter(|message| message.role == "assistant" && !message.content.trim().is_empty())
        .collect::<Vec<_>>();
    let Some(message) = assistant_messages.iter().rev().nth(ordinal - 1) else {
        return Ok("当前会话没有可复制的 assistant 回复。".into());
    };
    Ok(format!(
        "最近第 {} 条 assistant 回复（{} chars）：\n{}",
        ordinal,
        message.content.chars().count(),
        message.content
    ))
}

pub(super) fn handle_save_control_command(
    store: &AppStore,
    conversation: &Conversation,
    argument_raw: &str,
) -> AppResult<String> {
    let format = argument_raw
        .split_whitespace()
        .next()
        .unwrap_or("markdown")
        .to_lowercase();
    if !matches!(format.as_str(), "" | "md" | "markdown") {
        return Ok("用法：/save [markdown]".into());
    }
    let messages = store.messages(&conversation.id, None)?;
    let export_dir = store.data_dir().join("exports").join("conversations");
    fs::create_dir_all(&export_dir)?;
    let path = export_dir.join(format!("{}.md", conversation.id));
    let content = conversation_markdown_export(conversation, &messages);
    fs::write(&path, content)?;
    Ok(format!(
        "会话已保存：{}\n- messages: {}\n- title: {}",
        path.to_string_lossy(),
        messages.len(),
        conversation.title
    ))
}

fn conversation_markdown_export(conversation: &Conversation, messages: &[ChatMessage]) -> String {
    let mut lines = Vec::new();
    lines.push(format!("# {}", markdown_single_line(&conversation.title)));
    lines.push(String::new());
    lines.push(format!("- conversationId: {}", conversation.id));
    lines.push(format!("- createdAt: {}", conversation.created_at));
    lines.push(format!("- updatedAt: {}", conversation.updated_at));
    lines.push(format!("- agentId: {}", conversation.agent_id));
    if let Some(persona_id) = conversation.persona_id.as_deref() {
        lines.push(format!("- personaId: {persona_id}"));
    }
    lines.push(String::new());
    for message in messages {
        lines.push(format!(
            "## {} - {}",
            markdown_single_line(&message.role),
            message.created_at
        ));
        lines.push(String::new());
        lines.push(message.content.trim().to_string());
        lines.push(String::new());
    }
    lines.join("\n")
}

fn markdown_single_line(value: &str) -> String {
    value.replace(['\r', '\n'], " ").trim().to_string()
}

pub(super) fn handle_goal_control_command(
    store: &AppStore,
    conversation: &Conversation,
    argument_raw: &str,
    app: Option<&AppHandle>,
) -> AppResult<String> {
    let trimmed = argument_raw.trim();
    let mut parts = trimmed.split_whitespace();
    let action = parts.next().unwrap_or("").to_lowercase();
    match action.as_str() {
        "" | "status" | "show" | "list" => {
            let state = goal_state::agent_goal_status(store, &conversation.id)?;
            Ok(format_agent_goal_status(state.as_ref()))
        }
        "pause" => {
            let state = goal_state::pause_agent_goal(store, &conversation.id, Some("user-paused"))?;
            emit_agent_goal_event(
                app,
                "paused",
                &conversation.id,
                state.as_ref(),
                state
                    .as_ref()
                    .and_then(|state| state.paused_reason.as_deref()),
            );
            Ok(format_agent_goal_status(state.as_ref()))
        }
        "resume" => {
            let state = goal_state::resume_agent_goal(store, &conversation.id, true)?;
            emit_agent_goal_event(app, "resumed", &conversation.id, state.as_ref(), None);
            Ok(format_agent_goal_status(state.as_ref()))
        }
        "clear" | "done" | "remove" | "rm" => {
            let state = goal_state::clear_agent_goal(store, &conversation.id)?;
            emit_agent_goal_event(app, "cleared", &conversation.id, state.as_ref(), None);
            Ok("已清除当前 standing goal。".into())
        }
        "set" if !parts.collect::<Vec<_>>().is_empty() => {
            let rest = trimmed[action.len()..].trim();
            set_goal_from_control_args(store, conversation, rest, app)
        }
        _ => set_goal_from_control_args(store, conversation, trimmed, app),
    }
}

fn set_goal_from_control_args(
    store: &AppStore,
    conversation: &Conversation,
    raw: &str,
    app: Option<&AppHandle>,
) -> AppResult<String> {
    let (goal, max_turns) = parse_goal_text_and_max_turns(raw);
    if goal.trim().is_empty() {
        return Ok("用法：/goal <text> 或 /goal pause|resume|clear|status".into());
    }
    let state = goal_state::set_agent_goal(store, &conversation.id, &goal, max_turns)?;
    emit_agent_goal_event(app, "set", &conversation.id, Some(&state), None);
    Ok(format!(
        "已设置 standing goal。\n{}",
        format_agent_goal_status(Some(&state))
    ))
}

fn parse_goal_text_and_max_turns(raw: &str) -> (String, Option<u32>) {
    let mut max_turns = None;
    let mut kept = Vec::new();
    let mut parts = raw.split_whitespace();
    while let Some(part) = parts.next() {
        if part == "--max-turns" || part == "--max" {
            if let Some(value) = parts.next().and_then(|value| value.parse::<u32>().ok()) {
                max_turns = Some(value);
            }
            continue;
        }
        if let Some(value) = part
            .strip_prefix("--max-turns=")
            .or_else(|| part.strip_prefix("--max="))
            .and_then(|value| value.parse::<u32>().ok())
        {
            max_turns = Some(value);
            continue;
        }
        kept.push(part);
    }
    (kept.join(" "), max_turns)
}

pub(super) fn handle_subgoal_control_command(
    store: &AppStore,
    conversation: &Conversation,
    argument_raw: &str,
    app: Option<&AppHandle>,
) -> AppResult<String> {
    let trimmed = argument_raw.trim();
    let mut parts = trimmed.split_whitespace();
    let action = parts.next().unwrap_or("").to_lowercase();
    match action.as_str() {
        "" | "status" | "list" | "show" => {
            let state = goal_state::agent_goal_status(store, &conversation.id)?;
            Ok(format_agent_subgoals(state.as_ref()))
        }
        "remove" | "rm" | "delete" | "del" => {
            let index = parts
                .next()
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(0);
            let state = goal_state::remove_agent_subgoal(store, &conversation.id, index)?;
            emit_agent_goal_event(
                app,
                "subgoal_removed",
                &conversation.id,
                state.as_ref(),
                None,
            );
            Ok(format_agent_subgoals(state.as_ref()))
        }
        "clear" => {
            let state = goal_state::clear_agent_subgoals(store, &conversation.id)?;
            emit_agent_goal_event(
                app,
                "subgoals_cleared",
                &conversation.id,
                state.as_ref(),
                None,
            );
            Ok("已清空当前 standing goal 的 subgoals。".into())
        }
        _ => {
            if trimmed.is_empty() {
                return Ok("用法：/subgoal <text> 或 /subgoal remove N|clear".into());
            }
            let state = goal_state::add_agent_subgoal(store, &conversation.id, trimmed)?;
            emit_agent_goal_event(app, "subgoal_added", &conversation.id, state.as_ref(), None);
            Ok(format_agent_subgoals(state.as_ref()))
        }
    }
}

fn format_agent_goal_status(state: Option<&crate::models::AgentGoalState>) -> String {
    let Some(state) = state else {
        return "当前没有 standing goal。使用 /goal <text> 设置。".into();
    };
    let subgoals = if state.subgoals.is_empty() {
        String::new()
    } else {
        format!(", {} subgoal(s)", state.subgoals.len())
    };
    let reason = state
        .paused_reason
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .map(|value| format!(" - {value}"))
        .unwrap_or_default();
    format!(
        "Goal ({}, {}/{} turns{}{}): {}",
        state.status, state.turns_used, state.max_turns, subgoals, reason, state.goal
    )
}

fn format_agent_subgoals(state: Option<&crate::models::AgentGoalState>) -> String {
    let Some(state) = state else {
        return "当前没有 standing goal。先使用 /goal <text> 设置。".into();
    };
    if state.subgoals.is_empty() {
        return "当前 goal 没有 subgoals。使用 /subgoal <text> 添加。".into();
    }
    let lines = state
        .subgoals
        .iter()
        .enumerate()
        .map(|(index, value)| format!("- {}. {}", index + 1, value))
        .collect::<Vec<_>>()
        .join("\n");
    format!("当前 subgoals：\n{lines}")
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

pub(super) fn handle_platform_tools_control_command(
    store: &AppStore,
    argument_raw: &str,
) -> AppResult<String> {
    let default_agent = store.agent(None)?;
    let (counts, all_names, _) = agent_toolset_inventory(store, &default_agent)?;
    let (action, platform, names) = parse_platform_tools_args(argument_raw);
    let (toolset_names, mcp_targets): (Vec<_>, Vec<_>) = names
        .into_iter()
        .partition(|name| !looks_like_mcp_tool_target(name));
    let mut config = store.config()?;
    let mut configured = platform_toolsets_map(&config);
    let mut mcp_servers = store.static_list("mcpServers")?;
    let mut mcp_changed = false;
    let mut mcp_missing_servers = BTreeSet::new();
    match action.as_str() {
        "" | "list" | "status" | "show" => {}
        "enable" => {
            if toolset_names.is_empty() && mcp_targets.is_empty() {
                return Ok(
                    "用法：/platform-tools enable <toolset|server:tool...> [--platform cli]".into(),
                );
            }
            ensure_known_toolsets(&toolset_names, &all_names)?;
            if !toolset_names.is_empty() {
                let enabled = configured
                    .entry(platform.clone())
                    .or_insert_with(BTreeSet::new);
                for name in toolset_names {
                    enabled.insert(name);
                }
                persist_platform_toolsets_map(&mut config, &configured);
                store.set_config(config)?;
            }
            if !mcp_targets.is_empty() {
                let result = apply_mcp_tool_filter_changes(&mut mcp_servers, &mcp_targets, true);
                mcp_changed = result.changed;
                mcp_missing_servers = result.missing_servers;
                if mcp_changed {
                    store.set_mcp_servers(mcp_servers.clone())?;
                }
            }
        }
        "disable" => {
            if toolset_names.is_empty() && mcp_targets.is_empty() {
                return Ok(
                    "用法：/platform-tools disable <toolset|server:tool...> [--platform cli]"
                        .into(),
                );
            }
            ensure_known_toolsets(&toolset_names, &all_names)?;
            if !toolset_names.is_empty() {
                let enabled = configured.entry(platform.clone()).or_insert_with(|| {
                    counts
                        .keys()
                        .filter(|name| !hermes_platform_toolset_default_off(name))
                        .cloned()
                        .collect()
                });
                for name in toolset_names {
                    enabled.remove(&name);
                }
                persist_platform_toolsets_map(&mut config, &configured);
                store.set_config(config)?;
            }
            if !mcp_targets.is_empty() {
                let result = apply_mcp_tool_filter_changes(&mut mcp_servers, &mcp_targets, false);
                mcp_changed = result.changed;
                mcp_missing_servers = result.missing_servers;
                if mcp_changed {
                    store.set_mcp_servers(mcp_servers.clone())?;
                }
            }
        }
        "reset" | "clear" => {
            configured.remove(&platform);
            persist_platform_toolsets_map(&mut config, &configured);
            store.set_config(config)?;
        }
        _ => {
            return Ok(
                "用法：/platform-tools [list [platform]|enable <toolset...> [--platform cli]|disable <toolset...> [--platform cli]|reset [platform]]"
                    .into(),
            );
        }
    }
    Ok(format_platform_tools_control_reply(
        &platform,
        &configured,
        &counts,
        &mcp_servers,
        &mcp_missing_servers,
        mcp_changed,
    ))
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

pub(super) fn handle_reload_mcp_control_command(store: &AppStore) -> AppResult<String> {
    store.reload_from_disk()?;
    let servers = store.static_list("mcpServers")?;
    let enabled = servers
        .iter()
        .filter(|server| {
            server
                .get("enabled")
                .and_then(Value::as_bool)
                .unwrap_or(true)
        })
        .count();
    Ok(format!(
        "已重新读取 MCP server 配置：{} enabled / {} total。当前 agent 会在下一轮工具定义解析时使用最新配置。",
        enabled,
        servers.len()
    ))
}

pub(super) fn handle_reload_control_command(store: &AppStore) -> AppResult<String> {
    store.reload_from_disk()?;
    let config = store.config()?;
    let providers = store.providers()?;
    let personas = store.personas()?;
    let agents = store.agents()?;
    let mcp_servers = store.static_list("mcpServers")?;
    Ok(format!(
        "已重新读取磁盘状态：agentEngine={} approvalMode={} providers={} personas={} agents={} mcpServers={}。",
        config.chat.agent_engine,
        config.chat.tool_approval_mode,
        providers.len(),
        personas.len(),
        agents.len(),
        mcp_servers.len()
    ))
}

pub(super) fn handle_browser_control_command(
    store: &AppStore,
    argument_raw: &str,
) -> AppResult<String> {
    let action = argument_raw
        .split_whitespace()
        .next()
        .unwrap_or("status")
        .to_lowercase();
    match action.as_str() {
        "" | "status" | "show" | "list" | "ls" => format_browser_control_status(store),
        "connect" | "disconnect" => Ok(
            "浏览器连接由 browser_create_session/browser_close_session 工具或设置页管理；此控制命令当前提供 status/list。"
                .into(),
        ),
        _ => Ok("用法：/browser [status|list]".into()),
    }
}

pub(super) fn format_browser_control_status(store: &AppStore) -> AppResult<String> {
    let providers = store.browser_providers()?;
    let enabled = store.enabled_browser_provider()?;
    let tool_note = "可用工具集：browser_navigate/browser_snapshot/browser_cdp/browser_create_session/browser_close_session 等。";
    if providers.is_empty() {
        return Ok(format!(
            "Browser providers：0 configured\n{tool_note}\n可在设置页添加 Browserbase 或 browser-use provider。"
        ));
    }
    let rows = providers
        .iter()
        .map(|provider| {
            let active = enabled
                .as_ref()
                .is_some_and(|active| active.id == provider.id);
            format!(
                "- {} [{}] enabled={} active={} type={} baseUrl={} apiKeyEnv={} project={} record={} timeout={}s",
                provider.id,
                provider.name,
                provider.enabled,
                active,
                provider.provider_type,
                provider.base_url,
                provider.api_key_env,
                if provider.project_id.trim().is_empty() {
                    "-"
                } else {
                    provider.project_id.as_str()
                },
                provider.record_sessions,
                provider.timeout_seconds
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let active = enabled
        .as_ref()
        .map(|provider| provider.id.as_str())
        .unwrap_or("none");
    Ok(format!(
        "Browser providers：{} configured, active={active}\n{rows}\n{tool_note}",
        providers.len()
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

fn parse_platform_tools_args(argument_raw: &str) -> (String, String, Vec<String>) {
    let mut parts = argument_raw
        .split_whitespace()
        .map(str::to_string)
        .collect::<Vec<_>>();
    let action = parts
        .first()
        .map(|part| part.to_ascii_lowercase())
        .unwrap_or_default();
    if !parts.is_empty() {
        parts.remove(0);
    }
    let mut platform = "cli".to_string();
    let mut names = Vec::new();
    let mut index = 0;
    while index < parts.len() {
        let part = parts[index].trim();
        if part == "--platform" || part == "-p" {
            if let Some(value) = parts.get(index + 1) {
                platform = normalize_hermes_platform_name(value);
                index += 2;
                continue;
            }
        }
        if let Some(value) = part.strip_prefix("--platform=") {
            platform = normalize_hermes_platform_name(value);
            index += 1;
            continue;
        }
        if matches!(
            action.as_str(),
            "list" | "status" | "show" | "reset" | "clear"
        ) && names.is_empty()
            && !part.starts_with('-')
        {
            platform = normalize_hermes_platform_name(part);
        } else {
            let name = normalize_toolset_name(part);
            if !name.is_empty() {
                names.push(name);
            }
        }
        index += 1;
    }
    (action, platform, names)
}

fn normalize_hermes_platform_name(platform: &str) -> String {
    let normalized = platform.trim().to_ascii_lowercase().replace('_', "-");
    match normalized.as_str() {
        "" => "cli".into(),
        "tui" | "desktop" | "chat" | "synthchat" => "cli".into(),
        "api-server" | "api_server" | "api" => "api_server".into(),
        other => other.to_string(),
    }
}

fn looks_like_mcp_tool_target(name: &str) -> bool {
    let mut parts = name.splitn(2, ':');
    parts.next().is_some_and(|server| !server.trim().is_empty())
        && parts.next().is_some_and(|tool| !tool.trim().is_empty())
}

#[derive(Default)]
struct McpToolFilterChangeResult {
    changed: bool,
    missing_servers: BTreeSet<String>,
}

fn apply_mcp_tool_filter_changes(
    servers: &mut [Value],
    targets: &[String],
    enable: bool,
) -> McpToolFilterChangeResult {
    let mut result = McpToolFilterChangeResult::default();
    for target in targets {
        let Some((server_name, tool_name)) = target.split_once(':') else {
            continue;
        };
        let server_name = server_name.trim();
        let tool_name = tool_name.trim();
        if server_name.is_empty() || tool_name.is_empty() {
            continue;
        }
        let Some(server) = servers
            .iter_mut()
            .find(|server| mcp_server_matches_name(server, server_name))
        else {
            result.missing_servers.insert(server_name.to_string());
            continue;
        };
        let Some(object) = server.as_object_mut() else {
            result.missing_servers.insert(server_name.to_string());
            continue;
        };
        let tools = object.entry("tools").or_insert_with(|| json!({}));
        if !tools.is_object() {
            *tools = json!({});
        }
        let tools_object = tools.as_object_mut().expect("tools object");
        let mut exclude = tools_object
            .get("exclude")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let before = exclude.clone();
        if enable {
            exclude.retain(|item| item != tool_name);
            tools_object.remove("include");
        } else if !exclude.iter().any(|item| item == tool_name) {
            exclude.push(tool_name.to_string());
            tools_object.remove("include");
        }
        if exclude.is_empty() {
            tools_object.remove("exclude");
        } else {
            tools_object.insert(
                "exclude".into(),
                Value::Array(exclude.iter().cloned().map(Value::String).collect()),
            );
        }
        if before != exclude {
            result.changed = true;
        }
    }
    result
}

fn mcp_server_matches_name(server: &Value, wanted: &str) -> bool {
    let wanted_norm = normalize_mcp_server_toolset_component(wanted);
    ["id", "name"]
        .iter()
        .filter_map(|key| server.get(*key).and_then(Value::as_str))
        .any(|value| {
            value == wanted || normalize_mcp_server_toolset_component(value) == wanted_norm
        })
}

fn platform_toolsets_map(config: &crate::models::AppConfig) -> BTreeMap<String, BTreeSet<String>> {
    config
        .chat
        .auxiliary_task_assignments
        .get("hermesPlatformToolsets")
        .or_else(|| {
            config
                .chat
                .auxiliary_task_assignments
                .get("hermes_platform_toolsets")
        })
        .and_then(Value::as_object)
        .map(|object| {
            object
                .iter()
                .map(|(platform, value)| {
                    (
                        normalize_hermes_platform_name(platform),
                        value
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(Value::as_str)
                            .map(normalize_toolset_name)
                            .filter(|name| !name.is_empty())
                            .collect::<BTreeSet<_>>(),
                    )
                })
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default()
}

fn persist_platform_toolsets_map(
    config: &mut crate::models::AppConfig,
    map: &BTreeMap<String, BTreeSet<String>>,
) {
    let value = Value::Object(
        map.iter()
            .map(|(platform, names)| {
                (
                    platform.clone(),
                    Value::Array(names.iter().cloned().map(Value::String).collect()),
                )
            })
            .collect(),
    );
    if !config.chat.auxiliary_task_assignments.is_object() {
        config.chat.auxiliary_task_assignments = json!({});
    }
    if let Some(object) = config.chat.auxiliary_task_assignments.as_object_mut() {
        object.insert("hermesPlatformToolsets".into(), value);
        object.remove("hermes_platform_toolsets");
    }
}

fn hermes_platform_toolset_default_off(name: &str) -> bool {
    matches!(
        name,
        "moa"
            | "homeassistant"
            | "spotify"
            | "discord"
            | "discord-admin"
            | "discord_admin"
            | "video"
            | "video-gen"
            | "video_gen"
            | "x-search"
            | "x_search"
    )
}

fn format_platform_tools_control_reply(
    platform: &str,
    configured: &BTreeMap<String, BTreeSet<String>>,
    counts: &BTreeMap<String, usize>,
    mcp_servers: &[Value],
    mcp_missing_servers: &BTreeSet<String>,
    mcp_changed: bool,
) -> String {
    let explicit = configured.get(platform);
    let enabled = explicit.cloned().unwrap_or_else(|| {
        counts
            .keys()
            .filter(|name| !hermes_platform_toolset_default_off(name))
            .cloned()
            .collect()
    });
    let rows = counts
        .iter()
        .map(|(name, count)| {
            let state = if enabled.contains(name) {
                "enabled"
            } else {
                "disabled"
            };
            format!("- {}: {} ({count})", name, state)
        })
        .collect::<Vec<_>>()
        .join("\n");
    let mode = if explicit.is_some() {
        "explicit"
    } else {
        "default"
    };
    let configured_platforms = if configured.is_empty() {
        "-".into()
    } else {
        configured.keys().cloned().collect::<Vec<_>>().join(", ")
    };
    let mcp_rows = format_platform_tools_mcp_filters(mcp_servers);
    let mcp_missing = if mcp_missing_servers.is_empty() {
        "-".into()
    } else {
        mcp_missing_servers
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join(", ")
    };
    format!(
        "Hermes Platform Tools：\n- platform: {platform}\n- mode: {mode}\n- configuredPlatforms: {configured_platforms}\n- storage: config.chat.auxiliaryTaskAssignments.hermesPlatformToolsets\n- mcpFilterChanged: {mcp_changed}\n- missingMcpServers: {mcp_missing}\n\nToolsets：\n{}\n\nMCP filters：\n{mcp_rows}",
        if rows.is_empty() {
            "- none".into()
        } else {
            rows
        }
    )
}

fn format_platform_tools_mcp_filters(mcp_servers: &[Value]) -> String {
    if mcp_servers.is_empty() {
        return "- none".into();
    }
    mcp_servers
        .iter()
        .map(|server| {
            let name = server
                .get("id")
                .or_else(|| server.get("name"))
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let tools = server.get("tools").and_then(Value::as_object);
            let include = tools
                .and_then(|tools| tools.get("include"))
                .and_then(Value::as_array)
                .map(|items| format_string_value_list(items))
                .filter(|text| !text.is_empty())
                .unwrap_or_else(|| "-".into());
            let exclude = tools
                .and_then(|tools| tools.get("exclude"))
                .and_then(Value::as_array)
                .map(|items| format_string_value_list(items))
                .filter(|text| !text.is_empty())
                .unwrap_or_else(|| "-".into());
            format!("- {name}: include={include} exclude={exclude}")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_string_value_list(values: &[Value]) -> String {
    values
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

pub(super) fn handle_model_control_command(
    store: &AppStore,
    conversation: &Conversation,
    persona: &Persona,
    argument_raw: &str,
) -> AppResult<String> {
    let agent = store.agent(Some(&conversation.agent_id))?;
    let mut next_persona = persona.clone();
    let providers = store.providers()?;
    let argument = argument_raw.trim();
    if argument.is_empty() || matches!(argument.to_lowercase().as_str(), "list" | "status" | "show")
    {
        return format_model_control_reply(&agent, persona, &providers, None);
    }
    if matches!(argument.to_lowercase().as_str(), "reset" | "clear") {
        next_persona.llm_model.clear();
        let saved = store.save_persona(next_persona)?;
        return format_model_control_reply(
            &agent,
            &saved,
            &providers,
            Some("已清除当前角色的模型 ID；服务商仍由通讯录固定。"),
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
        return Ok("用法：/model [model] 或 /model reset。服务商由通讯录固定。".into());
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
        if next_persona.llm_provider.trim() != provider.id {
            return Ok("服务商由通讯录配置固定；请到通讯录修改对话服务商。这里仅切换模型 ID。".into());
        }
    } else if !model.trim().is_empty() {
        if let Ok(provider) = select_llm_provider(&providers, model.trim()) {
            if !provider.enabled {
                return Err(AppError::BadRequest(format!(
                    "llm provider {} is disabled",
                    provider.id
                )));
            }
            return Ok("服务商由通讯录配置固定；请到通讯录修改对话服务商。这里仅切换模型 ID。".into());
        }
    }
    if !model.trim().is_empty() {
        let active_provider_id = next_persona.llm_provider.trim();
        if active_provider_id.is_empty() {
            return Err(AppError::BadRequest(
                "请先在通讯录中为当前角色选择对话服务商。".into(),
            ));
        }
        let active_provider = select_llm_provider(&providers, active_provider_id)?;
        if !active_provider.enabled {
            return Err(AppError::BadRequest(format!(
                "llm provider {} is disabled",
                active_provider.id
            )));
        }
        if let Some(alias) = resolve_model_alias(model.trim(), active_provider, &providers) {
            let alias_matches_active_provider = alias
                .provider_id
                .as_deref()
                .map(|provider_id| provider_id == active_provider.id)
                .unwrap_or(true);
            if alias_matches_active_provider {
                next_persona.llm_model = alias.model;
                resolved_alias = Some(alias.alias);
            } else {
                next_persona.llm_model = model.trim().to_string();
            }
        } else if let Some(route) =
            resolve_model_family_route(model.trim(), active_provider, &providers)
        {
            let route_matches_active_provider = route
                .provider_id
                .as_deref()
                .map(|provider_id| provider_id == active_provider.id)
                .unwrap_or(true);
            if route_matches_active_provider {
                next_persona.llm_model = route.model;
            } else {
                next_persona.llm_model = model.trim().to_string();
            }
        } else {
            next_persona.llm_model = model.trim().to_string();
        }
    }
    let saved = store.save_persona(next_persona)?;
    let prefix = resolved_alias
        .as_deref()
        .map(|alias| format!("已更新当前角色的模型设置。resolvedAlias: {alias}"))
        .unwrap_or_else(|| "已更新当前角色的模型设置。".into());
    format_model_control_reply(&agent, &saved, &providers, Some(&prefix))
}

pub(super) fn selected_provider_id<'a>(
    persona: &'a Persona,
    _agent: &'a AgentDefinition,
) -> Option<&'a str> {
    if !persona.llm_provider.trim().is_empty() {
        Some(persona.llm_provider.as_str())
    } else {
        None
    }
}

pub(super) fn effective_llm_persona(persona: &Persona, _agent: &AgentDefinition) -> Persona {
    persona.clone()
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
pub(super) struct ModelAliasResolution {
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

pub(super) fn resolve_model_family_route(
    raw_model: &str,
    current_provider: &LlmProvider,
    providers: &[LlmProvider],
) -> Option<ModelAliasResolution> {
    let provider_hint = model_family_provider_hint(raw_model)?;
    let model = normalize_model_for_provider_hint(raw_model, provider_hint);
    let provider_id = if provider_matches_alias_hint(current_provider, provider_hint) {
        Some(current_provider.id.clone())
    } else if provider_catalog_contains_model(current_provider, &model) {
        Some(current_provider.id.clone())
    } else {
        providers
            .iter()
            .find(|provider| provider.enabled && provider_catalog_contains_model(provider, &model))
            .or_else(|| {
                providers.iter().find(|provider| {
                    provider.enabled && provider_matches_alias_hint(provider, provider_hint)
                })
            })
            .map(|provider| provider.id.clone())
    }?;
    Some(ModelAliasResolution {
        alias: raw_model.trim().to_string(),
        model,
        provider_id: Some(provider_id),
    })
}

#[derive(Debug, Clone)]
pub(super) struct LlmRouteCorrection {
    pub provider_hint: String,
    pub from_provider_id: Option<String>,
    pub to_provider_id: String,
    pub requested_model: String,
    pub effective_model: String,
}

pub(super) fn reconcile_model_family_provider(
    persona: &mut Persona,
    providers: &mut Vec<LlmProvider>,
) -> Option<LlmRouteCorrection> {
    let requested_model = persona.llm_model.trim().to_string();
    if requested_model.is_empty() || providers.is_empty() {
        return None;
    }
    let provider_hint = model_family_provider_hint(&requested_model)?;
    let effective_model = normalize_model_for_provider_hint(&requested_model, provider_hint);
    let from_provider_id = providers.first().map(|provider| provider.id.clone());
    if let Some(target_index) = providers
        .iter()
        .position(|provider| provider_catalog_contains_model(provider, &effective_model))
    {
        if target_index == 0 {
            if effective_model != requested_model {
                persona.llm_model = effective_model.clone();
                return Some(LlmRouteCorrection {
                    provider_hint: provider_hint.to_string(),
                    from_provider_id: from_provider_id.clone(),
                    to_provider_id: from_provider_id.unwrap_or_default(),
                    requested_model,
                    effective_model,
                });
            }
            return None;
        }
        let target = providers.remove(target_index);
        let to_provider_id = target.id.clone();
        providers.insert(0, target);
        persona.llm_provider = to_provider_id.clone();
        persona.llm_model = effective_model.clone();
        return Some(LlmRouteCorrection {
            provider_hint: provider_hint.to_string(),
            from_provider_id,
            to_provider_id,
            requested_model,
            effective_model,
        });
    }
    if providers
        .first()
        .is_some_and(|provider| provider_matches_alias_hint(provider, provider_hint))
    {
        if effective_model != requested_model {
            persona.llm_model = effective_model.clone();
            return Some(LlmRouteCorrection {
                provider_hint: provider_hint.to_string(),
                from_provider_id: from_provider_id.clone(),
                to_provider_id: from_provider_id.unwrap_or_default(),
                requested_model,
                effective_model,
            });
        }
        return None;
    }
    let target_index = providers
        .iter()
        .position(|provider| provider_matches_alias_hint(provider, provider_hint))?;
    let target = providers.remove(target_index);
    let to_provider_id = target.id.clone();
    providers.insert(0, target);
    persona.llm_provider = to_provider_id.clone();
    persona.llm_model = effective_model.clone();
    Some(LlmRouteCorrection {
        provider_hint: provider_hint.to_string(),
        from_provider_id,
        to_provider_id,
        requested_model,
        effective_model,
    })
}

fn provider_catalog_contains_model(provider: &LlmProvider, model: &str) -> bool {
    let needle = model.trim().to_ascii_lowercase();
    if needle.is_empty() {
        return false;
    }
    if provider.model.trim().eq_ignore_ascii_case(&needle) {
        return true;
    }
    provider
        .models
        .as_object()
        .is_some_and(|models| models.keys().any(|id| id.eq_ignore_ascii_case(&needle)))
}

pub(super) fn model_family_provider_hint(raw_model: &str) -> Option<&'static str> {
    let lower = raw_model.trim().to_ascii_lowercase();
    if lower.is_empty() {
        return None;
    }
    let bare = lower.rsplit('/').next().unwrap_or(lower.as_str());
    if lower.starts_with("xiaomi/") || bare.starts_with("mimo-") || bare.starts_with("mino-") {
        return Some("xiaomi");
    }
    if lower.starts_with("minimax/")
        || lower.starts_with("minimaxai/")
        || bare.starts_with("minimax-")
        || bare.starts_with("minimaxm")
    {
        return Some("minimax");
    }
    None
}

fn normalize_model_for_provider_hint(raw_model: &str, provider_hint: &str) -> String {
    let trimmed = raw_model.trim();
    if provider_hint == "xiaomi" {
        let bare = trimmed
            .rsplit_once('/')
            .map(|(_, model)| model)
            .unwrap_or(trimmed);
        let lower = bare.to_ascii_lowercase();
        if lower.starts_with("mino-") {
            return format!("mimo-{}", &bare[5..]).to_ascii_lowercase();
        }
        return lower;
    }
    if provider_hint == "minimax" {
        let bare = trimmed
            .rsplit_once('/')
            .map(|(_, model)| model)
            .unwrap_or(trimmed);
        let lower = bare.to_ascii_lowercase();
        if lower.starts_with("minimax-m") {
            return format!("MiniMax-M{}", &bare[9..]);
        }
        if lower.starts_with("minimaxm") {
            return format!("MiniMax-M{}", &bare[8..]);
        }
        return bare.to_string();
    }
    trimmed.to_string()
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
    fields.iter().any(|field| {
        let field = field.to_ascii_lowercase();
        field.contains(&hint)
            || (hint == "xiaomi" && (field.contains("mimo") || field.contains("xiaomimimo")))
    })
}

pub(super) fn format_model_control_reply(
    agent: &AgentDefinition,
    persona: &Persona,
    providers: &[LlmProvider],
    prefix: Option<&str>,
) -> AppResult<String> {
    let provider = selected_provider_id(persona, agent)
        .map(|provider_id| select_llm_provider(providers, provider_id))
        .transpose()?;
    let effective_persona = effective_llm_persona(persona, agent);
    let effective_model = if !effective_persona.llm_model.trim().is_empty() {
        effective_persona.llm_model.trim()
    } else {
        ""
    };
    let persona_note = if !persona.llm_provider.trim().is_empty() {
        "\n- note: 当前对话使用通讯录角色的服务商/模型；绑定 agent 会跟随该角色配置。"
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
    let prefix = prefix.map(|value| format!("{value}\n")).unwrap_or_default();
    Ok(format!(
        "{}当前模型设置：\n- persona: {} ({})\n- boundAgent: {} ({})\n- personaProvider: {}\n- personaModel: {}\n- activeProvider: {}\n- effectiveModel: {}\n- providerFallback: disabled{}\n\n可用 providers：\n{}",
        prefix,
        persona.name,
        persona.id,
        agent.name,
        agent.id,
        if persona.llm_provider.trim().is_empty() {
            "-"
        } else {
            persona.llm_provider.trim()
        },
        if persona.llm_model.trim().is_empty() {
            "-"
        } else {
            persona.llm_model.trim()
        },
        provider
            .map(|provider| format!("{} ({})", provider.name, provider.id))
            .unwrap_or_else(|| "-".into()),
        if effective_model.is_empty() {
            "-"
        } else {
            effective_model
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

pub(super) fn handle_title_control_command(
    store: &AppStore,
    conversation: &Conversation,
    argument_raw: &str,
) -> AppResult<String> {
    let title = argument_raw.trim();
    if title.is_empty() || matches!(title.to_lowercase().as_str(), "status" | "show") {
        let current = store.conversation(&conversation.id)?;
        return Ok(format!("当前会话标题：{}", current.title));
    }
    let title = title.chars().take(120).collect::<String>();
    store.rename_conversation(&conversation.id, title.clone())?;
    Ok(format!("已更新当前会话标题：{title}"))
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

pub(super) async fn handle_usage_control_command_with_account(
    store: &AppStore,
    argument_raw: &str,
) -> AppResult<String> {
    let local = handle_usage_control_command(store)?;
    let wants_account = argument_raw.split_whitespace().any(|part| {
        matches!(
            part.to_ascii_lowercase().as_str(),
            "account" | "quota" | "remote" | "limits"
        )
    });
    if !wants_account {
        return Ok(local);
    }
    let account = fetch_openrouter_account_usage_for_config(store)
        .await
        .unwrap_or_else(|error| format!("Account usage：unavailable ({error})"));
    Ok(format!("{local}\n\n{account}"))
}

pub(super) async fn handle_auth_control_command(
    store: &AppStore,
    argument_raw: &str,
) -> AppResult<String> {
    let selector = argument_raw.trim().to_ascii_lowercase();
    let mut raw_parts = argument_raw.trim().split_whitespace();
    match raw_parts.next().map(|part| part.to_ascii_lowercase()) {
        Some(command) if command == "refresh" => {
            let provider = raw_parts.next().unwrap_or("");
            return handle_auth_refresh_control_command(provider).await;
        }
        Some(command) if matches!(command.as_str(), "pool" | "credentials" | "credential") => {
            let action = raw_parts.next().unwrap_or("");
            let provider = raw_parts.next().unwrap_or("");
            let target = raw_parts.next().unwrap_or("");
            return handle_auth_pool_control_command(action, provider, target);
        }
        Some(command) if matches!(command.as_str(), "login" | "add") => {
            let provider = raw_parts.next().unwrap_or("");
            return handle_auth_login_control_command(provider).await;
        }
        Some(command) if matches!(command.as_str(), "poll" | "complete") => {
            let provider = raw_parts.next().unwrap_or("");
            let device_auth_id = raw_parts.next().unwrap_or("");
            let user_code = raw_parts.next().unwrap_or("");
            let extra = raw_parts.next().unwrap_or("");
            let extra2 = raw_parts.next().unwrap_or("");
            return handle_auth_poll_control_command(
                provider,
                device_auth_id,
                user_code,
                extra,
                extra2,
            )
            .await;
        }
        _ => {}
    }
    let include_disabled = selector
        .split_whitespace()
        .any(|part| matches!(part, "all" | "--all" | "-a"));
    let provider_filter = selector
        .split_whitespace()
        .find(|part| !matches!(*part, "all" | "--all" | "-a" | "status" | "list" | "show"));
    let providers = store.providers()?;
    let mut lines = vec!["Provider auth status:".to_string()];
    let mut matched = 0usize;

    for provider in providers.iter() {
        if !include_disabled && !provider.enabled {
            continue;
        }
        if let Some(filter) = provider_filter {
            if !auth_provider_matches_filter(provider, filter) {
                continue;
            }
        }
        matched += 1;
        lines.push(format_auth_provider_status(provider));
    }

    if matched == 0 {
        if let Some(filter) = provider_filter {
            lines.push(format!(
                "- no provider matched `{filter}`{}",
                if include_disabled {
                    ""
                } else {
                    " among enabled providers; retry `/auth all` to include disabled providers"
                }
            ));
        } else {
            lines.push("- no enabled LLM providers configured".into());
        }
    }
    lines.push(String::new());
    lines.push("Notes: OpenAI Codex, Nous, MiniMax, xAI, and Google Gemini CLI OAuth login are available with `/auth login <provider>`; other OAuth login flows are still being ported. Secret values are never printed.".into());
    lines.push("Bedrock status checks AWS_ACCESS_KEY_ID/AWS_SECRET_ACCESS_KEY and static AWS shared credentials profiles.".into());
    Ok(lines.join("\n"))
}

fn handle_auth_pool_control_command(
    action: &str,
    provider: &str,
    target: &str,
) -> AppResult<String> {
    match action.to_ascii_lowercase().as_str() {
        "" | "list" | "ls" | "status" => {
            let filter = (!provider.trim().is_empty()).then_some(provider.trim());
            let entries = list_hermes_credential_pool(filter)?;
            if entries.is_empty() {
                return Ok(if let Some(filter) = filter {
                    format!("Hermes credential pool has no entries matching `{filter}`.")
                } else {
                    "Hermes credential pool has no entries.".into()
                });
            }
            let mut lines = vec!["Hermes credential pool:".to_string()];
            for entry in entries {
                lines.push(format!(
                    "- {} #{} {} id={} auth={} source={} state={} expiresAt={} baseUrl={}",
                    entry.provider_id,
                    entry.index,
                    entry.label,
                    entry.id.as_deref().unwrap_or("-"),
                    entry.auth_type.as_deref().unwrap_or("-"),
                    entry.source.as_deref().unwrap_or("-"),
                    entry.state,
                    entry.expires_at.as_deref().unwrap_or("-"),
                    entry.base_url.as_deref().unwrap_or("-"),
                ));
            }
            lines.push("Secret values were not printed.".into());
            Ok(lines.join("\n"))
        }
        "remove" | "rm" | "delete" | "del" => {
            if provider.trim().is_empty() || target.trim().is_empty() {
                return Ok("用法：/auth pool remove <provider> <index|id|label-prefix>".into());
            }
            let removed = remove_hermes_credential_pool_entry(provider, target)?;
            Ok(format!(
                "Removed Hermes credential pool entry:\n- provider: {}\n- index: {}\n- label: {}\n- source: {}\nSecret values were not printed.",
                removed.provider_id,
                removed.index,
                removed.label,
                removed.source.as_deref().unwrap_or("-")
            ))
        }
        "reset" => {
            if provider.trim().is_empty() {
                return Ok("用法：/auth pool reset <provider>".into());
            }
            let count = reset_hermes_credential_pool_statuses(provider)?;
            Ok(format!(
                "Reset status on {count} Hermes credential pool entr{} for `{}`.",
                if count == 1 { "y" } else { "ies" },
                provider.trim()
            ))
        }
        "add" => Ok("`/auth pool add` is intentionally not available in chat because it would store secrets in message history. Use `/auth login <provider>` for OAuth providers or the settings credential editor for API keys.".into()),
        _ => Ok("用法：/auth pool [list [provider]|remove <provider> <index|id|label-prefix>|reset <provider>]".into()),
    }
}

async fn handle_auth_login_control_command(provider: &str) -> AppResult<String> {
    match provider.to_ascii_lowercase().as_str() {
        "codex" | "openai-codex" | "openai_codex" => {
            let login = start_codex_device_code_login().await?;
            Ok(format!(
                "OpenAI Codex device-code login started:\n- verificationUrl: {}\n- userCode: {}\n- deviceAuthId: {}\n- pollIntervalSeconds: {}\n\nOpen the verification URL, enter the user code, then run:\n/auth poll openai-codex {} {}\n\nSecret values were not printed.",
                login.verification_uri,
                login.user_code,
                login.device_auth_id,
                login.interval_seconds,
                login.device_auth_id,
                login.user_code
            ))
        }
        "nous" | "nous-oauth" | "nous_portal" | "nous-portal" => {
            let login = start_nous_device_code_login().await?;
            Ok(format!(
                "Nous device-code login started:\n- verificationUrl: {}\n- verificationUrlComplete: {}\n- userCode: {}\n- deviceCode: {}\n- expiresInSeconds: {}\n- pollIntervalSeconds: {}\n\nOpen the verification URL, approve the login, then run:\n/auth poll nous {}\n\nSecret values were not printed.",
                login.verification_uri,
                login.verification_uri_complete,
                login.user_code,
                login.device_code,
                login.expires_in,
                login.interval_seconds,
                login.device_code
            ))
        }
        "minimax" | "minimax-oauth" | "minimax_oauth" => {
            let login = start_minimax_oauth_login().await?;
            Ok(format!(
                "MiniMax OAuth login started:\n- verificationUrl: {}\n- userCode: {}\n- codeVerifier: {}\n- expiredIn: {}\n- intervalMs: {}\n- region: {}\n\nOpen the verification URL, enter the user code, then run:\n/auth poll minimax-oauth {} {}\n\nSecret values were not printed.",
                login.verification_uri,
                login.user_code,
                login.code_verifier,
                login.expired_in,
                login.interval_ms.map(|value| value.to_string()).unwrap_or_else(|| "-".into()),
                login.region,
                login.user_code,
                login.code_verifier
            ))
        }
        "xai" | "x-ai" | "x.ai" | "grok" | "xai-oauth" | "x-ai-oauth" | "grok-oauth"
        | "xai-grok-oauth" => {
            let login = start_xai_oauth_login().await?;
            Ok(format!(
                "xAI OAuth login started:\n- authorizationUrl: {}\n- redirectUri: {}\n- state: {}\n- codeVerifier: {}\n- codeChallenge: {}\n\nOpen the authorization URL. After approval, paste the callback URL or bare code with:\n/auth poll xai-oauth <callback_or_code> {} {} {}\n\nSecret values were not printed.",
                login.authorize_url,
                login.redirect_uri,
                login.state,
                login.code_verifier,
                login.code_challenge,
                login.state,
                login.code_verifier,
                login.code_challenge
            ))
        }
        "anthropic" | "claude" | "claude-oauth" | "anthropic-oauth" => {
            let login = start_anthropic_oauth_login()?;
            Ok(format!(
                "Anthropic OAuth login started:\n- authorizationUrl: {}\n- redirectUri: {}\n- state: {}\n- codeVerifier: {}\n- codeChallenge: {}\n\nOpen the authorization URL. After approval, paste the callback URL or code#state value with:\n/auth poll anthropic <callback_or_code> {} {}\n\nSecret values were not printed.",
                login.authorize_url,
                login.redirect_uri,
                login.state,
                login.code_verifier,
                login.code_challenge,
                login.state,
                login.code_verifier
            ))
        }
        "gemini" | "google-gemini-cli" | "gemini-cli" | "gemini-oauth" => {
            let login = start_google_gemini_oauth_login()?;
            Ok(format!(
                "Google Gemini OAuth login started:\n- authorizationUrl: {}\n- redirectUri: {}\n- state: {}\n- codeVerifier: {}\n\nOpen the authorization URL. After approval, paste the callback URL or bare code with:\n/auth poll google-gemini-cli <callback_or_code> {} {}\n\nSecret values were not printed.",
                login.authorize_url,
                login.redirect_uri,
                login.state,
                login.code_verifier,
                login.state,
                login.code_verifier
            ))
        }
        "spotify" | "spotify-oauth" | "spotify_pkce" | "spotify-pkce" => {
            let login = start_spotify_oauth_login()?;
            Ok(format!(
                "Spotify OAuth login started:\n- authorizationUrl: {}\n- redirectUri: {}\n- state: {}\n- codeVerifier: {}\n- scope: {}\n\nOpen the authorization URL. After approval, paste the callback URL or bare code with:\n/auth poll spotify <callback_or_code> {} {}\n\nSecret values were not printed.",
                login.authorize_url,
                login.redirect_uri,
                login.state,
                login.code_verifier,
                login.scope,
                login.state,
                login.code_verifier
            ))
        }
        "qwen" | "qwen-oauth" | "qwen_cli" | "qwen-cli" => Ok(
            "Qwen OAuth login uses the external Qwen CLI, matching Hermes `auth add qwen-oauth`:\n- command: qwen auth qwen-oauth\n- hermesCommand: hermes auth add qwen-oauth\n- source: qwen_cli\n- desktopBoundary: external_cli_login\n\nAfter the Qwen CLI writes its OAuth credentials, run:\n/auth refresh qwen-oauth\n\nSecret values were not printed."
                .into(),
        ),
        "claude-code" | "claude_code" | "claude-code-oauth" | "claude-setup-token" => Ok(
            "Claude Code subscription login uses the external Claude Code CLI, matching Hermes dashboard provider `claude-code`:\n- command: claude setup-token\n- hermesCommand: hermes auth add anthropic\n- source: claude-code setup-token / Anthropic OAuth credential store\n- desktopBoundary: external_cli_login\n\nAfter Claude Code writes or prints a setup-token, use the Anthropic/Claude credential store or run:\n/auth refresh anthropic\n\nSecret values were not printed."
                .into(),
        ),
        "" => Ok("用法：/auth login <anthropic|claude-code|openai-codex|nous|minimax-oauth|xai-oauth|google-gemini-cli|spotify|qwen-oauth>".into()),
        _ => Ok(format!(
            "Auth login for `{provider}` is not implemented yet. 当前支持：anthropic, claude-code, openai-codex, nous, minimax-oauth, xai-oauth, google-gemini-cli, spotify, qwen-oauth。"
        )),
    }
}

async fn handle_auth_poll_control_command(
    provider: &str,
    device_auth_id: &str,
    user_code: &str,
    extra: &str,
    extra2: &str,
) -> AppResult<String> {
    match provider.to_ascii_lowercase().as_str() {
        "codex" | "openai-codex" | "openai_codex" => {
            if device_auth_id.trim().is_empty() || user_code.trim().is_empty() {
                return Ok(
                    "用法：/auth poll openai-codex <device_auth_id> <user_code>".into(),
                );
            }
            let credential =
                complete_codex_device_code_login(device_auth_id.trim(), user_code.trim()).await?;
            Ok(format!(
                "Auth login completed:\n- provider: {}\n- source: {}\n- expiresAt: {}\n- baseUrl: {}\nSecret values were not printed.",
                credential.provider_id,
                credential.source,
                credential.expires_at.as_deref().unwrap_or("-"),
                credential.base_url.as_deref().unwrap_or("-")
            ))
        }
        "nous" | "nous-oauth" | "nous_portal" | "nous-portal" => {
            if device_auth_id.trim().is_empty() {
                return Ok("用法：/auth poll nous <device_code>".into());
            }
            let credential = complete_nous_device_code_login(device_auth_id.trim()).await?;
            Ok(format!(
                "Auth login completed:\n- provider: {}\n- source: {}\n- expiresAt: {}\n- baseUrl: {}\nSecret values were not printed.",
                credential.provider_id,
                credential.source,
                credential.expires_at.as_deref().unwrap_or("-"),
                credential.base_url.as_deref().unwrap_or("-")
            ))
        }
        "minimax" | "minimax-oauth" | "minimax_oauth" => {
            if device_auth_id.trim().is_empty() || user_code.trim().is_empty() {
                return Ok("用法：/auth poll minimax-oauth <user_code> <code_verifier>".into());
            }
            let credential =
                complete_minimax_oauth_login(device_auth_id.trim(), user_code.trim()).await?;
            Ok(format!(
                "Auth login completed:\n- provider: {}\n- source: {}\n- expiresAt: {}\n- baseUrl: {}\nSecret values were not printed.",
                credential.provider_id,
                credential.source,
                credential.expires_at.as_deref().unwrap_or("-"),
                credential.base_url.as_deref().unwrap_or("-")
            ))
        }
        "xai" | "x-ai" | "x.ai" | "grok" | "xai-oauth" | "x-ai-oauth" | "grok-oauth"
        | "xai-grok-oauth" => {
            if device_auth_id.trim().is_empty()
                || user_code.trim().is_empty()
                || extra.trim().is_empty()
                || extra2.trim().is_empty()
            {
                return Ok("用法：/auth poll xai-oauth <callback_or_code> <state> <code_verifier> <code_challenge>".into());
            }
            let credential = complete_xai_oauth_login(
                device_auth_id.trim(),
                user_code.trim(),
                extra.trim(),
                extra2.trim(),
            )
            .await?;
            Ok(format!(
                "Auth login completed:\n- provider: {}\n- source: {}\n- expiresAt: {}\n- baseUrl: {}\nSecret values were not printed.",
                credential.provider_id,
                credential.source,
                credential.expires_at.as_deref().unwrap_or("-"),
                credential.base_url.as_deref().unwrap_or("-")
            ))
        }
        "anthropic" | "claude" | "claude-oauth" | "anthropic-oauth" => {
            if device_auth_id.trim().is_empty()
                || user_code.trim().is_empty()
                || extra.trim().is_empty()
            {
                return Ok(
                    "用法：/auth poll anthropic <callback_or_code> <state> <code_verifier>".into(),
                );
            }
            let credential = complete_anthropic_oauth_login(
                device_auth_id.trim(),
                user_code.trim(),
                extra.trim(),
            )
            .await?;
            Ok(format!(
                "Auth login completed:\n- provider: {}\n- source: {}\n- expiresAt: {}\n- baseUrl: {}\nSecret values were not printed.",
                credential.provider_id,
                credential.source,
                credential.expires_at.as_deref().unwrap_or("-"),
                credential.base_url.as_deref().unwrap_or("-")
            ))
        }
        "gemini" | "google-gemini-cli" | "gemini-cli" | "gemini-oauth" => {
            if device_auth_id.trim().is_empty()
                || user_code.trim().is_empty()
                || extra.trim().is_empty()
            {
                return Ok("用法：/auth poll google-gemini-cli <callback_or_code> <state> <code_verifier>".into());
            }
            let credential = complete_google_gemini_oauth_login(
                device_auth_id.trim(),
                user_code.trim(),
                extra.trim(),
            )
            .await?;
            Ok(format!(
                "Auth login completed:\n- provider: {}\n- source: {}\n- expiresAt: {}\n- baseUrl: {}\nSecret values were not printed.",
                credential.provider_id,
                credential.source,
                credential.expires_at.as_deref().unwrap_or("-"),
                credential.base_url.as_deref().unwrap_or("-")
            ))
        }
        "spotify" | "spotify-oauth" | "spotify_pkce" | "spotify-pkce" => {
            if device_auth_id.trim().is_empty()
                || user_code.trim().is_empty()
                || extra.trim().is_empty()
            {
                return Ok(
                    "用法：/auth poll spotify <callback_or_code> <state> <code_verifier>".into(),
                );
            }
            let credential = complete_spotify_oauth_login(
                device_auth_id.trim(),
                user_code.trim(),
                extra.trim(),
            )
            .await?;
            Ok(format!(
                "Auth login completed:\n- provider: {}\n- source: {}\n- expiresAt: {}\n- baseUrl: {}\nSecret values were not printed.",
                credential.provider_id,
                credential.source,
                credential.expires_at.as_deref().unwrap_or("-"),
                credential.base_url.as_deref().unwrap_or("-")
            ))
        }
        "qwen" | "qwen-oauth" | "qwen_cli" | "qwen-cli" => Ok(
            "Qwen OAuth does not use SynthChat browser polling. Run the external Qwen CLI login first, then import/refresh the written credentials with:\n/auth refresh qwen-oauth"
                .into(),
        ),
        "claude-code" | "claude_code" | "claude-code-oauth" | "claude-setup-token" => Ok(
            "Claude Code setup-token login does not use SynthChat browser polling. Run the external Claude Code CLI flow first:\nclaude setup-token\n\nThen verify/import the resulting Anthropic credential with:\n/auth refresh anthropic"
                .into(),
        ),
        "" => Ok("用法：/auth poll <anthropic|claude-code|openai-codex|nous|minimax-oauth|xai-oauth|google-gemini-cli|spotify|qwen-oauth> <device_auth_id|device_code|user_code|callback_or_code> [user_code|code_verifier|state] [code_challenge]".into()),
        _ => Ok(format!(
            "Auth poll for `{provider}` is not implemented yet. 当前支持：anthropic, claude-code, openai-codex, nous, minimax-oauth, xai-oauth, google-gemini-cli, spotify, qwen-oauth。"
        )),
    }
}

async fn handle_auth_refresh_control_command(provider: &str) -> AppResult<String> {
    match provider {
        "qwen" | "qwen-oauth" | "qwen_cli" | "qwen-cli" => {
            let credential = refresh_qwen_cli_oauth_credentials().await?;
            Ok(format!(
                "Auth refresh completed:\n- provider: {}\n- source: {}\n- expiresAt: {}\n- baseUrl: {}\nSecret values were not printed.",
                credential.provider_id,
                credential.source,
                credential.expires_at.as_deref().unwrap_or("-"),
                credential.base_url.as_deref().unwrap_or("-")
            ))
        }
        "minimax" | "minimax-oauth" | "minimax_oauth" => {
            let credential = refresh_minimax_oauth_credentials().await?;
            Ok(format!(
                "Auth refresh completed:\n- provider: {}\n- source: {}\n- expiresAt: {}\n- baseUrl: {}\nSecret values were not printed.",
                credential.provider_id,
                credential.source,
                credential.expires_at.as_deref().unwrap_or("-"),
                credential.base_url.as_deref().unwrap_or("-")
            ))
        }
        "xai" | "x-ai" | "x.ai" | "grok" | "xai-oauth" | "x-ai-oauth" | "grok-oauth"
        | "xai-grok-oauth" => {
            let credential = refresh_xai_oauth_credentials().await?;
            Ok(format!(
                "Auth refresh completed:\n- provider: {}\n- source: {}\n- expiresAt: {}\n- baseUrl: {}\nSecret values were not printed.",
                credential.provider_id,
                credential.source,
                credential.expires_at.as_deref().unwrap_or("-"),
                credential.base_url.as_deref().unwrap_or("-")
            ))
        }
        "codex" | "openai-codex" | "openai_codex" => {
            let credential = refresh_codex_oauth_credentials().await?;
            Ok(format!(
                "Auth refresh completed:\n- provider: {}\n- source: {}\n- expiresAt: {}\n- baseUrl: {}\nSecret values were not printed.",
                credential.provider_id,
                credential.source,
                credential.expires_at.as_deref().unwrap_or("-"),
                credential.base_url.as_deref().unwrap_or("-")
            ))
        }
        "anthropic" | "claude" | "claude-oauth" | "anthropic-oauth" => {
            let credential = refresh_anthropic_oauth_credentials().await?;
            Ok(format!(
                "Auth refresh completed:\n- provider: {}\n- source: {}\n- expiresAt: {}\n- baseUrl: {}\nSecret values were not printed.",
                credential.provider_id,
                credential.source,
                credential.expires_at.as_deref().unwrap_or("-"),
                credential.base_url.as_deref().unwrap_or("-")
            ))
        }
        "claude-code" | "claude_code" | "claude-code-oauth" | "claude-setup-token" => Ok(
            "Claude Code setup-token credentials are owned by the external Claude Code / Anthropic credential flow. Run:\nclaude setup-token\n\nIf the token is mirrored into the Hermes Anthropic OAuth file, refresh it with:\n/auth refresh anthropic"
                .into(),
        ),
        "gemini" | "google-gemini-cli" | "gemini-cli" | "gemini-oauth" => {
            let credential = refresh_google_gemini_oauth_credentials().await?;
            Ok(format!(
                "Auth refresh completed:\n- provider: {}\n- source: {}\n- expiresAt: {}\n- baseUrl: {}\nSecret values were not printed.",
                credential.provider_id,
                credential.source,
                credential.expires_at.as_deref().unwrap_or("-"),
                credential.base_url.as_deref().unwrap_or("-")
            ))
        }
        "nous" | "nous-oauth" | "nous_portal" | "nous-portal" => {
            let credential = refresh_nous_oauth_credentials().await?;
            Ok(format!(
                "Auth refresh completed:\n- provider: {}\n- source: {}\n- expiresAt: {}\n- baseUrl: {}\nSecret values were not printed.",
                credential.provider_id,
                credential.source,
                credential.expires_at.as_deref().unwrap_or("-"),
                credential.base_url.as_deref().unwrap_or("-")
            ))
        }
        "spotify" | "spotify-oauth" | "spotify_pkce" | "spotify-pkce" => {
            let credential = refresh_spotify_oauth_credentials().await?;
            Ok(format!(
                "Auth refresh completed:\n- provider: {}\n- source: {}\n- expiresAt: {}\n- baseUrl: {}\nSecret values were not printed.",
                credential.provider_id,
                credential.source,
                credential.expires_at.as_deref().unwrap_or("-"),
                credential.base_url.as_deref().unwrap_or("-")
            ))
        }
        "" => Ok(
            "用法：/auth refresh <anthropic|claude-code|qwen-oauth|minimax-oauth|xai-oauth|openai-codex|google-gemini-cli|nous|spotify>"
                .into(),
        ),
        _ => Ok(format!(
            "Auth refresh for `{provider}` is not implemented yet. 当前支持：anthropic, claude-code, qwen-oauth, minimax-oauth, xai-oauth, openai-codex, google-gemini-cli, nous, spotify。"
        )),
    }
}

async fn handle_qqbot_control_command(store: &AppStore, argument_raw: &str) -> AppResult<String> {
    let mut parts = argument_raw.split_whitespace();
    let action = parts.next().unwrap_or("status").to_lowercase();
    match action.as_str() {
        "" | "status" | "show" => format_qqbot_control_status(store),
        "onboard" | "register" | "qr-register" | "qr" => {
            let timeout_seconds = parts
                .next()
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(600)
                .clamp(30, 3600);
            qqbot_onboard_register(store, timeout_seconds).await
        }
        "decrypt" => {
            let encrypted = parts.next().unwrap_or_default();
            let bind_key = parts.next().unwrap_or_default();
            if encrypted.is_empty() || bind_key.is_empty() {
                return Ok(
                    "用法：/qqbot decrypt <bot_encrypt_secret> <bind_key> [--show]\n默认不打印 secret；加 --show 才会显示明文。"
                        .into(),
                );
            }
            let decrypted = qqbot_decrypt_secret(encrypted, bind_key)?;
            if parts.any(|part| part == "--show") {
                Ok(format!("QQBot clientSecret decrypted:\n{decrypted}"))
            } else {
                Ok(format!(
                    "QQBot clientSecret decrypted successfully; plaintext was not printed. length={}",
                    decrypted.chars().count()
                ))
            }
        }
        _ => Ok(
            "用法：/qqbot [status|onboard [timeoutSeconds]|decrypt <bot_encrypt_secret> <bind_key> [--show]]"
                .into(),
        ),
    }
}

fn format_qqbot_control_status(store: &AppStore) -> AppResult<String> {
    let config = store.config()?;
    let qqbot = &config.qqbot;
    let configured = qqbot_config_string(qqbot, &["appId", "app_id"])
        .filter(|value| !value.trim().is_empty())
        .is_some()
        && qqbot_config_string(qqbot, &["clientSecret", "client_secret", "secret", "token"])
            .filter(|value| !value.trim().is_empty())
            .is_some();
    let enabled = qqbot
        .get("enabled")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let portal = qqbot_onboard_portal_base_url(qqbot)?;
    let home = qqbot_config_string(qqbot, &["homeTarget", "home_target", "homeChannel"])
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "-".into());
    Ok(format!(
        "QQBot：\n- enabled: {}\n- configured: {}\n- appId: {}\n- clientSecret: {}\n- homeTarget: {}\n- portalBaseUrl: {}\n\n可用命令：\n- /qqbot onboard [timeoutSeconds]\n- /qqbot decrypt <bot_encrypt_secret> <bind_key> [--show]",
        enabled,
        configured,
        if qqbot_config_string(qqbot, &["appId", "app_id"]).filter(|value| !value.trim().is_empty()).is_some() {
            "present"
        } else {
            "missing"
        },
        if qqbot_config_string(qqbot, &["clientSecret", "client_secret", "secret", "token"]).filter(|value| !value.trim().is_empty()).is_some() {
            "present"
        } else {
            "missing"
        },
        home,
        portal
    ))
}

async fn qqbot_onboard_register(store: &AppStore, timeout_seconds: u64) -> AppResult<String> {
    let original_config = store.config()?;
    let portal_base = qqbot_onboard_portal_base_url(&original_config.qqbot)?;
    let client = reqwest::Client::builder()
        .timeout(StdDuration::from_secs(10))
        .build()
        .map_err(|error| {
            AppError::BadRequest(format!("failed to build QQBot onboard client: {error}"))
        })?;
    let deadline = Instant::now() + StdDuration::from_secs(timeout_seconds);
    let mut last_connect_url = String::new();
    let mut last_task_id = String::new();

    for refresh_count in 0..=3 {
        let bind_key = qqbot_generate_bind_key();
        let task_id = qqbot_create_bind_task(&client, &portal_base, &bind_key).await?;
        last_task_id = task_id.clone();
        last_connect_url = qqbot_build_connect_url(&task_id);

        while Instant::now() < deadline {
            match qqbot_poll_bind_result(&client, &portal_base, &task_id).await? {
                QqBotBindPoll::Pending | QqBotBindPoll::None => {
                    tokio::time::sleep(StdDuration::from_secs(2)).await;
                }
                QqBotBindPoll::Expired => {
                    if refresh_count >= 3 {
                        return Ok(format!(
                            "QQBot onboard QR 已过期且刷新次数已用完。\n- lastTaskId: {}\n- lastConnectUrl: {}",
                            last_task_id, last_connect_url
                        ));
                    }
                    break;
                }
                QqBotBindPoll::Completed {
                    app_id,
                    encrypted_secret,
                    user_openid,
                } => {
                    let client_secret = qqbot_decrypt_secret(&encrypted_secret, &bind_key)?;
                    let mut config = store.config()?;
                    if !config.qqbot.is_object() {
                        config.qqbot = json!({});
                    }
                    if let Some(object) = config.qqbot.as_object_mut() {
                        object.insert("enabled".into(), json!(true));
                        object.insert("appId".into(), json!(app_id));
                        object.insert("clientSecret".into(), json!(client_secret));
                        object.insert("portalBaseUrl".into(), json!(portal_base));
                    }
                    store.set_config(config)?;
                    return Ok(format!(
                        "QQBot onboard 完成，凭据已写入配置。\n- appId: {}\n- userOpenid: {}\n- taskId: {}\n- connectUrl: {}\nSecret values were not printed.",
                        app_id,
                        if user_openid.trim().is_empty() { "-" } else { user_openid.as_str() },
                        task_id,
                        last_connect_url
                    ));
                }
            }
        }
    }

    Ok(format!(
        "QQBot onboard 超时（{} 秒）。\n- lastTaskId: {}\n- lastConnectUrl: {}\n请用 QQ 手机端打开 connectUrl 扫码后重试。",
        timeout_seconds, last_task_id, last_connect_url
    ))
}

pub(super) fn qqbot_generate_bind_key() -> String {
    use base64::Engine as _;
    let mut bytes = Vec::with_capacity(32);
    bytes.extend_from_slice(Uuid::new_v4().as_bytes());
    bytes.extend_from_slice(Uuid::new_v4().as_bytes());
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

pub(super) fn qqbot_decrypt_secret(encrypted_base64: &str, key_base64: &str) -> AppResult<String> {
    use base64::Engine as _;
    let key = base64::engine::general_purpose::STANDARD
        .decode(key_base64)
        .map_err(|error| AppError::BadRequest(format!("invalid QQBot bind key base64: {error}")))?;
    if key.len() != 32 {
        return Err(AppError::BadRequest(format!(
            "invalid QQBot bind key length: expected 32 bytes, got {}",
            key.len()
        )));
    }
    let raw = base64::engine::general_purpose::STANDARD
        .decode(encrypted_base64)
        .map_err(|error| {
            AppError::BadRequest(format!("invalid QQBot encrypted secret base64: {error}"))
        })?;
    if raw.len() < 12 + 16 {
        return Err(AppError::BadRequest(
            "invalid QQBot encrypted secret: payload is too short".into(),
        ));
    }
    let (iv, ciphertext_with_tag) = raw.split_at(12);
    let cipher = Aes256Gcm::new_from_slice(&key)
        .map_err(|error| AppError::BadRequest(format!("invalid QQBot AES-GCM key: {error}")))?;
    let plaintext = cipher
        .decrypt(Nonce::from_slice(iv), ciphertext_with_tag)
        .map_err(|error| {
            AppError::BadRequest(format!("failed to decrypt QQBot secret: {error}"))
        })?;
    String::from_utf8(plaintext).map_err(|error| {
        AppError::BadRequest(format!("QQBot decrypted secret is not UTF-8: {error}"))
    })
}

pub(super) fn qqbot_build_connect_url(task_id: &str) -> String {
    format!(
        "https://q.qq.com/qqbot/openclaw/connect.html?task_id={}&_wv=2&source=hermes",
        qqbot_percent_encode_url_component(task_id)
    )
}

async fn qqbot_create_bind_task(
    client: &reqwest::Client,
    portal_base: &str,
    bind_key: &str,
) -> AppResult<String> {
    let response = client
        .post(format!("{portal_base}/lite/create_bind_task"))
        .headers(qqbot_onboard_headers())
        .json(&json!({ "key": bind_key }))
        .send()
        .await
        .map_err(|error| AppError::BadRequest(format!("QQBot create_bind_task failed: {error}")))?;
    let value = qqbot_onboard_response_json(response, "QQBot create_bind_task").await?;
    qqbot_onboard_retcode_ok(&value, "create_bind_task")?;
    value
        .pointer("/data/task_id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            AppError::BadRequest(format!("QQBot create_bind_task missing task_id: {value}"))
        })
}

async fn qqbot_poll_bind_result(
    client: &reqwest::Client,
    portal_base: &str,
    task_id: &str,
) -> AppResult<QqBotBindPoll> {
    let response = client
        .post(format!("{portal_base}/lite/poll_bind_result"))
        .headers(qqbot_onboard_headers())
        .json(&json!({ "task_id": task_id }))
        .send()
        .await
        .map_err(|error| AppError::BadRequest(format!("QQBot poll_bind_result failed: {error}")))?;
    let value = qqbot_onboard_response_json(response, "QQBot poll_bind_result").await?;
    qqbot_onboard_retcode_ok(&value, "poll_bind_result")?;
    let data = value.get("data").unwrap_or(&Value::Null);
    let status = data.get("status").and_then(Value::as_i64).unwrap_or(0);
    match status {
        1 => Ok(QqBotBindPoll::Pending),
        2 => Ok(QqBotBindPoll::Completed {
            app_id: data
                .get("bot_appid")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            encrypted_secret: data
                .get("bot_encrypt_secret")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            user_openid: data
                .get("user_openid")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        }),
        3 => Ok(QqBotBindPoll::Expired),
        _ => Ok(QqBotBindPoll::None),
    }
}

enum QqBotBindPoll {
    None,
    Pending,
    Completed {
        app_id: String,
        encrypted_secret: String,
        user_openid: String,
    },
    Expired,
}

fn qqbot_onboard_portal_base_url(config: &Value) -> AppResult<String> {
    let base = qqbot_config_string(config, &["portalBaseUrl", "portal_base_url"])
        .or_else(|| std::env::var("QQ_PORTAL_BASE_URL").ok())
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| {
            let scheme = qqbot_config_string(config, &["portalScheme", "portal_scheme"])
                .or_else(|| std::env::var("QQ_PORTAL_SCHEME").ok())
                .unwrap_or_else(|| "https".into());
            let host = qqbot_config_string(config, &["portalHost", "portal_host"])
                .or_else(|| std::env::var("QQ_PORTAL_HOST").ok())
                .unwrap_or_else(|| "q.qq.com".into());
            format!("{}://{}", scheme.trim().trim_end_matches(':'), host.trim())
        });
    reqwest::Url::parse(&base)
        .map_err(|error| AppError::BadRequest(format!("invalid QQBot portalBaseUrl: {error}")))?;
    Ok(base)
}

fn qqbot_onboard_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
    let user_agent = format!(
        "QQBotAdapter/1.1.0 (Rust/{}; SynthChat/{})",
        std::env::consts::OS,
        option_env!("CARGO_PKG_VERSION").unwrap_or("dev")
    );
    headers.insert(
        USER_AGENT,
        HeaderValue::from_str(&user_agent)
            .unwrap_or_else(|_| HeaderValue::from_static("QQBotAdapter/1.1.0")),
    );
    headers
}

async fn qqbot_onboard_response_json(response: reqwest::Response, label: &str) -> AppResult<Value> {
    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|error| AppError::BadRequest(format!("{label} response read failed: {error}")))?;
    if !status.is_success() {
        return Err(AppError::BadRequest(format!(
            "{label} failed ({}): {}",
            status.as_u16(),
            truncate_output(&text, 500)
        )));
    }
    serde_json::from_str::<Value>(&text)
        .map_err(|error| AppError::BadRequest(format!("{label} returned invalid JSON: {error}")))
}

fn qqbot_onboard_retcode_ok(value: &Value, label: &str) -> AppResult<()> {
    let retcode = value.get("retcode").and_then(Value::as_i64).unwrap_or(0);
    if retcode == 0 {
        return Ok(());
    }
    let message = value
        .get("msg")
        .and_then(Value::as_str)
        .unwrap_or("QQBot onboard request failed");
    Err(AppError::BadRequest(format!(
        "QQBot {label} retcode={retcode}: {message}"
    )))
}

fn qqbot_config_string(config: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| config.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn qqbot_percent_encode_url_component(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

fn auth_provider_matches_filter(provider: &LlmProvider, filter: &str) -> bool {
    let haystack = format!(
        "{} {} {} {} {} {}",
        provider.id,
        provider.name,
        provider.provider_type,
        provider.preset.as_deref().unwrap_or_default(),
        provider.base_url,
        provider.model
    )
    .to_ascii_lowercase();
    haystack.contains(filter)
}

fn format_auth_provider_status(provider: &LlmProvider) -> String {
    let auth_type = infer_auth_type_for_provider(provider);
    let auth_status = provider_auth_credential_status(provider);
    format!(
        "- {} ({}) type={} preset={} enabled={} auth={} status={}{}{}",
        provider.name,
        provider.id,
        empty_dash(&provider.provider_type),
        empty_dash(provider.preset.as_deref().unwrap_or_default()),
        provider.enabled,
        auth_type,
        auth_status.state,
        auth_status
            .source
            .as_deref()
            .map(|source| format!(" source={source}"))
            .unwrap_or_default(),
        if auth_status.hints.is_empty() {
            String::new()
        } else {
            format!(" hints={}", auth_status.hints.join(","))
        }
    )
}

struct ProviderAuthStatus {
    state: &'static str,
    source: Option<String>,
    hints: Vec<String>,
}

fn provider_auth_credential_status(provider: &LlmProvider) -> ProviderAuthStatus {
    if auth_provider_looks_like_bedrock(provider) {
        return bedrock_auth_credential_status();
    }
    if provider
        .api_key
        .as_deref()
        .map(auth_secret_present)
        .unwrap_or(false)
    {
        return ProviderAuthStatus {
            state: "present",
            source: Some("provider.apiKey".into()),
            hints: Vec::new(),
        };
    }

    let mut hints = BTreeSet::new();
    let configured_env = provider.api_key_env.trim();
    if !configured_env.is_empty() {
        if auth_looks_like_inline_secret(configured_env) {
            return ProviderAuthStatus {
                state: "present",
                source: Some("apiKeyEnv:inline".into()),
                hints: Vec::new(),
            };
        }
        hints.insert(configured_env.to_string());
        if auth_env_has_secret(configured_env) {
            return ProviderAuthStatus {
                state: "present",
                source: Some(format!("env:{configured_env}")),
                hints: Vec::new(),
            };
        }
    }

    for env_name in auth_env_candidates_for_provider(provider) {
        hints.insert(env_name.to_string());
        if auth_env_has_secret(env_name) {
            return ProviderAuthStatus {
                state: "present",
                source: Some(format!("env:{env_name}")),
                hints: Vec::new(),
            };
        }
    }
    let bitwarden_candidates = auth_env_candidates_for_provider(provider);
    if let Some(bitwarden_env) = bitwarden_candidates
        .iter()
        .copied()
        .find(|env_name| resolve_bitwarden_secret(std::slice::from_ref(env_name)).is_some())
    {
        return ProviderAuthStatus {
            state: "present",
            source: Some(format!("bitwarden:{bitwarden_env}")),
            hints: Vec::new(),
        };
    }

    if let Some(credential) = hermes_auth_store_status(provider) {
        return ProviderAuthStatus {
            state: "present",
            source: Some(credential.source),
            hints: credential
                .expires_at
                .map(|expires_at| vec![format!("expiresAt={expires_at}")])
                .unwrap_or_default(),
        };
    }
    if let Some(status) = hermes_auth_store_credential_status(provider) {
        let mut hints = Vec::new();
        if let Some(expires_at) = status.expires_at {
            hints.push(format!("expiresAt={expires_at}"));
        }
        if let Some(note) = status.note {
            hints.push(note);
        }
        return ProviderAuthStatus {
            state: status.state,
            source: Some(status.source),
            hints,
        };
    }
    if let Some(status) = hermes_external_credential_status(provider) {
        let mut hints = Vec::new();
        if let Some(expires_at) = status.expires_at {
            hints.push(format!("expiresAt={expires_at}"));
        }
        if let Some(note) = status.note {
            hints.push(note);
        }
        return ProviderAuthStatus {
            state: status.state,
            source: Some(status.source),
            hints,
        };
    }

    ProviderAuthStatus {
        state: "missing",
        source: None,
        hints: hints.into_iter().collect(),
    }
}

fn infer_auth_type_for_provider(provider: &LlmProvider) -> &'static str {
    let haystack = auth_provider_haystack(provider);
    if haystack.contains("bedrock") || haystack.contains("aws") {
        "aws_sdk"
    } else if haystack.contains("copilot") {
        "external_process"
    } else if haystack.contains("openai-codex") || haystack.contains("nous") {
        "oauth_device_code"
    } else if haystack.contains("oauth")
        || haystack.contains("qwen")
        || haystack.contains("google-gemini-cli")
        || haystack.contains("gemini-cli")
        || haystack.contains("minimax-oauth")
        || haystack.contains("xai-oauth")
        || haystack.contains("grok-oauth")
    {
        "oauth_external"
    } else {
        "api_key"
    }
}

fn auth_provider_looks_like_bedrock(provider: &LlmProvider) -> bool {
    let haystack = auth_provider_haystack(provider);
    haystack.contains("bedrock") || haystack.contains("aws")
}

fn auth_provider_haystack(provider: &LlmProvider) -> String {
    format!(
        "{} {} {} {} {} {}",
        provider.id,
        provider.name,
        provider.provider_type,
        provider.preset.as_deref().unwrap_or_default(),
        provider.base_url,
        provider.model
    )
    .to_ascii_lowercase()
}

fn bedrock_auth_credential_status() -> ProviderAuthStatus {
    let env_pair =
        auth_env_has_secret("AWS_ACCESS_KEY_ID") && auth_env_has_secret("AWS_SECRET_ACCESS_KEY");
    if env_pair {
        return ProviderAuthStatus {
            state: "present",
            source: Some("env:AWS_ACCESS_KEY_ID/AWS_SECRET_ACCESS_KEY".into()),
            hints: Vec::new(),
        };
    }
    let profile = std::env::var("AWS_PROFILE")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "default".into());
    if auth_static_aws_credentials_file_has_profile(&profile) {
        return ProviderAuthStatus {
            state: "present",
            source: Some(format!("aws-profile:{profile}")),
            hints: Vec::new(),
        };
    }
    ProviderAuthStatus {
        state: "missing",
        source: None,
        hints: [
            "AWS_ACCESS_KEY_ID",
            "AWS_SECRET_ACCESS_KEY",
            "AWS_PROFILE",
            "AWS_SHARED_CREDENTIALS_FILE",
        ]
        .into_iter()
        .map(str::to_string)
        .collect(),
    }
}

fn auth_static_aws_credentials_file_has_profile(profile: &str) -> bool {
    let Some(path) = auth_aws_shared_credentials_path() else {
        return false;
    };
    let Ok(text) = fs::read_to_string(path) else {
        return false;
    };
    let expected = format!("[{}]", profile.trim());
    text.lines().any(|line| line.trim() == expected)
}

fn auth_aws_shared_credentials_path() -> Option<std::path::PathBuf> {
    std::env::var_os("AWS_SHARED_CREDENTIALS_FILE")
        .filter(|value| !value.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|value| !value.is_empty())
                .or_else(|| std::env::var_os("USERPROFILE").filter(|value| !value.is_empty()))
                .map(std::path::PathBuf::from)
                .map(|home| home.join(".aws").join("credentials"))
        })
}

fn auth_env_candidates_for_provider(provider: &LlmProvider) -> Vec<&'static str> {
    let haystack = auth_provider_haystack(provider);
    let mut candidates = Vec::new();
    if haystack.contains("openrouter") {
        candidates.extend(["OPENROUTER_API_KEY", "OPENAI_API_KEY"]);
    } else if haystack.contains("anthropic")
        || provider.model.to_ascii_lowercase().contains("claude")
    {
        candidates.extend([
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_TOKEN",
            "CLAUDE_CODE_OAUTH_TOKEN",
        ]);
    } else if haystack.contains("gemini") || haystack.contains("google") {
        candidates.extend(["GOOGLE_API_KEY", "GEMINI_API_KEY"]);
    } else if haystack.contains("kimi") || haystack.contains("moonshot") {
        candidates.extend([
            "KIMI_API_KEY",
            "KIMI_CODING_API_KEY",
            "KIMI_CN_API_KEY",
            "MOONSHOT_API_KEY",
        ]);
    } else if haystack.contains("minimax") {
        candidates.extend(["MINIMAX_API_KEY", "MINIMAX_CN_API_KEY"]);
    } else if haystack.contains("xai") || haystack.contains("x.ai") || haystack.contains("grok") {
        candidates.push("XAI_API_KEY");
    } else if haystack.contains("zai") || haystack.contains("z.ai") || haystack.contains("glm") {
        candidates.extend(["GLM_API_KEY", "ZAI_API_KEY", "Z_AI_API_KEY"]);
    } else if haystack.contains("deepseek") {
        candidates.push("DEEPSEEK_API_KEY");
    } else if haystack.contains("stepfun") || haystack.contains("step-plan") {
        candidates.push("STEPFUN_API_KEY");
    } else if haystack.contains("copilot") || haystack.contains("github") {
        candidates.extend(["COPILOT_GITHUB_TOKEN", "GH_TOKEN", "GITHUB_TOKEN"]);
    } else if haystack.contains("opencode") {
        candidates.push("OPENCODE_API_KEY");
    } else if haystack.contains("kilo") {
        candidates.push("KILOCODE_API_KEY");
    } else if haystack.contains("huggingface") || haystack.contains("hugging-face") {
        candidates.extend(["HF_TOKEN", "HF_API_KEY", "HUGGINGFACE_API_KEY"]);
    } else if haystack.contains("novita") {
        candidates.push("NOVITA_API_KEY");
    } else if haystack.contains("nvidia") || haystack.contains("nemotron") {
        candidates.push("NVIDIA_API_KEY");
    } else if haystack.contains("xiaomi") || haystack.contains("mimo") {
        candidates.push("XIAOMI_API_KEY");
    } else if haystack.contains("tencent") || haystack.contains("tokenhub") {
        candidates.push("TOKENHUB_API_KEY");
    } else if haystack.contains("arcee") {
        candidates.push("ARCEE_API_KEY");
    } else if haystack.contains("gmi") {
        candidates.push("GMI_API_KEY");
    } else if haystack.contains("cohere") {
        candidates.push("COHERE_API_KEY");
    } else if haystack.contains("dashscope")
        || haystack.contains("alibaba")
        || haystack.contains("qwen")
    {
        candidates.extend(["DASHSCOPE_API_KEY", "ALIBABA_CODING_PLAN_API_KEY"]);
    } else if provider.provider_type.eq_ignore_ascii_case("openai")
        || provider.id.to_ascii_lowercase().contains("openai")
        || provider
            .preset
            .as_deref()
            .unwrap_or_default()
            .to_ascii_lowercase()
            .contains("openai")
        || provider
            .base_url
            .to_ascii_lowercase()
            .contains("api.openai.com")
    {
        candidates.extend(["OPENAI_API_KEY", "OPENROUTER_API_KEY"]);
    }
    candidates
}

fn auth_env_has_secret(env_name: &str) -> bool {
    std::env::var(env_name)
        .ok()
        .map(|value| auth_secret_present(value.trim()))
        .unwrap_or(false)
}

fn auth_secret_present(value: &str) -> bool {
    let lower = value.trim().to_ascii_lowercase();
    !lower.is_empty()
        && !matches!(
            lower.as_str(),
            "placeholder" | "changeme" | "change-me" | "todo" | "none" | "null"
        )
}

fn auth_looks_like_inline_secret(value: &str) -> bool {
    let trimmed = value.trim();
    trimmed.starts_with("sk-")
        || trimmed.starts_with("or-")
        || trimmed.starts_with("sk_")
        || trimmed.starts_with("xai-")
        || trimmed.starts_with("ghp_")
        || trimmed.starts_with("github_pat_")
}

fn empty_dash(value: &str) -> &str {
    if value.trim().is_empty() {
        "-"
    } else {
        value.trim()
    }
}

async fn fetch_openrouter_account_usage_for_config(store: &AppStore) -> AppResult<String> {
    let provider = store
        .providers()?
        .into_iter()
        .find(provider_looks_like_openrouter)
        .ok_or_else(|| AppError::BadRequest("no OpenRouter provider configured".into()))?;
    let token = provider_control_api_key(&provider)
        .ok_or_else(|| AppError::BadRequest("OpenRouter API key is not configured".into()))?;
    let base_url = if provider.base_url.trim().is_empty() {
        "https://openrouter.ai/api/v1".into()
    } else {
        provider.base_url.trim().trim_end_matches('/').to_string()
    };
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(
            provider.timeout_seconds.clamp(1, 30),
        ))
        .build()
        .map_err(|error| AppError::BadRequest(format!("OpenRouter client error: {error}")))?;
    let credits = client
        .get(format!("{base_url}/credits"))
        .bearer_auth(&token)
        .header("Accept", "application/json")
        .send()
        .await
        .and_then(|response| response.error_for_status())
        .map_err(|error| AppError::BadRequest(format!("OpenRouter credits error: {error}")))?
        .json::<Value>()
        .await
        .map_err(|error| AppError::BadRequest(format!("OpenRouter credits JSON error: {error}")))?;
    let key = match client
        .get(format!("{base_url}/key"))
        .bearer_auth(&token)
        .header("Accept", "application/json")
        .send()
        .await
    {
        Ok(response) => match response.error_for_status() {
            Ok(response) => response.json::<Value>().await.unwrap_or_else(|_| json!({})),
            Err(_) => json!({}),
        },
        Err(_) => json!({}),
    };
    Ok(format_openrouter_account_usage(&credits, &key))
}

fn provider_looks_like_openrouter(provider: &LlmProvider) -> bool {
    let haystack = format!(
        "{} {} {} {} {}",
        provider.id,
        provider.name,
        provider.provider_type,
        provider.preset.as_deref().unwrap_or_default(),
        provider.base_url
    )
    .to_ascii_lowercase();
    haystack.contains("openrouter")
}

pub(super) fn provider_control_api_key(provider: &LlmProvider) -> Option<String> {
    provider
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|value| auth_secret_present(value))
        .map(str::to_string)
        .or_else(|| {
            let env_field = provider.api_key_env.trim();
            if env_field.is_empty() {
                return None;
            }
            if env_field.contains("sk-") || env_field.starts_with("or-") {
                return auth_secret_present(env_field).then(|| env_field.to_string());
            }
            std::env::var(env_field)
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| auth_secret_present(value))
        })
        .or_else(|| {
            auth_env_candidates_for_provider(provider)
                .into_iter()
                .find_map(|env_name| {
                    std::env::var(env_name)
                        .ok()
                        .map(|value| value.trim().to_string())
                        .filter(|value| auth_secret_present(value))
                })
        })
        .or_else(|| {
            resolve_hermes_runtime_credential(provider).and_then(|credential| {
                auth_secret_present(&credential.api_key).then_some(credential.api_key)
            })
        })
        .or_else(|| {
            let candidates = auth_env_candidates_for_provider(provider);
            resolve_bitwarden_secret(&candidates)
        })
}

pub(super) fn format_openrouter_account_usage(credits: &Value, key: &Value) -> String {
    let credits_data = credits.get("data").unwrap_or(credits);
    let key_data = key.get("data").unwrap_or(key);
    let total_credits = credits_data
        .get("total_credits")
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    let total_usage = credits_data
        .get("total_usage")
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    let balance = (total_credits - total_usage).max(0.0);
    let mut lines = vec![
        "Account usage：OpenRouter".to_string(),
        format!("- Credits balance: ${balance:.2}"),
    ];
    let limit = key_data.get("limit").and_then(Value::as_f64);
    let remaining = key_data.get("limit_remaining").and_then(Value::as_f64);
    if let (Some(limit), Some(remaining)) = (limit, remaining) {
        if limit > 0.0 {
            let used = ((limit - remaining).max(0.0) / limit * 100.0).round();
            let remaining_pct = (100.0 - used).max(0.0);
            let reset = key_data
                .get("limit_reset")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .map(|value| format!("; resets {value}"))
                .unwrap_or_default();
            lines.push(format!(
                "- API key quota: {remaining_pct:.0}% remaining ({used:.0}% used); ${remaining:.2} of ${limit:.2} remaining{reset}"
            ));
        }
    }
    if let Some(usage) = key_data.get("usage").and_then(Value::as_f64) {
        let mut usage_parts = vec![format!("${usage:.2} total")];
        for (field, label) in [
            ("usage_daily", "today"),
            ("usage_weekly", "this week"),
            ("usage_monthly", "this month"),
        ] {
            if let Some(value) = key_data.get(field).and_then(Value::as_f64) {
                if value > 0.0 {
                    usage_parts.push(format!("${value:.2} {label}"));
                }
            }
        }
        lines.push(format!("- API key usage: {}", usage_parts.join(" · ")));
    }
    lines.join("\n")
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

pub(super) fn handle_yolo_control_command(
    store: &AppStore,
    argument_raw: &str,
) -> AppResult<String> {
    let action = argument_raw
        .split_whitespace()
        .next()
        .unwrap_or("toggle")
        .to_lowercase();
    let mut config = store.config()?;
    let current = config.chat.tool_approval_mode.trim().to_lowercase();
    let next = match action.as_str() {
        "" | "toggle" => {
            if current == "never" {
                "risky"
            } else {
                "never"
            }
        }
        "on" | "enable" | "enabled" | "true" | "yes" | "allow" | "never" => "never",
        "off" | "disable" | "disabled" | "false" | "no" | "risky" => "risky",
        "status" | "show" => {
            return format_approval_policy_reply(store);
        }
        other => {
            return Ok(format!(
                "未知 YOLO 参数：{other}。用法：/yolo [on|off|status|toggle]"
            ));
        }
    };
    config.chat.tool_approval_mode = next.into();
    store.set_config(config)?;
    let mut reply = format_approval_policy_reply(store)?;
    reply.push_str(if next == "never" {
        "\nYOLO 已启用：普通工具审批默认跳过；hardline 风险仍会阻断。"
    } else {
        "\nYOLO 已关闭：审批模式已恢复为 risky。"
    });
    Ok(reply)
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
    argument_raw: &str,
) -> AppResult<String> {
    let mut parts = argument_raw.split_whitespace();
    let action = parts.next().unwrap_or("status").to_ascii_lowercase();
    match action.as_str() {
        "" | "status" | "show" | "current" => {
            format_profile_control_status(store, conversation, persona)
        }
        "list" | "ls" => format_profile_list(store),
        "create" | "new" => {
            let name = parts.next().ok_or_else(|| {
                AppError::BadRequest(
                    "用法：/profile create <name> [--clone [source]] [--clone-all [source]]".into(),
                )
            })?;
            let tail = parts.collect::<Vec<_>>();
            let (clone_from, clone_all) = parse_profile_clone_flags(store, &tail)?;
            let profile_dir =
                create_desktop_profile_snapshot(store, name, clone_from.as_deref(), clone_all)?;
            Ok(format!(
                "已创建 Hermes-style desktop profile：{}\n- path: {}\n- cloneAll: {}",
                normalize_desktop_profile_name(name)?,
                profile_dir.to_string_lossy(),
                clone_all
            ))
        }
        "use" | "switch" | "activate" => {
            let name = parts
                .next()
                .ok_or_else(|| AppError::BadRequest("用法：/profile use <name>".into()))?;
            let snapshot = load_desktop_profile_snapshot(store, name)?;
            apply_desktop_profile_snapshot(store, &snapshot)?;
            set_active_desktop_profile(store, name)?;
            Ok(format!(
                "已切换 Hermes-style desktop profile：{}",
                normalize_desktop_profile_name(name)?
            ))
        }
        "clone" => {
            let source = parts.next().ok_or_else(|| {
                AppError::BadRequest("用法：/profile clone <source> <target> [--clone-all]".into())
            })?;
            let target = parts.next().ok_or_else(|| {
                AppError::BadRequest("用法：/profile clone <source> <target> [--clone-all]".into())
            })?;
            let flags = parts.collect::<Vec<_>>();
            let clone_all = flags.iter().any(|flag| *flag == "--clone-all");
            let profile_dir =
                create_desktop_profile_snapshot(store, target, Some(source), clone_all)?;
            Ok(format!(
                "已克隆 Hermes-style desktop profile：{} -> {}\n- path: {}\n- cloneAll: {}",
                normalize_desktop_profile_name(source)?,
                normalize_desktop_profile_name(target)?,
                profile_dir.to_string_lossy(),
                clone_all
            ))
        }
        "delete" | "remove" | "rm" => {
            let name = parts
                .next()
                .ok_or_else(|| AppError::BadRequest("用法：/profile delete <name>".into()))?;
            let removed = delete_desktop_profile(store, name)?;
            Ok(format!(
                "已删除 Hermes-style desktop profile：{}\n- removed: {}",
                normalize_desktop_profile_name(name)?,
                removed.to_string_lossy()
            ))
        }
        "export" => {
            let name = parts.next().ok_or_else(|| {
                AppError::BadRequest("用法：/profile export <name> <path>".into())
            })?;
            let output = parts.next().ok_or_else(|| {
                AppError::BadRequest("用法：/profile export <name> <path>".into())
            })?;
            let snapshot = if normalize_desktop_profile_name(name)? == "desktop" {
                current_desktop_profile_snapshot(store, "desktop", false)?
            } else {
                load_desktop_profile_snapshot(store, name)?
            };
            let output_path = PathBuf::from(output);
            if let Some(parent) = output_path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
            {
                fs::create_dir_all(parent)?;
            }
            fs::write(&output_path, serde_json::to_string_pretty(&snapshot)?)?;
            Ok(format!(
                "已导出 Hermes-style desktop profile：{}\n- output: {}",
                normalize_desktop_profile_name(name)?,
                output_path.to_string_lossy()
            ))
        }
        "import" => {
            let archive = parts.next().ok_or_else(|| {
                AppError::BadRequest("用法：/profile import <path> [name]".into())
            })?;
            let override_name = parts.next();
            let text = fs::read_to_string(archive)?;
            let mut snapshot = serde_json::from_str::<Value>(&text)?;
            if let Some(name) = override_name {
                snapshot["name"] = json!(normalize_desktop_profile_name(name)?);
            }
            let name = snapshot
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| AppError::BadRequest("profile archive missing name".into()))?
                .to_string();
            let profile_dir = write_desktop_profile_snapshot(store, &name, &snapshot, false)?;
            Ok(format!(
                "已导入 Hermes-style desktop profile：{}\n- path: {}",
                normalize_desktop_profile_name(&name)?,
                profile_dir.to_string_lossy()
            ))
        }
        _ => Ok(
            r"用法：/profile [status|list|create|use|clone|delete|export|import]
示例：/profile create coder --clone
示例：/profile export desktop D:\profile-coder.json"
                .into(),
        ),
    }
}

fn format_profile_control_status(
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
    let active_profile = active_desktop_profile(store);
    Ok(format!(
        "当前 Profile：\n- user: {}\n- avatarPath: {}\n- desktopProfile: {}\n- persona: {} ({})\n- agent: {} ({})\n- conversation: {}\n- profilesRoot: {}\n\nProfile commands: /profile list, /profile create <name> [--clone], /profile use <name>, /profile export <name> <path>",
        profile.name,
        avatar,
        active_profile,
        persona.name,
        persona.id,
        agent.name,
        agent.id,
        conversation.id,
        desktop_profiles_root(store).to_string_lossy()
    ))
}

fn format_profile_list(store: &AppStore) -> AppResult<String> {
    let active = active_desktop_profile(store);
    let mut lines = vec![format!(
        "Hermes-style desktop profiles:\n- desktop{} path={} kind=current-store",
        if active == "desktop" { " *" } else { "" },
        store.data_dir().to_string_lossy()
    )];
    let root = desktop_profiles_root(store);
    if let Ok(entries) = fs::read_dir(&root) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
                continue;
            };
            if normalize_desktop_profile_name(name).is_err() {
                continue;
            }
            let snapshot_path = path.join("profile.json");
            let snapshot = read_gateway_runtime_json(&snapshot_path).unwrap_or_else(|| json!({}));
            let created_at = snapshot
                .get("createdAt")
                .and_then(Value::as_str)
                .unwrap_or("-");
            let persona_count = snapshot
                .get("personas")
                .and_then(Value::as_array)
                .map(Vec::len)
                .unwrap_or(0);
            let agent_count = snapshot
                .get("agents")
                .and_then(Value::as_array)
                .map(Vec::len)
                .unwrap_or(0);
            lines.push(format!(
                "- {}{} path={} personas={} agents={} createdAt={}",
                name,
                if active == name { " *" } else { "" },
                path.to_string_lossy(),
                persona_count,
                agent_count,
                created_at
            ));
        }
    }
    Ok(lines.join("\n"))
}

fn desktop_hermes_home(store: &AppStore) -> PathBuf {
    store.data_dir().join(".hermes")
}

fn desktop_profiles_root(store: &AppStore) -> PathBuf {
    desktop_hermes_home(store).join("profiles")
}

fn desktop_active_profile_path(store: &AppStore) -> PathBuf {
    desktop_hermes_home(store).join("active_profile")
}

fn normalize_desktop_profile_name(name: &str) -> AppResult<String> {
    let normalized = name.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return Err(AppError::BadRequest("profile name cannot be empty".into()));
    }
    if normalized == "default" {
        return Ok("desktop".into());
    }
    let valid = normalized.len() <= 64
        && normalized.chars().enumerate().all(|(index, ch)| {
            if index == 0 {
                ch.is_ascii_lowercase() || ch.is_ascii_digit()
            } else {
                ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-' || ch == '_'
            }
        });
    if !valid {
        return Err(AppError::BadRequest(format!(
            "Invalid profile name {name:?}. Must match [a-z0-9][a-z0-9_-]{{0,63}}"
        )));
    }
    if matches!(
        normalized.as_str(),
        "hermes" | "test" | "tmp" | "root" | "sudo" | "profile" | "chat" | "gateway"
    ) {
        return Err(AppError::BadRequest(format!(
            "Profile name {normalized:?} is reserved"
        )));
    }
    Ok(normalized)
}

fn desktop_profile_dir(store: &AppStore, name: &str) -> AppResult<PathBuf> {
    let normalized = normalize_desktop_profile_name(name)?;
    if normalized == "desktop" {
        return Ok(store.data_dir());
    }
    Ok(desktop_profiles_root(store).join(normalized))
}

fn active_desktop_profile(store: &AppStore) -> String {
    fs::read_to_string(desktop_active_profile_path(store))
        .ok()
        .and_then(|text| normalize_desktop_profile_name(&text).ok())
        .unwrap_or_else(|| "desktop".into())
}

fn set_active_desktop_profile(store: &AppStore, name: &str) -> AppResult<()> {
    let normalized = normalize_desktop_profile_name(name)?;
    if normalized != "desktop" {
        let dir = desktop_profile_dir(store, &normalized)?;
        if !dir.join("profile.json").exists() {
            return Err(AppError::NotFound(format!("profile {normalized}")));
        }
    }
    let path = desktop_active_profile_path(store);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, normalized)?;
    Ok(())
}

fn parse_profile_clone_flags(
    store: &AppStore,
    parts: &[&str],
) -> AppResult<(Option<String>, bool)> {
    let mut clone_from = None;
    let mut clone_all = false;
    let mut index = 0usize;
    while index < parts.len() {
        match parts[index] {
            "--clone" => {
                if let Some(next) = parts
                    .get(index + 1)
                    .filter(|value| !value.starts_with("--"))
                {
                    clone_from = Some(normalize_desktop_profile_name(next)?);
                    index += 1;
                } else {
                    clone_from = Some(active_desktop_profile(store));
                }
            }
            "--clone-all" => {
                clone_all = true;
                if let Some(next) = parts
                    .get(index + 1)
                    .filter(|value| !value.starts_with("--"))
                {
                    clone_from = Some(normalize_desktop_profile_name(next)?);
                    index += 1;
                } else {
                    clone_from = Some(active_desktop_profile(store));
                }
            }
            other => {
                return Err(AppError::BadRequest(format!(
                    "未知 profile 参数：{other}。可用：--clone [source], --clone-all [source]"
                )));
            }
        }
        index += 1;
    }
    Ok((clone_from, clone_all))
}

fn current_desktop_profile_snapshot(
    store: &AppStore,
    name: &str,
    clone_all: bool,
) -> AppResult<Value> {
    Ok(json!({
        "kind": "synthchat_desktop_profile",
        "schema": "hermes_profile_desktop_snapshot_v1",
        "name": normalize_desktop_profile_name(name)?,
        "createdAt": now_iso(),
        "desktopAdaptation": true,
        "cloneAll": clone_all,
        "profile": store.profile()?,
        "personas": store.personas()?,
        "agents": store.agents()?,
        "config": store.config()?.chat,
        "excluded": ["provider API keys", "OAuth tokens", "runtime process state", "conversation transcripts"],
        "note": "SynthChat desktop adapts Hermes isolated HERMES_HOME profiles as non-secret profile snapshots under data_dir/.hermes/profiles.",
    }))
}

fn load_desktop_profile_snapshot(store: &AppStore, name: &str) -> AppResult<Value> {
    let normalized = normalize_desktop_profile_name(name)?;
    if normalized == "desktop" {
        return current_desktop_profile_snapshot(store, "desktop", false);
    }
    let path = desktop_profile_dir(store, &normalized)?.join("profile.json");
    if !path.exists() {
        return Err(AppError::NotFound(format!("profile {normalized}")));
    }
    let text = fs::read_to_string(path)?;
    let snapshot = serde_json::from_str::<Value>(&text)?;
    Ok(snapshot)
}

fn write_desktop_profile_snapshot(
    store: &AppStore,
    name: &str,
    snapshot: &Value,
    overwrite: bool,
) -> AppResult<PathBuf> {
    let normalized = normalize_desktop_profile_name(name)?;
    if normalized == "desktop" {
        return Err(AppError::BadRequest(
            "desktop is the active store profile and cannot be overwritten as a named profile"
                .into(),
        ));
    }
    let root = desktop_profiles_root(store);
    fs::create_dir_all(&root)?;
    let root_abs = root.canonicalize()?;
    let profile_dir = root.join(&normalized);
    if profile_dir.exists() && !overwrite {
        return Err(AppError::BadRequest(format!(
            "Profile '{normalized}' already exists"
        )));
    }
    if profile_dir.exists() {
        let profile_abs = profile_dir.canonicalize()?;
        if !profile_abs.starts_with(&root_abs) {
            return Err(AppError::BadRequest(
                "profile path escaped profiles root".into(),
            ));
        }
    }
    fs::create_dir_all(&profile_dir)?;
    let mut saved = snapshot.clone();
    saved["name"] = json!(normalized);
    saved["savedAt"] = json!(now_iso());
    gateway_pairing_write_json(&profile_dir.join("profile.json"), &saved)?;
    gateway_pairing_write_json(
        &profile_dir.join("profile.yaml.json"),
        &json!({
            "description": saved.get("description").and_then(Value::as_str).unwrap_or(""),
            "descriptionAuto": false,
            "desktopAdaptation": true,
        }),
    )?;
    Ok(profile_dir)
}

fn create_desktop_profile_snapshot(
    store: &AppStore,
    name: &str,
    clone_from: Option<&str>,
    clone_all: bool,
) -> AppResult<PathBuf> {
    let source = clone_from.unwrap_or("desktop");
    let mut snapshot = load_desktop_profile_snapshot(store, source)
        .or_else(|_| current_desktop_profile_snapshot(store, "desktop", clone_all))?;
    snapshot["sourceProfile"] = json!(normalize_desktop_profile_name(source)?);
    snapshot["cloneAll"] = json!(clone_all);
    write_desktop_profile_snapshot(store, name, &snapshot, false)
}

fn apply_desktop_profile_snapshot(store: &AppStore, snapshot: &Value) -> AppResult<()> {
    if let Some(profile) = snapshot.get("profile") {
        store.set_profile(serde_json::from_value(profile.clone())?)?;
    }
    if let Some(config) = snapshot.get("config") {
        let mut app_config = store.config()?;
        app_config.chat = serde_json::from_value(config.clone())?;
        store.set_config(app_config)?;
    }
    if let Some(personas) = snapshot.get("personas").and_then(Value::as_array) {
        for persona in personas {
            store.save_persona(serde_json::from_value(persona.clone())?)?;
        }
    }
    if let Some(agents) = snapshot.get("agents").and_then(Value::as_array) {
        for agent in agents {
            store.save_agent(serde_json::from_value(agent.clone())?)?;
        }
    }
    Ok(())
}

fn delete_desktop_profile(store: &AppStore, name: &str) -> AppResult<PathBuf> {
    let normalized = normalize_desktop_profile_name(name)?;
    if normalized == "desktop" {
        return Err(AppError::BadRequest(
            "Cannot delete the active desktop store profile".into(),
        ));
    }
    if active_desktop_profile(store) == normalized {
        return Err(AppError::BadRequest(format!(
            "Cannot delete active profile '{normalized}'. Switch to desktop or another profile first."
        )));
    }
    let root = desktop_profiles_root(store);
    let profile_dir = desktop_profile_dir(store, &normalized)?;
    if !profile_dir.exists() {
        return Err(AppError::NotFound(format!("profile {normalized}")));
    }
    let root_abs = root.canonicalize()?;
    let profile_abs = profile_dir.canonicalize()?;
    if !profile_abs.starts_with(&root_abs) {
        return Err(AppError::BadRequest(
            "profile path escaped profiles root".into(),
        ));
    }
    fs::remove_dir_all(&profile_abs)?;
    Ok(profile_abs)
}

pub(super) fn handle_config_control_command(store: &AppStore) -> AppResult<String> {
    let config = store.config()?;
    let chat = config.chat;
    Ok(format!(
        "Agent/Chat 配置：\n- agentEngine: {}\n- busyInputMode: {}\n- autoTitle: {}\n- toolUseEnforcement: {}\n- toolApprovalMode: {}\n- toolParallel: {} (limit {})\n- queueWaitSeconds: {}\n- maxContextRounds: {}\n- shortContext: {} / {} tokens\n- intentAnalyzerMode: {}\n- toolRouterMode: {}\n- statusbar: {}\n- toolProgressDisplay: {}\n- skin: {}\n- indicator: {}\n- codexRuntime: {}\n- trustedToolPatterns: {}\n- trustedCommandPatterns: {}\n- skillHotReload: {} ({}s)\n- retention: {} ({} days)\n- storageLimits: messagesPerConversation={} agentRuns={} toolTraces={}",
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
        if chat.statusbar_enabled {
            "enabled"
        } else {
            "disabled"
        },
        chat.tool_progress_display,
        chat.display_skin,
        chat.busy_indicator_style,
        chat.codex_runtime,
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

pub(super) fn handle_snapshot_control_command(
    store: &AppStore,
    argument_raw: &str,
) -> AppResult<String> {
    let mut parts = argument_raw.split_whitespace();
    let action = parts.next().unwrap_or("list").to_lowercase();
    match action.as_str() {
        "" | "list" | "ls" | "status" | "show" => format_snapshot_control_status(store),
        "create" | "new" | "save" => {
            let label = parts.collect::<Vec<_>>().join(" ");
            let label = if label.trim().is_empty() {
                "manual snapshot"
            } else {
                label.trim()
            };
            let snapshot = store.create_state_snapshot(label)?;
            Ok(format!(
                "state snapshot 已创建：{}\nlabel={}\npath={}",
                snapshot_string(&snapshot, "id"),
                snapshot_string(&snapshot, "label"),
                snapshot_string(&snapshot, "statePath")
            ))
        }
        "restore" => {
            let selector = parts.next().unwrap_or_default();
            if selector.trim().is_empty() {
                return Ok("用法：/snapshot restore <snapshot-id-prefix>".into());
            }
            let snapshot_id = resolve_state_snapshot_selector(store, selector)?;
            let restored = store.restore_state_snapshot(&snapshot_id)?;
            Ok(format!(
                "state snapshot 已恢复：{}\n恢复前自动备份：{}",
                restored
                    .get("restored")
                    .map(|value| snapshot_string(value, "id"))
                    .unwrap_or_else(|| snapshot_id.clone()),
                restored
                    .get("preRestore")
                    .map(|value| snapshot_string(value, "id"))
                    .unwrap_or_else(|| "-".into())
            ))
        }
        "prune" | "cleanup" => {
            let keep = parts
                .next()
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(10);
            let deleted = store.prune_state_snapshots(keep)?;
            Ok(format!(
                "state snapshot 裁剪完成：保留最近 {} 个，删除 {} 个。",
                keep.max(1),
                deleted
            ))
        }
        _ => Ok("用法：/snapshot [list|create [label]|restore <id>|prune [keep]]".into()),
    }
}

pub(super) fn format_snapshot_control_status(store: &AppStore) -> AppResult<String> {
    let snapshots = store.state_snapshots()?;
    let workspace_snapshots = store.workspace_snapshots()?;
    if snapshots.is_empty() {
        return Ok(format!(
            "state snapshots：0\nworkspace snapshots：{}\n创建：/snapshot create [label]",
            workspace_snapshots.len()
        ));
    }
    let rows = snapshots
        .iter()
        .take(12)
        .map(|snapshot| {
            format!(
                "- {} created={} label={}",
                snapshot_string(snapshot, "id"),
                snapshot_string(snapshot, "createdAt"),
                snapshot_string(snapshot, "label")
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let suffix = if snapshots.len() > 12 {
        format!("\n... 还有 {} 个未显示", snapshots.len() - 12)
    } else {
        String::new()
    };
    Ok(format!(
        "state snapshots：{}\n{}{}\nworkspace snapshots：{}\n恢复：/snapshot restore <id前缀>",
        snapshots.len(),
        rows,
        suffix,
        workspace_snapshots.len()
    ))
}

fn resolve_state_snapshot_selector(store: &AppStore, selector: &str) -> AppResult<String> {
    let selector = selector.trim();
    let snapshots = store.state_snapshots()?;
    let mut matches = snapshots
        .iter()
        .filter_map(|snapshot| snapshot.get("id").and_then(Value::as_str))
        .filter(|id| *id == selector || id.starts_with(selector))
        .map(str::to_string)
        .collect::<Vec<_>>();
    matches.sort();
    matches.dedup();
    match matches.len() {
        0 => Err(AppError::NotFound(format!("state snapshot {selector}"))),
        1 => Ok(matches.remove(0)),
        _ => Err(AppError::BadRequest(format!(
            "snapshot selector {selector} 匹配多个快照：{}",
            matches.join(", ")
        ))),
    }
}

fn snapshot_string(snapshot: &Value, key: &str) -> String {
    snapshot
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or("-")
        .to_string()
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
    let (platform, action) = if matches!(platform, "pause" | "resume") {
        (parts.get(1).map(String::as_str).unwrap_or("all"), platform)
    } else if matches!(
        platform,
        "status"
            | "list"
            | "show"
            | "forensics"
            | "diagnostics"
            | "snapshot"
            | "memory"
            | "mem"
            | "restart"
            | "stop"
            | "halt"
            | "profiles"
            | "profile-status"
            | "service"
            | "services"
            | "slash-access"
            | "slash_access"
            | "pairing"
            | "pair"
            | "feishu-comment"
            | "feishu_comment"
            | "feishu-comments"
            | "feishu_comments"
    ) {
        ("all", platform)
    } else {
        (platform, action)
    };
    if platform == "all" {
        return match action {
            "status" | "list" | "show" => {
                let state = platform_adapter_status(store, None)?;
                let runtime = platform_runtime_status_snapshot(store, None)?;
                Ok(format!(
                    "{}\n{}",
                    format_platform_adapter_statuses(&state),
                    format_platform_runtime_status_line(&runtime)
                ))
            }
            "forensics" | "diagnostics" | "snapshot" => {
                let snapshot = platform_forensics_snapshot(store, None)?;
                Ok(format_platform_forensics_snapshot(&snapshot)?)
            }
            "memory" | "mem" => {
                let tag = parts.get(1).map(String::as_str).unwrap_or("status");
                let snapshot = platform_memory_monitor_snapshot(store, None, tag)?;
                Ok(format_platform_memory_monitor_snapshot(&snapshot)?)
            }
            "restart" => {
                let (drain_timeout, replace_requested, via_service, detached_restart) =
                    parse_gateway_restart_options_from_tail(
                        store,
                        &parts.iter().skip(1).map(String::as_str).collect::<Vec<_>>(),
                    )?;
                let snapshot = platform_restart_request_snapshot(
                    store,
                    None,
                    drain_timeout,
                    replace_requested,
                    via_service,
                    detached_restart,
                )?;
                Ok(format_platform_restart_request(&snapshot)?)
            }
            "stop" | "halt" => {
                let snapshot = platform_planned_stop_snapshot(store, None, "control_command")?;
                Ok(format_platform_planned_stop(&snapshot)?)
            }
            "profiles" | "profile-status" => {
                let snapshot = platform_gateway_profiles_snapshot(store)?;
                Ok(format_platform_gateway_profiles_snapshot(&snapshot)?)
            }
            "service" | "services" => {
                let snapshot =
                    platform_gateway_service_snapshot(store, parts.get(1).map(String::as_str))?;
                Ok(format_platform_gateway_service_snapshot(&snapshot)?)
            }
            "slash-access" | "slash_access" => {
                Ok(format_platform_slash_access_snapshot(store, None)?)
            }
            "pairing" | "pair" => {
                let tail = argument_raw
                    .split_whitespace()
                    .skip(1)
                    .collect::<Vec<_>>()
                    .join(" ");
                handle_platform_pairing_control_command(store, None, &tail)
            }
            "feishu-comment" | "feishu_comment" | "feishu-comments" | "feishu_comments" => {
                let tail = argument_raw
                    .split_whitespace()
                    .skip(1)
                    .collect::<Vec<_>>()
                    .join(" ");
                handle_feishu_comment_control_command(store, &tail)
            }
            "pause" => Ok("用法：/platform pause <platform>".into()),
            "resume" => Ok("用法：/platform resume <platform>".into()),
            _ => Ok(platforms_control_usage()),
        };
    }
    let state = match action {
        "status" | "list" | "show" => platform_adapter_status(store, Some(platform))?,
        "start" | "run" | "resume" | "retry" => {
            let Some(app_handle) = app.cloned() else {
                return Ok("当前运行环境不支持启动平台 adapter。".into());
            };
            start_platform_adapter(store, app_handle, platform).await?
        }
        "stop" | "halt" | "pause" => {
            let state = stop_platform_adapter(store, platform)?;
            if matches!(action, "stop" | "halt") {
                let _ = platform_planned_stop_snapshot(store, Some(platform), "control_command");
            }
            state
        }
        "forensics" | "diagnostics" | "snapshot" => {
            let snapshot = platform_forensics_snapshot(store, Some(platform))?;
            return Ok(format_platform_forensics_snapshot(&snapshot)?);
        }
        "memory" | "mem" => {
            let tag = parts.get(2).map(String::as_str).unwrap_or("status");
            let snapshot = platform_memory_monitor_snapshot(store, Some(platform), tag)?;
            return Ok(format_platform_memory_monitor_snapshot(&snapshot)?);
        }
        "restart" => {
            let (drain_timeout, replace_requested, via_service, detached_restart) =
                parse_gateway_restart_options_from_tail(
                    store,
                    &parts.iter().skip(2).map(String::as_str).collect::<Vec<_>>(),
                )?;
            let snapshot = platform_restart_request_snapshot(
                store,
                Some(platform),
                drain_timeout,
                replace_requested,
                via_service,
                detached_restart,
            )?;
            return Ok(format_platform_restart_request(&snapshot)?);
        }
        "slash-access" | "slash_access" => {
            return Ok(format_platform_slash_access_snapshot(
                store,
                Some(platform),
            )?);
        }
        "pairing" | "pair" => {
            let tail = argument_raw
                .split_whitespace()
                .skip(2)
                .collect::<Vec<_>>()
                .join(" ");
            return handle_platform_pairing_control_command(store, Some(platform), &tail);
        }
        _ => {
            return Ok(platforms_control_usage());
        }
    };
    let mut text = format_platform_adapter_state(&state);
    if action == "pause" {
        text.push_str("\nHermes compat: /platform pause maps to stopping this SynthChat adapter.");
    } else if matches!(action, "resume" | "retry") {
        text.push_str(
            "\nHermes compat: /platform resume maps to starting/retrying this SynthChat adapter.",
        );
    }
    Ok(text)
}

fn platforms_control_usage() -> String {
    "用法：/platforms [status|list|forensics|memory|restart|stop|profiles|service|slash-access|pairing|feishu-comment]、/platforms <platform> [status|start|stop|restart|forensics|memory|slash-access|pairing]，或 Hermes 兼容：/platform pause|resume <platform>".into()
}

fn format_platform_slash_access_snapshot(
    store: &AppStore,
    platform: Option<&str>,
) -> AppResult<String> {
    let config = store.config()?.messaging_gateway;
    let platforms = match platform {
        Some(platform) => vec![platform.to_ascii_lowercase()],
        None => config
            .get("platforms")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(|item| item.to_ascii_lowercase())
                    .collect::<Vec<_>>()
            })
            .filter(|items| !items.is_empty())
            .unwrap_or_else(|| vec!["messaging_gateway".into()]),
    };
    let policies = platforms
        .into_iter()
        .map(|platform| {
            json!({
                "platform": platform,
                "dm": messaging_gateway_slash_access_policy(&config, &platform, "dm").to_json(),
                "group": messaging_gateway_slash_access_policy(&config, &platform, "group").to_json(),
            })
        })
        .collect::<Vec<_>>();
    let snapshot = json!({
        "kind": "platform_slash_access",
        "schema": "hermes_gateway_slash_access_desktop_v1",
        "createdAt": now_iso(),
        "desktopAdaptation": true,
        "policies": policies,
    });
    Ok(format!(
        "平台 slash-access policy：\n{}",
        serde_json::to_string_pretty(&snapshot)?
    ))
}

fn handle_platform_pairing_control_command(
    store: &AppStore,
    platform: Option<&str>,
    argument_raw: &str,
) -> AppResult<String> {
    let mut parts = argument_raw.split_whitespace();
    let action = parts.next().unwrap_or("status").to_ascii_lowercase();
    match action.as_str() {
        "" | "status" | "list" | "show" => format_platform_pairing_snapshot(store, platform),
        "request" | "generate" | "new" => {
            let platform = platform
                .map(str::to_string)
                .or_else(|| parts.next().map(|value| value.to_ascii_lowercase()))
                .ok_or_else(|| {
                    AppError::BadRequest(
                        "用法：/platforms pairing request <platform> <userId> [userName]".into(),
                    )
                })?;
            let user_id = parts
                .next()
                .ok_or_else(|| AppError::BadRequest("pairing request requires userId".into()))?;
            let user_name = parts.collect::<Vec<_>>().join(" ");
            let code = gateway_pairing_generate_code(store, &platform, user_id, &user_name)?;
            Ok(format!(
                "Pairing code generated for {platform}:{user_id}.\ncode: {code}\nexpiresInSeconds: 3600\n注意：code 只显示这一次；pending 文件只保存 salted hash。"
            ))
        }
        "approve" => {
            let platform = platform
                .map(str::to_string)
                .or_else(|| parts.next().map(|value| value.to_ascii_lowercase()))
                .ok_or_else(|| {
                    AppError::BadRequest(
                        "用法：/platforms pairing approve <platform> <code>".into(),
                    )
                })?;
            let code = parts
                .next()
                .ok_or_else(|| AppError::BadRequest("pairing approve requires code".into()))?;
            let approved = gateway_pairing_approve_code(store, &platform, code)?;
            Ok(format!(
                "Pairing approved：{}:{} ({})\n已同步到 messagingGateway.platformConfigs.{}.pairedUsers。",
                platform,
                approved["userId"].as_str().unwrap_or("-"),
                approved["userName"].as_str().unwrap_or("-"),
                platform
            ))
        }
        "revoke" | "remove" => {
            let platform = platform
                .map(str::to_string)
                .or_else(|| parts.next().map(|value| value.to_ascii_lowercase()))
                .ok_or_else(|| {
                    AppError::BadRequest(
                        "用法：/platforms pairing revoke <platform> <userId>".into(),
                    )
                })?;
            let user_id = parts
                .next()
                .ok_or_else(|| AppError::BadRequest("pairing revoke requires userId".into()))?;
            let removed = gateway_pairing_revoke_user(store, &platform, user_id)?;
            Ok(format!(
                "Pairing revoked：{platform}:{user_id}; removed={removed}"
            ))
        }
        "clear" => {
            let target_platform = platform
                .map(str::to_string)
                .or_else(|| parts.next().map(|value| value.to_ascii_lowercase()));
            let count = gateway_pairing_clear_pending(store, target_platform.as_deref())?;
            Ok(format!("Cleared pending pairing requests: {count}"))
        }
        _ => Ok("用法：/platforms pairing [status|request <platform> <userId> [userName]|approve <platform> <code>|revoke <platform> <userId>|clear [platform]]；也可用 /platforms <platform> pairing ...".into()),
    }
}

fn format_platform_pairing_snapshot(store: &AppStore, platform: Option<&str>) -> AppResult<String> {
    let snapshot = json!({
        "kind": "platform_pairing",
        "schema": "hermes_gateway_pairing_desktop_v1",
        "createdAt": now_iso(),
        "desktopAdaptation": true,
        "platform": platform.unwrap_or("all"),
        "pending": gateway_pairing_list_pending(store, platform)?,
        "approved": gateway_pairing_list_approved(store, platform)?,
        "limits": {
            "codeTtlSeconds": 3600,
            "rateLimitSeconds": 600,
            "lockoutSeconds": 3600,
            "maxPendingPerPlatform": 3,
            "maxFailedAttempts": 5
        }
    });
    Ok(format!(
        "平台 pairing snapshot：\n{}",
        serde_json::to_string_pretty(&snapshot)?
    ))
}

fn handle_feishu_comment_control_command(
    store: &AppStore,
    argument_raw: &str,
) -> AppResult<String> {
    let mut parts = argument_raw.split_whitespace();
    let action = parts.next().unwrap_or("status").to_ascii_lowercase();
    match action.as_str() {
        "" | "status" | "list" | "show" => format_feishu_comment_rules_snapshot(store),
        "check" => {
            let doc_key = parts.next().ok_or_else(|| {
                AppError::BadRequest(
                    "用法：/platforms feishu-comment check <fileType:fileToken> <userOpenId>"
                        .into(),
                )
            })?;
            let user_open_id = parts.next().ok_or_else(|| {
                AppError::BadRequest(
                    "用法：/platforms feishu-comment check <fileType:fileToken> <userOpenId>"
                        .into(),
                )
            })?;
            let (file_type, file_token) = doc_key.split_once(':').ok_or_else(|| {
                AppError::BadRequest("doc key must be fileType:fileToken, e.g. docx:doccn123".into())
            })?;
            let inbound = json!({
                "message_type": "comment",
                "comment": {
                    "file_type": file_type,
                    "file_token": file_token
                },
                "source": {
                    "user_id": user_open_id
                }
            });
            let config = store.config()?.feishu;
            let denial = feishu_comment_access_denial(store, &config, &inbound)?;
            let snapshot = json!({
                "kind": "feishu_comment_access_check",
                "schema": "hermes_feishu_comment_rules_desktop_v1",
                "createdAt": now_iso(),
                "document": doc_key,
                "userOpenId": user_open_id,
                "user_open_id": user_open_id,
                "allowed": denial.is_none(),
                "denial": denial,
            });
            Ok(format!(
                "Feishu comment access check：\n{}",
                serde_json::to_string_pretty(&snapshot)?
            ))
        }
        "pairing" | "pair" => handle_feishu_comment_pairing_control_command(store, parts),
        _ => Ok("用法：/platforms feishu-comment [status|check <fileType:fileToken> <userOpenId>|pairing <list|add|remove|clear>]".into()),
    }
}

fn format_feishu_comment_rules_snapshot(store: &AppStore) -> AppResult<String> {
    let rules_path = feishu_comment_rules_path(store);
    let pairing_path = feishu_comment_pairing_path(store);
    let rules = gateway_pairing_read_json(&rules_path);
    let pairing = gateway_pairing_read_json(&pairing_path);
    let approved = feishu_comment_pairing_approved_entries(&pairing);
    let snapshot = json!({
        "kind": "feishu_comment_rules",
        "schema": "hermes_feishu_comment_rules_desktop_v1",
        "createdAt": now_iso(),
        "desktopAdaptation": true,
        "rulesPath": rules_path.to_string_lossy().to_string(),
        "rules_path": rules_path.to_string_lossy().to_string(),
        "rulesExists": rules_path.exists(),
        "rules_exists": rules_path.exists(),
        "pairingPath": pairing_path.to_string_lossy().to_string(),
        "pairing_path": pairing_path.to_string_lossy().to_string(),
        "pairingExists": pairing_path.exists(),
        "pairing_exists": pairing_path.exists(),
        "topLevel": {
            "enabled": rules.get("enabled").cloned().unwrap_or(json!(true)),
            "policy": rules.get("policy").cloned().unwrap_or(json!("pairing")),
            "allow_from": rules.get("allow_from").cloned().unwrap_or_else(|| json!([])),
        },
        "documentRuleCount": rules.get("documents").and_then(Value::as_object).map(|value| value.len()).unwrap_or(0),
        "document_rule_count": rules.get("documents").and_then(Value::as_object).map(|value| value.len()).unwrap_or(0),
        "approvedCount": approved.len(),
        "approved_count": approved.len(),
        "approved": approved,
        "rules": rules,
    });
    Ok(format!(
        "Feishu comment rules snapshot：\n{}",
        serde_json::to_string_pretty(&snapshot)?
    ))
}

fn handle_feishu_comment_pairing_control_command<'a, I>(
    store: &AppStore,
    mut parts: I,
) -> AppResult<String>
where
    I: Iterator<Item = &'a str>,
{
    let action = parts.next().unwrap_or("list").to_ascii_lowercase();
    match action.as_str() {
        "" | "status" | "list" | "show" => {
            let snapshot = json!({
                "kind": "feishu_comment_pairing",
                "schema": "hermes_feishu_comment_pairing_desktop_v1",
                "createdAt": now_iso(),
                "path": feishu_comment_pairing_path(store).to_string_lossy().to_string(),
                "approved": feishu_comment_pairing_approved_entries(
                    &gateway_pairing_read_json(&feishu_comment_pairing_path(store))
                ),
            });
            Ok(format!(
                "Feishu comment pairing：\n{}",
                serde_json::to_string_pretty(&snapshot)?
            ))
        }
        "add" | "approve" => {
            let user_open_id = parts.next().ok_or_else(|| {
                AppError::BadRequest(
                    "用法：/platforms feishu-comment pairing add <userOpenId>".into(),
                )
            })?;
            let added = feishu_comment_pairing_add(store, user_open_id)?;
            Ok(format!(
                "Feishu comment pairing added：{user_open_id}; added={added}"
            ))
        }
        "remove" | "revoke" | "delete" => {
            let user_open_id = parts.next().ok_or_else(|| {
                AppError::BadRequest(
                    "用法：/platforms feishu-comment pairing remove <userOpenId>".into(),
                )
            })?;
            let removed = feishu_comment_pairing_remove(store, user_open_id)?;
            Ok(format!(
                "Feishu comment pairing removed：{user_open_id}; removed={removed}"
            ))
        }
        "clear" => {
            feishu_comment_pairing_write(store, &json!({"approved": {}}))?;
            Ok("Feishu comment pairing cleared.".into())
        }
        _ => Ok("用法：/platforms feishu-comment pairing [list|add <userOpenId>|remove <userOpenId>|clear]".into()),
    }
}

fn feishu_comment_pairing_approved_entries(pairing: &Value) -> Vec<Value> {
    let Some(approved) = pairing.get("approved") else {
        return Vec::new();
    };
    let mut entries = if let Some(map) = approved.as_object() {
        map.iter()
            .map(|(user, meta)| {
                json!({
                    "userOpenId": user,
                    "user_open_id": user,
                    "approvedAt": meta.get("approved_at").or_else(|| meta.get("approvedAt")).cloned().unwrap_or(Value::Null),
                    "approved_at": meta.get("approved_at").or_else(|| meta.get("approvedAt")).cloned().unwrap_or(Value::Null),
                })
            })
            .collect::<Vec<_>>()
    } else {
        approved
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(|user| {
                        json!({
                            "userOpenId": user,
                            "user_open_id": user,
                            "approvedAt": null,
                            "approved_at": null,
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };
    entries.sort_by(|left, right| {
        left.get("userOpenId")
            .and_then(Value::as_str)
            .cmp(&right.get("userOpenId").and_then(Value::as_str))
    });
    entries
}

fn feishu_comment_pairing_write(store: &AppStore, value: &Value) -> AppResult<()> {
    gateway_pairing_write_json(&feishu_comment_pairing_path(store), value)
}

fn feishu_comment_pairing_add(store: &AppStore, user_open_id: &str) -> AppResult<bool> {
    let user_open_id = user_open_id.trim();
    if user_open_id.is_empty() {
        return Err(AppError::BadRequest("userOpenId cannot be empty".into()));
    }
    let path = feishu_comment_pairing_path(store);
    let mut pairing = gateway_pairing_read_json(&path);
    let root = ensure_json_object(&mut pairing);
    let approved = root.entry("approved").or_insert_with(|| json!({}));
    if !approved.is_object() {
        let existing = approved
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(|user| (user.to_string(), json!({"approved_at": Value::Null})))
                    .collect::<serde_json::Map<String, Value>>()
            })
            .unwrap_or_default();
        *approved = Value::Object(existing);
    }
    let map = approved.as_object_mut().unwrap();
    let added = !map.contains_key(user_open_id);
    map.insert(
        user_open_id.to_string(),
        json!({"approved_at": gateway_pairing_now_seconds()}),
    );
    gateway_pairing_write_json(&path, &pairing)?;
    Ok(added)
}

fn feishu_comment_pairing_remove(store: &AppStore, user_open_id: &str) -> AppResult<bool> {
    let user_open_id = user_open_id.trim();
    if user_open_id.is_empty() {
        return Err(AppError::BadRequest("userOpenId cannot be empty".into()));
    }
    let path = feishu_comment_pairing_path(store);
    let mut pairing = gateway_pairing_read_json(&path);
    let mut removed = false;
    if let Some(approved) = pairing.get_mut("approved") {
        if let Some(map) = approved.as_object_mut() {
            removed = map.remove(user_open_id).is_some();
        } else if let Some(items) = approved.as_array_mut() {
            let before = items.len();
            items.retain(|value| value.as_str() != Some(user_open_id));
            removed = before != items.len();
        }
    }
    gateway_pairing_write_json(&path, &pairing)?;
    Ok(removed)
}

const GATEWAY_PAIRING_ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
const GATEWAY_PAIRING_CODE_LEN: usize = 8;
const GATEWAY_PAIRING_CODE_TTL_SECONDS: i64 = 3600;
const GATEWAY_PAIRING_RATE_LIMIT_SECONDS: i64 = 600;
const GATEWAY_PAIRING_LOCKOUT_SECONDS: i64 = 3600;
const GATEWAY_PAIRING_MAX_PENDING_PER_PLATFORM: usize = 3;
const GATEWAY_PAIRING_MAX_FAILED_ATTEMPTS: u64 = 5;

fn gateway_pairing_dir(store: &AppStore) -> PathBuf {
    store.data_dir().join("platforms").join("pairing")
}

fn gateway_pairing_pending_path(store: &AppStore, platform: &str) -> PathBuf {
    gateway_pairing_dir(store).join(format!("{platform}-pending.json"))
}

fn gateway_pairing_approved_path(store: &AppStore, platform: &str) -> PathBuf {
    gateway_pairing_dir(store).join(format!("{platform}-approved.json"))
}

fn gateway_pairing_rate_limit_path(store: &AppStore) -> PathBuf {
    gateway_pairing_dir(store).join("_rate_limits.json")
}

fn gateway_pairing_now_seconds() -> i64 {
    Utc::now().timestamp()
}

fn gateway_pairing_read_json(path: &Path) -> Value {
    fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .unwrap_or_else(|| json!({}))
}

fn gateway_pairing_write_json(path: &Path, value: &Value) -> AppResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, serde_json::to_string_pretty(value)?)?;
    fs::rename(tmp, path)?;
    Ok(())
}

fn gateway_pairing_normalize_platform(platform: &str) -> String {
    platform.trim().to_ascii_lowercase()
}

fn gateway_pairing_normalize_user_id(platform: &str, user_id: &str) -> String {
    let raw = user_id.trim().to_ascii_lowercase();
    if platform == "whatsapp" && raw.contains(':') && raw.contains('@') {
        raw.replacen(':', "@", 1)
    } else {
        raw
    }
}

fn gateway_pairing_user_aliases(platform: &str, user_id: &str) -> BTreeSet<String> {
    let normalized = gateway_pairing_normalize_user_id(platform, user_id);
    let mut aliases = BTreeSet::from([normalized.clone()]);
    if platform == "whatsapp" {
        if let Some((left, domain)) = normalized.split_once('@') {
            aliases.insert(format!("{left}@{domain}"));
            if let Some((phone, _device)) = left.split_once(':') {
                aliases.insert(format!("{phone}@{domain}"));
            }
        }
    }
    aliases.retain(|value| !value.trim().is_empty());
    aliases
}

fn gateway_pairing_hash_code(code: &str, salt_hex: &str) -> AppResult<String> {
    use sha2::{Digest, Sha256};
    let salt = hex::decode(salt_hex)
        .map_err(|error| AppError::BadRequest(format!("invalid pairing salt: {error}")))?;
    let mut hasher = Sha256::new();
    hasher.update(salt);
    hasher.update(code.as_bytes());
    Ok(hex::encode(hasher.finalize()))
}

fn gateway_pairing_random_hex(bytes: usize) -> String {
    let mut out = Vec::with_capacity(bytes);
    while out.len() < bytes {
        out.extend_from_slice(Uuid::new_v4().as_bytes());
    }
    out.truncate(bytes);
    hex::encode(out)
}

fn gateway_pairing_generate_code_value() -> String {
    let mut bytes = Vec::with_capacity(GATEWAY_PAIRING_CODE_LEN);
    while bytes.len() < GATEWAY_PAIRING_CODE_LEN {
        bytes.extend_from_slice(Uuid::new_v4().as_bytes());
    }
    bytes
        .into_iter()
        .take(GATEWAY_PAIRING_CODE_LEN)
        .map(|byte| {
            GATEWAY_PAIRING_ALPHABET[(byte as usize) % GATEWAY_PAIRING_ALPHABET.len()] as char
        })
        .collect()
}

fn ensure_json_object(value: &mut Value) -> &mut serde_json::Map<String, Value> {
    if !value.is_object() {
        *value = json!({});
    }
    value.as_object_mut().unwrap()
}

fn gateway_pairing_cleanup_expired(store: &AppStore, platform: &str) -> AppResult<()> {
    let path = gateway_pairing_pending_path(store, platform);
    let mut pending = gateway_pairing_read_json(&path);
    let now = gateway_pairing_now_seconds();
    let mut changed = false;
    if let Some(map) = pending.as_object_mut() {
        let expired = map
            .iter()
            .filter_map(|(key, value)| {
                let created_at = value.get("created_at").and_then(Value::as_i64)?;
                ((now - created_at) > GATEWAY_PAIRING_CODE_TTL_SECONDS).then(|| key.clone())
            })
            .collect::<Vec<_>>();
        for key in expired {
            map.remove(&key);
            changed = true;
        }
    }
    if changed {
        gateway_pairing_write_json(&path, &pending)?;
    }
    Ok(())
}

fn gateway_pairing_is_locked_out(store: &AppStore, platform: &str) -> bool {
    let limits = gateway_pairing_read_json(&gateway_pairing_rate_limit_path(store));
    limits
        .get(format!("_lockout:{platform}"))
        .and_then(Value::as_i64)
        .map(|until| gateway_pairing_now_seconds() < until)
        .unwrap_or(false)
}

fn gateway_pairing_is_rate_limited(store: &AppStore, platform: &str, user_id: &str) -> bool {
    let limits = gateway_pairing_read_json(&gateway_pairing_rate_limit_path(store));
    gateway_pairing_user_aliases(platform, user_id)
        .into_iter()
        .any(|alias| {
            limits
                .get(format!("{platform}:{alias}"))
                .and_then(Value::as_i64)
                .map(|last| {
                    gateway_pairing_now_seconds() - last < GATEWAY_PAIRING_RATE_LIMIT_SECONDS
                })
                .unwrap_or(false)
        })
}

fn gateway_pairing_record_rate_limit(
    store: &AppStore,
    platform: &str,
    user_id: &str,
) -> AppResult<()> {
    let path = gateway_pairing_rate_limit_path(store);
    let mut limits = gateway_pairing_read_json(&path);
    let now = gateway_pairing_now_seconds();
    let map = ensure_json_object(&mut limits);
    for alias in gateway_pairing_user_aliases(platform, user_id) {
        map.insert(format!("{platform}:{alias}"), json!(now));
    }
    gateway_pairing_write_json(&path, &limits)
}

fn gateway_pairing_record_failed_attempt(store: &AppStore, platform: &str) -> AppResult<()> {
    let path = gateway_pairing_rate_limit_path(store);
    let mut limits = gateway_pairing_read_json(&path);
    let map = ensure_json_object(&mut limits);
    let fail_key = format!("_failures:{platform}");
    let failures = map.get(&fail_key).and_then(Value::as_u64).unwrap_or(0) + 1;
    if failures >= GATEWAY_PAIRING_MAX_FAILED_ATTEMPTS {
        map.insert(fail_key, json!(0));
        map.insert(
            format!("_lockout:{platform}"),
            json!(gateway_pairing_now_seconds() + GATEWAY_PAIRING_LOCKOUT_SECONDS),
        );
    } else {
        map.insert(fail_key, json!(failures));
    }
    gateway_pairing_write_json(&path, &limits)
}

fn gateway_pairing_generate_code(
    store: &AppStore,
    platform: &str,
    user_id: &str,
    user_name: &str,
) -> AppResult<String> {
    let platform = gateway_pairing_normalize_platform(platform);
    let user_id = gateway_pairing_normalize_user_id(&platform, user_id);
    gateway_pairing_cleanup_expired(store, &platform)?;
    if gateway_pairing_is_locked_out(store, &platform) {
        return Err(AppError::BadRequest(format!(
            "pairing platform {platform} is locked out"
        )));
    }
    if gateway_pairing_is_rate_limited(store, &platform, &user_id) {
        return Err(AppError::BadRequest(format!(
            "pairing request for {platform}:{user_id} is rate limited"
        )));
    }
    let path = gateway_pairing_pending_path(store, &platform);
    let mut pending = gateway_pairing_read_json(&path);
    let map = ensure_json_object(&mut pending);
    if map.len() >= GATEWAY_PAIRING_MAX_PENDING_PER_PLATFORM {
        return Err(AppError::BadRequest(format!(
            "pairing platform {platform} already has max pending requests"
        )));
    }
    let code = gateway_pairing_generate_code_value();
    let salt = gateway_pairing_random_hex(16);
    let code_hash = gateway_pairing_hash_code(&code, &salt)?;
    let entry_id = gateway_pairing_random_hex(8);
    map.insert(
        entry_id,
        json!({
            "hash": code_hash,
            "salt": salt,
            "user_id": user_id,
            "user_name": user_name,
            "created_at": gateway_pairing_now_seconds(),
        }),
    );
    gateway_pairing_write_json(&path, &pending)?;
    gateway_pairing_record_rate_limit(store, &platform, &user_id)?;
    Ok(code)
}

fn gateway_pairing_approve_code(store: &AppStore, platform: &str, code: &str) -> AppResult<Value> {
    let platform = gateway_pairing_normalize_platform(platform);
    gateway_pairing_cleanup_expired(store, &platform)?;
    if gateway_pairing_is_locked_out(store, &platform) {
        return Err(AppError::BadRequest(format!(
            "pairing platform {platform} is locked out"
        )));
    }
    let pending_path = gateway_pairing_pending_path(store, &platform);
    let mut pending = gateway_pairing_read_json(&pending_path);
    let code = code.trim().to_ascii_uppercase();
    let mut matched_key = None;
    let mut matched_entry = None;
    if let Some(map) = pending.as_object() {
        for (key, entry) in map {
            let Some(salt) = entry.get("salt").and_then(Value::as_str) else {
                continue;
            };
            let Some(stored_hash) = entry.get("hash").and_then(Value::as_str) else {
                continue;
            };
            let candidate = gateway_pairing_hash_code(&code, salt)?;
            if candidate == stored_hash {
                matched_key = Some(key.clone());
                matched_entry = Some(entry.clone());
                break;
            }
        }
    }
    let Some(entry) = matched_entry else {
        gateway_pairing_record_failed_attempt(store, &platform)?;
        return Err(AppError::BadRequest(
            "invalid or expired pairing code".into(),
        ));
    };
    if let Some(key) = matched_key {
        if let Some(map) = pending.as_object_mut() {
            map.remove(&key);
        }
        gateway_pairing_write_json(&pending_path, &pending)?;
    }
    let user_id = entry
        .get("user_id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let user_name = entry
        .get("user_name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    gateway_pairing_approve_user(store, &platform, &user_id, &user_name)?;
    gateway_pairing_sync_config_paired_user(store, &platform, &user_id)?;
    Ok(json!({
        "platform": platform,
        "userId": user_id,
        "userName": user_name,
    }))
}

fn gateway_pairing_approve_user(
    store: &AppStore,
    platform: &str,
    user_id: &str,
    user_name: &str,
) -> AppResult<()> {
    let path = gateway_pairing_approved_path(store, platform);
    let mut approved = gateway_pairing_read_json(&path);
    let aliases = gateway_pairing_user_aliases(platform, user_id);
    let map = ensure_json_object(&mut approved);
    let duplicate_keys = map
        .keys()
        .filter(|existing| {
            let existing_aliases = gateway_pairing_user_aliases(platform, existing);
            existing_aliases.iter().any(|alias| aliases.contains(alias))
        })
        .cloned()
        .collect::<Vec<_>>();
    for key in duplicate_keys {
        map.remove(&key);
    }
    map.insert(
        gateway_pairing_normalize_user_id(platform, user_id),
        json!({
            "user_name": user_name,
            "approved_at": gateway_pairing_now_seconds(),
        }),
    );
    gateway_pairing_write_json(&path, &approved)
}

fn gateway_pairing_sync_config_paired_user(
    store: &AppStore,
    platform: &str,
    user_id: &str,
) -> AppResult<()> {
    let mut config = store.config()?;
    let root = ensure_json_object(&mut config.messaging_gateway);
    let platform_configs = root.entry("platformConfigs").or_insert_with(|| json!({}));
    let platform_configs = ensure_json_object(platform_configs);
    let platform_entry = platform_configs
        .entry(platform.to_string())
        .or_insert_with(|| json!({}));
    let platform_object = ensure_json_object(platform_entry);
    let paired = platform_object
        .entry("pairedUsers")
        .or_insert_with(|| json!([]));
    if !paired.is_array() {
        *paired = json!([]);
    }
    let normalized = gateway_pairing_normalize_user_id(platform, user_id);
    let paired_array = paired.as_array_mut().unwrap();
    if !paired_array
        .iter()
        .any(|value| value.as_str() == Some(normalized.as_str()))
    {
        paired_array.push(json!(normalized));
    }
    store.set_config(config)
}

fn gateway_pairing_revoke_user(store: &AppStore, platform: &str, user_id: &str) -> AppResult<bool> {
    let platform = gateway_pairing_normalize_platform(platform);
    let path = gateway_pairing_approved_path(store, &platform);
    let mut approved = gateway_pairing_read_json(&path);
    let aliases = gateway_pairing_user_aliases(&platform, user_id);
    let mut removed = false;
    if let Some(map) = approved.as_object_mut() {
        let keys = map
            .keys()
            .filter(|existing| {
                gateway_pairing_user_aliases(&platform, existing)
                    .iter()
                    .any(|alias| aliases.contains(alias))
            })
            .cloned()
            .collect::<Vec<_>>();
        for key in keys {
            map.remove(&key);
            removed = true;
        }
    }
    gateway_pairing_write_json(&path, &approved)?;
    gateway_pairing_unsync_config_paired_user(store, &platform, user_id)?;
    Ok(removed)
}

fn gateway_pairing_unsync_config_paired_user(
    store: &AppStore,
    platform: &str,
    user_id: &str,
) -> AppResult<()> {
    let mut config = store.config()?;
    let aliases = gateway_pairing_user_aliases(platform, user_id);
    if let Some(paired) = config
        .messaging_gateway
        .get_mut("platformConfigs")
        .and_then(Value::as_object_mut)
        .and_then(|platforms| platforms.get_mut(platform))
        .and_then(Value::as_object_mut)
        .and_then(|platform| platform.get_mut("pairedUsers"))
        .and_then(Value::as_array_mut)
    {
        paired.retain(|value| {
            value
                .as_str()
                .map(|value| !aliases.contains(&gateway_pairing_normalize_user_id(platform, value)))
                .unwrap_or(true)
        });
    }
    store.set_config(config)
}

fn gateway_pairing_clear_pending(store: &AppStore, platform: Option<&str>) -> AppResult<usize> {
    let platforms = gateway_pairing_platforms(store, platform, "pending")?;
    let mut count = 0;
    for platform in platforms {
        let path = gateway_pairing_pending_path(store, &platform);
        let pending = gateway_pairing_read_json(&path);
        count += pending.as_object().map(|map| map.len()).unwrap_or(0);
        gateway_pairing_write_json(&path, &json!({}))?;
    }
    Ok(count)
}

fn gateway_pairing_platforms(
    store: &AppStore,
    platform: Option<&str>,
    suffix: &str,
) -> AppResult<Vec<String>> {
    if let Some(platform) = platform {
        return Ok(vec![gateway_pairing_normalize_platform(platform)]);
    }
    let dir = gateway_pairing_dir(store);
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut platforms = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        let suffix_name = format!("-{suffix}.json");
        if let Some(platform) = name.strip_suffix(&suffix_name) {
            if !platform.starts_with('_') {
                platforms.push(platform.to_string());
            }
        }
    }
    platforms.sort();
    platforms.dedup();
    Ok(platforms)
}

fn gateway_pairing_list_pending(store: &AppStore, platform: Option<&str>) -> AppResult<Vec<Value>> {
    let platforms = gateway_pairing_platforms(store, platform, "pending")?;
    let mut rows = Vec::new();
    for platform in platforms {
        gateway_pairing_cleanup_expired(store, &platform)?;
        let pending = gateway_pairing_read_json(&gateway_pairing_pending_path(store, &platform));
        if let Some(map) = pending.as_object() {
            for entry in map.values() {
                let hash = entry.get("hash").and_then(Value::as_str).unwrap_or("");
                let created_at = entry.get("created_at").and_then(Value::as_i64).unwrap_or(0);
                rows.push(json!({
                    "platform": platform,
                    "code": if hash.len() >= 8 { &hash[..8] } else { "legacy" },
                    "userId": entry.get("user_id").cloned().unwrap_or(Value::String(String::new())),
                    "userName": entry.get("user_name").cloned().unwrap_or(Value::String(String::new())),
                    "ageMinutes": ((gateway_pairing_now_seconds() - created_at).max(0)) / 60,
                }));
            }
        }
    }
    Ok(rows)
}

fn gateway_pairing_list_approved(
    store: &AppStore,
    platform: Option<&str>,
) -> AppResult<Vec<Value>> {
    let platforms = gateway_pairing_platforms(store, platform, "approved")?;
    let mut rows = Vec::new();
    for platform in platforms {
        let approved = gateway_pairing_read_json(&gateway_pairing_approved_path(store, &platform));
        if let Some(map) = approved.as_object() {
            for (user_id, entry) in map {
                rows.push(json!({
                    "platform": platform,
                    "userId": user_id,
                    "userName": entry.get("user_name").cloned().unwrap_or(Value::String(String::new())),
                    "approvedAt": entry.get("approved_at").cloned().unwrap_or(Value::Null),
                }));
            }
        }
    }
    Ok(rows)
}

const GATEWAY_SERVICE_RESTART_EXIT_CODE: u16 = 75;
const DEFAULT_GATEWAY_RESTART_DRAIN_TIMEOUT_SECONDS: f64 = 180.0;

fn gateway_runtime_dir(store: &AppStore) -> PathBuf {
    store.data_dir().join("platforms")
}

fn gateway_hermes_home(store: &AppStore) -> PathBuf {
    std::env::var_os("HERMES_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| store.data_dir().join(".hermes"))
}

fn gateway_runtime_status_path(store: &AppStore) -> PathBuf {
    gateway_runtime_dir(store).join("gateway_state.json")
}

fn hermes_gateway_runtime_status_path(store: &AppStore) -> PathBuf {
    gateway_hermes_home(store).join("gateway_state.json")
}

fn gateway_pid_path(store: &AppStore) -> PathBuf {
    gateway_runtime_dir(store).join("gateway.pid")
}

fn hermes_gateway_pid_path(store: &AppStore) -> PathBuf {
    gateway_hermes_home(store).join("gateway.pid")
}

fn gateway_runtime_lock_path(store: &AppStore) -> PathBuf {
    gateway_runtime_dir(store).join("gateway.lock")
}

fn hermes_gateway_runtime_lock_path(store: &AppStore) -> PathBuf {
    gateway_hermes_home(store).join("gateway.lock")
}

fn gateway_restart_request_path(store: &AppStore) -> PathBuf {
    gateway_runtime_dir(store).join("gateway_restart_request.json")
}

fn hermes_gateway_restart_request_path(store: &AppStore) -> PathBuf {
    gateway_hermes_home(store).join("gateway_restart_request.json")
}

fn gateway_restart_pending_path(store: &AppStore) -> PathBuf {
    gateway_runtime_dir(store).join("gateway_restart_pending.json")
}

fn hermes_gateway_restart_pending_path(store: &AppStore) -> PathBuf {
    gateway_hermes_home(store).join(".restart_pending.json")
}

fn gateway_restart_notify_path(store: &AppStore) -> PathBuf {
    gateway_runtime_dir(store).join("gateway_restart_notify.json")
}

fn hermes_gateway_restart_notify_path(store: &AppStore) -> PathBuf {
    gateway_hermes_home(store).join(".restart_notify.json")
}

fn gateway_restart_failure_counts_path(store: &AppStore) -> PathBuf {
    gateway_runtime_dir(store).join("restart_failure_counts.json")
}

fn hermes_gateway_restart_failure_counts_path(store: &AppStore) -> PathBuf {
    gateway_hermes_home(store).join(".restart_failure_counts")
}

fn gateway_planned_stop_path(store: &AppStore) -> PathBuf {
    gateway_runtime_dir(store).join("gateway_planned_stop.json")
}

fn hermes_gateway_planned_stop_path(store: &AppStore) -> PathBuf {
    gateway_hermes_home(store).join(".gateway-planned-stop.json")
}

fn gateway_takeover_path(store: &AppStore) -> PathBuf {
    gateway_runtime_dir(store).join("gateway_takeover.json")
}

fn hermes_gateway_takeover_path(store: &AppStore) -> PathBuf {
    gateway_hermes_home(store).join(".gateway-takeover.json")
}

fn gateway_scoped_lock_dir(store: &AppStore) -> PathBuf {
    std::env::var_os("HERMES_GATEWAY_LOCK_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| gateway_runtime_dir(store).join("gateway-locks"))
}

fn gateway_memory_monitor_path(store: &AppStore) -> PathBuf {
    gateway_runtime_dir(store).join("gateway_memory_monitor.json")
}

fn read_gateway_runtime_json(path: &Path) -> Option<Value> {
    fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .filter(Value::is_object)
}

fn configured_gateway_restart_drain_timeout(store: &AppStore) -> AppResult<f64> {
    let config = store.config()?;
    let configured = [
        "restartDrainTimeout",
        "restartDrainTimeoutSeconds",
        "restart_drain_timeout",
        "restart_drain_timeout_seconds",
    ]
    .into_iter()
    .find_map(|key| config.messaging_gateway.get(key))
    .and_then(value_to_f64);
    Ok(configured
        .unwrap_or(DEFAULT_GATEWAY_RESTART_DRAIN_TIMEOUT_SECONDS)
        .max(0.0))
}

fn value_to_f64(value: &Value) -> Option<f64> {
    match value {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => text.trim().parse::<f64>().ok(),
        _ => None,
    }
}

fn is_gateway_restart_replace_flag(raw: &str) -> bool {
    matches!(
        raw.trim().to_ascii_lowercase().as_str(),
        "replace" | "--replace" | "-r" | "takeover" | "--takeover"
    )
}

fn is_gateway_restart_service_flag(raw: &str) -> bool {
    matches!(
        raw.trim().to_ascii_lowercase().as_str(),
        "service" | "--service" | "via-service" | "--via-service" | "--service-manager"
    )
}

fn is_gateway_restart_detached_flag(raw: &str) -> bool {
    matches!(
        raw.trim().to_ascii_lowercase().as_str(),
        "detached" | "--detached" | "--detach" | "background" | "--background"
    )
}

fn parse_gateway_restart_options_from_tail(
    store: &AppStore,
    tail: &[&str],
) -> AppResult<(f64, bool, bool, bool)> {
    let configured = configured_gateway_restart_drain_timeout(store)?;
    let replace = tail
        .iter()
        .any(|part| is_gateway_restart_replace_flag(part));
    let via_service = tail
        .iter()
        .any(|part| is_gateway_restart_service_flag(part));
    let detached = tail
        .iter()
        .any(|part| is_gateway_restart_detached_flag(part));
    let Some(raw) = tail.iter().find(|part| {
        !part.trim().is_empty()
            && !is_gateway_restart_replace_flag(part)
            && !is_gateway_restart_service_flag(part)
            && !is_gateway_restart_detached_flag(part)
    }) else {
        return Ok((configured, replace, via_service, detached));
    };
    let drain_timeout = raw
        .trim()
        .parse::<f64>()
        .map(|value| value.max(0.0))
        .map_err(|_| {
            AppError::BadRequest(
                "restart drain timeout must be a non-negative number of seconds".into(),
            )
        })?;
    Ok((drain_timeout, replace, via_service, detached))
}

fn platform_restart_request_snapshot(
    store: &AppStore,
    platform: Option<&str>,
    drain_timeout_seconds: f64,
    replace_requested: bool,
    via_service: bool,
    detached_restart: bool,
) -> AppResult<Value> {
    let active_conversations_before_reload = platform_active_restart_conversations(store)?;
    store.reload_from_disk()?;
    let takeover_target = gateway_takeover_target_pid(store);
    let status = platform_runtime_status_snapshot(store, platform)?;
    let takeover = if replace_requested {
        Some(platform_takeover_marker_snapshot(
            store,
            platform,
            takeover_target,
        )?)
    } else {
        None
    };
    let planned_restart_notification = platform_restart_pending_notification_snapshot(
        store,
        platform,
        via_service,
        detached_restart,
    )?;
    let restart_failure_counts = platform_restart_failure_counts_snapshot(
        store,
        platform,
        "restart_request",
        Some(active_conversations_before_reload),
    )?;
    let snapshot = json!({
        "kind": "platform_restart_request",
        "schema": "hermes_gateway_restart_desktop_v1",
        "createdAt": now_iso(),
        "desktopAdaptation": true,
        "platform": platform.unwrap_or("all"),
        "restartRequested": true,
        "replaceRequested": replace_requested,
        "restartViaService": via_service,
        "detachedRestart": detached_restart,
        "expectedExitCode": if via_service { json!(GATEWAY_SERVICE_RESTART_EXIT_CODE) } else { Value::Null },
        "serviceManagerRestart": {
            "requested": via_service,
            "exitCode": GATEWAY_SERVICE_RESTART_EXIT_CODE,
            "meaning": "Hermes uses EX_TEMPFAIL/75 so an external service manager can relaunch the gateway after graceful drain.",
        },
        "restartNotification": planned_restart_notification,
        "restartFailureCounts": restart_failure_counts,
        "detachedRestartRequest": {
            "requested": detached_restart,
            "meaning": "Hermes detached restart launches a replacement process outside the current gateway lifecycle; SynthChat records the intent for desktop/external supervisors.",
        },
        "takeoverMarker": takeover,
        "serviceRestartExitCode": GATEWAY_SERVICE_RESTART_EXIT_CODE,
        "drainTimeoutSeconds": drain_timeout_seconds,
        "requestPath": gateway_restart_request_path(store).to_string_lossy().to_string(),
        "hermesHome": gateway_hermes_home(store).to_string_lossy().to_string(),
        "hermesRestartRequestPath": hermes_gateway_restart_request_path(store).to_string_lossy().to_string(),
        "hermesTakeoverMarker": hermes_gateway_takeover_path(store).to_string_lossy().to_string(),
        "mirroredToHermesHome": true,
        "statusPath": gateway_runtime_status_path(store).to_string_lossy().to_string(),
        "runtimeStatus": status,
        "note": "SynthChat desktop records Hermes restart intent and reloads local state; native adapters remain controlled through start/stop and external daemons remain explicit boundaries.",
    });
    gateway_pairing_write_json(&gateway_restart_request_path(store), &snapshot)?;
    gateway_pairing_write_json(&hermes_gateway_restart_request_path(store), &snapshot)?;
    Ok(snapshot)
}

fn platform_restart_pending_notification_snapshot(
    store: &AppStore,
    platform: Option<&str>,
    via_service: bool,
    detached_restart: bool,
) -> AppResult<Value> {
    let previous_notify = read_gateway_runtime_json(&hermes_gateway_restart_notify_path(store))
        .or_else(|| read_gateway_runtime_json(&gateway_restart_notify_path(store)));
    let marker = json!({
        "kind": "gateway_restart_pending_notification",
        "schema": "hermes_gateway_restart_notification_desktop_v1",
        "createdAt": now_iso(),
        "desktopAdaptation": true,
        "platform": platform.unwrap_or("all"),
        "pending": true,
        "notifyRequester": previous_notify.is_some(),
        "notifyHomeChannels": previous_notify.is_none(),
        "viaService": via_service,
        "detachedRestart": detached_restart,
        "restartNotifyPath": gateway_restart_notify_path(store).to_string_lossy().to_string(),
        "hermesRestartNotifyPath": hermes_gateway_restart_notify_path(store).to_string_lossy().to_string(),
        "restartPendingPath": gateway_restart_pending_path(store).to_string_lossy().to_string(),
        "hermesRestartPendingPath": hermes_gateway_restart_pending_path(store).to_string_lossy().to_string(),
        "previousRequesterNotification": previous_notify,
        "message": if previous_notify.is_some() {
            "Gateway restarted successfully. Your session continues."
        } else {
            "Gateway online - SynthChat/Hermes desktop runtime is back and ready."
        },
    });
    gateway_pairing_write_json(&gateway_restart_pending_path(store), &marker)?;
    gateway_pairing_write_json(&hermes_gateway_restart_pending_path(store), &marker)?;
    Ok(marker)
}

fn platform_active_restart_conversations(store: &AppStore) -> AppResult<BTreeSet<String>> {
    let mut active_conversations = BTreeSet::new();
    for run in store.agent_runs()? {
        if run.parent_run_id.is_some() {
            continue;
        }
        if !matches!(
            run.state.as_str(),
            "started" | "running" | "pendingApproval" | "needsClarification"
        ) {
            continue;
        }
        active_conversations.insert(run.conversation_id);
    }
    for conversation in store.conversations()? {
        let lifecycle = &conversation.metadata["hermesSessionLifecycle"];
        let resume_pending = lifecycle
            .get("resumePending")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let suspended = lifecycle
            .get("suspended")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if resume_pending && !suspended {
            active_conversations.insert(conversation.id);
        }
    }
    Ok(active_conversations)
}

fn platform_restart_failure_counts_snapshot(
    store: &AppStore,
    platform: Option<&str>,
    reason: &str,
    active_conversations_override: Option<BTreeSet<String>>,
) -> AppResult<Value> {
    const STUCK_LOOP_THRESHOLD: u64 = 3;
    let previous = read_gateway_runtime_json(&hermes_gateway_restart_failure_counts_path(store))
        .or_else(|| read_gateway_runtime_json(&gateway_restart_failure_counts_path(store)))
        .unwrap_or_else(|| json!({}));
    let active_conversations = match active_conversations_override {
        Some(conversations) => conversations,
        None => platform_active_restart_conversations(store)?,
    };
    let mut counts = serde_json::Map::new();
    let mut suspended = Vec::new();
    for conversation_id in active_conversations {
        let previous_count = previous
            .get(&conversation_id)
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let count = previous_count.saturating_add(1);
        counts.insert(conversation_id.clone(), json!(count));
        store.mark_hermes_session_resume_pending(
            &conversation_id,
            "restart_interrupted",
            "gateway_restart_failure_counts",
        )?;
        if count >= STUCK_LOOP_THRESHOLD {
            if let Ok(conversation) = store.conversation(&conversation_id) {
                mark_hermes_session_suspended(store, &conversation, "restart_stuck_loop")?;
                suspended.push(json!({
                    "conversationId": conversation_id,
                    "count": count,
                    "threshold": STUCK_LOOP_THRESHOLD,
                }));
            }
        }
    }
    let counts_value = Value::Object(counts);
    gateway_pairing_write_json(&gateway_restart_failure_counts_path(store), &counts_value)?;
    gateway_pairing_write_json(
        &hermes_gateway_restart_failure_counts_path(store),
        &counts_value,
    )?;
    Ok(json!({
        "kind": "gateway_restart_failure_counts",
        "schema": "hermes_gateway_restart_failure_counts_desktop_v1",
        "createdAt": now_iso(),
        "desktopAdaptation": true,
        "platform": platform.unwrap_or("all"),
        "reason": reason,
        "threshold": STUCK_LOOP_THRESHOLD,
        "activeSessionCount": counts_value.as_object().map(|map| map.len()).unwrap_or(0),
        "counts": counts_value,
        "suspended": suspended,
        "path": gateway_restart_failure_counts_path(store).to_string_lossy().to_string(),
        "hermesPath": hermes_gateway_restart_failure_counts_path(store).to_string_lossy().to_string(),
        "note": "Mirrors Hermes .restart_failure_counts: active sessions are incremented on restart intent; sessions at threshold are marked suspended to break restart loops.",
    }))
}

fn gateway_takeover_target_pid(store: &AppStore) -> Option<Value> {
    [hermes_gateway_pid_path(store), gateway_pid_path(store)]
        .into_iter()
        .find_map(|path| read_gateway_runtime_json(&path))
}

fn gateway_scoped_lock_records(store: &AppStore) -> Vec<Value> {
    let lock_dir = gateway_scoped_lock_dir(store);
    let Ok(entries) = fs::read_dir(&lock_dir) else {
        return Vec::new();
    };
    let mut records = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("lock") {
                return None;
            }
            let record = read_gateway_runtime_json(&path).unwrap_or_else(|| {
                json!({
                    "kind": "gateway_scoped_lock_unreadable",
                    "readable": false,
                })
            });
            Some(json!({
                "path": path.to_string_lossy().to_string(),
                "fileName": path.file_name().and_then(|name| name.to_str()).unwrap_or("").to_string(),
                "record": record,
            }))
        })
        .collect::<Vec<_>>();
    records.sort_by(|left, right| {
        left.get("fileName")
            .and_then(Value::as_str)
            .unwrap_or("")
            .cmp(right.get("fileName").and_then(Value::as_str).unwrap_or(""))
    });
    records
}

fn release_gateway_scoped_locks_for_owner(
    store: &AppStore,
    owner_record: Option<&Value>,
) -> AppResult<Value> {
    let before = gateway_scoped_lock_records(store);
    let owner_pid = owner_record
        .and_then(|record| record.get("pid"))
        .and_then(Value::as_u64);
    let owner_start_time = owner_record
        .and_then(|record| record.get("start_time"))
        .cloned()
        .unwrap_or(Value::Null);
    let mut released = Vec::new();
    if let Some(owner_pid) = owner_pid {
        for entry in &before {
            let Some(path) = entry.get("path").and_then(Value::as_str) else {
                continue;
            };
            let Some(record) = entry.get("record") else {
                continue;
            };
            let record_pid = record.get("pid").and_then(Value::as_u64);
            if record_pid != Some(owner_pid) {
                continue;
            }
            let record_start_time = record.get("start_time").cloned().unwrap_or(Value::Null);
            if !owner_start_time.is_null() && record_start_time != owner_start_time {
                continue;
            }
            fs::remove_file(path).map_err(|err| {
                AppError::BadRequest(format!("remove gateway scoped lock {path}: {err}"))
            })?;
            released.push(entry.clone());
        }
    }
    let after = gateway_scoped_lock_records(store);
    Ok(json!({
        "kind": "gateway_scoped_lock_release",
        "schema": "hermes_gateway_scoped_lock_release_desktop_v1",
        "createdAt": now_iso(),
        "desktopAdaptation": true,
        "lockDir": gateway_scoped_lock_dir(store).to_string_lossy().to_string(),
        "ownerPid": owner_pid,
        "ownerStartTime": owner_start_time,
        "beforeCount": before.len(),
        "releasedCount": released.len(),
        "afterCount": after.len(),
        "released": released,
        "remaining": after,
        "note": "SynthChat mirrors Hermes --replace scoped-lock cleanup by removing only lock records owned by the replaced gateway PID/start_time.",
    }))
}

fn platform_takeover_marker_snapshot(
    store: &AppStore,
    platform: Option<&str>,
    target_record: Option<Value>,
) -> AppResult<Value> {
    let target_pid = target_record
        .as_ref()
        .and_then(|record| record.get("pid"))
        .and_then(Value::as_u64)
        .unwrap_or_else(|| std::process::id() as u64);
    let target_start_time = target_record
        .as_ref()
        .and_then(|record| record.get("start_time"))
        .cloned()
        .unwrap_or(Value::Null);
    let scoped_lock_release =
        release_gateway_scoped_locks_for_owner(store, target_record.as_ref())?;
    let snapshot = json!({
        "kind": "platform_takeover_marker",
        "schema": "hermes_gateway_takeover_desktop_v1",
        "createdAt": now_iso(),
        "desktopAdaptation": true,
        "platform": platform.unwrap_or("all"),
        "target_pid": target_pid,
        "target_start_time": target_start_time,
        "replacer_pid": std::process::id(),
        "written_at": now_iso(),
        "ttlSeconds": 60,
        "markerPath": gateway_takeover_path(store).to_string_lossy().to_string(),
        "hermesHome": gateway_hermes_home(store).to_string_lossy().to_string(),
        "hermesTakeoverMarker": hermes_gateway_takeover_path(store).to_string_lossy().to_string(),
        "mirroredToHermesHome": true,
        "sourcePidRecord": target_record,
        "scopedLockRelease": scoped_lock_release,
        "note": "SynthChat desktop records Hermes --replace takeover intent without killing an external gateway process automatically.",
    });
    gateway_pairing_write_json(&gateway_takeover_path(store), &snapshot)?;
    gateway_pairing_write_json(&hermes_gateway_takeover_path(store), &snapshot)?;
    Ok(snapshot)
}

fn format_platform_restart_request(snapshot: &Value) -> AppResult<String> {
    Ok(format!(
        "平台 restart request：\n{}",
        serde_json::to_string_pretty(snapshot)?
    ))
}

fn platform_planned_stop_snapshot(
    store: &AppStore,
    platform: Option<&str>,
    reason: &str,
) -> AppResult<Value> {
    let status = platform_runtime_status_snapshot(store, platform)?;
    let snapshot = json!({
        "kind": "platform_planned_stop",
        "schema": "hermes_gateway_planned_stop_desktop_v1",
        "createdAt": now_iso(),
        "desktopAdaptation": true,
        "platform": platform.unwrap_or("all"),
        "targetPid": std::process::id(),
        "target_pid": std::process::id(),
        "targetStartTime": Value::Null,
        "target_start_time": Value::Null,
        "stopperPid": std::process::id(),
        "stopper_pid": std::process::id(),
        "writtenAt": now_iso(),
        "written_at": now_iso(),
        "ttlSeconds": 60,
        "reason": reason,
        "markerPath": gateway_planned_stop_path(store).to_string_lossy().to_string(),
        "hermesHome": gateway_hermes_home(store).to_string_lossy().to_string(),
        "hermesPlannedStopMarker": hermes_gateway_planned_stop_path(store).to_string_lossy().to_string(),
        "mirroredToHermesHome": true,
        "statusPath": gateway_runtime_status_path(store).to_string_lossy().to_string(),
        "runtimeStatus": status,
        "note": "SynthChat desktop records Hermes planned-stop intent so UI/status can distinguish intentional stop from unexpected gateway failure.",
    });
    gateway_pairing_write_json(&gateway_planned_stop_path(store), &snapshot)?;
    gateway_pairing_write_json(&hermes_gateway_planned_stop_path(store), &snapshot)?;
    Ok(snapshot)
}

fn format_platform_planned_stop(snapshot: &Value) -> AppResult<String> {
    Ok(format!(
        "平台 planned stop：\n{}",
        serde_json::to_string_pretty(snapshot)?
    ))
}

fn platform_gateway_profiles_snapshot(store: &AppStore) -> AppResult<Value> {
    let runtime = platform_runtime_status_snapshot(store, None)?;
    let hermes_home = std::env::var_os("HERMES_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| gateway_hermes_home(store));
    let profiles_root = hermes_home.join("profiles");
    let mut profiles = vec![json!({
        "id": "desktop",
        "kind": "synthchat-desktop",
        "active": true,
        "path": store.data_dir().to_string_lossy().to_string(),
        "gatewayPidFile": gateway_pid_path(store).to_string_lossy().to_string(),
        "gatewayPidFileExists": gateway_pid_path(store).exists(),
        "gatewayRunning": runtime.get("runtimeLockActive").and_then(Value::as_bool).unwrap_or(false),
        "skillCount": store.skills().map(|skills| skills.len()).unwrap_or(0),
    })];
    if hermes_home.exists() {
        profiles.push(hermes_profile_status_entry("default", &hermes_home, false));
    }
    if let Ok(entries) = fs::read_dir(&profiles_root) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let Some(id) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            profiles.push(hermes_profile_status_entry(id, &path, false));
        }
    }
    let snapshot = json!({
        "kind": "platform_gateway_profiles",
        "schema": "hermes_gateway_profiles_desktop_v1",
        "createdAt": now_iso(),
        "desktopAdaptation": true,
        "hermesHome": hermes_home.to_string_lossy().to_string(),
        "profilesRoot": profiles_root.to_string_lossy().to_string(),
        "profiles": profiles,
        "runtimeStatus": runtime,
        "note": "SynthChat desktop keeps its own active data_dir profile and reports visible Hermes profile directories for gateway parity diagnostics.",
    });
    gateway_pairing_write_json(
        &gateway_runtime_dir(store).join("gateway_profiles.json"),
        &snapshot,
    )?;
    Ok(snapshot)
}

fn hermes_profile_status_entry(id: &str, path: &Path, active: bool) -> Value {
    let pid_file = path.join("gateway.pid");
    let skills_dir = path.join("skills");
    let skill_count = fs::read_dir(&skills_dir)
        .ok()
        .map(|entries| {
            entries
                .flatten()
                .filter(|entry| entry.path().is_dir())
                .count()
        })
        .unwrap_or(0);
    json!({
        "id": id,
        "kind": "hermes-profile",
        "active": active,
        "path": path.to_string_lossy().to_string(),
        "gatewayPidFile": pid_file.to_string_lossy().to_string(),
        "gatewayPidFileExists": pid_file.exists(),
        "gatewayRunning": pid_file.exists(),
        "skillCount": skill_count,
        "profileYamlExists": path.join("profile.yaml").exists(),
        "configYamlExists": path.join("config.yaml").exists(),
    })
}

fn format_platform_gateway_profiles_snapshot(snapshot: &Value) -> AppResult<String> {
    Ok(format!(
        "平台 gateway profiles：\n{}",
        serde_json::to_string_pretty(snapshot)?
    ))
}

fn hermes_gateway_profile_suffix(store: &AppStore) -> Option<String> {
    let hermes_home = gateway_hermes_home(store);
    let profile = hermes_home
        .parent()
        .and_then(|parent| parent.file_name())
        .and_then(|name| name.to_str())
        .filter(|name| *name == "profiles")
        .and_then(|_| hermes_home.file_name())
        .and_then(|name| name.to_str())
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty());
    profile
}

fn hermes_gateway_service_name(store: &AppStore) -> String {
    hermes_gateway_profile_suffix(store)
        .map(|suffix| format!("hermes-gateway-{suffix}"))
        .unwrap_or_else(|| "hermes-gateway".into())
}

fn hermes_gateway_s6_profile(store: &AppStore) -> String {
    hermes_gateway_profile_suffix(store).unwrap_or_else(|| "default".into())
}

fn hermes_gateway_s6_service_name(store: &AppStore) -> String {
    format!("gateway-{}", hermes_gateway_s6_profile(store))
}

fn hermes_gateway_launchd_label(store: &AppStore) -> String {
    hermes_gateway_profile_suffix(store)
        .map(|suffix| format!("ai.hermes.gateway-{suffix}"))
        .unwrap_or_else(|| "ai.hermes.gateway".into())
}

fn windows_startup_dir() -> Option<PathBuf> {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .map(|path| {
            path.join("Microsoft")
                .join("Windows")
                .join("Start Menu")
                .join("Programs")
                .join("Startup")
        })
        .or_else(|| {
            std::env::var_os("USERPROFILE").map(|home| {
                PathBuf::from(home)
                    .join("AppData")
                    .join("Roaming")
                    .join("Microsoft")
                    .join("Windows")
                    .join("Start Menu")
                    .join("Programs")
                    .join("Startup")
            })
        })
}

fn sanitize_windows_task_file_name(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if matches!(ch, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') || ch.is_control()
            {
                '_'
            } else {
                ch
            }
        })
        .collect()
}

fn gateway_service_manager_candidates(store: &AppStore) -> Value {
    let service_name = hermes_gateway_service_name(store);
    let s6_profile = hermes_gateway_s6_profile(store);
    let s6_service_name = hermes_gateway_s6_service_name(store);
    let launchd_label = hermes_gateway_launchd_label(store);
    let task_name = hermes_gateway_profile_suffix(store)
        .map(|suffix| format!("Hermes_Gateway_{suffix}"))
        .unwrap_or_else(|| "Hermes_Gateway".into());
    let task_script = gateway_hermes_home(store)
        .join("gateway-service")
        .join(format!(
            "{}.cmd",
            sanitize_windows_task_file_name(&task_name)
        ));
    let startup_entry = windows_startup_dir().map(|dir| {
        dir.join(format!(
            "{}.cmd",
            sanitize_windows_task_file_name(&task_name)
        ))
    });
    json!({
        "serviceName": service_name,
        "commands": {
            "install": "hermes gateway install",
            "start": "hermes gateway start",
            "stop": "hermes gateway stop",
            "restart": "hermes gateway restart",
            "status": "hermes gateway status",
            "uninstall": "hermes gateway uninstall",
        },
        "systemd": {
            "supportedOn": "linux",
            "userUnitPath": std::env::var_os("USERPROFILE")
                .or_else(|| std::env::var_os("HOME"))
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("~"))
                .join(".config")
                .join("systemd")
                .join("user")
                .join(format!("{service_name}.service"))
                .to_string_lossy()
                .to_string(),
            "systemUnitPath": PathBuf::from("/etc/systemd/system")
                .join(format!("{service_name}.service"))
                .to_string_lossy()
                .to_string(),
        },
        "launchd": {
            "supportedOn": "macos",
            "label": launchd_label,
            "plistPath": std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("~"))
                .join("Library")
                .join("LaunchAgents")
                .join(format!("{launchd_label}.plist"))
                .to_string_lossy()
                .to_string(),
        },
        "windows": {
            "supportedOn": "windows",
            "scheduledTaskName": task_name,
            "taskScriptPath": task_script.to_string_lossy().to_string(),
            "startupEntryPath": startup_entry
                .map(|path| path.to_string_lossy().to_string())
                .unwrap_or_else(|| "<unresolved APPDATA/USERPROFILE>".into()),
        },
        "container": {
            "s6": {
                "supportedOn": "s6-overlay containers",
                "profile": s6_profile,
                "serviceName": s6_service_name,
                "dynamicScandir": "/run/service",
                "serviceDir": format!("/run/service/{s6_service_name}"),
                "commands": {
                    "register": "hermes gateway install",
                    "start": format!("s6-svc -u /run/service/{s6_service_name}"),
                    "stop": format!("s6-svc -d /run/service/{s6_service_name}"),
                    "restart": format!("s6-svc -t /run/service/{s6_service_name}"),
                    "status": format!("s6-svstat /run/service/{s6_service_name}"),
                    "unregister": "hermes gateway uninstall"
                },
                "runtimeRegistration": true
            },
            "docker": "Without s6, Docker/Podman restart policy is the service manager.",
        },
    })
}

fn gateway_service_state_path(store: &AppStore) -> PathBuf {
    gateway_runtime_dir(store).join("gateway_service.json")
}

fn gateway_service_artifact_dir(store: &AppStore) -> PathBuf {
    gateway_hermes_home(store).join("gateway-service")
}

fn gateway_service_run_command() -> String {
    std::env::current_exe()
        .ok()
        .map(|path| path.to_string_lossy().to_string())
        .filter(|path| !path.trim().is_empty())
        .unwrap_or_else(|| "synthchat-v1".into())
}

fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn s6_gateway_command_for_profile(profile: &str, run_command: &str) -> String {
    if profile == "default" {
        shell_single_quote(run_command)
    } else {
        format!(
            "{} --profile {}",
            shell_single_quote(run_command),
            shell_single_quote(profile)
        )
    }
}

fn write_gateway_service_artifacts(store: &AppStore, service_manager: &Value) -> AppResult<Value> {
    let artifact_dir = gateway_service_artifact_dir(store);
    fs::create_dir_all(&artifact_dir)?;
    let run_command = gateway_service_run_command();
    let windows_script = service_manager
        .get("windows")
        .and_then(|value| value.get("taskScriptPath"))
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .unwrap_or_else(|| artifact_dir.join("Hermes_Gateway.cmd"));
    if let Some(parent) = windows_script.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(
        &windows_script,
        format!(
            "@echo off\r\nsetlocal\r\nset HERMES_GATEWAY_SERVICE=1\r\n\"{}\"\r\n",
            run_command.replace('"', "\"\"")
        ),
    )?;
    let service_name = service_manager
        .get("serviceName")
        .and_then(Value::as_str)
        .unwrap_or("hermes-gateway");
    let systemd_unit_path = artifact_dir.join(format!("{service_name}.service"));
    fs::write(
        &systemd_unit_path,
        format!(
            "[Unit]\nDescription=Hermes Gateway ({service_name}) via SynthChat desktop\nAfter=network-online.target\n\n[Service]\nType=simple\nEnvironment=HERMES_GATEWAY_SERVICE=1\nExecStart={run_command}\nRestart=always\nRestartSec=5\nTimeoutStopSec=120\n\n[Install]\nWantedBy=default.target\n"
        ),
    )?;
    let launchd_label = service_manager
        .get("launchd")
        .and_then(|value| value.get("label"))
        .and_then(Value::as_str)
        .unwrap_or("ai.hermes.gateway");
    let launchd_plist_path = artifact_dir.join(format!("{launchd_label}.plist"));
    fs::write(
        &launchd_plist_path,
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n  <key>Label</key><string>{launchd_label}</string>\n  <key>ProgramArguments</key><array><string>{run_command}</string></array>\n  <key>EnvironmentVariables</key><dict><key>HERMES_GATEWAY_SERVICE</key><string>1</string></dict>\n  <key>RunAtLoad</key><true/>\n  <key>KeepAlive</key><true/>\n</dict>\n</plist>\n"
        ),
    )?;
    let s6 = service_manager
        .get("container")
        .and_then(|value| value.get("s6"))
        .cloned()
        .unwrap_or_else(|| json!({}));
    let s6_profile = s6
        .get("profile")
        .and_then(Value::as_str)
        .unwrap_or("default");
    let s6_service_name = s6
        .get("serviceName")
        .and_then(Value::as_str)
        .unwrap_or("gateway-default");
    let s6_dir = artifact_dir.join("s6").join(s6_service_name);
    let s6_log_dir = s6_dir.join("log");
    fs::create_dir_all(&s6_log_dir)?;
    fs::write(s6_dir.join("type"), "longrun\n")?;
    fs::write(
        s6_dir.join("run"),
        format!(
            "#!/command/with-contenv sh\n# shellcheck shell=sh\nset -e\nexport HOME=\"${{HOME:-/opt/data}}\"\nexport HERMES_GATEWAY_SERVICE=1\nexport HERMES_S6_SUPERVISED_CHILD=1\ncd \"$HOME\"\nexec {}\n",
            s6_gateway_command_for_profile(s6_profile, &run_command)
        ),
    )?;
    fs::write(
        s6_log_dir.join("run"),
        format!(
            "#!/command/with-contenv sh\n# shellcheck shell=sh\n: \"${{HERMES_HOME:={}}}\"\nlog_dir=\"$HERMES_HOME/logs/gateways/{}\"\nmkdir -p \"$log_dir\"\nexec s6-log 1 n10 s1000000 T \"$log_dir\"\n",
            shell_single_quote(&gateway_hermes_home(store).to_string_lossy()),
            s6_profile
        ),
    )?;
    fs::write(
        s6_dir.join("down"),
        "# Presence of this file lets operators register the service without auto-starting it.\n",
    )?;
    Ok(json!({
        "schema": "hermes_gateway_service_artifacts_desktop_v1",
        "artifactDir": artifact_dir.to_string_lossy().to_string(),
        "runCommand": run_command,
        "windowsTaskScriptPath": windows_script.to_string_lossy().to_string(),
        "systemdUnitTemplatePath": systemd_unit_path.to_string_lossy().to_string(),
        "launchdPlistTemplatePath": launchd_plist_path.to_string_lossy().to_string(),
        "s6ServiceDirTemplatePath": s6_dir.to_string_lossy().to_string(),
        "s6RunTemplatePath": s6_dir.join("run").to_string_lossy().to_string(),
        "s6LogRunTemplatePath": s6_log_dir.join("run").to_string_lossy().to_string(),
        "s6DownTemplatePath": s6_dir.join("down").to_string_lossy().to_string(),
    }))
}

fn remove_gateway_service_artifacts(store: &AppStore) -> Value {
    let artifact_dir = gateway_service_artifact_dir(store);
    let removed = fs::remove_dir_all(&artifact_dir).is_ok();
    json!({
        "schema": "hermes_gateway_service_artifacts_desktop_v1",
        "artifactDir": artifact_dir.to_string_lossy().to_string(),
        "removed": removed,
    })
}

fn gateway_service_external_application_plan(service_manager: &Value, artifacts: &Value) -> Value {
    let service_name = service_manager
        .get("serviceName")
        .and_then(Value::as_str)
        .unwrap_or("hermes-gateway");
    let systemd_user_unit = service_manager
        .get("systemd")
        .and_then(|value| value.get("userUnitPath"))
        .and_then(Value::as_str)
        .unwrap_or("~/.config/systemd/user/hermes-gateway.service");
    let systemd_system_unit = service_manager
        .get("systemd")
        .and_then(|value| value.get("systemUnitPath"))
        .and_then(Value::as_str)
        .unwrap_or("/etc/systemd/system/hermes-gateway.service");
    let launchd_label = service_manager
        .get("launchd")
        .and_then(|value| value.get("label"))
        .and_then(Value::as_str)
        .unwrap_or("ai.hermes.gateway");
    let launchd_plist = service_manager
        .get("launchd")
        .and_then(|value| value.get("plistPath"))
        .and_then(Value::as_str)
        .unwrap_or("~/Library/LaunchAgents/ai.hermes.gateway.plist");
    let windows_task = service_manager
        .get("windows")
        .and_then(|value| value.get("scheduledTaskName"))
        .and_then(Value::as_str)
        .unwrap_or("Hermes_Gateway");
    let windows_script = service_manager
        .get("windows")
        .and_then(|value| value.get("taskScriptPath"))
        .and_then(Value::as_str)
        .unwrap_or("<gateway-service>/Hermes_Gateway.cmd");
    let windows_startup = service_manager
        .get("windows")
        .and_then(|value| value.get("startupEntryPath"))
        .and_then(Value::as_str)
        .unwrap_or("<unresolved APPDATA/USERPROFILE>");
    let s6 = service_manager
        .get("container")
        .and_then(|value| value.get("s6"))
        .cloned()
        .unwrap_or_else(|| json!({}));
    let s6_service = s6
        .get("serviceName")
        .and_then(Value::as_str)
        .unwrap_or("gateway-default");
    let s6_service_dir = s6
        .get("serviceDir")
        .and_then(Value::as_str)
        .unwrap_or("/run/service/gateway-default");
    let systemd_template = artifacts
        .get("systemdUnitTemplatePath")
        .and_then(Value::as_str)
        .unwrap_or("<missing systemd artifact>");
    let launchd_template = artifacts
        .get("launchdPlistTemplatePath")
        .and_then(Value::as_str)
        .unwrap_or("<missing launchd artifact>");
    let s6_template = artifacts
        .get("s6ServiceDirTemplatePath")
        .and_then(Value::as_str)
        .unwrap_or("<missing s6 artifact>");
    json!({
        "schema": "hermes_gateway_service_external_application_plan_desktop_v1",
        "appliesOsServiceManager": false,
        "applyRequiresOperator": true,
        "boundary": "SynthChat writes Hermes-style gateway service artifacts and lifecycle state, but does not register them with systemd, launchd, Windows Task Scheduler, startup folders, or s6.",
        "artifacts": artifacts,
        "systemd": {
            "service_name": service_name,
            "user": {
                "unit_path": systemd_user_unit,
                "commands": [
                    format!("install -D {systemd_template} {systemd_user_unit}"),
                    "systemctl --user daemon-reload".to_string(),
                    format!("systemctl --user enable --now {service_name}.service"),
                    format!("systemctl --user status {service_name}.service")
                ]
            },
            "system": {
                "unit_path": systemd_system_unit,
                "commands": [
                    format!("sudo install -D {systemd_template} {systemd_system_unit}"),
                    "sudo systemctl daemon-reload".to_string(),
                    format!("sudo systemctl enable --now {service_name}.service"),
                    format!("sudo systemctl status {service_name}.service")
                ]
            }
        },
        "launchd": {
            "label": launchd_label,
            "plist_path": launchd_plist,
            "commands": [
                format!("cp {launchd_template} {launchd_plist}"),
                format!("launchctl bootstrap gui/$(id -u) {launchd_plist}"),
                format!("launchctl kickstart -k gui/$(id -u)/{launchd_label}"),
                format!("launchctl print gui/$(id -u)/{launchd_label}")
            ]
        },
        "windows": {
            "scheduled_task_name": windows_task,
            "task_script_path": windows_script,
            "startup_entry_path": windows_startup,
            "commands": [
                format!("schtasks /Create /TN {windows_task} /TR \"{windows_script}\" /SC ONLOGON /F"),
                format!("schtasks /Run /TN {windows_task}"),
                format!("schtasks /Query /TN {windows_task}"),
                format!("copy \"{windows_script}\" \"{windows_startup}\"")
            ]
        },
        "s6": {
            "service_name": s6_service,
            "service_dir": s6_service_dir,
            "template_dir": s6_template,
            "commands": [
                format!("cp -R {s6_template} {s6_service_dir}"),
                format!("rm -f {s6_service_dir}/down"),
                format!("s6-svc -u {s6_service_dir}"),
                format!("s6-svstat {s6_service_dir}")
            ]
        }
    })
}

fn platform_gateway_service_snapshot(
    store: &AppStore,
    requested_action: Option<&str>,
) -> AppResult<Value> {
    let runtime = platform_runtime_status_snapshot(store, None)?;
    let scoped_locks = gateway_scoped_lock_records(store);
    let requested_action = requested_action
        .filter(|action| !action.trim().is_empty())
        .unwrap_or("status");
    let service_manager = gateway_service_manager_candidates(store);
    let previous =
        read_gateway_runtime_json(&gateway_service_state_path(store)).unwrap_or_else(|| json!({}));
    let previous_installed = previous
        .get("serviceInstalled")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let previous_running = previous
        .get("serviceRunning")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let runtime_running = runtime
        .get("runtimeLockActive")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let normalized_action = requested_action.trim().to_ascii_lowercase();
    let mut service_installed = previous_installed;
    let mut service_running = previous_running && runtime_running;
    let mut lifecycle_event = "status";
    let mut artifacts = previous.get("artifacts").cloned().unwrap_or(Value::Null);
    match normalized_action.as_str() {
        "install" | "enable" | "add" => {
            service_installed = true;
            service_running = runtime_running;
            lifecycle_event = "installed";
            artifacts = write_gateway_service_artifacts(store, &service_manager)?;
        }
        "start" | "run" => {
            service_installed = true;
            service_running = runtime_running;
            lifecycle_event = "started";
            if artifacts.is_null() {
                artifacts = write_gateway_service_artifacts(store, &service_manager)?;
            }
        }
        "restart" | "reload" => {
            service_installed = true;
            service_running = runtime_running;
            lifecycle_event = "restarted";
            if artifacts.is_null() {
                artifacts = write_gateway_service_artifacts(store, &service_manager)?;
            }
        }
        "stop" | "disable" => {
            service_running = false;
            lifecycle_event = "stopped";
        }
        "uninstall" | "remove" | "delete" => {
            service_installed = false;
            service_running = false;
            lifecycle_event = "uninstalled";
            artifacts = remove_gateway_service_artifacts(store);
        }
        _ => {}
    }
    let external_application_plan =
        gateway_service_external_application_plan(&service_manager, &artifacts);
    let snapshot = json!({
        "kind": "platform_gateway_service",
        "schema": "hermes_gateway_service_desktop_v1",
        "createdAt": now_iso(),
        "desktopAdaptation": true,
        "manager": "synthchat-desktop",
        "requestedAction": requested_action,
        "lifecycleEvent": lifecycle_event,
        "serviceManagerBoundary": false,
        "externalServiceManagerBoundary": true,
        "serviceManager": service_manager,
        "serviceInstalled": service_installed,
        "serviceRunning": service_running,
        "serviceScope": "desktop",
        "desktopRuntimeRunning": runtime_running,
        "artifacts": artifacts,
        "externalApplicationPlan": external_application_plan.clone(),
        "external_application_plan": external_application_plan,
        "gatewayPids": [std::process::id()],
        "hasProcessServiceMismatch": false,
        "scopedLockDir": gateway_scoped_lock_dir(store).to_string_lossy().to_string(),
        "scopedLockCount": scoped_locks.len(),
        "scopedLocks": scoped_locks,
        "runtimeStatus": runtime,
        "note": "SynthChat desktop now persists Hermes-style gateway service lifecycle state and writes service artifact templates. Applying those templates to systemd/launchd/schtasks remains an explicit external service-manager step.",
    });
    gateway_pairing_write_json(&gateway_service_state_path(store), &snapshot)?;
    Ok(snapshot)
}

fn format_platform_gateway_service_snapshot(snapshot: &Value) -> AppResult<String> {
    Ok(format!(
        "平台 gateway service：\n{}",
        serde_json::to_string_pretty(snapshot)?
    ))
}

fn platform_runtime_status_snapshot(store: &AppStore, platform: Option<&str>) -> AppResult<Value> {
    let adapter_status = platform_adapter_status(store, platform)?;
    let adapters = adapter_status
        .get("adapters")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| vec![adapter_status.clone()]);
    let running_count = adapters
        .iter()
        .filter(|adapter| {
            adapter
                .get("status")
                .and_then(Value::as_str)
                .map(|status| matches!(status, "running" | "starting" | "reconnecting"))
                .unwrap_or(false)
        })
        .count();
    let active_agent_runs = store
        .agent_runs()?
        .into_iter()
        .filter(|run| {
            run.parent_run_id.is_none()
                && matches!(
                    run.state.as_str(),
                    "started" | "running" | "pendingApproval"
                )
        })
        .count();
    let restart_request = read_gateway_runtime_json(&gateway_restart_request_path(store))
        .or_else(|| read_gateway_runtime_json(&hermes_gateway_restart_request_path(store)));
    let planned_stop = read_gateway_runtime_json(&gateway_planned_stop_path(store))
        .or_else(|| read_gateway_runtime_json(&hermes_gateway_planned_stop_path(store)));
    let takeover_marker = read_gateway_runtime_json(&gateway_takeover_path(store))
        .or_else(|| read_gateway_runtime_json(&hermes_gateway_takeover_path(store)));
    let scoped_locks = gateway_scoped_lock_records(store);
    let pid_record = desktop_gateway_pid_record();
    let lock_record = json!({
        "kind": "hermes-gateway-runtime-lock",
        "pid": std::process::id(),
        "active": true,
        "desktopAdaptation": true,
        "updatedAt": now_iso(),
        "pidFile": gateway_pid_path(store).to_string_lossy().to_string(),
        "hermesPidFile": hermes_gateway_pid_path(store).to_string_lossy().to_string(),
    });
    let platform_states = adapters
        .iter()
        .filter_map(|adapter| {
            let platform = adapter.get("platform").and_then(Value::as_str)?;
            Some((
                platform.to_string(),
                json!({
                    "state": adapter.get("status").cloned().unwrap_or_else(|| json!("unknown")),
                    "mode": adapter.get("mode").cloned().unwrap_or_else(|| json!("unknown")),
                    "updated_at": adapter.get("updatedAt").cloned().unwrap_or_else(|| json!(now_iso())),
                }),
            ))
        })
        .collect::<serde_json::Map<_, _>>();
    let state = if running_count > 0 {
        "running"
    } else if adapters.iter().any(|adapter| {
        adapter
            .get("configured")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }) {
        "configured"
    } else {
        "idle"
    };
    let channel_directory = channel_directory_status_snapshot(store);
    let snapshot = json!({
        "kind": "hermes-gateway",
        "schema": "hermes_gateway_runtime_status_desktop_v1",
        "pid": std::process::id(),
        "gateway_state": state,
        "exit_reason": Value::Null,
        "restart_requested": restart_request.is_some(),
        "planned_stop": planned_stop.is_some(),
        "takeover_marker": takeover_marker.is_some(),
        "restartRequest": restart_request,
        "plannedStop": planned_stop,
        "takeoverMarker": takeover_marker,
        "pidFile": gateway_pid_path(store).to_string_lossy().to_string(),
        "runtimeLockFile": gateway_runtime_lock_path(store).to_string_lossy().to_string(),
        "hermesHome": gateway_hermes_home(store).to_string_lossy().to_string(),
        "hermesPidFile": hermes_gateway_pid_path(store).to_string_lossy().to_string(),
        "hermesRuntimeLockFile": hermes_gateway_runtime_lock_path(store).to_string_lossy().to_string(),
        "hermesRuntimeStatusFile": hermes_gateway_runtime_status_path(store).to_string_lossy().to_string(),
        "hermesPlannedStopMarker": hermes_gateway_planned_stop_path(store).to_string_lossy().to_string(),
        "hermesTakeoverMarker": hermes_gateway_takeover_path(store).to_string_lossy().to_string(),
        "mirroredToHermesHome": true,
        "scopedLockDir": gateway_scoped_lock_dir(store).to_string_lossy().to_string(),
        "scopedLockCount": scoped_locks.len(),
        "scopedLocks": scoped_locks,
        "runtimeLockActive": true,
        "pidRecord": pid_record,
        "runtimeLock": lock_record,
        "active_agents": active_agent_runs,
        "platform": platform.unwrap_or("all"),
        "platforms": platform_states,
        "channelDirectory": channel_directory,
        "daemonLifecycle": hermes_gateway_daemon_lifecycle_contract(store),
        "daemon_lifecycle": hermes_gateway_daemon_lifecycle_contract_snake(store),
        "updated_at": now_iso(),
    });
    gateway_pairing_write_json(&gateway_pid_path(store), snapshot.get("pidRecord").unwrap())?;
    gateway_pairing_write_json(
        &hermes_gateway_pid_path(store),
        snapshot.get("pidRecord").unwrap(),
    )?;
    gateway_pairing_write_json(
        &gateway_runtime_lock_path(store),
        snapshot.get("runtimeLock").unwrap(),
    )?;
    gateway_pairing_write_json(
        &hermes_gateway_runtime_lock_path(store),
        snapshot.get("runtimeLock").unwrap(),
    )?;
    gateway_pairing_write_json(&gateway_runtime_status_path(store), &snapshot)?;
    gateway_pairing_write_json(&hermes_gateway_runtime_status_path(store), &snapshot)?;
    Ok(snapshot)
}

fn hermes_gateway_daemon_lifecycle_contract(store: &AppStore) -> Value {
    let managed_process_plan = hermes_gateway_daemon_managed_process_plan();
    json!({
        "schema": "hermes_gateway_daemon_lifecycle_desktop_v1",
        "desktopAdaptation": true,
        "desktopRuntimeOwnsStatus": true,
        "desktop_runtime_owns_status": true,
        "pidFileMirroring": true,
        "pid_file_mirroring": true,
        "runtimeLockMirroring": true,
        "runtime_lock_mirroring": true,
        "restartIntentRecords": true,
        "restart_intent_records": true,
        "plannedStopMarkers": true,
        "planned_stop_markers": true,
        "takeoverMarkers": true,
        "takeover_markers": true,
        "serviceArtifactTemplates": true,
        "service_artifact_templates": true,
        "serviceStatePersistence": true,
        "service_state_persistence": true,
        "pythonGatewayDaemonEmbedded": false,
        "python_gateway_daemon_embedded": false,
        "managedProcessPlanReady": true,
        "managed_process_plan_ready": true,
        "managedProcessPlan": managed_process_plan.clone(),
        "managed_process_plan": managed_process_plan,
        "osServiceManagerApplied": false,
        "os_service_manager_applied": false,
        "externalSupervisorRequiredForPythonDaemon": true,
        "external_supervisor_required_for_python_daemon": true,
        "runtimeDir": gateway_runtime_dir(store).to_string_lossy().to_string(),
        "runtime_dir": gateway_runtime_dir(store).to_string_lossy().to_string(),
        "hermesHome": gateway_hermes_home(store).to_string_lossy().to_string(),
        "hermes_home": gateway_hermes_home(store).to_string_lossy().to_string(),
        "serviceStatePath": gateway_service_state_path(store).to_string_lossy().to_string(),
        "service_state_path": gateway_service_state_path(store).to_string_lossy().to_string(),
        "remainingBoundary": "SynthChat persists Hermes-compatible gateway status, PID/lock mirrors, restart/stop/takeover intents, and service artifact templates. It does not embed the Python gateway daemon or apply systemd/launchd/schtasks/s6 service-manager changes from this status path.",
        "remaining_boundary": "SynthChat persists Hermes-compatible gateway status, PID/lock mirrors, restart/stop/takeover intents, and service artifact templates. It does not embed the Python gateway daemon or apply systemd/launchd/schtasks/s6 service-manager changes from this status path."
    })
}

fn hermes_gateway_daemon_lifecycle_contract_snake(store: &AppStore) -> Value {
    let managed_process_plan = hermes_gateway_daemon_managed_process_plan_snake();
    json!({
        "schema": "hermes_gateway_daemon_lifecycle_desktop_v1",
        "desktop_adaptation": true,
        "desktop_runtime_owns_status": true,
        "pid_file_mirroring": true,
        "runtime_lock_mirroring": true,
        "restart_intent_records": true,
        "planned_stop_markers": true,
        "takeover_markers": true,
        "service_artifact_templates": true,
        "service_state_persistence": true,
        "python_gateway_daemon_embedded": false,
        "managed_process_plan_ready": true,
        "managed_process_plan": managed_process_plan,
        "os_service_manager_applied": false,
        "external_supervisor_required_for_python_daemon": true,
        "runtime_dir": gateway_runtime_dir(store).to_string_lossy().to_string(),
        "hermes_home": gateway_hermes_home(store).to_string_lossy().to_string(),
        "service_state_path": gateway_service_state_path(store).to_string_lossy().to_string(),
        "remaining_boundary": "SynthChat persists Hermes-compatible gateway status, PID/lock mirrors, restart/stop/takeover intents, and service artifact templates. It does not embed the Python gateway daemon or apply systemd/launchd/schtasks/s6 service-manager changes from this status path."
    })
}

fn hermes_gateway_daemon_managed_process_plan() -> Value {
    json!({
        "schema": "hermes_gateway_daemon_managed_process_plan_desktop_v1",
        "taskId": "hermes-gateway-daemon",
        "task_id": "hermes-gateway-daemon",
        "command": "hermes gateway run",
        "hermesCommand": "hermes gateway run",
        "hermes_command": "hermes gateway run",
        "replaceCommand": "hermes gateway run --replace",
        "replace_command": "hermes gateway run --replace",
        "managedProcessStartPayload": {
            "action": "start",
            "label": "Hermes gateway daemon",
            "command": "hermes gateway run",
            "taskId": "hermes-gateway-daemon",
            "notifyOnComplete": true,
            "watchPatterns": ["Gateway", "connected", "platform", "restart", "error"]
        },
        "managedProcessReplacePayload": {
            "action": "start",
            "label": "Hermes gateway daemon replace",
            "command": "hermes gateway run --replace",
            "taskId": "hermes-gateway-daemon",
            "notifyOnComplete": true,
            "watchPatterns": ["takeover", "replace", "Gateway", "error"]
        },
        "managedProcessStopPayload": {
            "action": "stop_all",
            "taskId": "hermes-gateway-daemon",
            "forget": false
        },
        "boundary": "This plan lets SynthChat's existing managed-process tool start or replace the external Hermes Python gateway daemon. It does not embed gateway/run.py and does not apply systemd, launchd, schtasks, or s6 service-manager changes."
    })
}

fn hermes_gateway_daemon_managed_process_plan_snake() -> Value {
    json!({
        "schema": "hermes_gateway_daemon_managed_process_plan_desktop_v1",
        "task_id": "hermes-gateway-daemon",
        "command": "hermes gateway run",
        "hermes_command": "hermes gateway run",
        "replace_command": "hermes gateway run --replace",
        "managed_process_start_payload": {
            "action": "start",
            "label": "Hermes gateway daemon",
            "command": "hermes gateway run",
            "taskId": "hermes-gateway-daemon",
            "task_id": "hermes-gateway-daemon",
            "notifyOnComplete": true,
            "notify_on_complete": true,
            "watchPatterns": ["Gateway", "connected", "platform", "restart", "error"],
            "watch_patterns": ["Gateway", "connected", "platform", "restart", "error"]
        },
        "managed_process_replace_payload": {
            "action": "start",
            "label": "Hermes gateway daemon replace",
            "command": "hermes gateway run --replace",
            "taskId": "hermes-gateway-daemon",
            "task_id": "hermes-gateway-daemon",
            "notifyOnComplete": true,
            "notify_on_complete": true,
            "watchPatterns": ["takeover", "replace", "Gateway", "error"],
            "watch_patterns": ["takeover", "replace", "Gateway", "error"]
        },
        "managed_process_stop_payload": {
            "action": "stop_all",
            "taskId": "hermes-gateway-daemon",
            "task_id": "hermes-gateway-daemon",
            "forget": false
        },
        "boundary": "This plan lets SynthChat's existing managed-process tool start or replace the external Hermes Python gateway daemon. It does not embed gateway/run.py and does not apply systemd, launchd, schtasks, or s6 service-manager changes."
    })
}

fn desktop_gateway_pid_record() -> Value {
    json!({
        "pid": std::process::id(),
        "kind": "hermes-gateway",
        "argv": [std::env::current_exe()
            .ok()
            .map(|path| path.to_string_lossy().to_string())
            .unwrap_or_else(|| "synthchat-v1".into())],
        "start_time": Value::Null,
        "desktopAdaptation": true,
        "updatedAt": now_iso(),
    })
}

fn format_platform_runtime_status_line(runtime: &Value) -> String {
    let channel_directory = runtime.get("channelDirectory").unwrap_or(&Value::Null);
    format!(
        "Gateway runtime：state={}, pid={}, activeAgents={}, restartRequested={}, plannedStop={}, pidFile={}, lockActive={}, channelTargets={}, originSessions={}",
        runtime
            .get("gateway_state")
            .and_then(Value::as_str)
            .unwrap_or("unknown"),
        runtime.get("pid").and_then(Value::as_u64).unwrap_or(0),
        runtime
            .get("active_agents")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        runtime
            .get("restart_requested")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        runtime
            .get("planned_stop")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        runtime
            .get("pidFile")
            .and_then(Value::as_str)
            .unwrap_or("-"),
        runtime
            .get("runtimeLockActive")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        channel_directory
            .get("targetCount")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        channel_directory
            .get("sessionDiscovery")
            .and_then(|value| value.get("originConversationCount"))
            .and_then(Value::as_u64)
            .unwrap_or(0),
    )
}

fn platform_memory_monitor_snapshot(
    store: &AppStore,
    platform: Option<&str>,
    tag: &str,
) -> AppResult<Value> {
    let runtime_status = platform_runtime_status_snapshot(store, platform)?;
    let memory = platform_memory_snapshot();
    let snapshot = json!({
        "kind": "platform_memory_monitor",
        "schema": "hermes_gateway_memory_monitor_desktop_v1",
        "createdAt": now_iso(),
        "desktopAdaptation": true,
        "platform": platform.unwrap_or("all"),
        "tag": if tag.trim().is_empty() { "status" } else { tag.trim() },
        "memory": memory,
        "runtimeStatus": runtime_status,
        "logLine": format!(
            "[MEMORY] {} rss={}MB active_agents={}",
            if tag.trim().is_empty() { "status" } else { tag.trim() },
            memory.get("rssMb").and_then(Value::as_u64).map(|value| value.to_string()).unwrap_or_else(|| "unavailable".into()),
            runtime_status.get("active_agents").and_then(Value::as_u64).unwrap_or(0)
        ),
    });
    gateway_pairing_write_json(&gateway_memory_monitor_path(store), &snapshot)?;
    Ok(snapshot)
}

fn format_platform_memory_monitor_snapshot(snapshot: &Value) -> AppResult<String> {
    Ok(format!(
        "平台 memory monitor snapshot：\n{}",
        serde_json::to_string_pretty(snapshot)?
    ))
}

fn read_marker_summary(path: &Path) -> Value {
    let exists = path.exists();
    let raw = fs::read_to_string(path).ok();
    let parsed = raw
        .as_ref()
        .and_then(|text| serde_json::from_str::<Value>(text).ok());
    json!({
        "path": path.to_string_lossy().to_string(),
        "exists": exists,
        "parsed": parsed,
        "rawPreview": raw.map(|text| truncate_for_prompt(&text.replace(['\r', '\n'], " "), 300)),
    })
}

fn platform_shutdown_forensics_context(store: &AppStore) -> AppResult<Value> {
    let drain_timeout = configured_gateway_restart_drain_timeout(store)?;
    let systemd_invocation = std::env::var("INVOCATION_ID").ok();
    let journal_stream = std::env::var("JOURNAL_STREAM").ok();
    let service_manager = gateway_service_manager_candidates(store);
    let timeout_headroom_seconds = 30.0;
    Ok(json!({
        "kind": "shutdown_context",
        "schema": "hermes_gateway_shutdown_context_desktop_v1",
        "createdAt": now_iso(),
        "signal": Value::Null,
        "signal_num": Value::Null,
        "pid": std::process::id(),
        "ppid": Value::Null,
        "parent": Value::Null,
        "self": {
            "pid": std::process::id(),
            "exe": std::env::current_exe()
                .ok()
                .map(|path| path.to_string_lossy().to_string()),
        },
        "systemd_invocation_id": systemd_invocation,
        "systemd_journal_stream": journal_stream,
        "under_systemd": std::env::var_os("INVOCATION_ID").is_some(),
        "hermesHome": gateway_hermes_home(store).to_string_lossy().to_string(),
        "takeover_marker": read_marker_summary(&hermes_gateway_takeover_path(store)),
        "planned_stop_marker": read_marker_summary(&hermes_gateway_planned_stop_path(store)),
        "desktopTakeoverMarker": read_marker_summary(&gateway_takeover_path(store)),
        "desktopPlannedStopMarker": read_marker_summary(&gateway_planned_stop_path(store)),
        "systemdTimingAlignment": {
            "available": false,
            "reason": "SynthChat desktop does not query systemctl during lightweight forensics.",
            "drain_timeout": drain_timeout,
            "headroom_seconds": timeout_headroom_seconds,
            "expected_min_timeout_stop_sec": drain_timeout + timeout_headroom_seconds,
        },
        "serviceManager": service_manager,
        "note": "SynthChat captures the Hermes shutdown-forensics shape without blocking on ps/systemctl subprocess probes inside the desktop runtime.",
    }))
}

fn platform_forensics_snapshot(store: &AppStore, platform: Option<&str>) -> AppResult<Value> {
    let adapter_status = platform_adapter_status(store, platform)?;
    let adapters = adapter_status
        .get("adapters")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| vec![adapter_status.clone()]);
    let running_count = adapters
        .iter()
        .filter(|adapter| {
            adapter
                .get("status")
                .and_then(Value::as_str)
                .map(|status| matches!(status, "running" | "starting" | "reconnecting"))
                .unwrap_or(false)
        })
        .count();
    let config = store.config()?;
    let messaging_gateway_platforms = config
        .messaging_gateway
        .get("platforms")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|value| value.as_str().map(str::to_string))
        .collect::<Vec<_>>();
    let process = json!({
        "pid": std::process::id(),
        "exe": std::env::current_exe()
            .ok()
            .map(|path| path.to_string_lossy().to_string()),
        "cwd": std::env::current_dir()
            .ok()
            .map(|path| path.to_string_lossy().to_string()),
    });
    Ok(json!({
        "kind": "platform_forensics",
        "schema": "hermes_gateway_shutdown_forensics_desktop_v1",
        "createdAt": now_iso(),
        "platform": platform.unwrap_or("all"),
        "desktopAdaptation": true,
        "process": process,
        "memory": platform_memory_snapshot(),
        "shutdownContext": platform_shutdown_forensics_context(store)?,
        "runtimeStatus": platform_runtime_status_snapshot(store, platform)?,
        "dataDir": store.data_dir().to_string_lossy().to_string(),
        "summary": {
            "adapterCount": adapters.len(),
            "runningCount": running_count,
            "configuredMessagingGatewayPlatforms": messaging_gateway_platforms,
        },
        "config": {
            "messagingGateway": {
                "enabled": config.messaging_gateway.get("enabled").and_then(Value::as_bool).unwrap_or(false),
                "urlConfigured": config.messaging_gateway.get("url")
                    .or_else(|| config.messaging_gateway.get("gatewayUrl"))
                    .and_then(Value::as_str)
                    .map(|value| !value.trim().is_empty())
                    .unwrap_or(false),
                "receivePathConfigured": config.messaging_gateway.get("path")
                    .or_else(|| config.messaging_gateway.get("receivePath"))
                    .or_else(|| config.messaging_gateway.get("receive_path"))
                    .and_then(Value::as_str)
                    .map(|value| !value.trim().is_empty())
                    .unwrap_or(false),
            }
        },
        "adapterStatus": adapter_status,
    }))
}

fn format_platform_forensics_snapshot(snapshot: &Value) -> AppResult<String> {
    Ok(format!(
        "平台 forensics snapshot：\n{}",
        serde_json::to_string_pretty(snapshot)?
    ))
}

fn platform_memory_snapshot() -> Value {
    match current_process_rss_bytes() {
        Some(bytes) => json!({
            "available": true,
            "rssBytes": bytes,
            "rssMb": ((bytes as f64) / (1024.0 * 1024.0)).round() as u64,
            "source": current_process_rss_source(),
        }),
        None => json!({
            "available": false,
            "rssBytes": Value::Null,
            "rssMb": Value::Null,
            "source": current_process_rss_source(),
        }),
    }
}

#[cfg(target_os = "windows")]
fn current_process_rss_source() -> &'static str {
    "windows_GetProcessMemoryInfo_WorkingSetSize"
}

#[cfg(target_os = "windows")]
fn current_process_rss_bytes() -> Option<u64> {
    use windows_sys::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    let mut counters = PROCESS_MEMORY_COUNTERS {
        cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
        PageFaultCount: 0,
        PeakWorkingSetSize: 0,
        WorkingSetSize: 0,
        QuotaPeakPagedPoolUsage: 0,
        QuotaPagedPoolUsage: 0,
        QuotaPeakNonPagedPoolUsage: 0,
        QuotaNonPagedPoolUsage: 0,
        PagefileUsage: 0,
        PeakPagefileUsage: 0,
    };
    let ok = unsafe {
        GetProcessMemoryInfo(
            GetCurrentProcess(),
            &mut counters,
            std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
        )
    };
    if ok == 0 {
        None
    } else {
        Some(counters.WorkingSetSize as u64)
    }
}

#[cfg(not(target_os = "windows"))]
fn current_process_rss_source() -> &'static str {
    "proc_self_status_VmRSS"
}

#[cfg(not(target_os = "windows"))]
fn current_process_rss_bytes() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    for line in status.lines() {
        let Some(rest) = line.strip_prefix("VmRSS:") else {
            continue;
        };
        let kb = rest
            .split_whitespace()
            .find_map(|part| part.parse::<u64>().ok())?;
        return Some(kb * 1024);
    }
    None
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
    let mode = state
        .get("mode")
        .and_then(Value::as_str)
        .unwrap_or(if runtime { "runtime" } else { "send_only" });
    let configured = state
        .get("configured")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let transport = state
        .get("transport")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let matrix = state
        .get("capabilityMatrix")
        .or_else(|| state.get("capability_matrix"));
    let send = matrix
        .and_then(|value| value.get("send"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let receive = matrix
        .and_then(|value| value.get("receive"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let lifecycle = matrix
        .and_then(|value| value.get("lifecycle"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let attachments = matrix
        .and_then(|value| value.get("attachments"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    format!(
        "{platform}: status={status}, mode={mode}, configured={configured}, transport={transport}, caps=send:{send}/receive:{receive}/lifecycle:{lifecycle}/attachments:{attachments}",
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
    let mode = state
        .get("mode")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let transport = state
        .get("transport")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    format!(
        "{platform} adapter：\n- status: {status}\n- mode: {mode}\n- transport: {transport}\n- received: {received}\n- triggered: {triggered}\n- updatedAt: {updated_at}\n- lastError: {last_error}"
    )
}

pub(super) fn handle_sethome_control_command(
    store: &AppStore,
    argument_raw: &str,
) -> AppResult<String> {
    let raw = argument_raw.trim();
    if raw.is_empty() || matches!(raw.to_lowercase().as_str(), "status" | "show" | "list") {
        return format_sethome_status(store);
    }
    let Some((platform, target)) = raw.split_once(':') else {
        return Ok("用法：/sethome <platform:target>，例如 /sethome telegram:-100123 或 /sethome slack:C123:1712345678.000100".into());
    };
    let platform = platform.trim().to_ascii_lowercase();
    let target = target.trim();
    if platform.is_empty() || target.is_empty() {
        return Ok("用法：/sethome <platform:target>".into());
    }
    let mut config = store.config()?;
    let target_label = apply_home_target_to_config(&mut config, &platform, target)?;
    store.set_config(config)?;
    Ok(format!("Home target 已设置：{target_label}"))
}

fn format_sethome_status(store: &AppStore) -> AppResult<String> {
    let targets = send_message_external_targets(store)?
        .into_iter()
        .filter_map(|entry| {
            let platform = entry.get("platform").and_then(Value::as_str)?;
            let home = entry
                .get("homeTarget")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or("none");
            Some(format!("- {platform}: {home}"))
        })
        .collect::<Vec<_>>();
    if targets.is_empty() {
        return Ok("当前没有可用 external home targets。".into());
    }
    Ok(format!("Home targets：\n{}", targets.join("\n")))
}

fn apply_home_target_to_config(
    config: &mut crate::models::AppConfig,
    platform: &str,
    target: &str,
) -> AppResult<String> {
    let mut parts = target.splitn(2, ':');
    let primary = parts.next().unwrap_or("").trim();
    let thread = parts
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if primary.is_empty() {
        return Err(AppError::BadRequest("home target is empty".into()));
    }
    match platform {
        "discord" => {
            set_json_string_field(&mut config.discord, "homeChannel", primary);
            Ok(format!("discord:{primary}"))
        }
        "telegram" => {
            set_json_string_field(&mut config.telegram, "homeChannel", primary);
            if let Some(thread) = thread {
                set_json_string_field(&mut config.telegram, "homeThreadId", thread);
                Ok(format!("telegram:{primary}:{thread}"))
            } else {
                Ok(format!("telegram:{primary}"))
            }
        }
        "slack" => {
            set_json_string_field(&mut config.slack, "homeChannel", primary);
            if let Some(thread) = thread {
                set_json_string_field(&mut config.slack, "homeThreadId", thread);
                Ok(format!("slack:{primary}:{thread}"))
            } else {
                Ok(format!("slack:{primary}"))
            }
        }
        "mattermost" => {
            set_json_string_field(&mut config.mattermost, "homeChannel", primary);
            if let Some(thread) = thread {
                set_json_string_field(&mut config.mattermost, "homeThreadId", thread);
                Ok(format!("mattermost:{primary}:{thread}"))
            } else {
                Ok(format!("mattermost:{primary}"))
            }
        }
        "feishu" | "lark" => {
            set_json_string_field(&mut config.feishu, "homeChannel", primary);
            if let Some(thread) = thread {
                set_json_string_field(&mut config.feishu, "homeThreadId", thread);
                Ok(format!("feishu:{primary}:{thread}"))
            } else {
                Ok(format!("feishu:{primary}"))
            }
        }
        "matrix" => {
            set_json_string_field(&mut config.matrix, "homeRoom", target);
            Ok(format!("matrix:{target}"))
        }
        "signal" => {
            set_json_string_field(&mut config.signal, "homeRecipient", target);
            Ok(format!("signal:{target}"))
        }
        "email" => {
            set_json_string_field(&mut config.email, "homeAddress", target);
            Ok(format!("email:{target}"))
        }
        "sms" => {
            set_json_string_field(&mut config.sms, "homeNumber", target);
            Ok(format!("sms:{target}"))
        }
        "dingtalk" => {
            set_json_string_field(&mut config.dingtalk, "homeTarget", target);
            Ok(format!("dingtalk:{target}"))
        }
        "whatsapp" => {
            set_json_string_field(&mut config.whatsapp, "homeChatId", target);
            Ok(format!("whatsapp:{target}"))
        }
        "qqbot" => {
            set_json_string_field(&mut config.qqbot, "homeTarget", target);
            Ok(format!("qqbot:{target}"))
        }
        "homeassistant" | "hass" => {
            set_json_string_field(&mut config.homeassistant, "homeNotifyTarget", target);
            Ok(format!("homeassistant:{target}"))
        }
        "bluebubbles" => {
            set_json_string_field(&mut config.bluebubbles, "homeChatId", target);
            Ok(format!("bluebubbles:{target}"))
        }
        _ => Err(AppError::BadRequest(format!(
            "unsupported home target platform: {platform}"
        ))),
    }
}

fn set_json_string_field(value: &mut Value, key: &str, text: &str) {
    if !value.is_object() {
        *value = json!({});
    }
    if let Some(object) = value.as_object_mut() {
        object.insert(key.to_string(), json!(text));
    }
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

pub(super) fn handle_curator_control_command(
    store: &AppStore,
    argument_raw: &str,
) -> AppResult<String> {
    let mut parts = argument_raw.split_whitespace();
    let action = parts.next().unwrap_or("status").to_lowercase();
    match action.as_str() {
        "" | "status" | "show" => {
            let state = crate::skills::skill_curator_state(store)?;
            Ok(format_skill_curator_state_reply(&state))
        }
        "run" | "scan" => {
            let report = crate::skills::curate_skills_report(store)?;
            Ok(format!(
                "Skill curator 已运行：\n- total: {}\n- external: {}\n- bundled: {}\n- auditAttention: {}\n- overlapClusters: {}\n- archiveCandidates: {}\n- report: {}",
                report.total_skills,
                report.external_skills,
                report.bundled_skills,
                report.audit_attention,
                report.overlap_clusters.len(),
                report.archive_candidates.len(),
                report.report_path
            ))
        }
        "pause" => {
            let state = crate::skills::set_skill_curator_paused(store, true)?;
            Ok(format_skill_curator_state_reply(&state))
        }
        "resume" => {
            let state = crate::skills::set_skill_curator_paused(store, false)?;
            Ok(format_skill_curator_state_reply(&state))
        }
        "pin" => {
            let Some(selector) = parts.next() else {
                return Ok("用法：/curator pin <skill-id>".into());
            };
            let state = crate::skills::pin_skill_for_curator(store, selector)?;
            Ok(format_skill_curator_state_reply(&state))
        }
        "unpin" => {
            let Some(selector) = parts.next() else {
                return Ok("用法：/curator unpin <skill-id>".into());
            };
            let state = crate::skills::unpin_skill_for_curator(store, selector)?;
            Ok(format_skill_curator_state_reply(&state))
        }
        "archive" => {
            let Some(selector) = parts.next() else {
                return Ok("用法：/curator archive <skill-id> [reason]".into());
            };
            let reason = parts.collect::<Vec<_>>().join(" ");
            let archived = crate::skills::archive_skill_for_curator(
                store,
                selector,
                (!reason.trim().is_empty()).then_some(reason.as_str()),
            )?;
            Ok(format!(
                "Skill 已归档：\n- archiveId: {}\n- skillId: {}\n- name: {}\n- archivePath: {}",
                archived.archive_id, archived.skill_id, archived.name, archived.archive_path
            ))
        }
        "restore" => {
            let Some(selector) = parts.next() else {
                return Ok("用法：/curator restore <archive-id|skill-id-prefix>".into());
            };
            let restored = crate::skills::restore_skill_for_curator(store, selector)?;
            Ok(format!(
                "Skill 已恢复：\n- archiveId: {}\n- skillId: {}\n- name: {}\n- originalPath: {}",
                restored.archive_id, restored.skill_id, restored.name, restored.original_path
            ))
        }
        "list-archived" | "archived" => {
            let state = crate::skills::skill_curator_state(store)?;
            let active = state
                .archived
                .iter()
                .filter(|record| record.restored_at.is_none())
                .collect::<Vec<_>>();
            if active.is_empty() {
                return Ok("当前没有未恢复的 archived skills。".into());
            }
            let rows = active
                .iter()
                .take(20)
                .map(|record| {
                    format!(
                        "- {} [{}] archiveId={} reason={}",
                        record.skill_id,
                        record.name,
                        record.archive_id,
                        truncate_for_prompt(&record.reason.replace('\n', " "), 120)
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            let suffix = if active.len() > 20 {
                format!("\n... 还有 {} 个 archived skills 未显示。", active.len() - 20)
            } else {
                String::new()
            };
            Ok(format!("Archived skills：\n{}{}", rows, suffix))
        }
        _ => Ok(
            "用法：/curator [status|run|pause|resume|pin <skill-id>|unpin <skill-id>|archive <skill-id> [reason]|restore <archive-id>|list-archived]"
                .into(),
        ),
    }
}

fn format_skill_curator_state_reply(state: &crate::models::SkillCuratorState) -> String {
    let pinned = if state.pinned_skill_ids.is_empty() {
        "none".to_string()
    } else {
        state.pinned_skill_ids.join(", ")
    };
    let active_archived = state
        .archived
        .iter()
        .filter(|record| record.restored_at.is_none())
        .count();
    format!(
        "Skill curator：\n- paused: {}\n- runCount: {}\n- lastRunAt: {}\n- lastReportPath: {}\n- pinned: {}\n- archivedActive: {} / {}",
        state.paused,
        state.run_count,
        state.last_run_at.as_deref().unwrap_or("never"),
        state.last_report_path.as_deref().unwrap_or("none"),
        pinned,
        active_archived,
        state.archived.len()
    )
}

pub(super) fn handle_plugins_control_command(
    store: &AppStore,
    argument_raw: &str,
) -> AppResult<String> {
    let mut parts = argument_raw.split_whitespace();
    let action = parts.next().unwrap_or("list").to_lowercase();
    match action.as_str() {
        "" | "list" | "show" | "status" => {
            let query = parts.collect::<Vec<_>>().join(" ");
            let plugins = crate::plugins::list_plugins(store)?;
            format_plugins_control_reply(plugins, &query)
        }
        "search" | "find" => {
            let query = parts.collect::<Vec<_>>().join(" ");
            let plugins = crate::plugins::list_plugins(store)?;
            format_plugins_control_reply(plugins, &query)
        }
        "enable" | "on" => {
            let Some(selector) = parts.next() else {
                return Ok("用法：/plugins enable <plugin-id>".into());
            };
            let id = resolve_plugin_selector(store, selector)?;
            let plugins = crate::plugins::toggle_plugin(store, &id, true)?;
            let plugin = plugins
                .iter()
                .find(|plugin| plugin.id == id)
                .ok_or_else(|| AppError::NotFound(format!("plugin {id}")))?;
            Ok(format!(
                "Plugin 已启用：{} [{}] kind={} source={}",
                plugin.id, plugin.name, plugin.kind, plugin.source
            ))
        }
        "disable" | "off" => {
            let Some(selector) = parts.next() else {
                return Ok("用法：/plugins disable <plugin-id>".into());
            };
            let id = resolve_plugin_selector(store, selector)?;
            let plugins = crate::plugins::toggle_plugin(store, &id, false)?;
            let plugin = plugins
                .iter()
                .find(|plugin| plugin.id == id)
                .ok_or_else(|| AppError::NotFound(format!("plugin {id}")))?;
            Ok(format!(
                "Plugin 已禁用：{} [{}] kind={} source={}",
                plugin.id, plugin.name, plugin.kind, plugin.source
            ))
        }
        _ => Ok("用法：/plugins [list [query]|search <query>|enable <id>|disable <id>]".into()),
    }
}

pub(super) async fn handle_kanban_control_command(
    store: &AppStore,
    argument_raw: &str,
) -> AppResult<String> {
    let mut parts = argument_raw.split_whitespace();
    let action = parts.next().unwrap_or("list").to_lowercase();
    let rest = argument_raw
        .trim()
        .strip_prefix(action.as_str())
        .unwrap_or("")
        .trim();
    match action.as_str() {
        "" | "list" | "ls" => {
            let payload = if rest.is_empty() {
                json!({"limit": 50})
            } else {
                json!({"status": rest, "limit": 50})
            };
            kanban_list_tool(store, &payload)
        }
        "show" | "view" => {
            let Some(task_id) = parts.next() else {
                return Ok("用法：/kanban show <task-id>".into());
            };
            kanban_show_tool(store, &json!({"taskId": task_id}))
        }
        "create" | "add" => {
            let args = parse_kanban_create_control_args(rest)?;
            kanban_create_tool(store, &args)
        }
        "comment" => {
            let Some(task_id) = parts.next() else {
                return Ok("用法：/kanban comment <task-id> <text>".into());
            };
            let body = rest.strip_prefix(task_id).unwrap_or("").trim();
            if body.is_empty() {
                return Ok("用法：/kanban comment <task-id> <text>".into());
            }
            kanban_comment_tool(store, &json!({"taskId": task_id, "body": body, "author": "slash"}))
        }
        "complete" | "done" => {
            let Some(task_id) = parts.next() else {
                return Ok("用法：/kanban complete <task-id> <summary>".into());
            };
            let summary = rest.strip_prefix(task_id).unwrap_or("").trim();
            if summary.is_empty() {
                return Ok("用法：/kanban complete <task-id> <summary>".into());
            }
            kanban_complete_tool(store, &json!({"taskId": task_id, "summary": summary}))
        }
        "block" => {
            let Some(task_id) = parts.next() else {
                return Ok("用法：/kanban block <task-id> <reason>".into());
            };
            let reason = rest.strip_prefix(task_id).unwrap_or("").trim();
            if reason.is_empty() {
                return Ok("用法：/kanban block <task-id> <reason>".into());
            }
            kanban_block_tool(store, &json!({"taskId": task_id, "reason": reason}))
        }
        "unblock" => {
            let Some(task_id) = parts.next() else {
                return Ok("用法：/kanban unblock <task-id> [note]".into());
            };
            let note = rest.strip_prefix(task_id).unwrap_or("").trim();
            kanban_unblock_tool(store, &json!({"taskId": task_id, "note": note}))
        }
        "heartbeat" | "ping" => {
            let Some(task_id) = parts.next() else {
                return Ok("用法：/kanban heartbeat <task-id> [note]".into());
            };
            let note = rest.strip_prefix(task_id).unwrap_or("").trim();
            kanban_heartbeat_tool(store, &json!({"taskId": task_id, "note": note}))
        }
        "link" => {
            let Some(parent_id) = parts.next() else {
                return Ok("用法：/kanban link <parent-id> <child-id>".into());
            };
            let Some(child_id) = parts.next() else {
                return Ok("用法：/kanban link <parent-id> <child-id>".into());
            };
            kanban_link_tool(store, &json!({"parentId": parent_id, "childId": child_id}))
        }
        "specify" => {
            let Some(task_id) = parts.next() else {
                return Ok("用法：/kanban specify <task-id>".into());
            };
            kanban_specify_tool(store, &json!({"taskId": task_id, "author": "slash"})).await
        }
        "decompose" => {
            let (objective, create, max_tasks) = parse_kanban_decompose_control_args(rest);
            if objective.trim().is_empty() {
                return Ok("用法：/kanban decompose [--create] [--max N] <objective>".into());
            }
            kanban_decompose_tool(
                store,
                &json!({"objective": objective, "create": create, "maxTasks": max_tasks}),
            )
            .await
        }
        "stats" | "status" => format_kanban_control_stats(store),
        _ => Ok("用法：/kanban [list [status]|show <id>|create [--id id] [--status status] <title>|comment <id> <text>|complete <id> <summary>|block <id> <reason>|unblock <id> [note]|heartbeat <id> [note]|link <parent> <child>|specify <id>|decompose [--create] [--max N] <objective>|stats]".into()),
    }
}

fn parse_kanban_create_control_args(raw: &str) -> AppResult<Value> {
    let mut task_id = None;
    let mut status = None;
    let mut assignee = None;
    let mut priority = None;
    let mut title = Vec::new();
    let mut parts = raw.split_whitespace();
    while let Some(part) = parts.next() {
        match part {
            "--id" | "--task-id" => task_id = parts.next().map(str::to_string),
            "--status" => status = parts.next().map(str::to_string),
            "--assignee" => assignee = parts.next().map(str::to_string),
            "--priority" => priority = parts.next().and_then(|value| value.parse::<i64>().ok()),
            _ => title.push(part),
        }
    }
    let title = title.join(" ").trim().to_string();
    if title.is_empty() {
        return Ok(json!({"usage": "/kanban create [--id id] [--status status] <title>"}));
    }
    let mut payload = json!({"title": title, "createdBy": "slash"});
    if let Some(task_id) = task_id {
        payload["taskId"] = json!(task_id);
    }
    if let Some(status) = status {
        payload["status"] = json!(status);
    }
    if let Some(assignee) = assignee {
        payload["assignee"] = json!(assignee);
    }
    if let Some(priority) = priority {
        payload["priority"] = json!(priority);
    }
    Ok(payload)
}

fn parse_kanban_decompose_control_args(raw: &str) -> (String, bool, u64) {
    let mut create = false;
    let mut max_tasks = 5u64;
    let mut objective = Vec::new();
    let mut parts = raw.split_whitespace();
    while let Some(part) = parts.next() {
        match part {
            "--create" | "create" => create = true,
            "--max" | "--max-tasks" => {
                if let Some(value) = parts.next().and_then(|value| value.parse::<u64>().ok()) {
                    max_tasks = value.clamp(1, 20);
                }
            }
            _ => objective.push(part),
        }
    }
    (objective.join(" "), create, max_tasks)
}

fn format_kanban_control_stats(store: &AppStore) -> AppResult<String> {
    let tasks = store.agent_kanban_tasks()?;
    let mut counts = BTreeMap::<String, usize>::new();
    for task in &tasks {
        let status = task
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        *counts.entry(status).or_default() += 1;
    }
    let rows = if counts.is_empty() {
        "none".to_string()
    } else {
        counts
            .into_iter()
            .map(|(status, count)| format!("- {status}: {count}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    Ok(format!("Kanban：{} task(s)\n{}", tasks.len(), rows))
}

pub(super) fn handle_bundles_control_command(
    store: &AppStore,
    conversation: &Conversation,
    argument_raw: &str,
) -> AppResult<String> {
    let mut parts = argument_raw.split_whitespace();
    let action = parts.next().unwrap_or("list").to_lowercase();
    match action.as_str() {
        "" | "list" => {
            let query = parts.collect::<Vec<_>>().join(" ");
            let bundles = crate::skills::list_skill_bundles(store)?;
            format_skill_bundles_control_reply(bundles, &query)
        }
        "search" | "find" => {
            let query = parts.collect::<Vec<_>>().join(" ");
            let bundles = crate::skills::list_skill_bundles(store)?;
            format_skill_bundles_control_reply(bundles, &query)
        }
        "show" | "inspect" | "info" => {
            let Some(selector) = parts.next() else {
                let bundles = crate::skills::list_skill_bundles(store)?;
                return format_skill_bundles_control_reply(bundles, "");
            };
            let bundle_id = resolve_skill_bundle_selector(store, selector)?;
            let bundles = crate::skills::list_skill_bundles(store)?;
            let bundle = bundles
                .into_iter()
                .find(|bundle| bundle.id == bundle_id)
                .ok_or_else(|| AppError::NotFound(format!("skill bundle {bundle_id}")))?;
            Ok(format_skill_bundle_inspect_reply(&bundle))
        }
        "install" | "enable" | "add" => {
            let Some(selector) = parts.next() else {
                return Ok("用法：/bundles install <bundle-id>".into());
            };
            let bundle_id = resolve_skill_bundle_selector(store, selector)?;
            let bundles = crate::skills::list_skill_bundles(store)?;
            let bundle = bundles
                .iter()
                .find(|bundle| bundle.id == bundle_id)
                .ok_or_else(|| AppError::NotFound(format!("skill bundle {bundle_id}")))?;
            let enabled =
                crate::skills::install_skill_bundle(store, &bundle_id, Some(&conversation.agent_id))?;
            let installed_count = bundle
                .skill_ids
                .iter()
                .filter(|skill_id| enabled.iter().any(|skill| &skill.id == *skill_id && skill.enabled))
                .count();
            Ok(format!(
                "Skill bundle 已安装：{} [{}]\n- agent: {}\n- enabledSkills: {} / {}\n- skills: {}",
                bundle.id,
                bundle.name,
                conversation.agent_id,
                installed_count,
                bundle.skill_ids.len(),
                bundle.skill_ids.join(", ")
            ))
        }
        "create" | "delete" | "remove" | "rm" | "reload" | "refresh" => Ok(
            "SynthChat 当前 /bundles 支持 list/search/show/install；create/delete/reload 还没有对应后端 API。"
                .into(),
        ),
        _ => Ok("用法：/bundles [list [query]|search <query>|show <id>|install <id>]".into()),
    }
}

const MAX_DIRECT_SKILL_INVOCATION_CHARS: usize = 16_000;

#[derive(Debug, Clone)]
pub(super) struct DirectSkillSlashInvocation {
    pub message: String,
}

pub(super) fn build_direct_skill_slash_invocation_for_content(
    store: &AppStore,
    conversation: &Conversation,
    content: &str,
) -> AppResult<Option<DirectSkillSlashInvocation>> {
    let trimmed = content.trim();
    if !(trimmed.starts_with('/') || trimmed.starts_with('／')) {
        return Ok(None);
    }
    let raw_body = trimmed
        .strip_prefix('/')
        .or_else(|| trimmed.strip_prefix('／'))
        .unwrap_or("");
    let mut raw_parts = raw_body.splitn(2, char::is_whitespace);
    let command_name = raw_parts.next().unwrap_or("").to_lowercase();
    let argument_raw = raw_parts.next().unwrap_or("").trim();

    if let Some(message) =
        build_direct_skill_slash_invocation(store, conversation, &command_name, argument_raw)?
    {
        return Ok(Some(DirectSkillSlashInvocation { message }));
    }
    if let Some(message) = build_direct_skill_bundle_slash_invocation(
        store,
        conversation,
        &command_name,
        argument_raw,
    )? {
        return Ok(Some(DirectSkillSlashInvocation { message }));
    }
    Ok(None)
}

fn build_direct_skill_slash_invocation(
    store: &AppStore,
    conversation: &Conversation,
    command_name: &str,
    argument_raw: &str,
) -> AppResult<Option<String>> {
    let normalized = normalize_skill_slash_command_name(command_name);
    if normalized.is_empty() {
        return Ok(None);
    }
    let mut skills = crate::skills::list_skills(store)?;
    skills.sort_by(|left, right| left.id.cmp(&right.id));
    let Some(skill) = skills.into_iter().find(|skill| {
        normalize_skill_slash_command_name(&skill.name) == normalized
            || normalize_skill_slash_command_name(&skill.id.replace('/', "-")) == normalized
    }) else {
        return Ok(None);
    };
    store.enable_agent_skills(&conversation.agent_id, vec![skill.id.clone()])?;
    let instruction = argument_raw.trim();
    let instruction_line = if instruction.is_empty() {
        String::new()
    } else {
        format!("\n- instruction: {instruction}")
    };
    let message = build_skill_invocation_message(store, &skill, instruction, &format!(
        "[IMPORTANT: The user has invoked the \"{}\" skill, indicating they want you to follow its instructions. The full skill content is loaded below.]",
        skill.name
    ))?;
    Ok(Some(format!(
        "{message}\n\n[SynthChat skill slash metadata: command=/{normalized}; agent={}; enabledSkill={}{}]",
        conversation.agent_id, skill.id, instruction_line
    )))
}

fn build_direct_skill_bundle_slash_invocation(
    store: &AppStore,
    conversation: &Conversation,
    command_name: &str,
    argument_raw: &str,
) -> AppResult<Option<String>> {
    let normalized = normalize_skill_bundle_command_name(command_name);
    if normalized.is_empty() {
        return Ok(None);
    }
    let bundles = crate::skills::list_skill_bundles(store)?;
    let Some(bundle) = bundles.iter().find(|bundle| {
        bundle.id.to_lowercase() == normalized
            || normalize_skill_bundle_command_name(&bundle.name) == normalized
    }) else {
        return Ok(None);
    };
    let enabled =
        crate::skills::install_skill_bundle(store, &bundle.id, Some(&conversation.agent_id))?;
    let skills = crate::skills::list_skills(store)?;
    let mut loaded_names = Vec::new();
    let mut missing = Vec::new();
    let mut blocks = Vec::new();
    for skill_id in &bundle.skill_ids {
        let Some(skill) = skills.iter().find(|skill| &skill.id == skill_id) else {
            missing.push(skill_id.clone());
            continue;
        };
        if !enabled
            .iter()
            .any(|enabled| enabled.id == skill.id && enabled.enabled)
        {
            missing.push(skill_id.clone());
            continue;
        }
        blocks.push(build_skill_invocation_message(
            store,
            skill,
            "",
            &format!("[Loaded as part of the \"{}\" skill bundle.]", bundle.name),
        )?);
        loaded_names.push(skill.name.clone());
    }
    if blocks.is_empty() {
        return Ok(None);
    }
    let mut header = vec![
        format!(
            "[IMPORTANT: The user has invoked the \"{}\" skill bundle, loading {} skills together. Treat every skill below as active guidance for this turn.]",
            bundle.name,
            loaded_names.len()
        ),
        String::new(),
        format!("Bundle: {}", bundle.name),
        format!("Skills loaded: {}", loaded_names.join(", ")),
    ];
    if !missing.is_empty() {
        header.push(format!("Skills missing (skipped): {}", missing.join(", ")));
    }
    if !bundle.description.trim().is_empty() {
        header.push(String::new());
        header.push(format!("Bundle instruction: {}", bundle.description.trim()));
    }
    if !argument_raw.trim().is_empty() {
        header.push(String::new());
        header.push(format!("User instruction: {}", argument_raw.trim()));
    }
    Ok(Some(format!(
        "{}\n\n{}",
        header.join("\n"),
        blocks.join("\n\n")
    )))
}

fn build_skill_invocation_message(
    store: &AppStore,
    skill: &EnhancedSkillSummary,
    user_instruction: &str,
    activation_note: &str,
) -> AppResult<String> {
    let skill_path = skill_markdown_path_for_invocation(store, skill);
    let mut content = fs::read_to_string(&skill_path)?;
    content = truncate_chars_for_direct_skill(content, MAX_DIRECT_SKILL_INVOCATION_CHARS);
    let skill_dir = skill_path.parent().map(Path::to_path_buf);
    let mut parts = vec![
        activation_note.to_string(),
        String::new(),
        content.trim().to_string(),
    ];
    if let Some(skill_dir) = &skill_dir {
        parts.push(String::new());
        parts.push(format!("[Skill directory: {}]", skill_dir.display()));
        parts.push(
            "Resolve any relative paths in this skill (e.g. `scripts/foo.js`, `templates/config.yaml`) against that directory, then run them with the terminal tool using the absolute path."
                .to_string(),
        );
        let supporting = supporting_skill_files(skill_dir)?;
        if !supporting.is_empty() {
            parts.push(String::new());
            parts.push("[This skill has supporting files:]".to_string());
            for file in supporting {
                parts.push(format!(
                    "- {}  ->  {}",
                    file.display(),
                    skill_dir.join(&file).display()
                ));
            }
            parts.push(
                "Load supporting files with the skill tools when available, or read/run them directly by absolute path."
                    .to_string(),
            );
        }
    }
    if !user_instruction.trim().is_empty() {
        parts.push(String::new());
        parts.push(format!(
            "The user has provided the following instruction alongside the skill invocation: {}",
            user_instruction.trim()
        ));
    }
    Ok(parts.join("\n"))
}

fn skill_markdown_path_for_invocation(store: &AppStore, skill: &EnhancedSkillSummary) -> PathBuf {
    let path = PathBuf::from(skill.path.trim());
    if path.is_absolute() {
        path
    } else {
        store.data_dir().join(path)
    }
}

fn supporting_skill_files(skill_dir: &Path) -> AppResult<Vec<PathBuf>> {
    let mut supporting = Vec::new();
    for subdir in ["references", "templates", "scripts", "assets"] {
        let root = skill_dir.join(subdir);
        if !root.exists() {
            continue;
        }
        collect_supporting_skill_files(skill_dir, &root, &mut supporting)?;
    }
    supporting.sort();
    supporting.truncate(80);
    Ok(supporting)
}

fn collect_supporting_skill_files(
    skill_dir: &Path,
    current: &Path,
    supporting: &mut Vec<PathBuf>,
) -> AppResult<()> {
    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        let metadata = entry.metadata()?;
        if metadata.is_dir() {
            collect_supporting_skill_files(skill_dir, &path, supporting)?;
        } else if metadata.is_file() {
            if let Ok(relative) = path.strip_prefix(skill_dir) {
                supporting.push(relative.to_path_buf());
            }
        }
    }
    Ok(())
}

fn truncate_chars_for_direct_skill(value: String, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value;
    }
    let mut truncated = value.chars().take(max_chars).collect::<String>();
    truncated.push_str("\n\n[Skill content truncated]");
    truncated
}

fn normalize_skill_slash_command_name(value: &str) -> String {
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
        } else if ch == ' ' || ch == '_' || ch == '-' {
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

fn normalize_skill_bundle_command_name(value: &str) -> String {
    value
        .trim()
        .trim_start_matches('/')
        .trim_start_matches('／')
        .to_lowercase()
        .replace('_', "-")
}

pub(super) fn format_skill_bundles_control_reply(
    mut bundles: Vec<SkillBundle>,
    query: &str,
) -> AppResult<String> {
    let query = query.trim().to_lowercase();
    if !query.is_empty() {
        bundles.retain(|bundle| skill_bundle_matches_query(bundle, &query));
    }
    if bundles.is_empty() {
        return Ok("没有匹配的 skill bundles。可尝试 /skills reload。".into());
    }
    bundles.sort_by(|left, right| left.id.cmp(&right.id));
    let total = bundles.len();
    let skill_count = bundles
        .iter()
        .map(|bundle| bundle.skill_ids.len())
        .sum::<usize>();
    let rows = bundles
        .iter()
        .take(20)
        .map(|bundle| {
            format!(
                "- {} [{}] skills={} :: {}",
                bundle.id,
                bundle.name,
                bundle.skill_ids.len(),
                truncate_for_prompt(&bundle.description.replace('\n', " "), 140)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let suffix = if total > 20 {
        format!("\n... 还有 {} 个 bundle 未显示。", total - 20)
    } else {
        String::new()
    };
    Ok(format!(
        "Skill bundles：{} bundle(s), {} skill(s)\n{}{}",
        total, skill_count, rows, suffix
    ))
}

pub(super) fn format_skill_bundle_inspect_reply(bundle: &SkillBundle) -> String {
    let skills = if bundle.skill_ids.is_empty() {
        "none".to_string()
    } else {
        bundle.skill_ids.join(", ")
    };
    format!(
        "Skill bundle：{} ({})\n- skills: {}\n- description: {}\n- skillIds: {}",
        bundle.name,
        bundle.id,
        bundle.skill_ids.len(),
        truncate_for_prompt(&bundle.description.replace('\n', " "), 400),
        skills
    )
}

fn resolve_skill_bundle_selector(store: &AppStore, selector: &str) -> AppResult<String> {
    let selector = selector.trim();
    if selector.is_empty() {
        return Err(AppError::BadRequest(
            "skill bundle selector is empty".into(),
        ));
    }
    let needle = selector.to_lowercase();
    let bundles = crate::skills::list_skill_bundles(store)?;
    if let Some(bundle) = bundles
        .iter()
        .find(|bundle| bundle.id.to_lowercase() == needle || bundle.name.to_lowercase() == needle)
    {
        return Ok(bundle.id.clone());
    }
    let matches = bundles
        .iter()
        .filter(|bundle| {
            bundle.id.to_lowercase().starts_with(&needle)
                || skill_bundle_matches_query(bundle, &needle)
        })
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [bundle] => Ok(bundle.id.clone()),
        [] => Err(AppError::NotFound(format!("skill bundle {selector}"))),
        _ => Err(AppError::BadRequest(format!(
            "skill bundle selector is ambiguous: {selector}"
        ))),
    }
}

fn skill_bundle_matches_query(bundle: &SkillBundle, query: &str) -> bool {
    [
        bundle.id.as_str(),
        bundle.name.as_str(),
        bundle.description.as_str(),
    ]
    .iter()
    .any(|value| value.to_lowercase().contains(query))
        || bundle
            .skill_ids
            .iter()
            .any(|skill_id| skill_id.to_lowercase().contains(query))
}

pub(super) fn format_plugins_control_reply(
    mut plugins: Vec<crate::models::PluginSummary>,
    query: &str,
) -> AppResult<String> {
    let query = query.trim().to_lowercase();
    if !query.is_empty() {
        plugins.retain(|plugin| plugin_matches_query(plugin, &query));
    }
    if plugins.is_empty() {
        return Ok("没有匹配的 plugins。".into());
    }
    plugins.sort_by(|left, right| {
        right
            .enabled
            .cmp(&left.enabled)
            .then_with(|| left.id.cmp(&right.id))
    });
    let total = plugins.len();
    let enabled = plugins.iter().filter(|plugin| plugin.enabled).count();
    let rows = plugins
        .iter()
        .take(20)
        .map(|plugin| {
            format!(
                "- {} [{}] enabled={} kind={} source={} tools={} hooks={} env={} :: {}",
                plugin.id,
                plugin.name,
                plugin.enabled,
                plugin.kind,
                plugin.source,
                plugin.provided_tools.len(),
                plugin.provided_hooks.len(),
                plugin.requires_env.join(","),
                truncate_for_prompt(&plugin.description.replace('\n', " "), 140)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let suffix = if total > 20 {
        format!("\n... 还有 {} 个 plugin 未显示。", total - 20)
    } else {
        String::new()
    };
    Ok(format!(
        "Plugins：enabled {} / {}\n{}{}",
        enabled, total, rows, suffix
    ))
}

fn resolve_plugin_selector(store: &AppStore, selector: &str) -> AppResult<String> {
    let selector = selector.trim().to_lowercase();
    if selector.is_empty() {
        return Err(AppError::BadRequest("plugin selector is empty".into()));
    }
    let plugins = crate::plugins::list_plugins(store)?;
    if let Some(plugin) = plugins
        .iter()
        .find(|plugin| plugin.id.to_lowercase() == selector)
    {
        return Ok(plugin.id.clone());
    }
    let matches = plugins
        .iter()
        .filter(|plugin| plugin_matches_query(plugin, &selector))
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [plugin] => Ok(plugin.id.clone()),
        [] => Err(AppError::NotFound(format!("plugin {selector}"))),
        _ => Err(AppError::BadRequest(format!(
            "plugin selector is ambiguous: {selector}"
        ))),
    }
}

fn plugin_matches_query(plugin: &crate::models::PluginSummary, query: &str) -> bool {
    [
        plugin.id.as_str(),
        plugin.name.as_str(),
        plugin.description.as_str(),
        plugin.source.as_str(),
        plugin.kind.as_str(),
        plugin.author.as_str(),
    ]
    .iter()
    .any(|value| value.to_lowercase().contains(query))
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
    let lifecycle = hermes_session_lifecycle_snapshot(conversation, active.as_ref());
    Ok(format!(
        "Agent 状态：{}\n- conversation: {} ({})\n- persona: {} ({})\n- agent: {} ({})\n- allowShell: {}\n- runs: {}\n- queuePending: {}\n- pendingApprovals: {}\n- scheduledJobs: {} enabled / {} total\nHermes session lifecycle:\n{}",
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
        jobs.len(),
        serde_json::to_string_pretty(&lifecycle).unwrap_or_else(|_| lifecycle.to_string())
    ))
}

fn hermes_session_lifecycle_snapshot(
    conversation: &Conversation,
    active: Option<&AgentRunRecord>,
) -> Value {
    let stored = conversation
        .metadata
        .get("hermesSessionLifecycle")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let suspended = stored
        .get("suspended")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let resume_pending = stored
        .get("resumePending")
        .or_else(|| stored.get("resume_pending"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    json!({
        "schema": "hermes_gateway_session_lifecycle_desktop_v1",
        "sessionKey": stored
            .get("sessionKey")
            .or_else(|| stored.get("session_key"))
            .and_then(Value::as_str)
            .unwrap_or(conversation.id.as_str()),
        "sessionId": stored
            .get("sessionId")
            .or_else(|| stored.get("session_id"))
            .and_then(Value::as_str)
            .unwrap_or(conversation.id.as_str()),
        "suspended": suspended,
        "resumePending": resume_pending,
        "resumeReason": stored
            .get("resumeReason")
            .or_else(|| stored.get("resume_reason"))
            .cloned()
            .unwrap_or(Value::Null),
        "isFreshReset": stored
            .get("isFreshReset")
            .or_else(|| stored.get("is_fresh_reset"))
            .and_then(Value::as_bool)
            .unwrap_or(false),
        "wasAutoReset": stored
            .get("wasAutoReset")
            .or_else(|| stored.get("was_auto_reset"))
            .and_then(Value::as_bool)
            .unwrap_or(false),
        "autoResetReason": stored
            .get("autoResetReason")
            .or_else(|| stored.get("auto_reset_reason"))
            .cloned()
            .unwrap_or(Value::Null),
        "activeRunId": active.map(|run| run.run_id.clone()),
        "activeRunState": active.map(|run| run.state.clone()),
        "updatedAt": stored
            .get("updatedAt")
            .or_else(|| stored.get("updated_at"))
            .cloned()
            .unwrap_or(Value::Null),
        "desktopAdaptation": true,
        "mirrorsHermesSessionEntry": true,
    })
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

pub(super) fn handle_debug_control_command(
    store: &AppStore,
    conversation: &Conversation,
    argument: &str,
) -> AppResult<String> {
    let run = select_agent_run_for_conversation(store, &conversation.id, argument)?;
    let debug_dir = store
        .data_dir()
        .join("exports")
        .join("debug")
        .join(&conversation.id);
    fs::create_dir_all(&debug_dir)?;
    let stamp = now_iso().replace(':', "").replace('-', "").replace('.', "");
    let report_path = debug_dir.join(format!("debug-{stamp}.md"));
    let bundle_path = debug_dir.join(format!("debug-{stamp}.json"));
    let report = format_debug_control_report(store, conversation, run.as_ref(), &bundle_path)?;
    fs::write(&report_path, report)?;
    if let Some(run) = run.as_ref() {
        fs::write(
            &bundle_path,
            export_agent_run_bundle(store, run.run_id.clone())?,
        )?;
    }
    let run_line = run
        .as_ref()
        .map(|run| format!("run={}", run.run_id))
        .unwrap_or_else(|| "run=none".into());
    let bundle_line = if run.is_some() {
        format!("\nbundle: {}", bundle_path.to_string_lossy())
    } else {
        "\nbundle: 当前会话暂无 agent run，未生成 run bundle".into()
    };
    Ok(format!(
        "debug report 已生成：{}\n{}{}",
        report_path.to_string_lossy(),
        run_line,
        bundle_line
    ))
}

fn format_debug_control_report(
    store: &AppStore,
    conversation: &Conversation,
    run: Option<&AgentRunRecord>,
    bundle_path: &std::path::Path,
) -> AppResult<String> {
    let config = store.config()?;
    let messages = store.messages(&conversation.id, None)?;
    let queue = store.agent_queue()?;
    let approvals = store.tool_approvals()?;
    let runs = store.agent_runs()?;
    let artifacts = if let Some(run) = run {
        store.tool_artifacts_for_run(&run.run_id)?
    } else {
        Vec::new()
    };
    let run_section = if let Some(run) = run {
        format!(
            "## Run\n\n- id: {}\n- state: {}\n- startedAt: {}\n- updatedAt: {}\n- completedAt: {}\n- error: {}\n- request: {}\n- checkpoints: {}\n- toolEvents: {}\n- artifacts: {}\n- bundle: {}\n",
            run.run_id,
            run.state,
            run.started_at,
            run.updated_at,
            run.completed_at.as_deref().unwrap_or("-"),
            run.error.as_deref().unwrap_or("-"),
            markdown_single_line(&run.user_request),
            run.checkpoints.len(),
            run.tool_events.len(),
            artifacts.len(),
            bundle_path.to_string_lossy()
        )
    } else {
        "## Run\n\n当前会话暂无 agent run。\n".into()
    };
    let recent_messages = messages
        .iter()
        .rev()
        .take(8)
        .map(|message| {
            format!(
                "- {} {}: {}",
                message.created_at,
                message.role,
                truncate_for_prompt(&markdown_single_line(&message.content), 220)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    Ok(format!(
        "# SynthChat Debug Report\n\n- generatedAt: {}\n- conversation: {} ({})\n- personaId: {}\n- agentId: {}\n- messages: {}\n- queueItems: {}\n- pendingApprovals: {}\n- totalRuns: {}\n- agentEngine: {}\n- approvalMode: {}\n- busyInputMode: {}\n\n{}\n\n## Recent Messages\n\n{}\n",
        now_iso(),
        markdown_single_line(&conversation.title),
        conversation.id,
        conversation.persona_id.as_deref().unwrap_or("-"),
        conversation.agent_id,
        messages.len(),
        queue
            .iter()
            .filter(|item| item.conversation_id == conversation.id && item.status == "pending")
            .count(),
        approvals.iter().filter(|approval| approval.status == "pending").count(),
        runs.len(),
        config.chat.agent_engine,
        config.chat.tool_approval_mode,
        config.chat.busy_input_mode,
        run_section,
        if recent_messages.is_empty() {
            "- none".into()
        } else {
            recent_messages
        }
    ))
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

pub(super) fn handle_retry_control_command(
    store: &AppStore,
    conversation: &Conversation,
    persona: &Persona,
    app: Option<&AppHandle>,
) -> AppResult<String> {
    let Some(message) = store
        .messages(&conversation.id, None)?
        .into_iter()
        .rev()
        .find(|message| message.role == "user" && !message.content.trim().is_empty())
    else {
        return Ok("当前会话没有可重试的用户消息。".into());
    };
    let (_, queued) =
        enqueue_prompt_for_conversation(store, conversation, persona, message.content.trim())?;
    emit_agent_queue_event(app, "queued", Some(&queued), Some(&conversation.id));
    Ok(format!(
        "已重试最后一条用户消息，加入 agent 队列：{}。\n{}",
        queued.id,
        truncate_for_prompt(&queued.content.replace('\n', " "), 180)
    ))
}

pub(super) fn handle_undo_control_command(
    store: &AppStore,
    conversation: &Conversation,
    persona: &Persona,
    argument_raw: &str,
    app: Option<&AppHandle>,
) -> AppResult<String> {
    let turn_count = argument_raw
        .split_whitespace()
        .next()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(1)
        .clamp(1, 20);
    let messages = store.messages(&conversation.id, None)?;
    let Some(target_index) = nth_user_message_from_end_index(&messages, turn_count) else {
        return Ok(format!(
            "当前会话没有足够的用户轮次可回退：需要 {turn_count} 个。"
        ));
    };
    let prompt = messages[target_index].content.trim().to_string();
    if prompt.is_empty() {
        return Ok("目标用户消息为空，无法重新排队。".into());
    }
    let ids = messages[target_index..]
        .iter()
        .map(|message| message.id.clone())
        .collect::<Vec<_>>();
    let removed = store.remove_messages(&conversation.id, &ids)?;
    let (_, queued) = enqueue_prompt_for_conversation(store, conversation, persona, &prompt)?;
    emit_agent_queue_event(app, "queued", Some(&queued), Some(&conversation.id));
    Ok(format!(
        "已回退 {turn_count} 个用户轮次，删除 {removed} 条后续消息，并重新加入 agent 队列：{}。\n{}",
        queued.id,
        truncate_for_prompt(&queued.content.replace('\n', " "), 180)
    ))
}

fn nth_user_message_from_end_index(messages: &[ChatMessage], n: usize) -> Option<usize> {
    let mut seen = 0usize;
    for (index, message) in messages.iter().enumerate().rev() {
        if message.role == "user" && !message.content.trim().is_empty() {
            seen += 1;
            if seen == n {
                return Some(index);
            }
        }
    }
    None
}

pub(super) fn handle_branch_control_command(
    store: &AppStore,
    conversation: &Conversation,
    persona: &Persona,
    argument_raw: &str,
) -> AppResult<String> {
    let messages = store.messages(&conversation.id, None)?;
    if messages.is_empty() {
        return Ok("当前会话还没有消息，无法创建分支。".into());
    }
    let title = argument_raw.trim();
    let title = if title.is_empty() {
        format!("{} (branch)", conversation.title)
    } else {
        title.chars().take(120).collect::<String>()
    };
    let branch = store.create_conversation(Some(title), Some(persona.id.clone()))?;
    let copied = messages
        .iter()
        .map(|message| {
            let mut copied = ChatMessage::new(
                branch.id.clone(),
                &message.role,
                message.content.clone(),
                &message.source,
            );
            copied.account_id = message.account_id.clone();
            copied.provider_data = message.provider_data.clone();
            copied
        })
        .collect::<Vec<_>>();
    store.replace_conversation_messages(&branch.id, copied)?;
    store.set_conversation_metadata_value(
        &branch.id,
        "branchedFromConversationId",
        json!(conversation.id.clone()),
    )?;
    store.set_conversation_metadata_value(&branch.id, "branchSource", json!("control-command"))?;
    Ok(format!(
        "已创建分支会话：{}。\n- title: {}\n- copiedMessages: {}\n- parent: {}",
        branch.id,
        branch.title,
        messages.len(),
        conversation.id
    ))
}

pub(super) fn handle_new_control_command(
    store: &AppStore,
    persona: &Persona,
    argument_raw: &str,
) -> AppResult<String> {
    let raw_title = argument_raw.trim();
    let title = if raw_title.is_empty() {
        None
    } else {
        Some(raw_title.chars().take(120).collect::<String>())
    };
    let conversation = store.create_conversation(title, Some(persona.id.clone()))?;
    let lifecycle = json!({
        "schema": "hermes_gateway_session_lifecycle_desktop_v1",
        "sessionKey": conversation.id,
        "sessionId": conversation.id,
        "suspended": false,
        "resumePending": false,
        "resumeReason": Value::Null,
        "isFreshReset": true,
        "wasAutoReset": false,
        "autoResetReason": Value::Null,
        "reason": "control_new",
        "updatedAt": now_iso(),
        "source": "control-command",
        "desktopAdaptation": true,
    });
    store.set_conversation_metadata_value(&conversation.id, "hermesSessionLifecycle", lifecycle)?;
    Ok(format!(
        "已创建新会话：{}。\n- title: {}\n- persona: {}",
        conversation.id, conversation.title, persona.id
    ))
}

pub(super) fn enqueue_prompt_for_conversation(
    store: &AppStore,
    conversation: &Conversation,
    persona: &Persona,
    prompt: &str,
) -> AppResult<(ChatMessage, crate::models::AgentQueuedRequest)> {
    enqueue_prompt_for_conversation_with_origin(
        store,
        conversation,
        persona,
        prompt,
        Some("desktop-control-queue"),
        None,
    )
}

pub(super) fn enqueue_prompt_for_conversation_with_origin(
    store: &AppStore,
    conversation: &Conversation,
    persona: &Persona,
    prompt: &str,
    source: Option<&str>,
    provider_data: Option<Value>,
) -> AppResult<(ChatMessage, crate::models::AgentQueuedRequest)> {
    let source = source
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("desktop-control-queue");
    let mut user_message = ChatMessage::new(
        conversation.id.clone(),
        "user",
        prompt.trim().to_string(),
        source,
    );
    user_message.provider_data = provider_data;
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
            provider_data: item.request_provider_data(),
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
            Ok(messages) => {
                crate::wechat_settings::finalize_queued_wechat_turn(
                    store,
                    &messages,
                    item.provider_data.as_ref(),
                    item.started_at.as_deref(),
                )
                .await?;
                "completed"
            }
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

pub(super) fn format_sessions_control_status(
    store: &AppStore,
    argument_raw: &str,
) -> AppResult<String> {
    let mut parts = argument_raw.split_whitespace();
    let first = parts.next().unwrap_or("").trim();
    let limit = first
        .parse::<usize>()
        .ok()
        .map(|value| value.clamp(1, 50))
        .unwrap_or(12);
    let query = if first.parse::<usize>().is_ok() {
        parts.collect::<Vec<_>>().join(" ")
    } else {
        std::iter::once(first)
            .chain(parts)
            .filter(|part| !part.trim().is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    }
    .to_lowercase();
    let mut conversations = store.conversations()?;
    if !query.is_empty() {
        conversations.retain(|conversation| {
            [
                conversation.id.as_str(),
                conversation.title.as_str(),
                conversation.last_message.as_str(),
                conversation.agent_id.as_str(),
                conversation.persona_id.as_deref().unwrap_or(""),
            ]
            .iter()
            .any(|value| value.to_lowercase().contains(&query))
        });
    }
    if conversations.is_empty() {
        return Ok("没有匹配的会话。".into());
    }
    let total = conversations.len();
    conversations.truncate(limit);
    let rows = conversations
        .iter()
        .map(|conversation| {
            let message_count = store
                .messages(&conversation.id, None)
                .map(|messages| messages.len())
                .unwrap_or(0);
            let parent = conversation
                .metadata
                .get("branchedFromConversationId")
                .and_then(Value::as_str)
                .unwrap_or("-");
            format!(
                "- {} title={} updated={} messages={} persona={} parent={} last={}",
                conversation.id,
                truncate_for_prompt(&conversation.title.replace('\n', " "), 80),
                conversation.updated_at,
                message_count,
                conversation.persona_id.as_deref().unwrap_or("-"),
                parent,
                truncate_for_prompt(&conversation.last_message.replace('\n', " "), 100)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let suffix = if total > conversations.len() {
        format!(
            "\n... 还有 {} 个匹配会话未显示。",
            total - conversations.len()
        )
    } else {
        String::new()
    };
    Ok(format!(
        "最近会话：{} / {} shown\n{}{}",
        conversations.len(),
        total,
        rows,
        suffix
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
