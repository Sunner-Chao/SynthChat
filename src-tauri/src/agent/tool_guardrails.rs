use std::collections::HashMap;

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::models::ChatConfig;

use super::truncate_for_prompt;

const CONTRACT_FAILURE_REPEAT_LIMIT: u32 = 2;
const STALE_FILE_MUTATION_REPEAT_LIMIT: u32 = 2;

pub(super) struct ToolLoopGuardrails {
    warnings_enabled: bool,
    hard_stop_enabled: bool,
    exact_failure_warn_after: u32,
    same_tool_failure_warn_after: u32,
    no_progress_warn_after: u32,
    exact_failure_limit: u32,
    same_tool_failure_limit: u32,
    no_progress_limit: u32,
    exact_failures: HashMap<String, u32>,
    same_tool_failures: HashMap<String, u32>,
    contract_failures: HashMap<String, u32>,
    same_tool_contract_failures: HashMap<String, u32>,
    stale_file_mutation_failures: HashMap<String, u32>,
    no_progress: HashMap<String, (String, u32)>,
}

impl ToolLoopGuardrails {
    pub(super) fn new(config: &ChatConfig) -> Self {
        Self {
            warnings_enabled: config.tool_guardrail_warnings_enabled,
            hard_stop_enabled: config.tool_guardrail_hard_stop_enabled,
            exact_failure_warn_after: config.tool_guardrail_exact_failure_warn_after.max(1),
            same_tool_failure_warn_after: config.tool_guardrail_same_tool_failure_warn_after.max(1),
            no_progress_warn_after: config.tool_guardrail_no_progress_warn_after.max(1),
            exact_failure_limit: config.tool_guardrail_exact_failure_limit.max(1),
            same_tool_failure_limit: config.tool_guardrail_same_tool_failure_limit.max(1),
            no_progress_limit: config.tool_guardrail_no_progress_limit.max(1),
            exact_failures: HashMap::new(),
            same_tool_failures: HashMap::new(),
            contract_failures: HashMap::new(),
            same_tool_contract_failures: HashMap::new(),
            stale_file_mutation_failures: HashMap::new(),
            no_progress: HashMap::new(),
        }
    }

    pub(super) fn before_call(
        &self,
        tool_name: &str,
        payload: &Value,
    ) -> Option<ToolGuardrailOutcome> {
        let signature = tool_call_signature(tool_name, payload);
        if !self.hard_stop_enabled {
            return None;
        }
        if let Some(count) = self.exact_failures.get(&signature) {
            if *count >= self.exact_failure_limit {
                return Some(ToolGuardrailOutcome::halt(format!(
                    "Tool loop guardrail stopped {tool_name}: the same call failed {count} times with identical arguments. Change strategy instead of retrying it unchanged."
                )));
            }
        }
        if is_idempotent_tool(tool_name) {
            if let Some((_hash, count)) = self.no_progress.get(&signature) {
                if *count >= self.no_progress_limit {
                    return Some(ToolGuardrailOutcome::halt(format!(
                        "Tool loop guardrail stopped {tool_name}: this read-only call returned the same result {count} times. Use the existing result or change the query."
                    )));
                }
            }
        }
        None
    }

