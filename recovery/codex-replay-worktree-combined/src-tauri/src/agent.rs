use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::AppHandle;
use futures::{SinkExt, StreamExt};
use tokio_tungstenite::{connect_async, tungstenite::Message};

use crate::{
    error::{AppError, AppResult},
    mcp,
    models::{
        new_id, now_iso, tool_event_kind, AgentCheckpointRecord, AgentDefinition, AgentRunRecord,
        ChatMessage, McpCallResult, SendChatRequest, ToolApprovalRequest, ToolEvent,
        ToolTraceEntry, SkillPromptBlock,
    },
    store::{AppStore, ManagedProcess},
};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::Command,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentControlCommandView {
    pub name: String,
    pub aliases: Vec<String>,
    pub description: String,
    pub category: String,
}

static LAST_BROWSER_URLS: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
static BROWSER_HISTORIES: OnceLock<Mutex<HashMap<String, Vec<String>>>> = OnceLock::new();

pub async fn run_chat_turn(
    store: &AppStore,
    request: SendChatRequest,
    _app: Option<&AppHandle>,
) -> AppResult<Vec<ChatMessage>> {
    let conversation = match request.conversation_id.as_deref() {
        Some(id) if !id.trim().is_empty() => store.conversation(id)?,
        _ => store.create_conversation(None, request.persona_id.clone())?,
    };
    let persona = store.persona(
        request
            .persona_id
            .as_deref()
            .or(conversation.persona_id.as_deref()),
    )?;
    let agent = store.agent(Some(&conversation.agent_id))?;
    let user = store.append_message(ChatMessage::new(
        conversation.id.clone(),
        "user",
        request.content.clone(),
        "desktop",
    ))?;

    let mut run = AgentRunRecord::new(conversation.id.clone(), persona.id.clone(), agent.id.clone());
    run.user_request = request.content.clone();
    run.state = "running".into();
    let saved_run = store.save_agent_run(run.clone())?;

    let history = store.messages(&conversation.id, Some(30))?;
    let provider = store.provider(Some(&persona.llm_provider)).or_else(|_| store.provider(None))?;
    let mut observations = Vec::new();
    let mut assistant_text = String::new();
    let skill_blocks = crate::skills::prompt_blocks_for_request(store, &agent, &request.content)?;
    for iteration in 0..agent.max_tool_iterations.max(1).min(8) {
        let planner_prompt = agent_planner_prompt(&observations, &skill_blocks);
        let reply = crate::llm::complete_chat(
            &provider,
            &persona,
            planner_prompt.clone(),
            history.clone(),
            &request.content,
        )
        .await?;
        let decision = parse_agent_decision(&reply.content);
        match decision
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("final")
        {
            "tool" => {
                let tool_name = decision.get("tool").and_then(Value::as_str).unwrap_or("");
                let payload = decision.get("payload").cloned().unwrap_or_else(|| json!({}));
                match execute_recovery_internal_tool(store, &agent, &saved_run.run_id, tool_name, payload).await {
                    Ok((text, event)) => {
                        observations.push(format!(
                            "Iteration {} tool {} result:\n{}",
                            iteration + 1,
                            tool_name,
                            text
                        ));
                        let tool_message = store.append_message(ChatMessage::new(
                            conversation.id.clone(),
                            "tool",
                            json!({"type": "toolEvent", "event": event}).to_string(),
                            "desktop-agent-tool",
                        ))?;
                        run.tool_events.push(json!(tool_message.content));
                    }
                    Err(error) => {
                        observations.push(format!(
                            "Iteration {} tool {} error: {}",
                            iteration + 1,
                            tool_name,
                            error
                        ));
                    }
                }
            }
            _ => {
                assistant_text = decision
                    .get("content")
                    .or_else(|| decision.get("answer"))
                    .and_then(Value::as_str)
                    .unwrap_or(reply.content.trim())
                    .to_string();
                break;
            }
        }
    }
    if assistant_text.trim().is_empty() {
        assistant_text = if observations.is_empty() {
            recovery_reply(&request.content)
        } else {
            format!(
                "已完成可用工具检查，但当前恢复版 agent loop 未得到最终回答。\n\n{}",
                observations.join("\n\n")
            )
        };
    }
    run.state = "completed".into();
    run.updated_at = now_iso();
    run.completed_at = Some(run.updated_at.clone());
    store.save_agent_run(run)?;

    let assistant = store.append_message(ChatMessage::new(
        conversation.id,
        "assistant",
        assistant_text,
        "desktop-agent",
    ))?;
    Ok(vec![user, assistant])
}

fn recovery_reply(user_content: &str) -> String {
    let trimmed = user_content.trim();
    if trimmed.is_empty() {
        "Agent runtime recovery baseline is active. The previous full agent module must be restored before advanced tool orchestration is available.".into()
    } else {
        format!(
            "Agent runtime recovery baseline is active. I received: {trimmed}\n\nAdvanced Hermes-style tool orchestration is temporarily unavailable until the full agent module is restored."
        )
    }
}

fn agent_planner_prompt(observations: &[String], skill_blocks: &[SkillPromptBlock]) -> String {
    let observation_block = if observations.is_empty() {
        "No tool observations yet.".to_string()
    } else {
        observations.join("\n\n")
    };
    let skill_block = render_skill_prompt_blocks(skill_blocks);
    format!(
        r#"You are SynthChat's recovered agent runtime. Decide the next step from the user request and current observations.

Return JSON only. Do not wrap it in markdown.

Skill instructions:
{skill_block}

Available internal tools:
- read_file: payload {{"path":"relative/or/absolute/path","offset":0,"limit":12000}}
- search_files: payload {{"query":"text","path":".","target":"content|files","limit":20,"maxFiles":3000}}
- write_file: payload {{"path":"relative/or/absolute/path","content":"complete file content"}}
- patch: payload {{"path":"relative/or/absolute/path","search":"exact old text","replace":"new text","replaceAll":false}}
  or payload {{"path":"relative/or/absolute/path","replacements":[{{"search":"old","replace":"new"}}]}}
- terminal: payload {{"command":"shell command","cwd":".","timeoutSeconds":60}}
- process: payload {{"action":"start|list|state|stop","command":"shell command","cwd":".","label":"dev server","processId":"...","forget":false}}
- execute_code: payload {{"language":"python|javascript|powershell","code":"print('ok')","timeoutSeconds":60}}
- todo: payload {{"todos":[{{"content":"inspect code","status":"in_progress"}}]}}
- update_todo: same as todo. Use statuses pending, in_progress, completed.
- checkpoint: payload {{"summary":"what is done","state":"after_inspection","completedCallIds":[],"eventRefs":[]}}
- artifact: payload {{"name":"notes","content":"text to save"}}
- list_artifacts: payload {{}}
- browser_navigate: payload {{"url":"https://example.com"}}
- browser_snapshot: payload {{"url":"https://example.com","full":false}}
- browser_back: payload {{}}
- browser_get_images: payload {{"url":"https://example.com"}}
- browser_cdp: payload {{"cdpUrl":"ws://127.0.0.1:9222/devtools/page/...","method":"Runtime.evaluate","params":{{"expression":"document.title"}},"timeoutMs":10000}}
- browser_click: payload {{"cdpUrl":"ws://...","selector":"button[type=submit]"}}
- browser_type: payload {{"cdpUrl":"ws://...","selector":"input[name=q]","text":"hello","clear":true}}
- browser_press: payload {{"cdpUrl":"ws://...","key":"Enter"}}
- browser_scroll: payload {{"cdpUrl":"ws://...","x":0,"y":700}}
- browser_dialog: payload {{"cdpUrl":"ws://...","accept":true,"promptText":""}}
- browser_console: payload {{"cdpUrl":"ws://...","expression":"document.title"}}
- browser_supervisor_register: payload {{"cdpUrl":"ws://...","sessionId":"optional","providerType":"cdp"}}
- browser_supervisor_state: payload {{"runId":"optional"}}
- browser_supervisor_remove: payload {{"sessionId":"..."}}
- web_request: payload {{"url":"https://example.com/api","method":"GET","headers":{{}},"body":null}}

Use tools when the answer needs project context. Prefer search_files before read_file when you do not know the exact file.
Before write_file or patch, inspect the target file unless the user explicitly provided the full intended content.
Use terminal/process/execute_code only when command execution is necessary and the agent is configured to allow shell access.
For web page tasks, prefer browser_snapshot/browser_navigate first because they expose forms, inputs, links, and request clues before choosing click/type/fetch-style actions.
When enough context is available, return {{"action":"final","content":"your answer"}}.
If no tool is needed, answer directly with final.

Current observations:
{observation_block}"#
    )
}

fn render_skill_prompt_blocks(skill_blocks: &[SkillPromptBlock]) -> String {
    if skill_blocks.is_empty() {
        return "No enabled or explicitly requested skill instructions.".into();
    }
    skill_blocks
        .iter()
        .map(|block| {
            format!(
                "### Skill: {} ({})\n{}",
                block.name,
                block.id,
                block.content.trim()
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn parse_agent_decision(raw: &str) -> Value {
    let trimmed = raw.trim();
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        return value;
    }
    if let Some(json_text) = first_json_object(trimmed) {
        if let Ok(value) = serde_json::from_str::<Value>(&json_text) {
            return value;
        }
    }
    json!({"action": "final", "content": trimmed})
}

fn first_json_object(text: &str) -> Option<String> {
    let mut start = None;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (index, ch) in text.char_indices() {
        if start.is_none() {
            if ch == '{' {
                start = Some(index);
                depth = 1;
            }
            continue;
        }
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' && in_string {
            escaped = true;
            continue;
        }
        if ch == '"' {
            in_string = !in_string;
            continue;
        }
        if in_string {
            continue;
        }
        match ch {
            '{' => depth += 1,
            '}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    let begin = start?;
                    return Some(text[begin..=index].to_string());
                }
            }
            _ => {}
        }
    }
    None
}

