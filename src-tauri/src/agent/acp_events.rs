use serde_json::{json, Value};

use crate::models::{tool_event_kind as model_tool_event_kind, ChatMessage};

use super::{
    acp_history::acp_history_plan_update_from_todo_result, acp_tool_output::acp_format_tool_output,
    redact_json_value,
};

pub(super) fn acp_tool_event_notifications(session_id: &str, events: &[Value]) -> Vec<Value> {
    let mut notifications = Vec::new();
    for event in events {
        if let Some(update) = acp_tool_event_update_from_value(event) {
            notifications.push(acp_session_update_notification(session_id, update));
        }
        if let Some(plan) = acp_tool_event_plan_update(event) {
            notifications.push(acp_session_update_notification(session_id, plan));
        }
    }
    notifications
}

pub(super) fn acp_agent_message_notification(session_id: &str, text: &str) -> Value {
    acp_session_update_notification(
        session_id,
        json!({
            "sessionUpdate": "agent_message_chunk",
            "content": {
                "type": "text",
                "text": text
            }
        }),
    )
}

pub(super) fn acp_user_message_notification(session_id: &str, text: &str) -> Value {
    acp_session_update_notification(
        session_id,
        json!({
            "sessionUpdate": "user_message_chunk",
            "content": {
                "type": "text",
                "text": text
            }
        }),
    )
}