    pub(super) fn after_call(
        &mut self,
        tool_name: &str,
        payload: &Value,
        result: &str,
        failed: bool,
    ) -> Option<ToolGuardrailOutcome> {
        let signature = tool_call_signature(tool_name, payload);
        let failed = failed || classify_tool_failure(tool_name, result).0;
        if failed {
            let contract_failure = tool_contract_failure_detail(tool_name, result);
            let stale_file_failure = stale_file_mutation_failure_detail(tool_name, result);
            let exact_count = self
                .exact_failures
                .entry(signature.clone())
                .and_modify(|count| *count += 1)
                .or_insert(1);
            self.no_progress.remove(&signature);
            let same_count = self
                .same_tool_failures
                .entry(tool_name.to_string())
                .and_modify(|count| *count += 1)
                .or_insert(1);
            if let Some(contract_failure) = contract_failure {
                let contract_count = self
                    .contract_failures
                    .entry(signature.clone())
                    .and_modify(|count| *count += 1)
                    .or_insert(1);
                let same_tool_contract_count = self
                    .same_tool_contract_failures
                    .entry(tool_name.to_string())
                    .and_modify(|count| *count += 1)
                    .or_insert(1);
                if *contract_count >= CONTRACT_FAILURE_REPEAT_LIMIT {
                    return Some(ToolGuardrailOutcome::halt(format!(
                        "Tool loop guardrail stopped {tool_name}: required fields are missing from the tool payload and the same malformed call failed {contract_count} times. Rebuild the call using the documented schema instead of retrying it unchanged. Latest error: {contract_failure}"
                    )));
                }
                if *same_tool_contract_count >= CONTRACT_FAILURE_REPEAT_LIMIT {
                    return Some(ToolGuardrailOutcome::halt(format!(
                        "Tool loop guardrail stopped {tool_name}: required fields are still missing from the tool payload after {same_tool_contract_count} failed attempts in this run. Rebuild the call using the documented schema before retrying. Latest error: {contract_failure}"
                    )));
                }
            } else {
                self.contract_failures.remove(&signature);
                self.same_tool_contract_failures.remove(tool_name);
            }
            if let Some(stale_file_failure) = stale_file_failure {
                let stale_count = self
                    .stale_file_mutation_failures
                    .entry(tool_name.to_string())
                    .and_modify(|count| *count += 1)
                    .or_insert(1);
                if *stale_count >= STALE_FILE_MUTATION_REPEAT_LIMIT {
                    return Some(ToolGuardrailOutcome::halt(format!(
                        "Tool loop guardrail stopped {tool_name}: file state stayed stale after {stale_count} failed write attempts. Re-read the target file and pass the latest expectedSha256/expectedModifiedUnixMs, or switch to a scratch/artifact path. Latest error: {stale_file_failure}"
                    )));
                }
            } else {
                self.stale_file_mutation_failures.remove(tool_name);
            }
            if self.hard_stop_enabled && *same_count >= self.same_tool_failure_limit {
                return Some(ToolGuardrailOutcome::halt(format!(
                    "Tool loop guardrail stopped {tool_name}: it failed {same_count} times in this run. Inspect the latest error and choose a different tool path."
                )));
            }
            if self.hard_stop_enabled && *exact_count >= self.exact_failure_limit {
                return Some(ToolGuardrailOutcome::halt(format!(
                    "Tool loop guardrail stopped {tool_name}: identical arguments failed {exact_count} times. Stop repeating that call unchanged."
                )));
            }
            if self.warnings_enabled && *exact_count >= self.exact_failure_warn_after {
                return Some(ToolGuardrailOutcome::warn(format!(
                    "Tool loop guardrail warning for {tool_name}: identical arguments failed {exact_count} times. Inspect the latest error and change strategy instead of retrying unchanged."
                )));
            }
            if self.warnings_enabled && *same_count >= self.same_tool_failure_warn_after {
                return Some(ToolGuardrailOutcome::warn(format!(
                    "Tool loop guardrail warning for {tool_name}: it failed {same_count} times in this run. Diagnose the latest error, change arguments, or switch tools before retrying."
                )));
            }
            return None;
        }

        self.exact_failures.remove(&signature);
        self.same_tool_failures.remove(tool_name);
        self.contract_failures.remove(&signature);
        self.same_tool_contract_failures.remove(tool_name);
        self.stale_file_mutation_failures.remove(tool_name);
        if !is_idempotent_tool(tool_name) {
            self.no_progress.remove(&signature);
            return None;
        }
        let result_hash = stable_hash(result);
        let count = match self.no_progress.get(&signature) {
            Some((previous_hash, previous_count)) if previous_hash == &result_hash => {
                previous_count + 1
            }
            _ => 1,
        };
        self.no_progress.insert(signature, (result_hash, count));
        if self.hard_stop_enabled && count >= self.no_progress_limit {
            return Some(ToolGuardrailOutcome::halt(format!(
                "Tool loop guardrail stopped {tool_name}: repeated read-only calls returned the same result {count} times."
            )));
        }
        if self.warnings_enabled && count >= self.no_progress_warn_after {
            return Some(ToolGuardrailOutcome::warn(format!(
                "Tool loop guardrail warning for {tool_name}: it returned the same result {count} times. Reuse the existing result or change the query."
            )));
        }
        None
    }
}