async fn execute_recovery_internal_tool(
    store: &AppStore,
    agent: &AgentDefinition,
    conversation_id: &str,
    run_id: &str,
    tool_name: &str,
    payload: Value,
) -> AppResult<(String, ToolEvent)> {
    let started = Instant::now();
    let result = match tool_name {
        "read_file" => read_file_tool(agent, &payload),
        "search_files" => search_files_tool(agent, &payload),
        "write_file" => write_file_tool(agent, &payload),
        "patch" => patch_tool(agent, &payload),
        "terminal" => terminal_tool(agent, &payload).await,
        "process" => process_tool(store, agent, &payload).await,
        "execute_code" => execute_code_tool(agent, &payload).await,
        "todo" | "update_todo" => todo_tool(store, run_id, conversation_id, &payload),
        "checkpoint" => checkpoint_tool(store, run_id, &payload),
        "artifact" => artifact_tool(store, run_id, &payload),
        "list_artifacts" => list_artifacts_tool(store, run_id),
        "browser_navigate" => browser_navigate_tool(store, agent, run_id, &payload).await,
        "browser_snapshot" => browser_snapshot_tool(store, agent, run_id, &payload).await,
        "browser_back" => browser_back_tool(agent).await,
        "browser_get_images" => browser_get_images_tool(agent, &payload).await,
        "browser_cdp" => browser_cdp_tool(&payload).await,
        "browser_click" => browser_click_tool(&payload).await,
        "browser_type" => browser_type_tool(&payload).await,
        "browser_press" => browser_press_tool(&payload).await,
        "browser_scroll" => browser_scroll_tool(&payload).await,
        "browser_dialog" => browser_dialog_tool(&payload).await,
        "browser_console" => browser_console_tool(&payload).await,
        "browser_supervisor_register" => {
            browser_supervisor_register_tool(store, run_id, &payload).await
        }
        "browser_supervisor_state" => browser_supervisor_state_tool(store, run_id, &payload).await,
        "browser_supervisor_remove" => browser_supervisor_remove_tool(store, &payload).await,
        "web_request" => web_request_tool(&payload).await,
        other => Err(AppError::BadRequest(format!(
            "internal tool '{other}' is not available in the recovered runtime"
        ))),
    };
    let elapsed_ms = started.elapsed().as_millis();
    let (ok, text, error) = match result {
        Ok(text) => (true, text, None),
        Err(error) => (false, String::new(), Some(error.to_string())),
    };
    let event = ToolEvent {
        status: Some(if ok { "completed" } else { "failed" }.into()),
        reference_id: None,
        call_id: Some(new_id("call")),
        run_id: Some(run_id.to_string()),
        checkpoint_id: None,
        event_type: "internal_tool".into(),
        server_id: "__internal".into(),
        tool_name: tool_name.into(),
        ok,
        timed_out: false,
        elapsed_ms,
        kind: tool_event_kind("__internal", tool_name, None),
        title: format!("internal · {tool_name}"),
        summary: if ok {
            summarize_tool_text(&text)
        } else {
            error
                .clone()
                .unwrap_or_else(|| "internal tool failed".into())
        },
        path: payload.get("path").and_then(Value::as_str).map(str::to_string),
        exists: None,
        mime_type: Some("text/plain".into()),
        text: if text.is_empty() { None } else { Some(text.clone()) },
        error: error.clone(),
        raw: Some(json!({"payload": payload})),
    };
    store.append_tool_trace(ToolTraceEntry {
        id: new_id("trace"),
        created_at: now_iso(),
        server_id: "__internal".into(),
        tool_name: tool_name.into(),
        ok,
        timed_out: false,
        elapsed_ms,
        payload,
        event: event.clone(),
        error: error.clone(),
    })?;
    if let Some(error) = error {
        Err(AppError::BadRequest(error))
    } else {
        Ok((text, event))
    }
}

fn read_file_tool(agent: &AgentDefinition, payload: &Value) -> AppResult<String> {
    let path = payload
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::BadRequest("read_file requires payload.path".into()))?;
    let limit = payload
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(12000)
        .min(80000) as usize;
    let offset = payload
        .get("offset")
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;
    let root = workspace_root(agent)?;
    let full_path = resolve_workspace_path(&root, path)?;
    let content = fs::read_to_string(&full_path)?;
    let total_chars = content.chars().count();
    let slice: String = content.chars().skip(offset).take(limit).collect();
    Ok(format!(
        "path: {}\nchars: {} offset: {} limit: {}\n\n{}",
        full_path.display(),
        total_chars,
        offset,
        limit,
        slice
    ))
}

fn search_files_tool(agent: &AgentDefinition, payload: &Value) -> AppResult<String> {
    let query = payload
        .get("query")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    let target = payload
        .get("target")
        .and_then(Value::as_str)
        .unwrap_or("content");
    let limit = payload
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(20)
        .min(100) as usize;
    let max_files = payload
        .get("maxFiles")
        .or_else(|| payload.get("max_files"))
        .and_then(Value::as_u64)
        .unwrap_or(3000)
        .min(20000) as usize;
    let root = workspace_root(agent)?;
    let start = resolve_workspace_path(
        &root,
        payload.get("path").and_then(Value::as_str).unwrap_or("."),
    )?;
    if query.is_empty() {
        return Err(AppError::BadRequest("search_files requires a non-empty query".into()));
    }

    let mut checked = 0usize;
    let mut matches = Vec::new();
    search_recursive(&root, &start, &query, target, limit, max_files, &mut checked, &mut matches)?;
    Ok(format!(
        "query: {query}\ntarget: {target}\ncheckedFiles: {checked}\nmatches: {}\n\n{}",
        matches.len(),
        matches.join("\n")
    ))
}

fn write_file_tool(agent: &AgentDefinition, payload: &Value) -> AppResult<String> {
    let path = payload
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::BadRequest("write_file requires payload.path".into()))?;
    let content = payload
        .get("content")
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::BadRequest("write_file requires payload.content".into()))?;
    let root = workspace_root(agent)?;
    let full_path = resolve_workspace_target_path(&root, path)?;
    if let Some(parent) = full_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&full_path, content)?;
    Ok(format!(
        "wrote file: {}\nbytes: {}\nchars: {}",
        full_path.display(),
        content.len(),
        content.chars().count()
    ))
}

fn patch_tool(agent: &AgentDefinition, payload: &Value) -> AppResult<String> {
    let path = payload
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::BadRequest("patch requires payload.path".into()))?;
    let root = workspace_root(agent)?;
    let full_path = resolve_workspace_path(&root, path)?;
    let mut content = fs::read_to_string(&full_path)?;
    let replacements = normalized_replacements(payload)?;
    let mut applied = 0usize;
    for (search, replace, replace_all) in replacements {
        if search.is_empty() {
            return Err(AppError::BadRequest(
                "patch replacement search text cannot be empty".into(),
            ));
        }
        if replace_all {
            let count = content.matches(&search).count();
            if count == 0 {
                return Err(AppError::BadRequest(format!(
                    "patch search text was not found in {}",
                    full_path.display()
                )));
            }
            content = content.replace(&search, &replace);
            applied += count;
        } else if let Some(index) = content.find(&search) {
            content.replace_range(index..index + search.len(), &replace);
            applied += 1;
        } else {
            return Err(AppError::BadRequest(format!(
                "patch search text was not found in {}",
                full_path.display()
            )));
        }
    }
    fs::write(&full_path, content)?;
    Ok(format!(
        "patched file: {}\nreplacementsApplied: {}",
        full_path.display(),
        applied
    ))
}

fn normalized_replacements(payload: &Value) -> AppResult<Vec<(String, String, bool)>> {
    if let Some(items) = payload.get("replacements").and_then(Value::as_array) {
        let mut replacements = Vec::new();
        for item in items {
            let search = item
                .get("search")
                .or_else(|| item.get("old"))
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    AppError::BadRequest("each patch replacement requires search".into())
                })?;
            let replace = item
                .get("replace")
                .or_else(|| item.get("new"))
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    AppError::BadRequest("each patch replacement requires replace".into())
                })?;
            let replace_all = item
                .get("replaceAll")
                .or_else(|| item.get("replace_all"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            replacements.push((search.to_string(), replace.to_string(), replace_all));
        }
        if replacements.is_empty() {
            return Err(AppError::BadRequest(
                "patch replacements cannot be empty".into(),
            ));
        }
        return Ok(replacements);
    }
    let search = payload
        .get("search")
        .or_else(|| payload.get("old"))
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::BadRequest("patch requires search/replace".into()))?;
    let replace = payload
        .get("replace")
        .or_else(|| payload.get("new"))
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::BadRequest("patch requires search/replace".into()))?;
    let replace_all = payload
        .get("replaceAll")
        .or_else(|| payload.get("replace_all"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    Ok(vec![(search.to_string(), replace.to_string(), replace_all)])
}

fn todo_tool(
    store: &AppStore,
    run_id: &str,
    conversation_id: &str,
    _payload: &Value,
) -> AppResult<String> {
    let Some(todos_value) = payload.get("todos") else {
        return Ok(serde_json::to_string_pretty(&json!({
            "runId": run_id,
            "todos": store.agent_todos_for_run(run_id)?
        }))?);
    };
    let todos = todos_value
        .as_array()
        .ok_or_else(|| AppError::BadRequest("todo payload.todos must be an array".into()))?
        .iter()
        .filter_map(|item| {
            if let Some(text) = item.as_str() {
                return Some((text.to_string(), "pending".to_string()));
            }
            let content = item
                .get("content")
                .or_else(|| item.get("task"))
                .or_else(|| item.get("text"))
                .and_then(Value::as_str)?
                .to_string();
            let status = item
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("pending")
                .to_string();
            Some((content, normalize_todo_status(&status)))
        })
        .collect::<Vec<_>>();
    let saved = store.replace_agent_todos(run_id, conversation_id, todos)?;
    Ok(serde_json::to_string_pretty(&json!({
        "runId": run_id,
        "todos": saved
    }))?)
}

fn normalize_todo_status(status: &str) -> String {
    match status.trim().to_lowercase().as_str() {
        "done" | "complete" | "completed" => "completed".into(),
        "doing" | "active" | "in-progress" | "in_progress" => "in_progress".into(),
        "blocked" => "blocked".into(),
        _ => "pending".into(),
    }
}

fn checkpoint_tool(store: &AppStore, run_id: &str, payload: &Value) -> AppResult<String> {
    let summary = payload
        .get("summary")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| AppError::BadRequest("checkpoint requires payload.summary".into()))?
        .to_string();
    let state = payload
        .get("state")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("checkpoint")
        .to_string();
    let mut run = store.agent_run(run_id)?;
    let checkpoint = AgentCheckpointRecord {
        checkpoint_id: new_id("ckpt"),
        run_id: run_id.to_string(),
        iteration: run.checkpoints.len() as u32 + 1,
        created_at: now_iso(),
        state,
        completed_call_ids: payload_string_array(payload, "completedCallIds", "completed_call_ids"),
        event_refs: payload_string_array(payload, "eventRefs", "event_refs"),
        summary,
    };
    run.checkpoints.push(checkpoint.clone());
    run.updated_at = now_iso();
    store.save_agent_run(run)?;
    Ok(serde_json::to_string_pretty(&checkpoint)?)
}

