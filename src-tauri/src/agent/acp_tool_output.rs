use serde_json::Value;

use super::acp_events::acp_parse_tool_json_output;

pub(super) fn acp_format_tool_output(
    tool_name: &str,
    text: &str,
    raw_input: &Value,
) -> Option<String> {
    let (data, suffix) = acp_parse_tool_json_output(text)?;
    let suffix = suffix.as_deref();
    match tool_name {
        "terminal" | "execute_code" => {
            acp_append_tool_output_suffix(acp_format_execute_like_output(&data), suffix)
        }
        "todo" | "update_todo" => {
            acp_append_tool_output_suffix(acp_format_todo_output(&data), suffix)
        }
        "read_file" => {
            acp_append_tool_output_suffix(acp_format_read_file_output(&data, raw_input), suffix)
        }
        "patch" | "write_file" => acp_append_tool_output_suffix(
            acp_format_edit_output(tool_name, &data, raw_input),
            suffix,
        ),
        "search_files" => acp_format_search_files_output(&data, suffix),
        "skill_view" => acp_append_tool_output_suffix(acp_format_skill_view_output(&data), suffix),
        "skills_list" => {
            acp_append_tool_output_suffix(acp_format_skills_list_output(&data), suffix)
        }
        "skill_manage" => {
            acp_append_tool_output_suffix(acp_format_skill_manage_output(&data, raw_input), suffix)
        }
        "clarify" => acp_append_tool_output_suffix(acp_format_clarify_output(&data), suffix),
        "kanban_create" | "kanban_specify" | "kanban_list" | "kanban_show" | "kanban_complete"
        | "kanban_block" | "kanban_unblock" | "kanban_heartbeat" | "kanban_update"
        | "kanban_delete" | "kanban_comment" | "kanban_link" | "kanban_unlink"
        | "kanban_bulk_update" => {
            acp_append_tool_output_suffix(acp_format_kanban_output(tool_name, &data), suffix)
        }
        "ha_list_entities" | "ha_get_state" | "ha_list_services" | "ha_call_service" => {
            acp_append_tool_output_suffix(
                acp_format_integration_output("Home Assistant", tool_name, &data),
                suffix,
            )
        }
        "feishu_doc_read"
        | "feishu_drive_list_comments"
        | "feishu_drive_list_comment_replies"
        | "feishu_drive_update_comment_reaction"
        | "feishu_drive_reply_comment"
        | "feishu_drive_add_comment" => acp_append_tool_output_suffix(
            acp_format_integration_output("Feishu", tool_name, &data),
            suffix,
        ),
        "yb_query_group_info"
        | "yb_query_group_members"
        | "yb_search_sticker"
        | "yb_send_dm"
        | "yb_send_sticker" => acp_append_tool_output_suffix(
            acp_format_integration_output("Yuanbao", tool_name, &data),
            suffix,
        ),
        "discord" | "discord_admin" => acp_append_tool_output_suffix(
            acp_format_integration_output("Discord", tool_name, &data),
            suffix,
        ),
        "delegate_task" | "mixture_of_agents" => {
            acp_append_tool_output_suffix(acp_format_delegate_output(&data), suffix)
        }
        "session_search" => {
            acp_append_tool_output_suffix(acp_format_session_search_output(&data), suffix)
        }
        "memory" => {
            acp_append_tool_output_suffix(acp_format_memory_output(&data, raw_input), suffix)
        }
        "web_extract" => {
            acp_append_tool_output_suffix(acp_format_web_extract_output(&data), suffix)
        }
        "web_search" | "x_search" => {
            acp_append_tool_output_suffix(acp_format_web_search_output(&data), suffix)
        }
        "browser_navigate" | "browser_click" | "browser_type" | "browser_press"
        | "browser_scroll" | "browser_back" | "browser_snapshot" | "browser_console"
        | "browser_get_images" | "browser_vision" => {
            acp_append_tool_output_suffix(acp_format_browser_output(tool_name, &data), suffix)
        }
        "vision_analyze" | "video_analyze" | "image_generate" | "video_generate"
        | "text_to_speech" | "cronjob" | "send_message" => {
            acp_append_tool_output_suffix(acp_format_media_or_cron_output(tool_name, &data), suffix)
        }
        "process" => {
            acp_append_tool_output_suffix(acp_format_process_output(&data, raw_input), suffix)
        }
        _ => {
            acp_append_tool_output_suffix(acp_format_generic_json_output(tool_name, &data), suffix)
        }
    }
}

fn acp_append_tool_output_suffix(output: Option<String>, suffix: Option<&str>) -> Option<String> {
    let suffix = suffix.map(str::trim).filter(|value| !value.is_empty());
    match (output, suffix) {
        (Some(output), Some(suffix)) if !output.trim().is_empty() => {
            Some(format!("{output}\n\n{suffix}"))
        }
        (Some(output), _) => Some(output),
        (None, Some(suffix)) => Some(suffix.to_string()),
        (None, None) => None,
    }
}