pub(super) struct ToolGuardrailOutcome {
    pub(super) halt: bool,
    pub(super) message: String,
}

impl ToolGuardrailOutcome {
    fn warn(message: String) -> Self {
        Self {
            halt: false,
            message,
        }
    }

    fn halt(message: String) -> Self {
        Self {
            halt: true,
            message,
        }
    }
}

fn tool_call_signature(tool_name: &str, payload: &Value) -> String {
    format!("{tool_name}:{}", stable_hash(&canonical_json(payload)))
}

fn stable_hash(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn canonical_json(value: &Value) -> String {
    match value {
        Value::Object(object) => {
            let mut keys = object.keys().collect::<Vec<_>>();
            keys.sort();
            let entries = keys
                .into_iter()
                .map(|key| {
                    format!(
                        "{}:{}",
                        serde_json::to_string(key).unwrap_or_else(|_| "\"\"".into()),
                        canonical_json(&object[key])
                    )
                })
                .collect::<Vec<_>>();
            format!("{{{}}}", entries.join(","))
        }
        Value::Array(items) => {
            let entries = items.iter().map(canonical_json).collect::<Vec<_>>();
            format!("[{}]", entries.join(","))
        }
        _ => serde_json::to_string(value).unwrap_or_else(|_| value.to_string()),
    }
}

fn is_idempotent_tool(tool_name: &str) -> bool {
    if tool_name.starts_with("mcp_filesystem_")
        && (tool_name.contains("read")
            || tool_name.contains("list")
            || tool_name.contains("directory")
            || tool_name.contains("info")
            || tool_name.contains("search"))
    {
        return true;
    }
    matches!(
        tool_name,
        "read_file"
            | "search_files"
            | "tool_search"
            | "tool_describe"
            | "web_search"
            | "x_search"
            | "web_extract"
            | "web_request"
            | "session_search"
            | "skills_list"
            | "skill_view"
            | "recall_memory"
            | "browser_snapshot"
            | "browser_console"
            | "browser_get_images"
            | "weather"
            | "security_scan"
            | "ha_list_entities"
            | "ha_get_state"
            | "ha_list_services"
            | "feishu_doc_read"
            | "feishu_drive_list_comments"
            | "feishu_drive_list_comment_replies"
            | "spotify_search"
            | "spotify_albums"
            | "discord"
            | "list_artifacts"
    )
}

fn mutation_targets(tool_name: &str, payload: &Value) -> Vec<String> {
    if !matches!(
        tool_name,
        "write_file" | "patch" | "delete_file" | "move_file"
    ) {
        return vec![];
    }
    if tool_name == "patch"
        && (payload.get("patch").is_some()
            || payload
                .get("mode")
                .and_then(Value::as_str)
                .is_some_and(|mode| mode == "patch"))
    {
        return v4a_patch_target_paths(payload.get("patch").and_then(Value::as_str).unwrap_or(""));
    }
    match tool_name {
        "move_file" => [
            payload
                .get("src")
                .or_else(|| payload.get("source"))
                .or_else(|| payload.get("from")),
            payload
                .get("dst")
                .or_else(|| payload.get("target"))
                .or_else(|| payload.get("to")),
        ]
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect(),
        _ => payload
            .get("path")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| vec![value.to_string()])
            .unwrap_or_default(),
    }
}

pub(super) fn record_file_mutation_result(
    failed_mutations: &mut HashMap<String, String>,
    tool_name: &str,
    payload: &Value,
    result: &str,
    failed: bool,
) {
    let targets = mutation_targets(tool_name, payload);
    if targets.is_empty() {
        return;
    }
    let mutation_failed = failed || !file_mutation_result_landed(tool_name, result);
    if mutation_failed {
        let preview = error_preview(result, 180);
        for target in targets {
            failed_mutations.insert(target, preview.clone());
        }
    } else {
        for target in targets {
            failed_mutations.remove(&target);
        }
    }
}