fn artifact_tool(store: &AppStore, run_id: &str, payload: &Value) -> AppResult<String> {
    let content = payload
        .get("content")
        .or_else(|| payload.get("text"))
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::BadRequest("artifact requires payload.content".into()))?;
    let name = payload
        .get("name")
        .or_else(|| payload.get("toolName"))
        .or_else(|| payload.get("tool_name"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("artifact");
    let path = store.save_tool_artifact(run_id, name, content)?;
    Ok(serde_json::to_string_pretty(&json!({
        "runId": run_id,
        "name": name,
        "path": path.to_string_lossy(),
        "sizeBytes": content.len()
    }))?)
}

fn list_artifacts_tool(store: &AppStore, run_id: &str) -> AppResult<String> {
    Ok(serde_json::to_string_pretty(&json!({
        "runId": run_id,
        "artifacts": store.tool_artifacts_for_run(run_id)?
    }))?)
}

fn payload_string_array(payload: &Value, camel_key: &str, snake_key: &str) -> Vec<String> {
    payload
        .get(camel_key)
        .or_else(|| payload.get(snake_key))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

async fn terminal_tool(agent: &AgentDefinition, payload: &Value) -> AppResult<String> {
    ensure_shell_allowed(agent)?;
    let command = payload
        .get("command")
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::BadRequest("terminal requires payload.command".into()))?;
    let timeout_seconds = payload
        .get("timeoutSeconds")
        .or_else(|| payload.get("timeout"))
        .and_then(Value::as_u64)
        .unwrap_or(60)
        .clamp(1, 600);
    let cwd = workspace_cwd(agent, payload.get("cwd").or_else(|| payload.get("workdir")))?;
    run_shell_command(command, &cwd, timeout_seconds).await
}

async fn execute_code_tool(agent: &AgentDefinition, payload: &Value) -> AppResult<String> {
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
            )))
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
    let result = run_shell_command(&command, &root, timeout_seconds).await;
    let _ = fs::remove_file(&path);
    result
}

async fn delegate_task_tool(
    store: &AppStore,
    agent: &AgentDefinition,
    conversation_id: &str,
    parent_run_id: &str,
    payload: &Value,
) -> AppResult<String> {
    let task = payload
        .get("task")
        .or_else(|| payload.get("content"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| AppError::BadRequest("delegate_task requires payload.task".into()))?;
    let role = payload
        .get("role")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("subagent");
    let toolsets = payload_string_array(payload, "toolsets", "toolsets");
    let parent = store.agent_run(parent_run_id)?;
    let parent_depth = parent.subagent_depth.unwrap_or(0);
    if parent_depth >= agent.max_subagent_depth {
        return Err(AppError::BadRequest(format!(
            "delegate_task depth limit reached: {}",
            agent.max_subagent_depth
        )));
    }
    let child_count = store
        .agent_runs()?
        .into_iter()
        .filter(|run| run.parent_run_id.as_deref() == Some(parent_run_id))
        .count() as u32;
    if child_count >= agent.max_subagents {
        return Err(AppError::BadRequest(format!(
            "delegate_task subagent limit reached: {}",
            agent.max_subagents
        )));
    }

    let persona = store.persona(Some(&parent.persona_id))?;
    let provider = store
        .provider(Some(&persona.llm_provider))
        .or_else(|_| store.provider(None))?;
    let mut child =
        AgentRunRecord::new(conversation_id.to_string(), persona.id.clone(), agent.id.clone());
    child.parent_run_id = Some(parent_run_id.to_string());
    child.subagent_index = Some(child_count + 1);
    child.subagent_depth = Some(parent_depth + 1);
    child.subagent_can_delegate = Some(
        payload
            .get("canDelegate")
            .or_else(|| payload.get("can_delegate"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
            && parent_depth + 1 < agent.max_subagent_depth,
    );
    child.subagent_role = Some(role.to_string());
    child.subagent_task = Some(task.to_string());
    child.subagent_toolsets = toolsets.clone();
    child.user_request = task.to_string();
    child.state = "running".into();
    let child = store.save_agent_run(child)?;

    let system_prompt = format!(
        "You are a focused SynthChat subagent.\nRole: {role}\nToolsets: {}\nReturn a concise result for the parent agent. Do not ask the user follow-up questions.",
        if toolsets.is_empty() {
            "default leaf scope".into()
        } else {
            toolsets.join(", ")
        }
    );
    let history = store.messages(conversation_id, Some(12))?;
    let reply = crate::llm::complete_chat(&provider, &persona, system_prompt, history, task).await;
    match reply {
        Ok(reply) => {
            let mut saved = store.agent_run(&child.run_id)?;
            saved.state = "completed".into();
            saved.updated_at = now_iso();
            saved.completed_at = Some(saved.updated_at.clone());
            store.save_agent_run(saved)?;
            append_parent_phase_event(
                store,
                parent_run_id,
                "subagent_completed",
                json!({
                    "childRunId": child.run_id,
                    "role": role,
                    "task": task,
                    "toolsets": toolsets,
                    "summary": reply.content
                }),
            )?;
            Ok(serde_json::to_string_pretty(&json!({
                "childRunId": child.run_id,
                "role": role,
                "task": task,
                "result": reply.content
            }))?)
        }
        Err(error) => {
            let mut saved = store.agent_run(&child.run_id)?;
            saved.state = "failed".into();
            saved.error = Some(error.to_string());
            saved.updated_at = now_iso();
            saved.completed_at = Some(saved.updated_at.clone());
            store.save_agent_run(saved)?;
            append_parent_phase_event(
                store,
                parent_run_id,
                "subagent_failed",
                json!({
                    "childRunId": child.run_id,
                    "role": role,
                    "task": task,
                    "error": error.to_string()
                }),
            )?;
            Err(error)
        }
    }
}

fn append_parent_phase_event(
    store: &AppStore,
    run_id: &str,
    phase: &str,
    detail: Value,
) -> AppResult<()> {
    let mut run = store.agent_run(run_id)?;
    run.phase_events.push(AgentRunPhaseRecord {
        phase: phase.to_string(),
        detail,
        updated_at: now_iso(),
    });
    run.updated_at = now_iso();
    store.save_agent_run(run)?;
    Ok(())
}

async fn image_generate_tool(store: &AppStore, run_id: &str, payload: &Value) -> AppResult<String> {
    let prompt = payload
        .get("prompt")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| AppError::BadRequest("image_generate requires payload.prompt".into()))?;
    let provider = store
        .enabled_image_provider()?
        .ok_or_else(|| AppError::BadRequest("no enabled image provider configured".into()))?;
    match provider.provider_type.trim().to_lowercase().as_str() {
        "openai" | "openai-compatible" | "compatible" | "" => {
            openai_compatible_image_generate(store, run_id, &provider, prompt, payload).await
        }
        other => Err(AppError::BadRequest(format!(
            "unsupported image provider type: {other}"
        ))),
    }
}

async fn openai_compatible_image_generate(
    store: &AppStore,
    run_id: &str,
    provider: &ImageProvider,
    prompt: &str,
    payload: &Value,
) -> AppResult<String> {
    let mut url = reqwest::Url::parse(provider.base_url.trim())
        .map_err(|error| AppError::BadRequest(format!("invalid image provider URL: {error}")))?;
    if !url.path().ends_with("/images/generations") {
        let mut path = url.path().trim_end_matches('/').to_string();
        path.push_str("/images/generations");
        url.set_path(&path);
    }
    let size = payload
        .get("size")
        .and_then(Value::as_str)
        .unwrap_or("1024x1024");
    let count = payload.get("n").and_then(Value::as_u64).unwrap_or(1).clamp(1, 4);
    let model = payload
        .get("model")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(&provider.model);
    let mut body = json!({
        "model": model,
        "prompt": prompt,
        "size": size,
        "n": count,
        "response_format": "b64_json"
    });
    if let Some(extra) = payload.get("extra").and_then(Value::as_object) {
        if let Some(body_obj) = body.as_object_mut() {
            for (key, value) in extra {
                body_obj.insert(key.clone(), value.clone());
            }
        }
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(provider.timeout_seconds.max(1)))
        .user_agent("SynthChat-agent/1.0")
        .build()
        .map_err(|error| AppError::BadRequest(format!("failed to build image client: {error}")))?;
    let mut request = client.post(url.clone()).json(&body);
    if let Some(api_key) = provider_api_key(&provider.api_key, &provider.api_key_env) {
        request = request.bearer_auth(api_key);
    }
    let response = request
        .send()
        .await
        .map_err(|error| AppError::BadRequest(format!("image_generate failed: {error}")))?;
    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|error| AppError::BadRequest(format!("failed to read image response: {error}")))?;
    if !status.is_success() {
        return Err(AppError::BadRequest(format!(
            "image_generate returned HTTP {}: {}",
            status.as_u16(),
            truncate_output(&text, 2000)
        )));
    }
    let value = serde_json::from_str::<Value>(&text)
        .map_err(|error| AppError::BadRequest(format!("invalid image JSON: {error}")))?;
    let data = value
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| AppError::BadRequest("image response missing data array".into()))?;
    let mut artifacts = Vec::new();
    for item in data {
        if let Some(b64) = item.get("b64_json").and_then(Value::as_str) {
            let bytes = decode_base64_image(b64)?;
            let path = store.save_tool_binary_artifact(run_id, "image_generate", "png", &bytes)?;
            artifacts.push(json!({"path": path.to_string_lossy(), "source": "b64_json", "sizeBytes": bytes.len()}));
        } else if let Some(image_url) = item.get("url").and_then(Value::as_str) {
            validate_web_url(image_url)?;
            let (bytes, extension) = download_image_bytes(&client, image_url).await?;
            let path =
                store.save_tool_binary_artifact(run_id, "image_generate", &extension, &bytes)?;
            artifacts.push(json!({"path": path.to_string_lossy(), "source": image_url, "sizeBytes": bytes.len()}));
        }
    }
    if artifacts.is_empty() {
        return Err(AppError::BadRequest(
            "image response did not contain b64_json or url".into(),
        ));
    }
    Ok(serde_json::to_string_pretty(&json!({
        "providerId": provider.id,
        "model": model,
        "prompt": prompt,
        "artifacts": artifacts
    }))?)
}