pub(super) fn acp_final_agent_message_notifications(
    session_id: &str,
    messages: Vec<ChatMessage>,
    streamed_text: Option<&str>,
) -> Vec<Value> {
    let streamed_text = streamed_text
        .map(str::trim)
        .filter(|value| !value.is_empty());
    messages
        .into_iter()
        .filter(|message| message.role == "assistant" && !message.content.trim().is_empty())
        .filter(|message| {
            streamed_text
                .map(|streamed| message.content.trim() != streamed)
                .unwrap_or(true)
        })
        .map(|message| acp_agent_message_notification(session_id, &message.content))
        .collect()
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

fn acp_tool_event_plan_update(event: &Value) -> Option<Value> {
    let tool_name = acp_string_text(event, &["toolName", "tool_name", "name", "tool"]);
    if !matches!(tool_name.as_str(), "todo" | "update_todo") {
        return None;
    }
    if acp_tool_event_status(event, None, None) == "running" {
        return None;
    }
    let result_text = acp_tool_event_raw_output_text(event)?;
    acp_history_plan_update_from_todo_result(&result_text)
}

pub(super) fn acp_tool_event_update_from_value(event: &Value) -> Option<Value> {
    let tool_name = acp_string_text(event, &["toolName", "tool_name", "name", "tool"]);
    let server_id = acp_string_text(event, &["serverId", "server_id"]);
    let raw_input = event
        .get("raw")
        .and_then(|raw| raw.get("payload"))
        .cloned()
        .unwrap_or_else(|| json!({}));
    let mut title = acp_string_text(event, &["title"]);
    if title.is_empty()
        || title == tool_name
        || (!tool_name.is_empty() && title.ends_with(&format!(" · {tool_name}")))
    {
        title = acp_tool_event_title(&tool_name, &raw_input);
    }
    let mut call_id = acp_string_text(
        event,
        &[
            "callId",
            "call_id",
            "toolCallId",
            "tool_call_id",
            "referenceId",
            "reference_id",
        ],
    );
    if call_id.is_empty() {
        call_id = format!("{server_id}.{tool_name}")
            .trim_matches('.')
            .to_string();
    }
    if call_id.is_empty() {
        return None;
    }
    let raw_output_text = acp_tool_event_raw_output_text(event);
    let output_text = acp_tool_event_output_text(event);
    let status = acp_tool_event_status(event, raw_output_text.as_deref(), output_text.as_deref());
    let session_update = if status == "running" {
        "tool_call"
    } else {
        "tool_call_update"
    };
    let kind = acp_tool_event_kind_from_event(event, &server_id, &tool_name);
    let mut update = json!({
        "sessionUpdate": session_update,
        "toolCallId": call_id,
        "title": title,
        "kind": kind,
        "status": status
    });
    if session_update == "tool_call" {
        if acp_tool_start_includes_raw_input(&tool_name) {
            update["rawInput"] = raw_input.clone();
        }
        if let Some(content) = acp_tool_start_content_items(&tool_name, &raw_input) {
            update["content"] = content;
        }
    }
    if session_update == "tool_call_update" {
        update["rawInput"] = raw_input.clone();
        if let Some(text) = output_text {
            if !text.trim().is_empty() {
                if acp_tool_complete_includes_raw_output(&tool_name, raw_output_text.as_deref()) {
                    update["rawOutput"] = Value::String(text.clone());
                }
                update["content"] = acp_text_content_items(&text);
            }
        }
    }
    Some(redact_json_value(update))
}

pub(super) fn acp_text_content_items(text: &str) -> Value {
    json!([{
        "type": "content",
        "content": {
            "type": "text",
            "text": text
        }
    }])
}

fn acp_tool_start_includes_raw_input(tool_name: &str) -> bool {
    !matches!(
        tool_name,
        "read_file"
            | "web_extract"
            | "browser_navigate"
            | "search_files"
            | "todo"
            | "update_todo"
            | "skill_view"
            | "execute_code"
            | "skill_manage"
    )
}

fn acp_tool_complete_includes_raw_output(tool_name: &str, raw_output_text: Option<&str>) -> bool {
    let is_polished_tool = matches!(
        tool_name,
        "todo"
            | "update_todo"
            | "memory"
            | "session_search"
            | "delegate_task"
            | "read_file"
            | "write_file"
            | "patch"
            | "search_files"
            | "terminal"
            | "process"
            | "execute_code"
            | "skill_view"
            | "skills_list"
            | "skill_manage"
            | "recall_memory"
            | "manage_memory"
            | "web_search"
            | "x_search"
            | "web_extract"
            | "browser_navigate"
            | "browser_click"
            | "browser_type"
            | "browser_press"
            | "browser_scroll"
            | "browser_back"
            | "browser_snapshot"
            | "browser_console"
            | "browser_get_images"
            | "browser_vision"
            | "vision_analyze"
            | "image_generate"
            | "text_to_speech"
            | "voice_status"
            | "voice_playback"
            | "voice_recording"
            | "cronjob"
            | "send_message"
            | "clarify"
            | "discord"
            | "discord_admin"
            | "ha_list_entities"
            | "ha_get_state"
            | "ha_list_services"
            | "ha_call_service"
            | "feishu_doc_read"
            | "feishu_drive_list_comments"
            | "feishu_drive_list_comment_replies"
            | "feishu_drive_update_comment_reaction"
            | "feishu_drive_reply_comment"
            | "feishu_drive_add_comment"
            | "kanban_create"
            | "kanban_specify"
            | "kanban_show"
            | "kanban_update"
            | "kanban_delete"
            | "kanban_comment"
            | "kanban_complete"
            | "kanban_block"
            | "kanban_link"
            | "kanban_unlink"
            | "kanban_bulk_update"
            | "kanban_heartbeat"
            | "yb_query_group_info"
            | "yb_query_group_members"
            | "yb_search_sticker"
            | "yb_send_dm"
            | "yb_send_sticker"
            | "mixture_of_agents"
    );
    if is_polished_tool {
        return false;
    }
    !matches!(
        raw_output_text
            .and_then(acp_parse_tool_json_output)
            .map(|(value, _)| value),
        Some(Value::Object(_)) | Some(Value::Array(_))
    )
}

pub(super) fn acp_tool_diff_content_items(
    path: &str,
    old_text: Option<&str>,
    new_text: &str,
) -> Value {
    let mut item = json!({
        "type": "diff",
        "path": path,
        "oldText": old_text,
        "newText": new_text
    });
    if let Some(old_text) = old_text {
        item["old_text"] = Value::String(old_text.to_string());
    }
    item["new_text"] = Value::String(new_text.to_string());
    json!([item])
}

fn acp_tool_event_kind_from_event(event: &Value, server_id: &str, tool_name: &str) -> String {
    let event_kind = acp_string_text(event, &["kind"]);
    if matches!(
        event_kind.as_str(),
        "read" | "edit" | "execute" | "fetch" | "search" | "think" | "other"
    ) {
        return event_kind;
    }
    acp_tool_event_kind(server_id, tool_name)
}

pub(super) fn acp_tool_event_kind(server_id: &str, tool_name: &str) -> String {
    match tool_name {
        "read_file"
        | "browser_snapshot"
        | "browser_console"
        | "browser_vision"
        | "browser_get_images"
        | "skill_view"
        | "skills_list"
        | "vision_analyze"
        | "video_analyze"
        | "voice_status"
        | "recall_memory"
        | "feishu_doc_read"
        | "feishu_drive_list_comments"
        | "feishu_drive_list_comment_replies"
        | "yb_query_group_info"
        | "yb_query_group_members"
        | "ha_list_entities"
        | "ha_get_state"
        | "ha_list_services"
        | "__mcp_read_resource"
        | "__mcp_get_prompt" => "read".into(),
        "patch"
        | "write_file"
        | "delete_file"
        | "move_file"
        | "skill_manage"
        | "manage_memory"
        | "send_message"
        | "cronjob"
        | "feishu_drive_update_comment_reaction"
        | "feishu_drive_reply_comment"
        | "feishu_drive_add_comment" => "edit".into(),
        "terminal" | "process" | "execute_code" | "computer_use" | "browser_click"
        | "browser_press" | "browser_type" | "browser_scroll" | "browser_back" | "browser_cdp"
        | "browser_dialog" | "delegate_task" | "image_generate" | "video_generate"
        | "text_to_speech" | "voice_playback" | "voice_recording" | "yb_send_dm"
        | "yb_search_sticker" | "yb_send_sticker" | "ha_call_service" | "mixture_of_agents" => {
            "execute".into()
        }
        "web_request" | "web_extract" | "web_search" | "x_search" | "browser_navigate" => {
            "fetch".into()
        }
        "search_files" => "search".into(),
        "_thinking" => "think".into(),
        "memory" | "session_search" => "other".into(),
        _ => model_tool_event_kind(server_id, tool_name, None),
    }
}

pub(super) fn acp_tool_event_title(tool_name: &str, raw_input: &Value) -> String {
    match tool_name {
        "terminal" => acp_tool_title_with_value("terminal", raw_input, &["command"]),
        "process" => {
            let command = acp_string_text(raw_input, &["command"]);
            if !command.is_empty() {
                acp_tool_title_with_value("process", raw_input, &["command"])
            } else {
                let action = acp_string_text(raw_input, &["action"]);
                let action = if action.is_empty() {
                    "process"
                } else {
                    &action
                };
                let session_id = acp_string_text(
                    raw_input,
                    &["session_id", "sessionId", "processId", "process_id", "id"],
                );
                if session_id.is_empty() {
                    format!("process {action}")
                } else {
                    format!("process {action}: {session_id}")
                }
            }
        }
        "execute_code" => {
            let code = acp_string_text(raw_input, &["code"]);
            let first = code
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .unwrap_or("");
            if first.is_empty() {
                "python".into()
            } else {
                format!("python: {}", acp_truncate_title(first, 96))
            }
        }
        "read_file" => acp_tool_title_with_value("read", raw_input, &["path"]),
        "patch" => acp_tool_title_with_value("patch", raw_input, &["path", "file", "target"]),
        "write_file" => acp_tool_title_with_value("write", raw_input, &["path", "file"]),
        "search_files" => acp_tool_title_with_value("search", raw_input, &["pattern", "query"]),
        "session_search" => {
            let query = acp_string_text(raw_input, &["query"]);
            if !query.is_empty() {
                format!("session search: {}", acp_truncate_title(&query, 80))
            } else {
                let mode = acp_string_text(raw_input, &["mode", "kind", "action"]);
                if mode.is_empty() {
                    "session search".into()
                } else {
                    format!("session search ({mode})")
                }
            }
        }
        "memory" | "recall_memory" | "manage_memory" => {
            let action = acp_string_text(raw_input, &["action"]);
            let target = acp_string_text(raw_input, &["target", "scope"]);
            let query = acp_string_text(raw_input, &["query", "summary", "content", "text"]);
            let action = if action.is_empty() {
                match tool_name {
                    "recall_memory" => "search",
                    "manage_memory" => "manage",
                    _ => "memory",
                }
            } else {
                action.as_str()
            };
            let target = if !target.is_empty() {
                target
            } else if !query.is_empty() {
                acp_truncate_title(&query, 64)
            } else {
                "memory".into()
            };
            format!("memory {action}: {target}")
        }
        "web_search" | "x_search" => acp_tool_title_with_value("search", raw_input, &["query"]),
        "web_extract" => acp_tool_title_with_value("extract", raw_input, &["url", "urls"]),
        "browser_navigate" => acp_tool_title_with_value("navigate", raw_input, &["url"]),
        "browser_snapshot" => "browser snapshot".into(),
        "browser_vision" => acp_tool_title_with_value("browser vision", raw_input, &["question"]),
        "browser_get_images" => "browser images".into(),
        "vision_analyze" => {
            acp_tool_title_with_value("analyze image", raw_input, &["question", "prompt"])
        }
        "video_analyze" => {
            acp_tool_title_with_value("analyze video", raw_input, &["question", "prompt"])
        }
        "image_generate" => {
            acp_tool_title_with_value("generate image", raw_input, &["prompt", "description"])
        }
        "video_generate" => {
            acp_tool_title_with_value("generate video", raw_input, &["prompt", "description"])
        }
        "text_to_speech" => acp_tool_title_with_value("speak", raw_input, &["text"]),
        "cronjob" => {
            let action = acp_string_text(raw_input, &["action"]);
            let action = if action.is_empty() { "manage" } else { &action };
            let job_id = acp_string_text(raw_input, &["jobId", "job_id", "id", "name"]);
            if job_id.is_empty() {
                format!("cron {action}")
            } else {
                format!("cron {action}: {}", acp_truncate_title(&job_id, 64))
            }
        }
        "send_message" => {
            let action = acp_string_text(raw_input, &["action"]);
            let target =
                acp_string_text(raw_input, &["target", "conversationId", "conversation_id"]);
            if !target.is_empty() {
                format!("send message: {}", acp_truncate_title(&target, 64))
            } else if action == "list" || action == "targets" {
                "send message targets".into()
            } else {
                "send message".into()
            }
        }
        "clarify" => acp_tool_title_with_value("clarify", raw_input, &["question"]),
        "skill_view" => {
            let name = acp_string_text(raw_input, &["name", "skill"]);
            let file = acp_string_text(raw_input, &["filePath", "file_path", "path"]);
            if !name.is_empty() && !file.is_empty() {
                format!("skill view ({name}/{file})")
            } else if !name.is_empty() {
                format!("skill view ({name})")
            } else {
                "skill view".into()
            }
        }
        "skills_list" => {
            let category = acp_string_text(raw_input, &["category"]);
            if category.is_empty() {
                "skills list".into()
            } else {
                format!("skills list ({category})")
            }
        }
        "skill_manage" => {
            let action = acp_string_text(raw_input, &["action"]);
            let action = if action.is_empty() { "manage" } else { &action };
            let name = acp_string_text(raw_input, &["name", "skill"]);
            let file = acp_string_text(raw_input, &["filePath", "file_path", "path"]);
            let target = if !name.is_empty() && !file.is_empty() {
                format!("{name}/{file}")
            } else if !name.is_empty() {
                name
            } else {
                "?".into()
            };
            format!("skill {action}: {}", acp_truncate_title(&target, 64))
        }
        "delegate_task" => {
            let task_count = raw_input
                .get("tasks")
                .and_then(Value::as_array)
                .map(|items| items.len())
                .unwrap_or(0);
            if task_count > 0 {
                format!("delegate batch ({task_count} tasks)")
            } else {
                let goal = acp_string_text(raw_input, &["goal", "task"]);
                if goal.is_empty() {
                    "delegate task".into()
                } else {
                    format!("delegate: {}", acp_truncate_title(&goal, 60))
                }
            }
        }
        "todo" => {
            let count = raw_input
                .get("todos")
                .and_then(Value::as_array)
                .map(|items| items.len())
                .unwrap_or(0);
            if count == 1 {
                "todo (1 item)".into()
            } else if count > 1 {
                format!("todo ({count} items)")
            } else {
                "todo".into()
            }
        }
        _ => {
            if tool_name.is_empty() {
                "tool".into()
            } else {
                tool_name.to_string()
            }
        }
    }
}

pub(super) fn acp_tool_start_content_items(tool_name: &str, raw_input: &Value) -> Option<Value> {
    match tool_name {
        "terminal" => {
            let command = acp_string_text(raw_input, &["command"]);
            (!command.is_empty())
                .then(|| acp_text_content_items(&format!("```shell\n{command}\n```")))
        }
        "process" => {
            let command = acp_string_text(raw_input, &["command"]);
            let action = acp_string_text(raw_input, &["action"]);
            let session_id = acp_string_text(
                raw_input,
                &["session_id", "sessionId", "processId", "process_id", "id"],
            );
            if !command.is_empty() {
                Some(acp_text_content_items(&format!(
                    "Process {action}:\n```shell\n{command}\n```"
                )))
            } else if !action.is_empty() {
                let mut lines = vec![format!("Process action: {action}")];
                if !session_id.is_empty() {
                    lines.push(format!("Session: {session_id}"));
                }
                Some(acp_text_content_items(&lines.join("\n")))
            } else {
                None
            }
        }
        "execute_code" => {
            let code = acp_string_text(raw_input, &["code"]);
            (!code.is_empty()).then(|| acp_text_content_items(&format!("```python\n{code}\n```")))
        }
        "patch" | "write_file" => {
            let path = acp_string_text(raw_input, &["path", "file", "target"]);
            if let Some(diff) = acp_explicit_edit_diff_content(raw_input) {
                Some(diff)
            } else if !path.is_empty() {
                Some(acp_text_content_items(&format!(
                    "Approval prompt shows the diff for {path}"
                )))
            } else {
                None
            }
        }
        "search_files" => {
            let pattern = acp_string_text(raw_input, &["pattern", "query"]);
            (!pattern.is_empty())
                .then(|| acp_text_content_items(&format!("Search pattern: {pattern}")))
        }
        "session_search" => {
            let query = acp_string_text(raw_input, &["query"]);
            if !query.is_empty() {
                Some(acp_text_content_items(&format!("Session search: {query}")))
            } else {
                let mode = acp_string_text(raw_input, &["mode", "kind", "action"]);
                (!mode.is_empty())
                    .then(|| acp_text_content_items(&format!("Session search mode: {mode}")))
            }
        }
        "memory" | "recall_memory" | "manage_memory" => {
            let action = acp_string_text(raw_input, &["action"]);
            let target = acp_string_text(raw_input, &["target", "scope"]);
            let query = acp_string_text(raw_input, &["query"]);
            let mut lines = vec![format!(
                "Memory action: {}",
                if action.is_empty() { "memory" } else { &action }
            )];
            if !target.is_empty() {
                lines.push(format!("Target: {target}"));
            }
            if !query.is_empty() {
                lines.push(format!("Query: {query}"));
            }
            Some(acp_text_content_items(&lines.join("\n")))
        }
        "delegate_task" => {
            if let Some(tasks) = raw_input.get("tasks").and_then(Value::as_array) {
                let mut lines = vec![format!(
                    "Delegating {} task{}",
                    tasks.len(),
                    if tasks.len() == 1 { "" } else { "s" }
                )];
                for (idx, task) in tasks.iter().take(6).enumerate() {
                    let goal = acp_string_text(task, &["goal", "task", "content"]);
                    if !goal.is_empty() {
                        lines.push(format!("- {}: {}", idx + 1, acp_truncate_title(&goal, 120)));
                    }
                }
                return Some(acp_text_content_items(&lines.join("\n")));
            }
            let goal = acp_string_text(raw_input, &["goal", "task", "content"]);
            if goal.is_empty() {
                None
            } else {
                Some(acp_text_content_items(&format!(
                    "Delegating task: {}",
                    acp_truncate_title(&goal, 160)
                )))
            }
        }
        "web_search" | "x_search" => {
            let query = acp_string_text(raw_input, &["query"]);
            (!query.is_empty()).then(|| acp_text_content_items(&format!("Search query: {query}")))
        }
        "web_extract" | "read_file" => None,
        "browser_navigate" => {
            let url = acp_string_text(raw_input, &["url"]);
            (!url.is_empty()).then(|| {
                acp_text_content_items(
                    &serde_json::to_string_pretty(&json!({"url": url})).unwrap_or_else(|_| url),
                )
            })
        }
        "vision_analyze" | "video_analyze" => {
            let question = acp_string_text(raw_input, &["question", "prompt"]);
            let source = acp_string_text(raw_input, &["path", "url", "image_url", "videoUrl"]);
            let mut lines = Vec::new();
            if !question.is_empty() {
                lines.push(format!("Question: {}", acp_truncate_title(&question, 500)));
            }
            if !source.is_empty() {
                lines.push(format!("Source: {}", acp_truncate_title(&source, 300)));
            }
            (!lines.is_empty()).then(|| acp_text_content_items(&lines.join("\n")))
        }
        "image_generate" | "video_generate" => {
            let prompt = acp_string_text(raw_input, &["prompt", "description"]);
            (!prompt.is_empty()).then(|| {
                acp_text_content_items(&format!("Prompt: {}", acp_truncate_title(&prompt, 800)))
            })
        }
        "text_to_speech" => {
            let text = acp_string_text(raw_input, &["text"]);
            (!text.is_empty()).then(|| {
                acp_text_content_items(&format!("Text: {}", acp_truncate_title(&text, 800)))
            })
        }
        "cronjob" => {
            let action = acp_string_text(raw_input, &["action"]);
            let job_id = acp_string_text(raw_input, &["jobId", "job_id", "id", "name"]);
            let schedule = acp_string_text(raw_input, &["schedule", "cronExpr", "runAt"]);
            let prompt = acp_string_text(raw_input, &["prompt", "task", "content"]);
            let mut lines = vec![format!(
                "Cron action: {}",
                if action.is_empty() { "manage" } else { &action }
            )];
            if !job_id.is_empty() {
                lines.push(format!("Job: {}", acp_truncate_title(&job_id, 200)));
            }
            if !schedule.is_empty() {
                lines.push(format!("Schedule: {}", acp_truncate_title(&schedule, 200)));
            }
            if !prompt.is_empty() {
                lines.push(format!("Prompt: {}", acp_truncate_title(&prompt, 500)));
            }
            Some(acp_text_content_items(&lines.join("\n")))
        }
        "send_message" => {
            let target =
                acp_string_text(raw_input, &["target", "conversationId", "conversation_id"]);
            let role = acp_string_text(raw_input, &["role"]);
            let message = acp_string_text(raw_input, &["message", "text", "content", "body"]);
            let mut lines = Vec::new();
            if !target.is_empty() {
                lines.push(format!("Target: {}", acp_truncate_title(&target, 200)));
            }
            if !role.is_empty() {
                lines.push(format!("Role: {role}"));
            }
            if !message.is_empty() {
                lines.push(format!("Message: {}", acp_truncate_title(&message, 800)));
            }
            (!lines.is_empty()).then(|| acp_text_content_items(&lines.join("\n")))
        }
        "clarify" => {
            let question = acp_string_text(raw_input, &["question"]);
            let mut lines = Vec::new();
            if !question.is_empty() {
                lines.push(format!("Question: {}", acp_truncate_title(&question, 800)));
            }
            if let Some(choices) = raw_input.get("choices").and_then(Value::as_array) {
                for choice in choices.iter().take(6).filter_map(Value::as_str) {
                    lines.push(format!("- {}", acp_truncate_title(choice, 200)));
                }
            }
            (!lines.is_empty()).then(|| acp_text_content_items(&lines.join("\n")))
        }
        "todo" => {
            let todos = raw_input.get("todos").and_then(Value::as_array)?;
            let lines = todos
                .iter()
                .filter_map(|todo| {
                    let content = acp_string_text(todo, &["content", "text", "title"]);
                    if content.is_empty() {
                        return None;
                    }
                    let status = acp_string_text(todo, &["status"]);
                    Some(if status.is_empty() {
                        format!("- {content}")
                    } else {
                        format!("- [{status}] {content}")
                    })
                })
                .collect::<Vec<_>>();
            (!lines.is_empty()).then(|| acp_text_content_items(&lines.join("\n")))
        }
        "skill_view" => {
            let name = acp_string_text(raw_input, &["name", "skill"]);
            (!name.is_empty()).then(|| acp_text_content_items(&format!("Skill requested: {name}")))
        }
        "skill_manage" => {
            let action = acp_string_text(raw_input, &["action"]);
            let name = acp_string_text(raw_input, &["name", "skill"]);
            let file = acp_string_text(raw_input, &["filePath", "file_path", "path"]);
            let old_text = acp_string_text(raw_input, &["oldString", "old_string", "oldText"]);
            let new_text = acp_string_text(
                raw_input,
                &[
                    "newString",
                    "new_string",
                    "newText",
                    "content",
                    "fileContent",
                    "file_content",
                ],
            );
            if matches!(action.as_str(), "patch" | "write_file") && !new_text.is_empty() {
                let path = if !name.is_empty() && !file.is_empty() {
                    format!("skills/{name}/{file}")
                } else if !name.is_empty() {
                    format!("skills/{name}/SKILL.md")
                } else {
                    "skills/<unknown>".into()
                };
                Some(acp_tool_diff_content_items(
                    &path,
                    (!old_text.is_empty()).then_some(old_text.as_str()),
                    &new_text,
                ))
            } else if !name.is_empty() {
                let target = if file.is_empty() {
                    format!("skill '{name}'")
                } else {
                    format!("skill '{name}' ({file})")
                };
                Some(acp_text_content_items(&format!(
                    "Running skill_manage action '{}' on {target}",
                    if action.is_empty() { "manage" } else { &action }
                )))
            } else {
                None
            }
        }
        _ => {
            if raw_input.is_null()
                || raw_input
                    .as_object()
                    .map(|object| object.is_empty())
                    .unwrap_or(false)
            {
                None
            } else {
                Some(acp_text_content_items(
                    &serde_json::to_string_pretty(raw_input)
                        .unwrap_or_else(|_| raw_input.to_string()),
                ))
            }
        }
    }
}

fn acp_explicit_edit_diff_content(raw_input: &Value) -> Option<Value> {
    let diff = raw_input
        .get("__acpEditDiff")
        .or_else(|| raw_input.get("__acp_edit_diff"))?;
    let path = acp_string_text(diff, &["path", "file", "target"]);
    let path = if path.is_empty() {
        acp_string_text(raw_input, &["path", "file", "target"])
    } else {
        path
    };
    let old_text = acp_string_text(diff, &["oldText", "old_text", "oldString", "old_string"]);
    let new_text = acp_string_text(
        diff,
        &["newText", "new_text", "newString", "new_string", "content"],
    );
    if path.is_empty() || new_text.is_empty() {
        return None;
    }
    Some(acp_tool_diff_content_items(
        &path,
        (!old_text.is_empty()).then_some(old_text.as_str()),
        &new_text,
    ))
}

fn acp_tool_title_with_value(label: &str, raw_input: &Value, keys: &[&str]) -> String {
    let value = keys
        .iter()
        .find_map(|key| raw_input.get(*key))
        .map(|value| {
            if let Some(text) = value.as_str() {
                text.to_string()
            } else if let Some(items) = value.as_array() {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            } else {
                value.to_string()
            }
        })
        .map(|value| acp_truncate_title(value.trim(), 96))
        .filter(|value| !value.is_empty());
    value
        .map(|value| format!("{label}: {value}"))
        .unwrap_or_else(|| label.to_string())
}

fn acp_truncate_title(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let truncated = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        format!("{truncated}...")
    } else {
        truncated
    }
}