pub(super) fn file_mutation_result_landed(tool_name: &str, result: &str) -> bool {
    if !matches!(
        tool_name,
        "write_file" | "patch" | "delete_file" | "move_file"
    ) {
        return false;
    }
    let Ok(data) = serde_json::from_str::<Value>(result.trim()) else {
        return false;
    };
    if data.get("error").is_some() {
        return false;
    }
    if data
        .get("success")
        .and_then(Value::as_bool)
        .is_some_and(|success| !success)
    {
        return false;
    }
    match tool_name {
        "write_file" => data.get("bytes_written").is_some() || data.get("bytesWritten").is_some(),
        "patch" => data
            .get("success")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        "delete_file" => data
            .get("deleted")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        "move_file" => data.get("moved").and_then(Value::as_bool).unwrap_or(false),
        _ => false,
    }
}

pub(super) fn classify_tool_failure(tool_name: &str, result: &str) -> (bool, String) {
    if result.trim().is_empty() || file_mutation_result_landed(tool_name, result) {
        return (false, String::new());
    }
    if tool_name == "terminal" {
        if let Some(exit_code) = terminal_exit_code(result) {
            if exit_code != 0 {
                return (true, format!(" [exit {exit_code}]"));
            }
        }
        return (false, String::new());
    }
    if let Ok(data) = serde_json::from_str::<Value>(result.trim()) {
        if data
            .get("success")
            .and_then(Value::as_bool)
            .is_some_and(|success| !success)
        {
            return (
                true,
                tool_failure_suffix(data.get("error").or_else(|| data.get("message"))),
            );
        }
        if data.get("error").is_some() {
            return (true, tool_failure_suffix(data.get("error")));
        }
        if data.get("failed").and_then(Value::as_bool).unwrap_or(false) {
            return (true, tool_failure_suffix(data.get("message")));
        }
    }
    let lower = result.chars().take(500).collect::<String>().to_lowercase();
    if lower.contains("\"error\"") || lower.contains("\"failed\"") || result.starts_with("Error") {
        return (true, " [error]".into());
    }
    (false, String::new())
}

fn tool_contract_failure_detail(tool_name: &str, result: &str) -> Option<String> {
    let text = normalized_error_text(result);
    if text.is_empty() {
        return None;
    }
    let lower = text.to_lowercase();
    let tool_prefix = format!("{} requires payload", tool_name.to_lowercase());
    if lower.contains(&tool_prefix)
        || lower.contains("requires payload.")
        || lower.contains("requires payload ")
    {
        return Some(text);
    }
    None
}

fn stale_file_mutation_failure_detail(tool_name: &str, result: &str) -> Option<String> {
    if !matches!(
        tool_name,
        "write_file" | "patch" | "delete_file" | "move_file"
    ) {
        return None;
    }
    let text = normalized_error_text(result);
    if text
        .to_lowercase()
        .contains("file registry stale check failed")
    {
        Some(text)
    } else {
        None
    }
}

fn normalized_error_text(result: &str) -> String {
    if let Ok(data) = serde_json::from_str::<Value>(result.trim()) {
        if let Some(error) = data.get("error").and_then(Value::as_str) {
            return collapse_whitespace(error);
        }
        if let Some(message) = data.get("message").and_then(Value::as_str) {
            return collapse_whitespace(message);
        }
    }
    collapse_whitespace(result)
}

fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn terminal_exit_code(result: &str) -> Option<i64> {
    if let Ok(data) = serde_json::from_str::<Value>(result.trim()) {
        return data
            .get("exit_code")
            .or_else(|| data.get("exitCode"))
            .and_then(Value::as_i64);
    }
    result.lines().find_map(|line| {
        line.trim()
            .strip_prefix("exitCode:")
            .or_else(|| line.trim().strip_prefix("exit_code:"))
            .and_then(|value| value.trim().parse::<i64>().ok())
    })
}

fn tool_failure_suffix(value: Option<&Value>) -> String {
    value
        .and_then(Value::as_str)
        .map(|text| format!(" [{}]", error_preview(text, 80)))
        .unwrap_or_else(|| " [error]".into())
}