fn provider_api_key(inline: &Option<String>, env_name: &str) -> Option<String> {
    inline
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| {
            let env_name = env_name.trim();
            if env_name.is_empty() {
                None
            } else {
                std::env::var(env_name).ok()
            }
        })
}

fn decode_base64_image(value: &str) -> AppResult<Vec<u8>> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(value)
        .map_err(|error| AppError::BadRequest(format!("invalid image base64: {error}")))
}

async fn download_image_bytes(
    client: &reqwest::Client,
    image_url: &str,
) -> AppResult<(Vec<u8>, String)> {
    let response = client
        .get(image_url)
        .send()
        .await
        .map_err(|error| AppError::BadRequest(format!("image download failed: {error}")))?;
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_string();
    let bytes = response
        .bytes()
        .await
        .map_err(|error| AppError::BadRequest(format!("failed to read image bytes: {error}")))?
        .to_vec();
    Ok((bytes, image_extension_from_content_type(&content_type)))
}

fn image_extension_from_content_type(content_type: &str) -> String {
    if content_type.contains("jpeg") || content_type.contains("jpg") {
        "jpg".into()
    } else if content_type.contains("webp") {
        "webp".into()
    } else if content_type.contains("gif") {
        "gif".into()
    } else {
        "png".into()
    }
}

async fn process_tool(
    store: &AppStore,
    agent: &AgentDefinition,
    payload: &Value,
) -> AppResult<String> {
    ensure_shell_allowed(agent)?;
    let action = payload
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("list")
        .to_lowercase();
    match action.as_str() {
        "list" => Ok(serde_json::to_string_pretty(&store.managed_processes()?)?),
        "state" | "status" => {
            let process_id = payload
                .get("processId")
                .or_else(|| payload.get("id"))
                .and_then(Value::as_str)
                .ok_or_else(|| AppError::BadRequest("process state requires processId".into()))?;
            Ok(serde_json::to_string_pretty(
                &store.managed_process_state(process_id)?,
            )?)
        }
        "stop" => {
            let process_id = payload
                .get("processId")
                .or_else(|| payload.get("id"))
                .and_then(Value::as_str)
                .ok_or_else(|| AppError::BadRequest("process stop requires processId".into()))?;
            let forget = payload
                .get("forget")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            Ok(serde_json::to_string_pretty(
                &store.stop_managed_process(process_id, forget)?,
            )?)
        }
        "start" | "run" => start_managed_process(store, agent, payload).await,
        other => Err(AppError::BadRequest(format!(
            "unsupported process action: {other}"
        ))),
    }
}

async fn start_managed_process(
    store: &AppStore,
    agent: &AgentDefinition,
    payload: &Value,
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
    let mut child = shell_command(command);
    child
        .current_dir(&cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = child.spawn()?;
    let stdout_lines = Arc::new(Mutex::new(Vec::new()));
    let stderr_lines = Arc::new(Mutex::new(Vec::new()));
    if let Some(stdout) = child.stdout.take() {
        spawn_output_collector(stdout, stdout_lines.clone());
    }
    if let Some(stderr) = child.stderr.take() {
        spawn_output_collector(stderr, stderr_lines.clone());
    }
    let process = ManagedProcess {
        id: new_id("proc"),
        label,
        command: command.to_string(),
        cwd: Some(cwd.display().to_string()),
        pid: child.id(),
        started_at: now_iso(),
        stdout: stdout_lines,
        stderr: stderr_lines,
        child,
    };
    Ok(serde_json::to_string_pretty(
        &store.register_managed_process(process)?,
    )?)
}

fn spawn_output_collector<R>(stream: R, lines: Arc<Mutex<Vec<String>>>)
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut reader = BufReader::new(stream).lines();
        while let Ok(Some(line)) = reader.next_line().await {
            let Ok(mut lines) = lines.lock() else {
                break;
            };
            lines.push(line);
            let overflow = lines.len().saturating_sub(200);
            if overflow > 0 {
                lines.drain(0..overflow);
            }
        }
    });
}

async fn run_shell_command(command: &str, cwd: &Path, timeout_seconds: u64) -> AppResult<String> {
    let mut child = shell_command(command);
    child
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let child = child.spawn()?;
    let output = match tokio::time::timeout(Duration::from_secs(timeout_seconds), child.wait_with_output()).await {
        Ok(output) => output?,
        Err(_) => {
            return Err(AppError::BadRequest(format!(
                "command timed out after {timeout_seconds}s"
            )))
        }
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    Ok(format!(
        "cwd: {}\nexitCode: {}\nstdout:\n{}\nstderr:\n{}",
        cwd.display(),
        output.status.code().unwrap_or(-1),
        truncate_output(&stdout, 50_000),
        truncate_output(&stderr, 20_000)
    ))
}

fn shell_command(command: &str) -> Command {
    #[cfg(windows)]
    {
        let mut child = Command::new("powershell.exe");
        child.args(["-NoProfile", "-Command", command]);
        child
    }
    #[cfg(not(windows))]
    {
        let mut child = Command::new("sh");
        child.args(["-lc", command]);
        child
    }
}

fn ensure_shell_allowed(agent: &AgentDefinition) -> AppResult<()> {
    if agent.allow_shell {
        Ok(())
    } else {
        Err(AppError::BadRequest(
            "agent.allowShell is false; command execution tools are disabled".into(),
        ))
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

fn truncate_output(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        text.to_string()
    } else {
        let mut truncated = text.chars().take(max_chars).collect::<String>();
        truncated.push_str("\n[truncated]");
        truncated
    }
}

async fn browser_navigate_tool(
    store: &AppStore,
    agent: &AgentDefinition,
    run_id: &str,
    payload: &Value,
) -> AppResult<String> {
    let url = payload
        .get("url")
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::BadRequest("browser_navigate requires payload.url".into()))?;
    validate_web_url(url)?;
    remember_browser_url(agent, url)?;
    let html = fetch_url_text(url).await?;
    let supervisor = if let Some(cdp_url) = payload.get("cdpUrl").or_else(|| payload.get("cdp_url")).and_then(Value::as_str) {
        Some(register_browser_supervisor(store, run_id, payload, cdp_url)?)
    } else {
        None
    };
    let supervisor_text = supervisor
        .map(|value| format!("\nsupervisor:\n{}", serde_json::to_string_pretty(&value).unwrap_or_default()))
        .unwrap_or_default();
    Ok(format!(
        "navigated: {url}{supervisor_text}\n\n{}",
        build_browser_snapshot(url, &html, false)
    ))
}

async fn browser_snapshot_tool(agent: &AgentDefinition, payload: &Value) -> AppResult<String> {
    let url = payload
        .get("url")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| last_browser_url(agent).ok().flatten())
        .ok_or_else(|| {
            AppError::BadRequest(
                "browser_snapshot requires payload.url until a page has been navigated".into(),
            )
        })?;
    validate_web_url(&url)?;
    remember_browser_url(agent, &url)?;
    let full = payload
        .get("full")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let html = fetch_url_text(&url).await?;
    Ok(build_browser_snapshot(&url, &html, full))
}

async fn web_search_tool(store: &AppStore, payload: &Value) -> AppResult<String> {
    let query = payload
        .get("query")
        .or_else(|| payload.get("q"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| AppError::BadRequest("web_search requires payload.query".into()))?;
    let limit = payload
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(5)
        .clamp(1, 20) as usize;
    let provider = store
        .enabled_search_provider()?
        .ok_or_else(|| AppError::BadRequest("no enabled search provider configured".into()))?;
    match provider.provider_type.trim().to_lowercase().as_str() {
        "searxng" | "searx" | "" => searxng_search(&provider, query, limit, payload).await,
        other => Err(AppError::BadRequest(format!(
            "unsupported search provider type: {other}"
        ))),
    }
}

async fn searxng_search(
    provider: &SearchProvider,
    query: &str,
    limit: usize,
    payload: &Value,
) -> AppResult<String> {
    let mut url = reqwest::Url::parse(provider.base_url.trim())
        .map_err(|error| AppError::BadRequest(format!("invalid search provider URL: {error}")))?;
    if !url.path().ends_with("/search") {
        let mut path = url.path().trim_end_matches('/').to_string();
        path.push_str("/search");
        url.set_path(&path);
    }
    {
        let mut query_pairs = url.query_pairs_mut();
        query_pairs.append_pair("q", query);
        query_pairs.append_pair("format", "json");
        if let Some(language) = payload.get("language").and_then(Value::as_str) {
            if !language.trim().is_empty() {
                query_pairs.append_pair("language", language.trim());
            }
        }
        if let Some(categories) = payload.get("categories").and_then(Value::as_str) {
            if !categories.trim().is_empty() {
                query_pairs.append_pair("categories", categories.trim());
            }
        }
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(provider.timeout_seconds.max(1)))
        .user_agent("SynthChat-agent/1.0")
        .build()
        .map_err(|error| AppError::BadRequest(format!("failed to build search client: {error}")))?;
    let response = client
        .get(url.clone())
        .send()
        .await
        .map_err(|error| AppError::BadRequest(format!("web_search failed: {error}")))?;
    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|error| AppError::BadRequest(format!("failed to read search response: {error}")))?;
    if !status.is_success() {
        return Err(AppError::BadRequest(format!(
            "web_search returned HTTP {}: {}",
            status.as_u16(),
            truncate_output(&text, 2000)
        )));
    }
    let value = serde_json::from_str::<Value>(&text)
        .map_err(|error| AppError::BadRequest(format!("invalid search JSON: {error}")))?;
    Ok(serde_json::to_string_pretty(&normalize_search_results(
        provider, query, limit, url, value,
    ))?)
}