pub(super) fn acp_tool_event_status(
    event: &Value,
    raw_output_text: Option<&str>,
    output_text: Option<&str>,
) -> String {
    let status = acp_string_text(event, &["status"]);
    if !status.is_empty() {
        let normalized = acp_normalized_tool_status(&status);
        if normalized == "completed"
            && (acp_tool_output_implies_failure(raw_output_text)
                || acp_tool_output_implies_failure(output_text))
        {
            return "failed".into();
        }
        return normalized;
    }
    if acp_tool_output_implies_failure(raw_output_text)
        || acp_tool_output_implies_failure(output_text)
    {
        return "failed".into();
    }
    if event.get("ok").and_then(Value::as_bool).unwrap_or(false) {
        "completed".into()
    } else if event.get("error").is_some() {
        "failed".into()
    } else {
        "running".into()
    }
}

pub(super) fn acp_normalized_tool_status(status: &str) -> String {
    match status.trim() {
        "cancelled" => "completed".into(),
        value if !value.is_empty() => value.to_string(),
        _ => "completed".into(),
    }
}

pub(super) fn acp_tool_output_implies_failure(output_text: Option<&str>) -> bool {
    let Some(text) = output_text.map(str::trim).filter(|value| !value.is_empty()) else {
        return false;
    };
    if text.starts_with("Error executing tool '") {
        return true;
    }
    let Ok(data) = serde_json::from_str::<Value>(text) else {
        return false;
    };
    data.get("success")
        .or_else(|| data.get("ok"))
        .and_then(Value::as_bool)
        == Some(false)
        || data
            .get("exit_code")
            .or_else(|| data.get("returncode"))
            .and_then(Value::as_i64)
            .is_some_and(|code| code != 0)
}