fn acp_format_execute_like_output(data: &Value) -> Option<String> {
    let object = data.as_object()?;
    let output = object
        .get("output")
        .or_else(|| object.get("stdout"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim_end();
    let error = object
        .get("error")
        .or_else(|| object.get("stderr"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim_end();
    let exit_code = object
        .get("exit_code")
        .or_else(|| object.get("returncode"))
        .and_then(Value::as_i64);
    if output.is_empty() && error.is_empty() && exit_code.is_none() {
        return None;
    }
    let mut lines = vec![exit_code
        .map(|code| format!("Exit code: {code}"))
        .unwrap_or_else(|| "Execution complete".into())];
    if !output.is_empty() {
        lines.extend([
            "".into(),
            "Output:".into(),
            acp_truncate_output(output, 5000),
        ]);
    }
    if !error.is_empty() {
        lines.extend(["".into(), "Error:".into(), acp_truncate_output(error, 2000)]);
    }
    Some(lines.join("\n"))
}

fn acp_format_todo_output(data: &Value) -> Option<String> {
    let todos = data.get("todos")?.as_array()?;
    let mut lines = vec!["Todo list".to_string(), String::new()];
    for item in todos {
        let content = acp_string_text(item, &["content", "text", "title", "id"]);
        if content.is_empty() {
            continue;
        }
        let status = acp_string_text(item, &["status"]);
        if status.is_empty() {
            lines.push(format!("- {content}"));
        } else {
            lines.push(format!("- [{status}] {content}"));
        }
    }
    if let Some(summary) = data.get("summary").and_then(Value::as_object) {
        lines.extend([
            String::new(),
            format!(
                "Progress: {} completed, {} in progress, {} pending",
                summary
                    .get("completed")
                    .and_then(Value::as_i64)
                    .unwrap_or(0),
                summary
                    .get("in_progress")
                    .and_then(Value::as_i64)
                    .unwrap_or(0),
                summary.get("pending").and_then(Value::as_i64).unwrap_or(0)
            ),
        ]);
    }
    (lines.len() > 2).then(|| lines.join("\n"))
}

fn acp_format_delegate_output(data: &Value) -> Option<String> {
    if data.get("error").is_some() && !data.get("results").is_some_and(Value::is_array) {
        let error = acp_string_text(data, &["error", "message"]);
        return Some(format!(
            "Delegation failed: {}",
            if error.is_empty() {
                "unknown error"
            } else {
                &error
            }
        ));
    }
    let results = data.get("results").and_then(Value::as_array)?;
    let total = data
        .get("total_duration_seconds")
        .or_else(|| data.get("totalDurationSeconds"))
        .and_then(Value::as_f64);
    let mut header = format!(
        "Delegation results: {} task{}",
        results.len(),
        if results.len() == 1 { "" } else { "s" }
    );
    if let Some(total) = total {
        header.push_str(&format!(" in {total:.1}s"));
    }
    let mut lines = vec![header];
    for item in results.iter().take(6) {
        let index = item
            .get("task_index")
            .or_else(|| item.get("taskIndex"))
            .and_then(Value::as_i64)
            .map(|value| value + 1);
        let status = acp_string_text(item, &["status", "state", "stopReason"]);
        let summary = acp_string_text(item, &["summary", "final_response", "result"]);
        let error = acp_string_text(item, &["error"]);
        let model = acp_string_text(item, &["model"]);
        let role = acp_string_text(item, &["_child_role", "role"]);
        let duration = item
            .get("duration_seconds")
            .or_else(|| item.get("durationSeconds"))
            .and_then(Value::as_f64);
        let tools = item
            .get("tool_trace")
            .or_else(|| item.get("toolTrace"))
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|tool| {
                        acp_string_text(tool, &["tool", "name"])
                            .trim()
                            .is_empty()
                            .then_some(None)
                            .flatten()
                            .or_else(|| {
                                let text = acp_string_text(tool, &["tool", "name"]);
                                (!text.is_empty()).then_some(text)
                            })
                    })
                    .take(12)
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        let mut line = format!(
            "- Task {}: {}",
            index
                .map(|value| value.to_string())
                .unwrap_or_else(|| "?".into()),
            if status.is_empty() {
                "completed"
            } else {
                &status
            }
        );
        let mut bits = Vec::new();
        if !model.is_empty() {
            bits.push(model.clone());
        }
        if !role.is_empty() {
            bits.push(format!("role={role}"));
        }
        if let Some(duration) = duration {
            bits.push(format!("{duration:.1}s"));
        }
        if !bits.is_empty() {
            line.push_str(&format!(" ({})", bits.join(", ")));
        }
        if !summary.is_empty() {
            line.push_str(&format!(" - {}", acp_truncate_output(&summary, 1200)));
        }
        lines.push(line);
        if !error.is_empty() {
            lines.push(format!("  Error: {}", acp_truncate_output(&error, 800)));
        }
        if !model.is_empty() {
            lines.push(format!("  Model: {model}"));
        }
        if !tools.is_empty() {
            lines.push(format!("  Tools: {tools}"));
        }
    }
    Some(acp_truncate_output(&lines.join("\n"), 8000))
}

fn acp_format_session_search_output(data: &Value) -> Option<String> {
    if data.get("success").and_then(Value::as_bool) == Some(false) {
        let error = acp_string_text(data, &["error", "message"]);
        return Some(format!(
            "Session search failed: {}",
            if error.is_empty() {
                "unknown error"
            } else {
                &error
            }
        ));
    }
    let results = data.get("results").and_then(Value::as_array)?;
    let mode = acp_string_text(data, &["mode"]);
    let query = acp_string_text(data, &["query"]);
    let title = match mode.as_str() {
        "recent" => "Recent sessions".to_string(),
        "search" if !query.is_empty() => format!("Session search results for `{query}`"),
        "search" => "Session search results".to_string(),
        value if !value.is_empty() => format!("Session {value} results"),
        _ => "Sessions".to_string(),
    };
    let mut lines = vec![title];
    if results.is_empty() {
        let message = acp_string_text(data, &["message"]);
        lines.push(if message.is_empty() {
            "No matching sessions found.".into()
        } else {
            message
        });
        return Some(lines.join("\n"));
    }
    lines.push(format!("Found {} session(s).", results.len()));
    for item in results.iter().take(10) {
        let session_id = acp_string_text(item, &["session_id", "sessionId", "id"]);
        let item_title = acp_string_text(item, &["title", "name", "when"]);
        let preview = acp_string_text(item, &["preview", "snippet", "summary"]);
        let count = item
            .get("message_count")
            .or_else(|| item.get("messageCount"))
            .and_then(Value::as_u64);
        let active = acp_string_text(
            item,
            &[
                "last_active",
                "lastActive",
                "started_at",
                "startedAt",
                "when",
            ],
        );
        let source = acp_string_text(item, &["source"]);
        let label = if item_title.is_empty() {
            "Untitled session".into()
        } else {
            item_title
        };
        let mut line = if session_id.is_empty() {
            format!("- **{label}**")
        } else {
            format!("- **{label}** (`{session_id}`)")
        };
        let mut meta = Vec::new();
        if !active.is_empty() {
            meta.push(active);
        }
        if !source.is_empty() {
            meta.push(source);
        }
        if let Some(count) = count {
            meta.push(format!("{count} msgs"));
        }
        if !meta.is_empty() {
            line.push_str(&format!(" - {}", meta.join(", ")));
        }
        lines.push(line);
        if !preview.is_empty() {
            lines.push(format!("  {}", acp_truncate_output(&preview, 500)));
        }
    }
    Some(acp_truncate_output(&lines.join("\n"), 7000))
}

fn acp_format_memory_output(data: &Value, raw_input: &Value) -> Option<String> {
    let action = acp_string_text(raw_input, &["action"]).to_lowercase();
    let target = acp_string_text(raw_input, &["target", "scope"]);
    let content = acp_string_text(raw_input, &["content", "text", "memory"]);
    let message = acp_string_text(data, &["message", "status"]);
    let usage = acp_string_text(data, &["usage"]);
    let error = acp_string_text(data, &["error"]);
    let entry_count = data
        .get("entry_count")
        .or_else(|| data.get("entryCount"))
        .and_then(Value::as_u64);
    if action.is_empty()
        && target.is_empty()
        && content.is_empty()
        && message.is_empty()
        && entry_count.is_none()
    {
        return None;
    }
    let mut title = "Memory".to_string();
    if !action.is_empty() {
        title.push(' ');
        title.push_str(&action);
    }
    if data.get("success").and_then(Value::as_bool) == Some(false) {
        title.push_str(" failed");
    } else {
        title.push_str(" saved");
    }
    let mut lines = vec![title];
    if !target.is_empty() {
        lines.push(format!("- Target: {target}"));
    }
    if !content.is_empty() {
        lines.push(format!("- Content: {}", acp_truncate_output(&content, 500)));
    }
    if let Some(count) = entry_count {
        lines.push(format!("- Entries: {count}"));
    }
    if !message.is_empty() {
        lines.push(format!("- Result: {message}"));
    }
    if !usage.is_empty() {
        lines.push(format!("- Usage: {usage}"));
    }
    if !error.is_empty() {
        lines.push(format!("- Error: {error}"));
    }
    Some(lines.join("\n"))
}

fn acp_format_web_extract_output(data: &Value) -> Option<String> {
    if data.get("success").and_then(Value::as_bool) == Some(false) {
        let error = acp_string_text(data, &["error", "message"]);
        if !error.is_empty() {
            return Some(format!("Web extract failed: {error}"));
        }
    }
    let results = data.get("results").and_then(Value::as_array)?;
    let failures = results
        .iter()
        .filter_map(|item| {
            let error = acp_string_text(item, &["error"]);
            if error.is_empty() {
                return None;
            }
            let url = acp_string_text(item, &["url", "uri"]);
            Some((url, error))
        })
        .collect::<Vec<_>>();
    if failures.is_empty() {
        return Some(String::new());
    }
    let mut lines = vec![format!("Web extract failed: {} result(s)", failures.len())];
    for (url, error) in failures.iter().take(8) {
        if url.is_empty() {
            lines.push(format!("- {error}"));
        } else {
            lines.push(format!("- {url}: {error}"));
        }
    }
    Some(lines.join("\n"))
}

fn acp_format_read_file_output(data: &Value, raw_input: &Value) -> Option<String> {
    if let Some(error) = data.get("error").and_then(Value::as_str) {
        if data.get("content").is_none() {
            return Some(format!("Read failed: {error}"));
        }
    }
    let content = data.get("content")?.as_str()?;
    let path = acp_string_text(raw_input, &["path", "file"])
        .trim()
        .to_string();
    let path = if path.is_empty() { "file" } else { &path };
    let mut header = format!("Read {path}");
    let mut range_bits = Vec::new();
    if let Some(offset) = raw_input
        .get("offset")
        .or_else(|| raw_input.get("start_line"))
        .or_else(|| raw_input.get("startLine"))
        .and_then(Value::as_i64)
    {
        range_bits.push(format!("from line {offset}"));
    }
    if let Some(limit) = raw_input
        .get("limit")
        .or_else(|| raw_input.get("max_lines"))
        .or_else(|| raw_input.get("maxLines"))
        .and_then(Value::as_i64)
    {
        range_bits.push(format!("limit {limit}"));
    }
    if !range_bits.is_empty() {
        header.push_str(&format!(" ({})", range_bits.join(", ")));
    }
    if let Some(total) = data.get("total_lines").and_then(Value::as_i64) {
        header.push_str(&format!(" ({total} total lines)"));
    }
    Some(acp_truncate_output(
        &format!("{header}\n\n{}", acp_fenced_output(content, "")),
        7000,
    ))
}

fn acp_format_edit_output(tool_name: &str, data: &Value, raw_input: &Value) -> Option<String> {
    let path = acp_string_text(raw_input, &["path", "file", "target"]);
    let path = if path.is_empty() { "file" } else { &path };
    if data.get("success").and_then(Value::as_bool) == Some(false) || data.get("error").is_some() {
        let error = acp_string_text(data, &["error", "message"]);
        return Some(format!(
            "{tool_name} failed for {path}: {}",
            if error.is_empty() {
                "unknown error"
            } else {
                &error
            }
        ));
    }

    let mut lines = vec![format!("{tool_name} completed for {path}")];
    let message = acp_string_text(data, &["message"]);
    if !message.is_empty() {
        lines.push(message);
    }
    if let Some(value) = data
        .get("replacements")
        .or_else(|| data.get("replacement_count"))
        .or_else(|| data.get("replacementCount"))
        .or_else(|| data.get("replacementsApplied"))
        .and_then(acp_json_scalar_text)
    {
        lines.push(format!("Replacements: {value}"));
    }
    if let Some(files) = data.get("files_modified").and_then(Value::as_array) {
        let names = files
            .iter()
            .take(8)
            .filter_map(Value::as_str)
            .collect::<Vec<_>>();
        if !names.is_empty() {
            lines.push(format!("Files: {}", names.join(", ")));
        }
    }
    Some(lines.join("\n"))
}

fn acp_format_search_files_output(data: &Value, suffix: Option<&str>) -> Option<String> {
    if data.get("success").and_then(Value::as_bool) == Some(false) {
        let error = acp_string_text(data, &["error", "message"]);
        return Some(format!(
            "File search failed: {}",
            if error.is_empty() {
                "unknown error"
            } else {
                &error
            }
        ));
    }
    if let Some(files) = data.get("files").and_then(Value::as_array) {
        let total = data
            .get("total_count")
            .and_then(Value::as_u64)
            .unwrap_or(files.len() as u64);
        let mut lines = vec![
            "File search results".to_string(),
            format!("Found {total} file(s); showing {}.", files.len().min(20)),
            String::new(),
        ];
        lines.extend(
            files
                .iter()
                .take(20)
                .filter_map(Value::as_str)
                .map(|path| format!("- {path}")),
        );
        if data.get("truncated").and_then(Value::as_bool) == Some(true) {
            lines.extend([
                String::new(),
                "Results truncated; use offset to page through more files.".into(),
            ]);
        }
        if let Some(suffix) = suffix.map(str::trim).filter(|value| !value.is_empty()) {
            lines.extend([String::new(), suffix.to_string()]);
        }
        return Some(acp_truncate_output(&lines.join("\n"), 7000));
    }
    let matches = data.get("matches")?.as_array()?;
    let total = data
        .get("total_count")
        .and_then(Value::as_u64)
        .unwrap_or(matches.len() as u64);
    let mut lines = vec![
        "Search results".to_string(),
        format!(
            "Found {total} match(es); showing {}.",
            matches.len().min(12)
        ),
        String::new(),
    ];
    for item in matches.iter().take(12) {
        let path = acp_string_text(item, &["path", "file", "filename"]);
        let line = item
            .get("line")
            .or_else(|| item.get("line_number"))
            .map(Value::to_string)
            .unwrap_or_default();
        let content = acp_string_text(item, &["content", "text"]);
        if path.is_empty() {
            continue;
        }
        if line.is_empty() {
            lines.push(format!("- {path}"));
        } else {
            lines.push(format!("- {path}:{line}"));
        }
        if !content.is_empty() {
            lines.push(format!("  {}", acp_truncate_output(&content, 300)));
        }
    }
    if data.get("truncated").and_then(Value::as_bool) == Some(true) {
        lines.extend([
            String::new(),
            "Results truncated; use offset to page through more matches.".into(),
        ]);
    }
    if let Some(suffix) = suffix.map(str::trim).filter(|value| !value.is_empty()) {
        lines.extend([String::new(), suffix.to_string()]);
    }
    Some(acp_truncate_output(&lines.join("\n"), 7000))
}

fn acp_format_skill_view_output(data: &Value) -> Option<String> {
    if data.get("success").and_then(Value::as_bool) == Some(false) {
        let error = acp_string_text(data, &["error"]);
        return Some(format!(
            "Skill view failed: {}",
            if error.is_empty() {
                "unknown error"
            } else {
                &error
            }
        ));
    }
    let name = acp_string_text(data, &["name", "skill"]);
    let file = acp_string_text(data, &["file", "path", "file_path"]);
    let description = acp_string_text(data, &["description"]);
    if name.is_empty() && file.is_empty() && data.get("content").is_none() {
        return None;
    }
    let content = data.get("content").and_then(Value::as_str).unwrap_or("");
    let content_len = content.len();
    let mut lines = vec!["Skill loaded".to_string(), String::new()];
    if !name.is_empty() {
        lines.push(format!("- Name: {name}"));
    }
    if !file.is_empty() {
        lines.push(format!("- File: {file}"));
    }
    if !description.is_empty() {
        lines.push(format!("- Description: {description}"));
    }
    if content_len > 0 {
        lines.push(format!(
            "- Content: {content_len} chars loaded into agent context"
        ));
    }
    if let Some(linked) = data.get("linked_files").and_then(Value::as_object) {
        let linked_count = linked
            .values()
            .filter_map(Value::as_array)
            .map(|items| items.len())
            .sum::<usize>();
        if linked_count > 0 {
            lines.push(format!("- Linked files: {linked_count}"));
        }
    }
    let headings = acp_markdown_headings(content, 8);
    if !headings.is_empty() {
        lines.extend([String::new(), "Sections".into()]);
        lines.extend(headings.into_iter().map(|heading| format!("- {heading}")));
    }
    lines.extend([
        String::new(),
        "Full skill content is available to the agent but hidden here to keep ACP readable.".into(),
    ]);
    Some(lines.join("\n"))
}

fn acp_markdown_headings(content: &str, limit: usize) -> Vec<String> {
    let mut headings = Vec::new();
    for line in content.lines() {
        let stripped = line.trim();
        if stripped.starts_with('#') {
            let heading = stripped.trim_start_matches('#').trim();
            if !heading.is_empty() {
                headings.push(heading.to_string());
            }
        }
        if headings.len() >= limit {
            break;
        }
    }
    headings
}

fn acp_format_skill_manage_output(data: &Value, raw_input: &Value) -> Option<String> {
    let action = acp_string_text(raw_input, &["action"]);
    let mut name = acp_string_text(raw_input, &["name", "skill"]);
    if name.is_empty() {
        name = acp_string_text(data, &["name", "skill"]);
    }
    let mut file = acp_string_text(raw_input, &["filePath", "file_path", "file", "path"]);
    if file.is_empty() {
        file = acp_string_text(data, &["filePath", "file_path", "file"]);
    }
    let message = acp_string_text(data, &["message", "error"]);
    let replacements = data
        .get("replacements")
        .or_else(|| data.get("replacement_count"))
        .or_else(|| data.get("replacementCount"));
    let path = acp_string_text(data, &["path"]);
    if action.is_empty()
        && name.is_empty()
        && file.is_empty()
        && message.is_empty()
        && replacements.is_none()
        && path.is_empty()
    {
        return None;
    }
    let status = if data.get("success").and_then(Value::as_bool) == Some(false) {
        "Skill update failed"
    } else {
        "Skill updated"
    };
    let mut lines = vec![status.to_string(), String::new()];
    if !action.is_empty() {
        lines.push(format!("- Action: {action}"));
    }
    if !name.is_empty() {
        lines.push(format!("- Skill: {name}"));
    }
    if !file.is_empty() && action != "delete" {
        lines.push(format!("- File: {file}"));
    }
    if !message.is_empty() {
        lines.push(format!("- Result: {message}"));
    }
    if let Some(value) = replacements.and_then(acp_json_scalar_text) {
        lines.push(format!("- Replacements: {value}"));
    }
    if !path.is_empty() {
        lines.push(format!("- Path: {path}"));
    }
    Some(lines.join("\n"))
}

fn acp_format_skills_list_output(data: &Value) -> Option<String> {
    if let Some(error) = acp_failure_message(data) {
        return Some(format!("skills_list failed: {error}"));
    }
    let skills = data.get("skills").and_then(Value::as_array)?;
    let query = acp_string_text(data, &["query"]);
    let mut lines = vec![if query.is_empty() {
        format!("Available skills: {}", skills.len())
    } else {
        format!("Available skills for `{query}`: {}", skills.len())
    }];
    for skill in skills.iter().take(12) {
        let name = acp_string_text(skill, &["name", "id"]);
        if name.is_empty() {
            continue;
        }
        let source = acp_string_text(skill, &["source"]);
        let description = acp_string_text(skill, &["description"]);
        let mut line = format!("- {name}");
        if !source.is_empty() {
            line.push_str(&format!(" ({source})"));
        }
        if !description.is_empty() {
            line.push_str(&format!(" - {}", acp_truncate_output(&description, 140)));
        }
        lines.push(line);
    }
    if skills.len() > 12 {
        lines.push(format!("... {} more skill(s)", skills.len() - 12));
    }
    Some(acp_truncate_output(&lines.join("\n"), 5000))
}

fn acp_format_clarify_output(data: &Value) -> Option<String> {
    if let Some(error) = acp_failure_message(data) {
        return Some(format!("clarify failed: {error}"));
    }
    let question = acp_string_text(data, &["question", "text", "message"]);
    if question.is_empty() {
        return None;
    }
    let mut lines = vec![format!("Clarification required: {question}")];
    if let Some(choices) = data.get("choices").and_then(Value::as_array) {
        let labels = choices
            .iter()
            .filter_map(acp_json_scalar_text)
            .take(6)
            .collect::<Vec<_>>();
        if !labels.is_empty() {
            lines.push(format!("Choices: {}", labels.join(" | ")));
        }
    }
    Some(acp_truncate_output(&lines.join("\n"), 3000))
}

fn acp_format_kanban_output(tool_name: &str, data: &Value) -> Option<String> {
    if let Some(error) = acp_failure_message(data) {
        return Some(format!("{tool_name} failed: {error}"));
    }
    if let Some(tasks) = data.get("tasks").and_then(Value::as_array) {
        let mut lines = vec![format!("Kanban tasks: {}", tasks.len())];
        for task in tasks.iter().take(12) {
            if let Some(line) = acp_kanban_task_line(task) {
                lines.push(line);
            }
        }
        if tasks.len() > 12 {
            lines.push(format!("... {} more task(s)", tasks.len() - 12));
        }
        return Some(acp_truncate_output(&lines.join("\n"), 5000));
    }
    if let Some(task) = data.get("task") {
        let mut lines = vec![format!("{tool_name} completed")];
        if let Some(line) = acp_kanban_task_line(task) {
            lines.push(line);
        }
        let body = acp_string_text(task, &["body", "description", "result", "summary"]);
        if !body.is_empty() {
            lines.extend([String::new(), acp_truncate_output(&body, 1500)]);
        }
        return Some(acp_truncate_output(&lines.join("\n"), 5000));
    }
    let mut lines = vec![format!("{tool_name} completed")];
    for key in [
        "taskId",
        "task_id",
        "id",
        "parentId",
        "parent_id",
        "childId",
        "child_id",
        "status",
        "summary",
        "result",
        "message",
    ] {
        if let Some(value) = data.get(key).and_then(acp_json_scalar_text) {
            lines.push(format!("- **{key}:** {}", acp_truncate_output(&value, 300)));
        }
    }
    (lines.len() > 1).then(|| acp_truncate_output(&lines.join("\n"), 5000))
}

fn acp_kanban_task_line(task: &Value) -> Option<String> {
    let id = acp_string_text(task, &["id", "taskId", "task_id"]);
    let title = acp_string_text(task, &["title", "summary", "body"]);
    if id.is_empty() && title.is_empty() {
        return None;
    }
    let status = acp_string_text(task, &["status", "state"]);
    let assignee = acp_string_text(task, &["assignee"]);
    let mut tags = Vec::new();
    if !status.is_empty() {
        tags.push(status);
    }
    if !assignee.is_empty() {
        tags.push(format!("assignee {assignee}"));
    }
    let label = if id.is_empty() { "task" } else { &id };
    let title = if title.is_empty() {
        "(untitled)"
    } else {
        &title
    };
    if tags.is_empty() {
        Some(format!("- `{label}` - {}", acp_truncate_output(title, 180)))
    } else {
        Some(format!(
            "- `{label}` - {} [{}]",
            acp_truncate_output(title, 180),
            tags.join(", ")
        ))
    }
}

fn acp_format_integration_output(service: &str, tool_name: &str, data: &Value) -> Option<String> {
    if let Some(error) = acp_failure_message(data) {
        return Some(format!("{tool_name} failed: {error}"));
    }
    let mut lines = vec![format!("{service} {tool_name} completed")];
    for key in [
        "action",
        "status",
        "message",
        "id",
        "name",
        "title",
        "entity_id",
        "entityId",
        "service",
        "domain",
        "doc_token",
        "file_token",
        "comment_id",
        "channel_id",
        "guild_id",
        "group_id",
        "user_id",
        "url",
    ] {
        if let Some(value) = data.get(key).and_then(acp_json_scalar_text) {
            lines.push(format!("- **{key}:** {}", acp_truncate_output(&value, 300)));
        }
    }
    for key in [
        "entities", "services", "comments", "replies", "members", "stickers", "messages",
    ] {
        if let Some(items) = data.get(key).and_then(Value::as_array) {
            lines.push(format!("- **{key}:** {} item(s)", items.len()));
            for item in items.iter().take(6) {
                if let Some(summary) = acp_compact_object_summary(item) {
                    lines.push(format!("  - {}", acp_truncate_output(&summary, 220)));
                }
            }
        }
    }
    let content = acp_string_text(data, &["content", "text", "body", "result"]);
    if !content.is_empty() {
        lines.extend([String::new(), acp_truncate_output(&content, 1500)]);
    }
    (lines.len() > 1).then(|| acp_truncate_output(&lines.join("\n"), 6000))
}

fn acp_compact_object_summary(value: &Value) -> Option<String> {
    match value {
        Value::Object(object) => {
            let preferred = [
                "id",
                "name",
                "title",
                "entity_id",
                "state",
                "status",
                "content",
                "text",
                "url",
            ];
            let mut parts = Vec::new();
            for key in preferred {
                if let Some(text) = object.get(key).and_then(acp_json_scalar_text) {
                    parts.push(format!("{key}: {text}"));
                }
                if parts.len() >= 4 {
                    break;
                }
            }
            if parts.is_empty() {
                for (key, child) in object.iter().take(4) {
                    if let Some(text) = acp_json_scalar_text(child) {
                        parts.push(format!("{key}: {text}"));
                    }
                }
            }
            (!parts.is_empty()).then(|| parts.join(", "))
        }
        _ => acp_json_scalar_text(value),
    }
}

fn acp_failure_message(data: &Value) -> Option<String> {
    let failed = data.get("success").and_then(Value::as_bool) == Some(false)
        || data.get("ok").and_then(Value::as_bool) == Some(false)
        || data.get("error").is_some();
    failed.then(|| {
        let error = acp_string_text(data, &["error", "message"]);
        if error.is_empty() {
            "unknown error".to_string()
        } else {
            error
        }
    })
}

fn acp_format_generic_json_output(tool_name: &str, data: &Value) -> Option<String> {
    let title = if tool_name.is_empty() {
        "tool"
    } else {
        tool_name
    };
    match data {
        Value::Object(object) => {
            if object.is_empty() {
                return None;
            }
            let mut lines = vec![format!("{title} result")];
            for (key, value) in object.iter().take(12) {
                acp_push_json_summary_lines(&mut lines, key, value, 0);
            }
            Some(acp_truncate_output(&lines.join("\n"), 6000))
        }
        Value::Array(items) => {
            let mut lines = vec![format!(
                "{title}: {} item{}",
                items.len(),
                if items.len() == 1 { "" } else { "s" }
            )];
            for item in items.iter().take(8) {
                match item {
                    Value::Object(object) => {
                        let summary = object
                            .iter()
                            .take(4)
                            .filter_map(|(key, value)| {
                                acp_json_scalar_text(value).map(|text| {
                                    format!("{key}: {}", acp_truncate_output(&text, 120))
                                })
                            })
                            .collect::<Vec<_>>()
                            .join(", ");
                        if !summary.is_empty() {
                            lines.push(format!("- {summary}"));
                        }
                    }
                    _ => {
                        if let Some(text) = acp_json_scalar_text(item) {
                            lines.push(format!("- {}", acp_truncate_output(&text, 200)));
                        }
                    }
                }
            }
            Some(acp_truncate_output(&lines.join("\n"), 6000))
        }
        _ => acp_json_scalar_text(data),
    }
}

fn acp_push_json_summary_lines(lines: &mut Vec<String>, key: &str, value: &Value, depth: usize) {
    let indent = "  ".repeat(depth);
    match value {
        Value::Object(object) => {
            lines.push(format!("{indent}- **{key}:**"));
            for (child_key, child_value) in object.iter().take(8) {
                acp_push_json_summary_lines(lines, child_key, child_value, depth + 1);
            }
        }
        Value::Array(items) => {
            lines.push(format!(
                "{indent}- **{key}:** {} item{}",
                items.len(),
                if items.len() == 1 { "" } else { "s" }
            ));
            for item in items.iter().take(5) {
                if let Some(text) = acp_json_scalar_text(item) {
                    lines.push(format!("{indent}  - {}", acp_truncate_output(&text, 180)));
                } else if let Value::Object(object) = item {
                    let summary = object
                        .iter()
                        .take(4)
                        .filter_map(|(child_key, child_value)| {
                            acp_json_scalar_text(child_value).map(|text| {
                                format!("{child_key}: {}", acp_truncate_output(&text, 100))
                            })
                        })
                        .collect::<Vec<_>>()
                        .join(", ");
                    if !summary.is_empty() {
                        lines.push(format!("{indent}  - {summary}"));
                    }
                }
            }
        }
        _ => {
            if let Some(text) = acp_json_scalar_text(value) {
                lines.push(format!(
                    "{indent}- **{key}:** {}",
                    acp_truncate_output(&text, 300)
                ));
            }
        }
    }
}

fn acp_json_scalar_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        Value::Null => None,
        _ => None,
    }
}

fn acp_format_web_search_output(data: &Value) -> Option<String> {
    if data.get("success").and_then(Value::as_bool) == Some(false) {
        let error = acp_string_text(data, &["error", "message"]);
        if !error.is_empty() {
            return Some(format!("Web search failed: {error}"));
        }
    }
    let web = data
        .get("data")
        .and_then(|value| value.get("web"))
        .or_else(|| data.get("web"))?
        .as_array()?;
    let mut lines = vec![format!("Web results: {}", web.len())];
    for item in web.iter().take(10) {
        let title = acp_string_text(item, &["title", "url"]);
        let url = acp_string_text(item, &["url"]);
        let description = acp_string_text(item, &["description", "snippet"]);
        if title.is_empty() {
            continue;
        }
        lines.push(if url.is_empty() || url == title {
            format!("- {title}")
        } else {
            format!("- {title} - {url}")
        });
        if !description.is_empty() {
            lines.push(format!("  {}", acp_truncate_output(&description, 300)));
        }
    }
    Some(acp_truncate_output(&lines.join("\n"), 5000))
}

fn acp_format_browser_output(tool_name: &str, data: &Value) -> Option<String> {
    if data.get("success").and_then(Value::as_bool) == Some(false)
        || data.get("ok").and_then(Value::as_bool) == Some(false)
        || data.get("error").is_some()
    {
        let error = acp_string_text(data, &["error", "message"]);
        return Some(format!(
            "{tool_name} failed: {}",
            if error.is_empty() {
                "unknown error"
            } else {
                &error
            }
        ));
    }

    if tool_name == "browser_get_images" {
        if let Some(images) = data
            .get("images")
            .or_else(|| data.get("data"))
            .and_then(Value::as_array)
        {
            let mut lines = vec![format!("Images found: {}", images.len())];
            for image in images.iter().take(12) {
                let alt = acp_string_text(image, &["alt", "title", "text"]);
                let url = acp_string_text(image, &["url", "src"]);
                if alt.is_empty() && url.is_empty() {
                    continue;
                }
                let label = if alt.is_empty() { "image" } else { &alt };
                if url.is_empty() {
                    lines.push(format!("- {label}"));
                } else {
                    lines.push(format!("- {label} - {url}"));
                }
            }
            return Some(acp_truncate_output(&lines.join("\n"), 5000));
        }
    }

    let mut title = acp_string_text(data, &["title", "url", "status", "state"]);
    if title.is_empty() {
        title = tool_name.to_string();
    }
    let url = acp_string_text(data, &["url"]);
    let mut text = acp_string_text(
        data,
        &[
            "text", "content", "snapshot", "analysis", "message", "value",
        ],
    );
    if text.is_empty() {
        if let Some(vision) = data.get("vision") {
            text = acp_string_text(vision, &["analysis", "text", "content", "message"]);
        }
    }

    let mut lines = vec![title.clone()];
    if !url.is_empty() && url != title {
        lines.push(url);
    }
    if !text.is_empty() {
        lines.extend([String::new(), acp_truncate_output(&text, 5000)]);
    }
    Some(acp_truncate_output(&lines.join("\n"), 7000))
}

fn acp_format_media_or_cron_output(tool_name: &str, data: &Value) -> Option<String> {
    if data.get("success").and_then(Value::as_bool) == Some(false)
        || data.get("ok").and_then(Value::as_bool) == Some(false)
        || data.get("error").is_some()
    {
        let error = acp_string_text(data, &["error", "message"]);
        return Some(format!(
            "{tool_name} failed: {}",
            if error.is_empty() {
                "unknown error"
            } else {
                &error
            }
        ));
    }

    let mut lines = vec![format!("{tool_name} completed")];
    let priority_keys = [
        "file_path",
        "filePath",
        "path",
        "url",
        "image_url",
        "imageUrl",
        "videoUrl",
        "artifactPath",
        "job_id",
        "jobId",
        "task_id",
        "taskId",
        "id",
        "status",
        "message",
        "note",
        "next_run",
        "nextRun",
        "next_run_at",
        "nextRunAt",
        "providerId",
        "model",
        "voice",
        "format",
        "source",
    ];
    let mut pushed = false;
    for key in priority_keys {
        if let Some(value) = data.get(key).and_then(acp_json_scalar_text) {
            lines.push(format!("- **{key}:** {}", acp_truncate_output(&value, 500)));
            pushed = true;
        }
    }
    if let Some(artifact) = data.get("artifact").and_then(Value::as_object) {
        for key in ["path", "url", "source", "mimeType", "sizeBytes"] {
            if let Some(value) = artifact.get(key).and_then(acp_json_scalar_text) {
                lines.push(format!(
                    "- **artifact.{key}:** {}",
                    acp_truncate_output(&value, 500)
                ));
                pushed = true;
            }
        }
    }
    if let Some(artifacts) = data.get("artifacts").and_then(Value::as_array) {
        lines.push(format!("- **artifacts:** {} item(s)", artifacts.len()));
        for artifact in artifacts.iter().take(6) {
            let path = acp_string_text(artifact, &["path", "url", "source"]);
            if !path.is_empty() {
                lines.push(format!("  - {}", acp_truncate_output(&path, 500)));
                pushed = true;
            }
        }
    }
    let analysis = acp_string_text(data, &["analysis", "content", "text"]);
    if !analysis.is_empty() {
        lines.extend([String::new(), acp_truncate_output(&analysis, 1500)]);
        pushed = true;
    }
    pushed.then(|| acp_truncate_output(&lines.join("\n"), 7000))
}

fn acp_format_process_output(data: &Value, raw_input: &Value) -> Option<String> {
    if data.get("success").and_then(Value::as_bool) == Some(false) {
        let error = acp_string_text(data, &["error", "message"]);
        if !error.is_empty() {
            return Some(format!("Process error: {error}"));
        }
    }
    if let Some(processes) = data.get("processes").and_then(Value::as_array) {
        let mut lines = vec![format!("Processes: {}", processes.len())];
        for process in processes.iter().take(20) {
            let id = acp_string_text(process, &["session_id", "sessionId", "id"]);
            let mut status = acp_string_text(process, &["status", "state"]);
            if status.is_empty() {
                status = if process
                    .get("exited")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    "exited".into()
                } else {
                    "running".into()
                };
            }
            let command = acp_string_text(process, &["command"]);
            let mut bits = vec![status];
            if let Some(pid) = process.get("pid").and_then(Value::as_i64) {
                bits.push(format!("pid {pid}"));
            }
            if let Some(code) = process
                .get("exit_code")
                .or_else(|| process.get("exitCode"))
                .and_then(Value::as_i64)
            {
                bits.push(format!("exit {code}"));
            }
            let mut line = format!(
                "- `{}` - {}",
                if id.is_empty() { "?" } else { &id },
                bits.join(", ")
            );
            if !command.is_empty() {
                line.push_str(&format!(" - {}", acp_truncate_output(&command, 120)));
            }
            lines.push(line);
        }
        if processes.len() > 20 {
            lines.push(format!("... {} more process(es)", processes.len() - 20));
        }
        return Some(lines.join("\n"));
    }
    let action = acp_string_text(raw_input, &["action"]);
    let mut session_id = acp_string_text(
        data,
        &["session_id", "sessionId", "processId", "process_id", "id"],
    );
    if session_id.is_empty() {
        session_id = acp_string_text(
            raw_input,
            &["session_id", "sessionId", "processId", "process_id", "id"],
        );
    }
    let status = acp_string_text(data, &["status", "state"]);
    let output = acp_string_text(data, &["output", "stdout", "log", "new_output"]);
    let error = acp_string_text(data, &["error", "stderr"]);
    let message = acp_string_text(data, &["message"]);
    if action.is_empty()
        && status.is_empty()
        && output.is_empty()
        && error.is_empty()
        && message.is_empty()
    {
        return None;
    }
    let mut header = format!(
        "Process {}: {}",
        if action.is_empty() { "action" } else { &action },
        if status.is_empty() {
            "complete"
        } else {
            &status
        }
    );
    if !session_id.is_empty() {
        header.push_str(&format!(" (`{session_id}`)"));
    }
    let mut lines = vec![header];
    for (key, label) in [
        ("command", "Command"),
        ("pid", "PID"),
        ("exit_code", "Exit code"),
        ("returncode", "Exit code"),
        ("lines", "Lines"),
    ] {
        if let Some(value) = data.get(key).and_then(acp_json_scalar_text) {
            lines.push(format!("- **{label}:** {value}"));
        }
    }
    if !output.is_empty() {
        lines.extend([
            "".into(),
            "Output:".into(),
            acp_truncate_output(&output, 5000),
        ]);
    }
    if !error.is_empty() {
        lines.extend([
            "".into(),
            "Error:".into(),
            acp_truncate_output(&error, 2000),
        ]);
    }
    if !message.is_empty() && output.is_empty() && error.is_empty() {
        lines.push(message);
    }
    Some(acp_truncate_output(&lines.join("\n"), 7000))
}

fn acp_fenced_output(text: &str, language: &str) -> String {
    let mut longest = 0usize;
    let mut current = 0usize;
    for ch in text.chars() {
        if ch == '`' {
            current += 1;
            longest = longest.max(current);
        } else {
            current = 0;
        }
    }
    let fence = "`".repeat(longest.max(2) + 1);
    format!("{fence}{language}\n{text}\n{fence}")
}

fn acp_truncate_output(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_string();
    }
    let keep = limit.saturating_sub(80);
    format!(
        "{}\n... ({} chars total, truncated)",
        text.chars().take(keep).collect::<String>(),
        text.len()
    )
}

pub(super) fn acp_string_text(value: &Value, keys: &[&str]) -> String {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .unwrap_or("")
        .to_string()
}