fn normalize_search_results(
    provider: &SearchProvider,
    query: &str,
    limit: usize,
    url: reqwest::Url,
    value: Value,
) -> Value {
    let results = value
        .get("results")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .take(limit)
                .map(|item| {
                    json!({
                        "title": item.get("title").and_then(Value::as_str).unwrap_or_default(),
                        "url": item.get("url").and_then(Value::as_str).unwrap_or_default(),
                        "content": truncate_for_prompt(item.get("content").or_else(|| item.get("snippet")).and_then(Value::as_str).unwrap_or_default(), 1200),
                        "engine": item.get("engine").or_else(|| item.get("engines")).cloned().unwrap_or(Value::Null),
                        "score": item.get("score").cloned().unwrap_or(Value::Null)
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    json!({
        "providerId": provider.id,
        "providerType": provider.provider_type,
        "query": query,
        "requestUrl": url.to_string(),
        "count": results.len(),
        "results": results,
    })
}

async fn web_request_tool(payload: &Value) -> AppResult<String> {
    let url = payload
        .get("url")
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::BadRequest("web_request requires payload.url".into()))?;
    validate_web_url(url)?;
    let method = payload
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("GET")
        .to_uppercase();
    let method = reqwest::Method::from_bytes(method.as_bytes())
        .map_err(|error| AppError::BadRequest(format!("invalid HTTP method: {error}")))?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .user_agent("SynthChat-agent/1.0")
        .build()
        .map_err(|error| AppError::BadRequest(format!("failed to build HTTP client: {error}")))?;
    let mut request = client.request(method, url);
    if let Some(headers) = payload.get("headers").and_then(Value::as_object) {
        for (key, value) in headers {
            if let Some(value) = value.as_str() {
                request = request.header(key, value);
            }
        }
    }
    if let Some(body) = payload.get("body").filter(|value| !value.is_null()) {
        request = if let Some(text) = body.as_str() {
            request.body(text.to_string())
        } else {
            request.json(body)
        };
    }
    let response = request
        .send()
        .await
        .map_err(|error| AppError::BadRequest(format!("web_request failed: {error}")))?;
    let status = response.status();
    let final_url = response.url().to_string();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_string();
    let text = response
        .text()
        .await
        .map_err(|error| AppError::BadRequest(format!("failed to read response body: {error}")))?;
    Ok(format!(
        "status: {}\nurl: {}\ncontentType: {}\nbody:\n{}",
        status.as_u16(),
        final_url,
        content_type,
        truncate_output(&text, 80_000)
    ))
}

async fn fetch_url_text(url: &str) -> AppResult<String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .user_agent("SynthChat-agent/1.0")
        .build()
        .map_err(|error| AppError::BadRequest(format!("failed to build HTTP client: {error}")))?;
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|error| AppError::BadRequest(format!("browser fetch failed: {error}")))?;
    response
        .text()
        .await
        .map_err(|error| AppError::BadRequest(format!("failed to read page body: {error}")))
}

fn build_browser_snapshot(url: &str, html: &str, full: bool) -> String {
    let title = extract_title(html).unwrap_or_else(|| "(untitled)".into());
    let forms = extract_forms(html, 12);
    let inputs = extract_simple_elements(html, "input", 40, &["type", "name", "id", "placeholder", "value"]);
    let buttons = extract_button_like_elements(html, 30);
    let links = extract_links(html, 40);
    let request_clues = extract_simple_elements(
        html,
        "script",
        30,
        &["src", "type", "crossorigin", "integrity"],
    )
    .into_iter()
    .chain(extract_simple_elements(html, "img", 30, &["src", "alt", "loading"]))
    .chain(extract_simple_elements(html, "link", 30, &["href", "rel", "as"]))
    .collect::<Vec<_>>();
    let mut sections = vec![
        format!("url: {url}"),
        format!("title: {title}"),
        format!("forms:\n{}", format_list(forms)),
        format!("inputs:\n{}", format_list(inputs)),
        format!("buttons:\n{}", format_list(buttons)),
        format!("links:\n{}", format_list(links)),
        format!("requestClues:\n{}", format_list(request_clues)),
    ];
    if full {
        sections.push(format!(
            "textPreview:\n{}",
            truncate_output(&visible_text_preview(html), 20_000)
        ));
    }
    sections.join("\n\n")
}

fn append_supervisor_snapshot(
    store: &AppStore,
    run_id: &str,
    payload: &Value,
    snapshot: String,
) -> AppResult<String> {
    let requested_run_id = payload
        .get("runId")
        .or_else(|| payload.get("run_id"))
        .and_then(Value::as_str)
        .unwrap_or(run_id);
    let Some(state) = store.browser_supervisor_state(requested_run_id)? else {
        return Ok(snapshot);
    };
    let summary = json!({
        "runId": requested_run_id,
        "sessionId": state.get("sessionId").cloned().unwrap_or(Value::Null),
        "providerType": state.get("providerType").cloned().unwrap_or(Value::Null),
        "supervisorTask": state.get("supervisorTask").cloned().unwrap_or(Value::Null),
        "pendingDialogs": state.get("pendingDialogs").cloned().unwrap_or_else(|| json!([])),
        "frameTree": state.get("frameTree").cloned().unwrap_or(Value::Null),
        "requestLog": tail_json_array(state.get("requestLog"), 20),
        "recentEvents": tail_json_array(state.get("recentEvents"), 20),
        "lastDialogResult": state.get("lastDialogResult").cloned().unwrap_or(Value::Null),
        "updatedAt": state.get("updatedAt").cloned().unwrap_or(Value::Null)
    });
    Ok(format!(
        "{snapshot}\n\nsupervisorState:\n{}",
        serde_json::to_string_pretty(&summary)?
    ))
}

fn tail_json_array(value: Option<&Value>, limit: usize) -> Value {
    let items = value
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let skip = items.len().saturating_sub(limit);
    json!(items.into_iter().skip(skip).collect::<Vec<_>>())
}

fn remember_browser_url(agent: &AgentDefinition, url: &str) -> AppResult<()> {
    let state = LAST_BROWSER_URLS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut state = state
        .lock()
        .map_err(|_| AppError::BadRequest("browser state lock poisoned".into()))?;
    state.insert(agent.id.clone(), url.to_string());
    Ok(())
}

fn last_browser_url(agent: &AgentDefinition) -> AppResult<Option<String>> {
    let state = LAST_BROWSER_URLS.get_or_init(|| Mutex::new(HashMap::new()));
    let state = state
        .lock()
        .map_err(|_| AppError::BadRequest("browser state lock poisoned".into()))?;
    Ok(state.get(&agent.id).cloned())
}

fn validate_web_url(url: &str) -> AppResult<()> {
    let parsed = reqwest::Url::parse(url)
        .map_err(|error| AppError::BadRequest(format!("invalid URL: {error}")))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(AppError::BadRequest(
            "only http/https URLs are supported by recovered browser tools".into(),
        ));
    }
    if let Some(host) = parsed.host_str().map(str::to_lowercase) {
        if host == "169.254.169.254" || host == "metadata.google.internal" {
            return Err(AppError::BadRequest(
                "blocked metadata service URL".into(),
            ));
        }
    }
    Ok(())
}

fn extract_title(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let start = lower.find("<title")?;
    let content_start = lower[start..].find('>')? + start + 1;
    let end = lower[content_start..].find("</title>")? + content_start;
    Some(clean_text(&html[content_start..end]))
}

fn extract_forms(html: &str, limit: usize) -> Vec<String> {
    collect_tag_segments(html, "form", limit)
        .into_iter()
        .enumerate()
        .map(|(index, tag)| {
            format!(
                "@form{} method={} action={} id={} name={}",
                index + 1,
                html_attr(&tag, "method").unwrap_or_else(|| "GET".into()),
                html_attr(&tag, "action").unwrap_or_default(),
                html_attr(&tag, "id").unwrap_or_default(),
                html_attr(&tag, "name").unwrap_or_default()
            )
        })
        .collect()
}

fn extract_simple_elements(
    html: &str,
    tag: &str,
    limit: usize,
    attrs: &[&str],
) -> Vec<String> {
    collect_tag_segments(html, tag, limit)
        .into_iter()
        .enumerate()
        .map(|(index, segment)| {
            let fields = attrs
                .iter()
                .filter_map(|attr| html_attr(&segment, attr).map(|value| format!("{attr}={value}")))
                .collect::<Vec<_>>();
            format!("@{}{} {}", tag, index + 1, fields.join(" "))
        })
        .collect()
}

fn extract_button_like_elements(html: &str, limit: usize) -> Vec<String> {
    let mut buttons = extract_simple_elements(html, "button", limit, &["type", "name", "id", "aria-label"]);
    let remaining = limit.saturating_sub(buttons.len());
    if remaining > 0 {
        buttons.extend(
            collect_tag_segments(html, "input", remaining)
                .into_iter()
                .filter(|segment| {
                    html_attr(segment, "type")
                        .map(|value| matches!(value.to_lowercase().as_str(), "button" | "submit" | "reset"))
                        .unwrap_or(false)
                })
                .enumerate()
                .map(|(index, segment)| {
                    format!(
                        "@inputButton{} type={} value={} name={} id={}",
                        index + 1,
                        html_attr(&segment, "type").unwrap_or_default(),
                        html_attr(&segment, "value").unwrap_or_default(),
                        html_attr(&segment, "name").unwrap_or_default(),
                        html_attr(&segment, "id").unwrap_or_default()
                    )
                }),
        );
    }
    buttons
}

fn extract_links(html: &str, limit: usize) -> Vec<String> {
    let lower = html.to_ascii_lowercase();
    let mut cursor = 0usize;
    let mut links = Vec::new();
    while links.len() < limit {
        let Some(start_rel) = lower[cursor..].find("<a") else {
            break;
        };
        let start = cursor + start_rel;
        let Some(tag_end_rel) = lower[start..].find('>') else {
            break;
        };
        let tag_end = start + tag_end_rel;
        let segment = &html[start..=tag_end];
        let href = html_attr(segment, "href").unwrap_or_default();
        let text = lower[tag_end + 1..]
            .find("</a>")
            .map(|end_rel| clean_text(&html[tag_end + 1..tag_end + 1 + end_rel]))
            .unwrap_or_default();
        links.push(format!("@link{} href={} text={}", links.len() + 1, href, text));
        cursor = tag_end + 1;
    }
    links
}

fn collect_tag_segments(html: &str, tag: &str, limit: usize) -> Vec<String> {
    let lower = html.to_ascii_lowercase();
    let needle = format!("<{tag}");
    let mut cursor = 0usize;
    let mut result = Vec::new();
    while result.len() < limit {
        let Some(start_rel) = lower[cursor..].find(&needle) else {
            break;
        };
        let start = cursor + start_rel;
        let Some(end_rel) = lower[start..].find('>') else {
            break;
        };
        let end = start + end_rel;
        result.push(html[start..=end].to_string());
        cursor = end + 1;
    }
    result
}

fn html_attr(segment: &str, attr: &str) -> Option<String> {
    let lower = segment.to_ascii_lowercase();
    let key = format!("{}=", attr.to_ascii_lowercase());
    let pos = lower.find(&key)? + key.len();
    let rest = &segment[pos..];
    let mut chars = rest.chars();
    let first = chars.next()?;
    let value = if first == '"' || first == '\'' {
        let quote = first;
        chars.take_while(|ch| *ch != quote).collect::<String>()
    } else {
        std::iter::once(first)
            .chain(chars.take_while(|ch| !ch.is_whitespace() && *ch != '>'))
            .collect::<String>()
    };
    Some(clean_text(&value))
}