fn v4a_patch_target_paths(body: &str) -> Vec<String> {
    let mut paths = Vec::new();
    for line in body.lines().map(str::trim) {
        for prefix in [
            "*** Update File:",
            "*** Add File:",
            "*** Delete File:",
            "*** Move to:",
        ] {
            if let Some(path) = line.strip_prefix(prefix).map(str::trim) {
                if !path.is_empty() {
                    paths.push(path.to_string());
                }
            }
        }
        if let Some(rest) = line.strip_prefix("*** Move File:").map(str::trim) {
            if let Some((from, to)) = rest.split_once("->") {
                for path in [from.trim(), to.trim()] {
                    if !path.is_empty() {
                        paths.push(path.to_string());
                    }
                }
            }
        }
    }
    paths.sort();
    paths.dedup();
    paths
}

fn file_mutation_failure_footer(failed_mutations: &HashMap<String, String>) -> Option<String> {
    if failed_mutations.is_empty() {
        return None;
    }
    let mut rows = failed_mutations
        .iter()
        .map(|(path, error)| {
            format!(
                "- {}: {}",
                path,
                if error.trim().is_empty() {
                    "file mutation failed".into()
                } else {
                    error.clone()
                }
            )
        })
        .collect::<Vec<_>>();
    rows.sort();
    Some(format!(
        "文件变更校验：以下文件变更工具调用失败且未被后续成功操作覆盖，请不要声称这些文件已修改、删除或移动：\n{}",
        rows.join("\n")
    ))
}

pub(super) fn append_file_mutation_footer(
    response: &mut String,
    failed_mutations: &HashMap<String, String>,
) {
    if let Some(footer) = file_mutation_failure_footer(failed_mutations) {
        if !response.trim().is_empty() {
            response.push_str("\n\n");
        }
        response.push_str(&footer);
    }
}

pub(super) fn normalize_guardrail_halt_reply(response: &mut String, observations: &[String]) {
    let trimmed = response.trim();
    if !trimmed.starts_with("Tool loop guardrail stopped ") {
        return;
    }
    let mut lines = vec![
        "本轮已自动停止，原因是工具调用陷入重复模式。".to_string(),
        format!("停止原因：{}", localize_tool_guardrail_message(trimmed)),
    ];
    let recent = recent_tool_observation_summaries(observations, 3);
    if !recent.is_empty() {
        lines.push("最近工具结果：".into());
        lines.extend(recent.into_iter().map(|line| format!("- {line}")));
    }
    lines.push(
        "这不是前端丢失回复；是 agent 防循环保护生效。请换一个来源、换查询词，或降低重复读取同一页面的概率后重试。"
            .into(),
    );
    *response = lines.join("\n");
}

fn localize_tool_guardrail_message(message: &str) -> String {
    let tool_name = message
        .strip_prefix("Tool loop guardrail stopped ")
        .and_then(|rest| rest.split(':').next())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("tool");
    if message.contains("repeated read-only calls returned the same result")
        || message.contains("this read-only call returned the same result")
    {
        format!("{tool_name} 连续返回相同内容，继续调用不会推进任务。")
    } else if message.contains("required fields are missing from the tool payload")
        || message.contains("required fields are still missing from the tool payload")
    {
        format!("{tool_name} 缺少必填 payload 字段，继续重试同类调用不会成功。")
    } else if message.contains("identical arguments failed") || message.contains("same call failed")
    {
        format!("{tool_name} 使用相同参数反复失败。")
    } else if message.contains("failed") {
        format!("{tool_name} 在本轮中多次失败。")
    } else {
        message.to_string()
    }
}