pub(super) fn acp_parse_tool_json_output(text: &str) -> Option<(Value, Option<String>)> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut stream = serde_json::Deserializer::from_str(trimmed).into_iter::<Value>();
    let value = stream.next()?.ok()?;
    let suffix = trimmed
        .get(stream.byte_offset()..)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string);
    Some((value, suffix))
}

fn acp_tool_event_output_text(event: &Value) -> Option<String> {
    let tool_name = acp_string_text(event, &["toolName", "tool_name", "name", "tool"]);
    let raw_input = event
        .get("raw")
        .and_then(|raw| raw.get("payload"))
        .unwrap_or(&Value::Null);
    for key in ["text", "error", "summary"] {
        if let Some(text) = event
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
        {
            let text = if key == "text" {
                acp_format_tool_output(&tool_name, text, raw_input).unwrap_or_else(|| text.into())
            } else {
                text.to_string()
            };
            if acp_string_text(event, &["status"]) == "cancelled"
                && !text.starts_with("[cancelled]")
            {
                return Some(format!("[cancelled] {text}"));
            }
            return Some(text);
        }
    }
    None
}

pub(super) fn acp_tool_event_raw_output_text(event: &Value) -> Option<String> {
    for key in ["text", "output", "rawOutput", "raw_output", "error"] {
        if let Some(text) = event.get(key).and_then(Value::as_str) {
            return Some(text.to_string());
        }
    }
    None
}

fn acp_string_text(value: &Value, keys: &[&str]) -> String {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .unwrap_or("")
        .to_string()
}