fn visible_text_preview(html: &str) -> String {
    clean_text(&strip_tags(html))
}

fn strip_tags(html: &str) -> String {
    let mut text = String::new();
    let mut in_tag = false;
    for ch in html.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                text.push(' ');
            }
            _ if !in_tag => text.push(ch),
            _ => {}
        }
    }
    text
}

fn clean_text(text: &str) -> String {
    text.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn format_list(items: Vec<String>) -> String {
    if items.is_empty() {
        "(none)".into()
    } else {
        items.join("\n")
    }
}

fn search_recursive(
    root: &Path,
    dir: &Path,
    query: &str,
    target: &str,
    limit: usize,
    max_files: usize,
    checked: &mut usize,
    matches: &mut Vec<String>,
) -> AppResult<()> {
    if matches.len() >= limit || *checked >= max_files {
        return Ok(());
    }
    for entry in fs::read_dir(dir)? {
        if matches.len() >= limit || *checked >= max_files {
            break;
        }
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if path.is_dir() {
            if should_skip_dir(&name) {
                continue;
            }
            search_recursive(root, &path, query, target, limit, max_files, checked, matches)?;
            continue;
        }
        *checked += 1;
        let rel = path.strip_prefix(root).unwrap_or(&path).display().to_string();
        if target == "files" || target == "path" || target == "files_only" {
            if rel.to_lowercase().contains(&query.to_lowercase()) {
                matches.push(rel);
            }
            continue;
        }
        if likely_binary(&path) {
            continue;
        }
        let Ok(content) = fs::read_to_string(&path) else {
            continue;
        };
        if let Some(line) = find_line(&content, query) {
            matches.push(format!("{rel}: {line}"));
        }
    }
    Ok(())
}

fn workspace_root(agent: &AgentDefinition) -> AppResult<PathBuf> {
    let root = if agent.workspace_dir.trim().is_empty() {
        std::env::current_dir()?
    } else {
        PathBuf::from(agent.workspace_dir.trim())
    };
    Ok(root.canonicalize()?)
}

fn resolve_workspace_path(root: &Path, input: &str) -> AppResult<PathBuf> {
    let candidate = {
        let path = PathBuf::from(input);
        if path.is_absolute() {
            path
        } else {
            root.join(path)
        }
    };
    let canonical = candidate.canonicalize()?;
    if canonical.starts_with(root) {
        Ok(canonical)
    } else {
        Err(AppError::BadRequest(format!(
            "path is outside workspace: {}",
            candidate.display()
        )))
    }
}

fn resolve_workspace_target_path(root: &Path, input: &str) -> AppResult<PathBuf> {
    let candidate = {
        let path = PathBuf::from(input);
        if path.is_absolute() {
            path
        } else {
            root.join(path)
        }
    };
    if candidate.exists() {
        return resolve_workspace_path(root, input);
    }
    let mut existing_ancestor = candidate.as_path();
    while !existing_ancestor.exists() {
        existing_ancestor = existing_ancestor
            .parent()
            .ok_or_else(|| AppError::BadRequest(format!("path has no existing ancestor: {input}")))?;
    }
    let ancestor_canonical = existing_ancestor.canonicalize()?;
    if !ancestor_canonical.starts_with(root) {
        return Err(AppError::BadRequest(format!(
            "path is outside workspace: {}",
            candidate.display()
        )));
    }
    Ok(candidate)
}

fn should_skip_dir(name: &str) -> bool {
    matches!(
        name,
        ".git" | "node_modules" | "target" | "dist" | "build" | ".next" | ".venv" | "__pycache__"
    )
}

fn likely_binary(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|ext| ext.to_str()).unwrap_or("").to_lowercase().as_str(),
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "ico" | "pdf" | "zip" | "7z" | "rar" | "exe"
            | "dll" | "pdb" | "rlib" | "rmeta" | "bin" | "wasm"
    )
}

fn find_line(content: &str, query: &str) -> Option<String> {
    let query_lower = query.to_lowercase();
    content.lines().enumerate().find_map(|(index, line)| {
        if line.to_lowercase().contains(&query_lower) {
            Some(format!("line {}: {}", index + 1, line.trim()))
        } else {
            None
        }
    })
}

fn summarize_tool_text(text: &str) -> String {
    let compact = text.lines().take(3).collect::<Vec<_>>().join(" ");
    if compact.chars().count() > 160 {
        compact.chars().take(160).collect()
    } else if compact.is_empty() {
        "tool completed".into()
    } else {
        compact
    }
}

pub async fn call_mcp_tool_with_retry(
    store: &AppStore,
    server_id: String,
    tool_name: String,
    payload: Value,
    timeout_seconds: Option<u64>,
    retry_count: usize,
    retry_backoff_ms: usize,
) -> AppResult<McpCallResult> {
    let mut last = None;
    for attempt in 0..=retry_count {
        match mcp::call_tool(
            store,
            server_id.clone(),
            tool_name.clone(),
            payload.clone(),
            timeout_seconds,
        )
        .await
        {
            Ok(result) if result.ok || attempt == retry_count => return Ok(result),
            Ok(result) => last = Some(result),
            Err(error) if attempt == retry_count => return Err(error),
            Err(error) => {
                last = Some(McpCallResult {
                    ok: false,
                    timed_out: false,
                    elapsed_ms: 0,
                    stdout: String::new(),
                    stderr: error.to_string(),
                    error: Some(error.to_string()),
                });
            }
        }
        tokio::time::sleep(Duration::from_millis(retry_backoff_ms as u64)).await;
    }
    Ok(last.unwrap_or(McpCallResult {
        ok: false,
        timed_out: false,
        elapsed_ms: 0,
        stdout: String::new(),
        stderr: String::new(),
        error: Some("tool call retry loop ended without a result".into()),
    }))
}

async fn continue_agent_run_after_approval(
    store: &AppStore,
    approval: &ToolApprovalRequest,
) -> AppResult<()> {
    let run_id = approval
        .run_id
        .as_deref()
        .ok_or_else(|| AppError::BadRequest("approved tool call missing runId".into()))?;
    let mut run = store.agent_run(run_id)?;
    let conversation = store.conversation(&run.conversation_id)?;
    let persona = store.persona(Some(&run.persona_id))?;
    let agent = store.agent(Some(&run.agent_id))?;
    let provider = store
        .provider(Some(&persona.llm_provider))
        .or_else(|_| store.provider(None))?;
    let history = store.messages(&run.conversation_id, Some(30))?;
    let skill_blocks = crate::skills::prompt_blocks_for_request(store, &agent, &run.user_request)?;
    let memory_blocks = memory_prompt_blocks(store, &persona)?;
    let short_context = store.short_context(&run.conversation_id)?;
    let mcp_tools = available_mcp_tool_definitions(store, &agent)?;
    let mut observations = vec![approved_tool_observation(approval)];
    let mut assistant_text = String::new();

    run.state = "running".into();
    run.completed_at = None;
    run.updated_at = now_iso();
    store.save_agent_run(run.clone())?;

    for iteration in 0..agent.max_tool_iterations.max(1).min(8) {
        let planner_prompt = agent_planner_prompt(
            &observations,
            &skill_blocks,
            &memory_blocks,
            &short_context,
            &mcp_tools,
        );
        let reply = crate::llm::complete_chat(
            &provider,
            &persona,
            planner_prompt.clone(),
            history.clone(),
            &run.user_request,
        )
        .await?;
        let decision = parse_agent_decision(&reply.content);
        append_planner_trace(
            store,
            &run.run_id,
            &run.conversation_id,
            &persona.id,
            &agent.id,
            iteration + 1,
            &planner_prompt,
            &reply.content,
            &decision,
        )?;

        match decision
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("final")
        {
            "tool" => {
                let tool_name = decision.get("tool").and_then(Value::as_str).unwrap_or("");
                let payload = decision
                    .get("payload")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                if is_internal_tool(tool_name) {
                    if let Some(reason) = tool_approval_reason(
                        store,
                        "__internal",
                        tool_name,
                        &payload,
                        is_risky_tool_call(tool_name, &payload),
                    )? {
                        append_tool_approval_request(
                            store,
                            &run.conversation_id,
                            &persona.id,
                            &agent.id,
                            &run.run_id,
                            "__internal",
                            tool_name,
                            payload,
                            reason,
                        )?;
                        mark_run_pending_approval(store, &run.run_id)?;
                        append_waiting_for_approval_message(
                            store,
                            &run.conversation_id,
                            "__internal",
                            tool_name,
                        )?;
                        return Ok(());
                    }
                    match execute_recovery_internal_tool(
                        store,
                        &agent,
                        &run.conversation_id,
                        &run.run_id,
                        tool_name,
                        payload,
                    )
                    .await
                    {
                        Ok((text, event)) => {
                            observations.push(format!(
                                "Continuation iteration {} tool {} result:\n{}",
                                iteration + 1,
                                tool_name,
                                text
                            ));
                            record_tool_event_for_run(
                                store,
                                &run.conversation_id,
                                &run.run_id,
                                event,
                            )?;
                        }
                        Err(error) => observations.push(format!(
                            "Continuation iteration {} tool {} error: {}",
                            iteration + 1,
                            tool_name,
                            error
                        )),
                    }
                } else if let Some(definition) = resolve_mcp_tool(&mcp_tools, tool_name) {
                    if let Some(reason) = tool_approval_reason(
                        store,
                        &definition.server_id,
                        &definition.tool_name,
                        &payload,
                        definition.requires_approval,
                    )? {
                        append_tool_approval_request(
                            store,
                            &run.conversation_id,
                            &persona.id,
                            &agent.id,
                            &run.run_id,
                            &definition.server_id,
                            &definition.tool_name,
                            payload,
                            reason,
                        )?;
                        mark_run_pending_approval(store, &run.run_id)?;
                        append_waiting_for_approval_message(
                            store,
                            &run.conversation_id,
                            &definition.server_id,
                            &definition.tool_name,
                        )?;
                        return Ok(());
                    }
                    match execute_recovery_mcp_tool(store, &run.run_id, &definition, payload).await
                    {
                        Ok((text, event)) => {
                            observations.push(format!(
                                "Continuation iteration {} tool {} result:\n{}",
                                iteration + 1,
                                tool_name,
                                text
                            ));
                            record_tool_event_for_run(
                                store,
                                &run.conversation_id,
                                &run.run_id,
                                event,
                            )?;
                        }
                        Err(error) => observations.push(format!(
                            "Continuation iteration {} tool {} error: {}",
                            iteration + 1,
                            tool_name,
                            error
                        )),
                    }
                } else {
                    observations.push(format!(
                        "Continuation iteration {} tool {} error: tool is not available",
                        iteration + 1,
                        tool_name
                    ));
                }
            }
            _ => {
                assistant_text = decision
                    .get("content")
                    .or_else(|| decision.get("answer"))
                    .and_then(Value::as_str)
                    .unwrap_or(reply.content.trim())
                    .to_string();
                break;
            }
        }
    }

    if assistant_text.trim().is_empty() {
        assistant_text = format!(
            "审批后的工具调用已执行，但当前恢复版 agent loop 未得到最终回答。\n\n{}",
            observations.join("\n\n")
        );
    }
    store.append_message(ChatMessage::new(
        conversation.id,
        "assistant",
        assistant_text,
        "desktop-agent",
    ))?;
    let mut final_run = store.agent_run(&run.run_id)?;
    final_run.state = "completed".into();
    final_run.updated_at = now_iso();
    final_run.completed_at = Some(final_run.updated_at.clone());
    store.save_agent_run(final_run)?;
    Ok(())
}