fn recent_tool_observation_summaries(observations: &[String], limit: usize) -> Vec<String> {
    observations
        .iter()
        .rev()
        .filter(|line| {
            line.contains(" tool ")
                && (line.contains(" result:") || line.contains(" error:"))
                && !line.contains(" guardrail:")
        })
        .take(limit)
        .map(|line| {
            truncate_for_prompt(&line.split_whitespace().collect::<Vec<_>>().join(" "), 220)
        })
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

fn error_preview(value: &str, max_chars: usize) -> String {
    let mut text = value.trim().to_string();
    if let Ok(json) = serde_json::from_str::<Value>(&text) {
        if let Some(error) = json.get("error").and_then(Value::as_str) {
            text = error.to_string();
        }
    }
    truncate_for_prompt(
        &text.split_whitespace().collect::<Vec<_>>().join(" "),
        max_chars,
    )
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::models::ChatConfig;

    use super::{
        classify_tool_failure, is_idempotent_tool, stable_hash, stale_file_mutation_failure_detail,
        tool_call_signature, tool_contract_failure_detail, ToolLoopGuardrails,
    };

    #[test]
    fn idempotent_tool_set_includes_hermes_read_only_tools() {
        for tool_name in [
            "tool_search",
            "tool_describe",
            "skills_list",
            "skill_view",
            "recall_memory",
            "security_scan",
            "mcp_filesystem_read_file",
            "mcp_filesystem_list_directory",
            "mcp_filesystem_get_file_info",
            "mcp_filesystem_search_files",
        ] {
            assert!(is_idempotent_tool(tool_name), "{tool_name}");
        }
    }

    #[test]
    fn classify_tool_failure_matches_hermes_error_signals() {
        assert!(classify_tool_failure("web_request", r#"{"error":"not found"}"#).0);
        assert!(classify_tool_failure("memory", r#"{"success":false,"error":"full"}"#).0);
        assert!(classify_tool_failure("search_files", "Error executing tool").0);
        assert!(
            classify_tool_failure(
                "terminal",
                "cwd: .\nexitCode: 2\nstdout:\n\nstderr:\nfailed"
            )
            .0
        );
        assert!(!classify_tool_failure("write_file", r#"{"success":true,"bytesWritten":12}"#).0);
    }

    #[test]
    fn contract_failure_detection_matches_payload_schema_errors() {
        assert_eq!(
            tool_contract_failure_detail(
                "write_file",
                "bad request: bad request: write_file requires payload.path"
            ),
            Some("bad request: bad request: write_file requires payload.path".into())
        );
        assert!(tool_contract_failure_detail(
            "read_file",
            r#"{"error":"read_file requires payload.path"}"#
        )
        .is_some());
        assert!(tool_contract_failure_detail("search_files", "Error executing tool").is_none());
    }

    #[test]
    fn stale_file_mutation_failure_detection_matches_registry_errors() {
        assert!(stale_file_mutation_failure_detail(
            "write_file",
            r#"{"error":"file registry stale check failed for notes.txt"}"#
        )
        .is_some());
        assert!(stale_file_mutation_failure_detail(
            "read_file",
            r#"{"error":"file registry stale check failed for notes.txt"}"#
        )
        .is_none());
        assert!(stale_file_mutation_failure_detail("write_file", r#"{"ok":true}"#).is_none());
    }

    #[test]
    fn tool_call_signature_uses_hermes_style_canonical_sha256() {
        let left = json!({
            "query": "agent",
            "filters": {
                "kind": "code",
                "limit": 10
            }
        });
        let right = json!({
            "filters": {
                "limit": 10,
                "kind": "code"
            },
            "query": "agent"
        });
        let left_signature = tool_call_signature("search_files", &left);
        let right_signature = tool_call_signature("search_files", &right);

        assert_eq!(left_signature, right_signature);
        let (_, hash) = left_signature.split_once(':').unwrap();
        assert_eq!(hash.len(), 64);
        assert!(hash.chars().all(|ch| ch.is_ascii_hexdigit()));
    }

    #[test]
    fn stable_hash_is_sha256_hex() {
        assert_eq!(
            stable_hash(""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn guardrails_warn_without_halting_by_default() {
        let config = ChatConfig::default();
        assert!(config.tool_guardrail_warnings_enabled);
        assert!(!config.tool_guardrail_hard_stop_enabled);
        let mut guardrails = ToolLoopGuardrails::new(&config);
        let payload = json!({"query": "missing"});

        let first = guardrails.after_call("search_files", &payload, "Error executing tool", true);
        assert!(first.is_none());
        let second = guardrails
            .after_call("search_files", &payload, "Error executing tool", true)
            .unwrap();
        assert!(!second.halt);
        assert!(second.message.contains("warning"));

        for _ in 0..10 {
            let outcome =
                guardrails.after_call("search_files", &payload, "Error executing tool", true);
            if let Some(outcome) = outcome {
                assert!(!outcome.halt);
            }
        }
        assert!(guardrails.before_call("search_files", &payload).is_none());
    }

    #[test]
    fn guardrails_halt_only_when_hard_stop_is_enabled() {
        let config = ChatConfig {
            tool_guardrail_hard_stop_enabled: true,
            tool_guardrail_exact_failure_limit: 2,
            ..ChatConfig::default()
        };
        let mut guardrails = ToolLoopGuardrails::new(&config);
        let payload = json!({"query": "missing"});

        let _ = guardrails.after_call("search_files", &payload, "Error executing tool", true);
        let halt = guardrails
            .after_call("search_files", &payload, "Error executing tool", true)
            .unwrap();
        assert!(halt.halt);
        assert!(halt.message.contains("stopped"));

        let before = guardrails.before_call("search_files", &payload).unwrap();
        assert!(before.halt);
    }

    #[test]
    fn guardrails_halt_repeated_contract_failures_by_default() {
        let config = ChatConfig::default();
        let mut guardrails = ToolLoopGuardrails::new(&config);
        let payload = json!({});

        let first = guardrails.after_call(
            "write_file",
            &payload,
            "bad request: bad request: write_file requires payload.path",
            true,
        );
        assert!(first.is_none());

        let halt = guardrails
            .after_call(
                "write_file",
                &payload,
                "bad request: bad request: write_file requires payload.path",
                true,
            )
            .unwrap();
        assert!(halt.halt);
        assert!(halt.message.contains("required fields are missing"));
    }

    #[test]
    fn guardrails_halt_same_tool_contract_failures_even_with_changed_payload() {
        let config = ChatConfig::default();
        let mut guardrails = ToolLoopGuardrails::new(&config);

        let first = guardrails.after_call(
            "write_file",
            &json!({}),
            "bad request: bad request: write_file requires payload.path",
            true,
        );
        assert!(first.is_none());

        let halt = guardrails
            .after_call(
                "write_file",
                &json!({"path": "notes.txt"}),
                "bad request: bad request: write_file requires payload.content",
                true,
            )
            .unwrap();
        assert!(halt.halt);
        assert!(halt.message.contains("required fields are still missing"));
    }

    #[test]
    fn guardrails_halt_repeated_read_file_contract_failures_by_default() {
        let config = ChatConfig::default();
        let mut guardrails = ToolLoopGuardrails::new(&config);
        let payload = json!({});

        let first = guardrails.after_call(
            "read_file",
            &payload,
            "bad request: bad request: read_file requires payload.path",
            true,
        );
        assert!(first.is_none());

        let halt = guardrails
            .after_call(
                "read_file",
                &payload,
                "bad request: bad request: read_file requires payload.path",
                true,
            )
            .unwrap();
        assert!(halt.halt);
        assert!(halt.message.contains("required fields are missing"));
    }

    #[test]
    fn guardrails_halt_repeated_stale_file_mutation_failures_by_default() {
        let config = ChatConfig::default();
        let mut guardrails = ToolLoopGuardrails::new(&config);

        let first = guardrails.after_call(
            "write_file",
            &json!({"path": "read_pdf.py", "content": "print(1)"}),
            r#"{"error":"file registry stale check failed for read_pdf.py"}"#,
            true,
        );
        assert!(first.is_none());

        let halt = guardrails
            .after_call(
                "write_file",
                &json!({"path": "read_pdf.py", "content": "print(2)"}),
                r#"{"error":"file registry stale check failed for read_pdf.py"}"#,
                true,
            )
            .unwrap();
        assert!(halt.halt);
        assert!(halt.message.contains("file state stayed stale"));
        assert!(halt.message.contains("Re-read the target file"));
    }
}