fn approved_tool_observation(approval: &ToolApprovalRequest) -> String {
    let result = approval.result.as_ref();
    let stdout = result
        .and_then(|value| value.get("stdout"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let stderr = result
        .and_then(|value| value.get("stderr"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let error = result
        .and_then(|value| value.get("error"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let text = if !stdout.trim().is_empty() {
        stdout
    } else if !stderr.trim().is_empty() {
        stderr
    } else {
        error
    };
    format!(
        "Approved tool {}.{} result:\n{}",
        approval.server_id, approval.tool_name, text
    )
}

fn mark_run_pending_approval(store: &AppStore, run_id: &str) -> AppResult<()> {
    let mut run = store.agent_run(run_id)?;
    run.state = "pendingApproval".into();
    run.updated_at = now_iso();
    store.save_agent_run(run)?;
    Ok(())
}

fn append_waiting_for_approval_message(
    store: &AppStore,
    conversation_id: &str,
    server_id: &str,
    tool_name: &str,
) -> AppResult<ChatMessage> {
    store.append_message(ChatMessage::new(
        conversation_id.to_string(),
        "assistant",
        format!("下一步工具调用正在等待审批：{} · {}", server_id, tool_name),
        "desktop-agent",
    ))
}

pub async fn approve_tool_call_and_resume(
    store: &AppStore,
    approval_id: String,
    timeout_seconds: Option<u64>,
    _app: Option<&AppHandle>,
) -> AppResult<ToolApprovalRequest> {
    let approval = approve_tool_call_common(store, approval_id, timeout_seconds).await?;
    if approval.status == "approved" {
        continue_agent_run_after_approval(store, &approval).await?;
    }
    Ok(approval)
}

pub async fn approve_tool_call_always_and_resume(
    store: &AppStore,
    approval_id: String,
    timeout_seconds: Option<u64>,
    app: Option<&AppHandle>,
) -> AppResult<ToolApprovalRequest> {
    let approval = store.tool_approval(&approval_id)?;
    store.trust_tool_pattern(format!("{}.{}", approval.server_id, approval.tool_name))?;
    approve_tool_call_and_resume(store, approval_id, timeout_seconds, app).await
}

pub async fn approve_tool_call_server_and_resume(
    store: &AppStore,
    approval_id: String,
    timeout_seconds: Option<u64>,
    app: Option<&AppHandle>,
) -> AppResult<ToolApprovalRequest> {
    let approval = store.tool_approval(&approval_id)?;
    store.trust_tool_pattern(format!("{}.*", approval.server_id))?;
    approve_tool_call_and_resume(store, approval_id, timeout_seconds, app).await
}

async fn approve_tool_call_common(
    store: &AppStore,
    approval_id: String,
    timeout_seconds: Option<u64>,
) -> AppResult<ToolApprovalRequest> {
    let approval = store.tool_approval(&approval_id)?;
    let result = if approval.server_id == "__internal" {
        let agent_id = approval
            .agent_id
            .as_deref()
            .ok_or_else(|| AppError::BadRequest("internal approval missing agentId".into()))?;
        let conversation_id = approval.conversation_id.as_deref().ok_or_else(|| {
            AppError::BadRequest("internal approval missing conversationId".into())
        })?;
        let run_id = approval
            .run_id
            .as_deref()
            .ok_or_else(|| AppError::BadRequest("internal approval missing runId".into()))?;
        let agent = store.agent(Some(agent_id))?;
        match execute_recovery_internal_tool(
            store,
            &agent,
            conversation_id,
            run_id,
            &approval.tool_name,
            approval.payload.clone(),
        )
        .await
        {
            Ok((stdout, event)) => {
                record_tool_event_for_run(store, conversation_id, run_id, event)?;
                McpCallResult {
                    ok: true,
                    timed_out: false,
                    elapsed_ms: 0,
                    stdout,
                    stderr: String::new(),
                    error: None,
                }
            }
            Err(error) => McpCallResult {
                ok: false,
                timed_out: false,
                elapsed_ms: 0,
                stdout: String::new(),
                stderr: error.to_string(),
                error: Some(error.to_string()),
            },
        }
    } else {
        let result = call_mcp_tool_with_retry(
            store,
            approval.server_id.clone(),
            approval.tool_name.clone(),
            approval.payload.clone(),
            timeout_seconds,
            0,
            0,
        )
        .await?;
        if let (Some(conversation_id), Some(run_id)) =
            (approval.conversation_id.as_deref(), approval.run_id.as_deref())
        {
            let definition = ToolDefinition {
                name: format!("{}.{}", approval.server_id, approval.tool_name),
                display_name: approval.tool_name.clone(),
                description: String::new(),
                source: "mcp".into(),
                server_id: approval.server_id.clone(),
                tool_name: approval.tool_name.clone(),
                input_schema: json!({"type": "object"}),
                requires_approval: false,
            };
            let event = mcp_result_to_tool_event(run_id, &definition, &result);
            record_tool_event_for_run(store, conversation_id, run_id, event)?;
        }
        result
    };
    store.update_tool_approval(
        &approval_id,
        if result.ok { "approved" } else { "failed" },
        Some(json!(result)),
        result.error.clone(),
    )
}

pub fn deny_tool_call_and_update_run(
    store: &AppStore,
    approval_id: String,
    reason: Option<String>,
    _app: Option<&AppHandle>,
) -> AppResult<ToolApprovalRequest> {
    store.update_tool_approval(&approval_id, "denied", None, reason)
}

pub fn list_agent_control_commands() -> Vec<AgentControlCommandView> {
    vec![AgentControlCommandView {
        name: "doctor".into(),
        aliases: vec!["status".into()],
        description: "Show recovery baseline status.".into(),
        category: "Info".into(),
    }]
}

pub fn spawn_background_chat_turn_for_job(
    _app: AppHandle,
    _conversation_id: String,
    _persona_id: String,
    _prompt: String,
    _job: Option<crate::models::ScheduledAgentJob>,
) {
}

pub fn export_agent_run_bundle(store: &AppStore, run_id: String) -> AppResult<String> {
    let run = store.agent_run(&run_id)?;
    let child_runs = store
        .agent_runs()?
        .into_iter()
        .filter(|item| item.parent_run_id.as_deref() == Some(&run_id))
        .collect::<Vec<_>>();
    let planner_traces = store
        .planner_traces()?
        .into_iter()
        .filter(|trace| trace.run_id == run_id)
        .collect::<Vec<_>>();
    let tool_traces = store
        .tool_traces()?
        .into_iter()
        .filter(|trace| trace.event.run_id.as_deref() == Some(&run_id))
        .collect::<Vec<_>>();
    let approvals = store
        .tool_approvals()?
        .into_iter()
        .filter(|approval| approval.run_id.as_deref() == Some(&run_id))
        .collect::<Vec<_>>();
    let artifacts = store.tool_artifacts_for_run(&run_id)?;
    let todos = store.agent_todos_for_run(&run_id)?;
    Ok(serde_json::to_string_pretty(&json!({
        "run": run,
        "childRuns": child_runs,
        "artifacts": artifacts,
        "todos": todos,
        "plannerTraces": planner_traces,
        "toolTraces": tool_traces,
        "approvals": approvals,
        "recoveryBaseline": true
    }))?)
}

pub fn list_agent_run_artifacts(store: &AppStore, run_id: String) -> AppResult<Vec<Value>> {
    store.tool_artifacts_for_run(&run_id)
}

pub async fn drain_all_agent_queues(
    store: &AppStore,
    app: Option<&AppHandle>,
) -> AppResult<Vec<crate::models::AgentQueuedRequest>> {
    let mut drained = Vec::new();
    while let Some(item) = store.claim_next_agent_request("")? {
        let request = SendChatRequest {
            conversation_id: Some(item.conversation_id.clone()),
            persona_id: Some(item.persona_id.clone()),
            content: item.content.clone(),
        };
        let status = match run_chat_turn(store, request, app).await {
            Ok(_) => "completed",
            Err(_) => "failed",
        };
        store.complete_agent_queue_item(&item.id, status, None)?;
        let mut completed = item;
        completed.status = status.into();
        completed.completed_at = Some(now_iso());
        drained.push(completed);
    }
    Ok(drained)
}

pub async fn resume_agent_run(
    store: &AppStore,
    run_id: String,
    _checkpoint_id: Option<String>,
    _app: Option<&AppHandle>,
) -> AppResult<AgentRunRecord> {
    store.agent_run(&run_id)
}

pub async fn rerun_agent_run(
    store: &AppStore,
    run_id: String,
    app: Option<&AppHandle>,
) -> AppResult<Vec<ChatMessage>> {
    let run = store.agent_run(&run_id)?;
    run_chat_turn(
        store,
        SendChatRequest {
            conversation_id: Some(run.conversation_id),
            persona_id: Some(run.persona_id),
            content: run.user_request,
        },
        app,
    )
    .await
}

pub async fn diagnose_agent_run(
    store: &AppStore,
    run_id: String,
    _app: Option<&AppHandle>,
) -> AppResult<ChatMessage> {
    let run = store.agent_run(&run_id)?;
    let content = format!(
        "Run {} state={} error={}",
        run.run_id,
        run.state,
        run.error.as_deref().unwrap_or("")
    );
    Ok(store.append_message(ChatMessage::new(
        run.conversation_id,
        "assistant",
        content,
        "desktop-agent-recovery",
    ))?)
}

pub fn abort_agent_run(
    store: &AppStore,
    run_id: String,
    reason: Option<String>,
    _app: Option<&AppHandle>,
) -> AppResult<AgentRunRecord> {
    store.abort_agent_run(&run_id, reason)
}

pub fn recovery_agent_error() -> AppError {
    AppError::BadRequest("agent runtime recovery baseline is active".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_short_context() -> ShortContextState {
        ShortContextState {
            conversation_id: "conv".into(),
            boundary_id: None,
            summary: String::new(),
            summary_tokens: 0,
            summary_messages: 0,
        }
    }

    #[test]
    fn risky_tool_classifier_allows_read_only_browser_and_file_tools() {
        assert!(!is_risky_tool_call("read_file", &json!({"path": "src/main.rs"})));
        assert!(!is_risky_tool_call(
            "browser_snapshot",
            &json!({"url": "https://example.com"})
        ));
        assert!(!is_risky_tool_call(
            "web_request",
            &json!({"url": "https://example.com", "method": "GET"})
        ));
    }

    #[test]
    fn risky_tool_classifier_flags_mutating_or_executing_tools() {
        assert!(is_risky_tool_call("patch", &json!({"path": "src/main.rs"})));
        assert!(is_risky_tool_call(
            "terminal",
            &json!({"command": "cargo check"})
        ));
        assert!(is_risky_tool_call(
            "process",
            &json!({"action": "start", "command": "npm run dev"})
        ));
        assert!(is_risky_tool_call(
            "web_request",
            &json!({"url": "https://example.com", "method": "POST"})
        ));
    }

    #[test]
    fn trusted_tool_patterns_match_exact_server_and_global_rules() {
        assert!(trusted_tool_patterns_match(
            &["__internal.read_file".into()],
            "__internal",
            "read_file"
        ));
        assert!(trusted_tool_patterns_match(
            &["browser.*".into()],
            "browser",
            "snapshot"
        ));
        assert!(trusted_tool_patterns_match(
            &["*".into()],
            "__internal",
            "terminal"
        ));
        assert!(!trusted_tool_patterns_match(
            &["__internal.read_file".into()],
            "__internal",
            "terminal"
        ));
    }

    #[test]
    fn planner_prompt_includes_enabled_skill_blocks() {
        let skill = SkillPromptBlock {
            id: "local/test-skill".into(),
            name: "Test Skill".into(),
            content: "Use the test workflow before answering.".into(),
        };
        let prompt = agent_planner_prompt(&[], &[skill], &[], &empty_short_context(), &[]);
        assert!(prompt.contains("Skill: Test Skill (local/test-skill)"));
        assert!(prompt.contains("Use the test workflow before answering."));
    }

    #[test]
    fn planner_prompt_handles_empty_skill_blocks() {
        let prompt = agent_planner_prompt(&[], &[], &[], &empty_short_context(), &[]);
        assert!(prompt.contains("No enabled or explicitly requested skill instructions."));
    }

    #[test]
    fn planner_prompt_includes_memory_blocks() {
        let memory = MemoryEntry {
            id: "mem-test".into(),
            persona_id: "persona".into(),
            summary: "The user prefers concise implementation summaries.".into(),
            importance: 5,
            created_at: now_iso(),
            updated_at: now_iso(),
        };
        let prompt = agent_planner_prompt(&[], &[], &[memory], &empty_short_context(), &[]);
        assert!(prompt.contains("Relevant memory"));
        assert!(prompt.contains("concise implementation summaries"));
    }

    #[test]
    fn planner_prompt_includes_short_context_summary() {
        let short_context = ShortContextState {
            conversation_id: "conv".into(),
            boundary_id: Some("msg-boundary".into()),
            summary: "Earlier discussion established the backend failure mode.".into(),
            summary_tokens: 12,
            summary_messages: 4,
        };
        let prompt = agent_planner_prompt(&[], &[], &[], &short_context, &[]);
        assert!(prompt.contains("Conversation summary"));
        assert!(prompt.contains("msg-boundary"));
        assert!(prompt.contains("backend failure mode"));
    }

    #[test]
    fn planner_prompt_exposes_delegate_task_tool() {
        let prompt = agent_planner_prompt(&[], &[], &[], &empty_short_context(), &[]);
        assert!(prompt.contains("delegate_task"));
        assert!(is_internal_tool("delegate_task"));
    }

    #[test]
    fn planner_prompt_exposes_web_search_tool() {
        let prompt = agent_planner_prompt(&[], &[], &[], &empty_short_context(), &[]);
        assert!(prompt.contains("web_search"));
        assert!(is_internal_tool("web_search"));
    }

    #[test]
    fn planner_prompt_exposes_image_generate_tool() {
        let prompt = agent_planner_prompt(&[], &[], &[], &empty_short_context(), &[]);
        assert!(prompt.contains("image_generate"));
        assert!(is_internal_tool("image_generate"));
    }

    #[test]
    fn planner_prompt_lists_mcp_tools() {
        let tool = ToolDefinition {
            name: "browser.snapshot".into(),
            display_name: "snapshot".into(),
            description: "Inspect the current page".into(),
            source: "mcp".into(),
            server_id: "browser".into(),
            tool_name: "snapshot".into(),
            input_schema: json!({"type": "object", "properties": {"url": {"type": "string"}}}),
            requires_approval: false,
        };
        let prompt = agent_planner_prompt(&[], &[], &[], &empty_short_context(), &[tool]);
        assert!(prompt.contains("browser.snapshot"));
        assert!(prompt.contains("Inspect the current page"));
    }

    #[test]
    fn resolve_mcp_tool_accepts_registered_names() {
        let tool = ToolDefinition {
            name: "browser.snapshot".into(),
            display_name: "snapshot".into(),
            description: String::new(),
            source: "mcp".into(),
            server_id: "browser".into(),
            tool_name: "snapshot".into(),
            input_schema: json!({"type": "object"}),
            requires_approval: false,
        };
        assert_eq!(
            resolve_mcp_tool(&[tool.clone()], "browser.snapshot")
                .map(|tool| tool.tool_name),
            Some("snapshot".into())
        );
        assert_eq!(
            resolve_mcp_tool(&[tool], "snapshot").map(|tool| tool.server_id),
            Some("browser".into())
        );
    }

    #[test]
    fn normalize_search_results_limits_and_shapes_results() {
        let provider = SearchProvider {
            id: "search".into(),
            name: "SearXNG".into(),
            provider_type: "searxng".into(),
            base_url: "http://localhost:8080".into(),
            enabled: true,
            timeout_seconds: 10,
        };
        let url = reqwest::Url::parse("http://localhost:8080/search?q=test&format=json").unwrap();
        let normalized = normalize_search_results(
            &provider,
            "test",
            1,
            url,
            json!({
                "results": [
                    {"title": "One", "url": "https://one.example", "content": "first"},
                    {"title": "Two", "url": "https://two.example", "content": "second"}
                ]
            }),
        );
        assert_eq!(normalized["count"], 1);
        assert_eq!(normalized["results"][0]["title"], "One");
    }

    #[test]
    fn image_helpers_decode_base64_and_detect_extensions() {
        let bytes = decode_base64_image("iVBORw0KGgo=").unwrap();
        assert!(!bytes.is_empty());
        assert_eq!(image_extension_from_content_type("image/jpeg"), "jpg");
        assert_eq!(image_extension_from_content_type("image/webp"), "webp");
        assert_eq!(image_extension_from_content_type("application/octet-stream"), "png");
    }

    #[test]
    fn approved_tool_observation_prefers_stdout() {
        let approval = ToolApprovalRequest {
            id: "approval-test".into(),
            created_at: now_iso(),
            updated_at: now_iso(),
            status: "approved".into(),
            conversation_id: Some("conv".into()),
            persona_id: Some("persona".into()),
            agent_id: Some("agent".into()),
            run_id: Some("run".into()),
            server_id: "browser".into(),
            tool_name: "snapshot".into(),
            payload: json!({}),
            reason: "test".into(),
            result: Some(json!({
                "ok": true,
                "timedOut": false,
                "elapsedMs": 1,
                "stdout": "page text",
                "stderr": "ignored",
                "error": null
            })),
            error: None,
        };
        let observation = approved_tool_observation(&approval);
        assert!(observation.contains("browser.snapshot"));
        assert!(observation.contains("page text"));
        assert!(!observation.contains("ignored"));
    }

    #[test]
    fn planner_step_summary_marks_tool_decisions() {
        let decision = json!({"action": "tool", "tool": "browser_snapshot"});
        assert_eq!(summarize_planner_step(&decision), "tool:browser_snapshot");
        assert!(planner_decision_error(&decision).is_none());
    }

    #[test]
    fn planner_decision_error_marks_missing_tool_name() {
        let decision = json!({"action": "tool", "payload": {}});
        assert_eq!(summarize_planner_step(&decision), "tool:<missing tool>");
        assert_eq!(
            planner_decision_error(&decision),
            Some("tool action missing tool name".into())
        );
    }

    #[test]
    fn exported_run_bundle_includes_child_runs_and_artifacts() {
        let dir = std::env::temp_dir().join(format!("synthchat-agent-export-{}", new_id("test")));
        let path = dir.join("state.json");
        let store = AppStore::new(path).unwrap();
        let mut parent = AgentRunRecord::new("conv".into(), "persona".into(), "agent".into());
        parent.user_request = "parent task".into();
        store.save_agent_run(parent.clone()).unwrap();

        let mut child = AgentRunRecord::new("conv".into(), "persona".into(), "agent".into());
        child.parent_run_id = Some(parent.run_id.clone());
        child.subagent_index = Some(1);
        store.save_agent_run(child).unwrap();
        store
            .save_tool_artifact(&parent.run_id, "notes", "artifact text")
            .unwrap();

        let bundle = export_agent_run_bundle(&store, parent.run_id.clone()).unwrap();
        let value: Value = serde_json::from_str(&bundle).unwrap();
        assert_eq!(value["run"]["runId"], parent.run_id);
        assert_eq!(value["childRuns"].as_array().unwrap().len(), 1);
        assert_eq!(value["artifacts"].as_array().unwrap().len(), 1);
        assert_eq!(
            list_agent_run_artifacts(&store, parent.run_id)
                .unwrap()
                .len(),
            1
        );

        let _ = fs::remove_dir_all(dir);
    }
}
