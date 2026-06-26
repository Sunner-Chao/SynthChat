use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
    time::UNIX_EPOCH,
};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::{
    error::{AppError, AppResult},
    models::AgentDefinition,
    store::AppStore,
};

use super::{
    diagnostics::{
        lsp_clear_baseline_for_path, lsp_delta_diagnostics_blocking, lsp_snapshot_baseline_blocking,
    },
    edit_diagnostics_for_paths_with_baselines, likely_binary, positive_or_default,
    resolve_workspace_path, resolve_workspace_target_path, should_skip_dir, workspace_root,
};

static FILE_STATE_PATH_LOCKS: OnceLock<Mutex<BTreeMap<String, Arc<Mutex<()>>>>> = OnceLock::new();
static FILE_TOOL_LOOP_TRACKER: OnceLock<Mutex<BTreeMap<String, FileToolLoopState>>> =
    OnceLock::new();
static PATCH_FAILURE_TRACKER: OnceLock<Mutex<BTreeMap<String, BTreeMap<String, usize>>>> =
    OnceLock::new();

#[derive(Debug, Clone, Default)]
struct FileToolLoopState {
    last_key: Option<String>,
    consecutive: usize,
}

fn file_state_path_lock(path: &Path) -> Arc<Mutex<()>> {
    let key = path.to_string_lossy().to_string();
    let locks = FILE_STATE_PATH_LOCKS.get_or_init(|| Mutex::new(BTreeMap::new()));
    let mut locks = locks
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    locks
        .entry(key)
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

pub(super) fn with_file_state_path_locks<T>(
    paths: &[&Path],
    action: impl FnOnce() -> AppResult<T>,
) -> AppResult<T> {
    let mut unique = paths
        .iter()
        .map(|path| path.to_string_lossy().to_string())
        .collect::<Vec<_>>();
    unique.sort();
    unique.dedup();
    let lock_paths = unique.iter().map(PathBuf::from).collect::<Vec<_>>();
    let locks = lock_paths
        .iter()
        .map(|path| file_state_path_lock(path))
        .collect::<Vec<_>>();
    let _guards = locks
        .iter()
        .map(|lock| {
            lock.lock()
                .map_err(|_| AppError::BadRequest("file state path lock was poisoned".into()))
        })
        .collect::<AppResult<Vec<_>>>()?;
    action()
}

fn file_state_actor(payload: &Value, fallback: &str) -> (String, Option<String>) {
    let run_id = payload_string(payload, &["runId", "run_id", "taskId", "task_id"]);
    let actor = payload_string(
        payload,
        &[
            "taskId", "task_id", "agentId", "agent_id", "runId", "run_id",
        ],
    )
    .or_else(|| run_id.clone())
    .unwrap_or_else(|| fallback.to_string());
    (actor, run_id)
}

fn file_tool_loop_task_id(payload: &Value) -> String {
    payload_string(payload, &["runId", "run_id", "taskId", "task_id"])
        .unwrap_or_else(|| "default".to_string())
}

fn track_file_tool_loop(payload: &Value, key: String, label: &str) -> AppResult<Option<String>> {
    let task_id = file_tool_loop_task_id(payload);
    let tracker = FILE_TOOL_LOOP_TRACKER.get_or_init(|| Mutex::new(BTreeMap::new()));
    let mut tracker = tracker
        .lock()
        .map_err(|_| AppError::BadRequest("file tool loop tracker was poisoned".into()))?;
    let state = tracker.entry(task_id).or_default();
    if state.last_key.as_deref() == Some(key.as_str()) {
        state.consecutive += 1;
    } else {
        state.last_key = Some(key);
        state.consecutive = 1;
    }
    if state.consecutive >= 4 {
        return Err(AppError::BadRequest(format!(
            "{label} loop BLOCKED: this exact file tool request was repeated {} times consecutively. Use a different offset/limit, narrow the query, or act on the information already returned.",
            state.consecutive
        )));
    }
    if state.consecutive == 3 {
        return Ok(Some(format!(
            "\n\n[Warning: You have repeated this exact {label} request 3 times consecutively. Use a different offset/limit, narrow the query, or proceed with the information already returned.]"
        )));
    }
    Ok(None)
}

pub(super) fn notify_file_tool_loop_other_call(run_id: &str) {
    let tracker = FILE_TOOL_LOOP_TRACKER.get_or_init(|| Mutex::new(BTreeMap::new()));
    if let Ok(mut tracker) = tracker.lock() {
        if let Some(state) = tracker.get_mut(run_id) {
            state.last_key = None;
            state.consecutive = 0;
        }
    }
}

fn record_patch_failure(payload: &Value, path: &Path) -> AppResult<usize> {
    let task_id = file_tool_loop_task_id(payload);
    let path_key = path.to_string_lossy().to_string();
    let tracker = PATCH_FAILURE_TRACKER.get_or_init(|| Mutex::new(BTreeMap::new()));
    let mut tracker = tracker
        .lock()
        .map_err(|_| AppError::BadRequest("patch failure tracker was poisoned".into()))?;
    let task_failures = tracker.entry(task_id).or_default();
    let count = task_failures.entry(path_key).or_insert(0);
    *count += 1;
    Ok(*count)
}

fn reset_patch_failures(payload: &Value, paths: &[&Path]) {
    let task_id = file_tool_loop_task_id(payload);
    let tracker = PATCH_FAILURE_TRACKER.get_or_init(|| Mutex::new(BTreeMap::new()));
    if let Ok(mut tracker) = tracker.lock() {
        if let Some(task_failures) = tracker.get_mut(&task_id) {
            for path in paths {
                task_failures.remove(&path.to_string_lossy().to_string());
            }
            if task_failures.is_empty() {
                tracker.remove(&task_id);
            }
        }
    }
}

fn patch_failure_escalation_hint(count: usize) -> String {
    if count < 3 {
        String::new()
    } else {
        format!(
            "\n\n[Patch failure #{count}: Stop retrying the same stale search text. Re-read the target file, use a longer exact context window, or fall back to write_file with the full intended content.]"
        )
    }
}

pub(super) fn read_file_tool(
    store: &AppStore,
    agent: &AgentDefinition,
    payload: &Value,
) -> AppResult<String> {
    let path = payload
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::BadRequest("read_file requires payload.path".into()))?;
    let root = workspace_root(agent)?;
    let full_path = resolve_workspace_path(&root, path)?;
    if has_pdf_extension(&full_path) {
        return read_pdf_file_tool(store, &full_path, payload);
    }
    if likely_binary(&full_path) {
        return Err(AppError::BadRequest(format!(
            "read_file refused binary or non-text file: {}",
            full_path.display()
        )));
    }
    let content = fs::read_to_string(&full_path)?;
    let state = file_state(&full_path, &content)?;
    let mode = payload
        .get("mode")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if matches!(mode.as_str(), "raw" | "full" | "plain") {
        let max_chars = payload
            .get("maxChars")
            .or_else(|| payload.get("max_chars"))
            .and_then(Value::as_u64)
            .unwrap_or(80_000)
            .clamp(1_000, 500_000) as usize;
        let loop_warning = track_file_tool_loop(
            payload,
            format!("read:{}:raw:{max_chars}", full_path.display()),
            "read_file",
        )?;
        let total_chars = content.chars().count();
        if total_chars > max_chars {
            return Err(AppError::BadRequest(format!(
                "read_file raw mode refused {} chars from {}; pass a larger maxChars or use line pagination",
                total_chars,
                full_path.display()
            )));
        }
        let (reader, reader_run_id) = file_state_actor(payload, "read_file");
        store.record_file_read_state(
            &full_path.to_string_lossy(),
            &state.sha256,
            state.modified_unix_ms,
            state.bytes,
            false,
            Some(&reader),
            reader_run_id.as_deref(),
        )?;
        return Ok(format!(
            "path: {}\nmode: raw\nbytes: {}\nchars: {}\nsha256: {}\nmodifiedUnixMs: {}\n\n{}",
            full_path.display(),
            state.bytes,
            total_chars,
            state.sha256,
            state.modified_unix_ms,
            strip_utf8_bom_str(&content)
        ) + loop_warning.as_deref().unwrap_or(""));
    }
    let char_mode = matches!(mode.as_str(), "char" | "chars" | "characters")
        || payload.get("charOffset").is_some()
        || payload.get("char_offset").is_some()
        || payload.get("charLimit").is_some()
        || payload.get("char_limit").is_some();
    if char_mode {
        let limit = payload
            .get("charLimit")
            .or_else(|| payload.get("char_limit"))
            .or_else(|| payload.get("limit"))
            .and_then(Value::as_u64)
            .unwrap_or(12000)
            .min(80000) as usize;
        let offset = payload
            .get("charOffset")
            .or_else(|| payload.get("char_offset"))
            .or_else(|| payload.get("offset"))
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
        let loop_warning = track_file_tool_loop(
            payload,
            format!("read:{}:chars:{offset}:{limit}", full_path.display()),
            "read_file",
        )?;
        let total_chars = content.chars().count();
        let slice: String = content.chars().skip(offset).take(limit).collect();
        let partial = offset > 0 || offset + limit < total_chars;
        let (reader, reader_run_id) = file_state_actor(payload, "read_file");
        store.record_file_read_state(
            &full_path.to_string_lossy(),
            &state.sha256,
            state.modified_unix_ms,
            state.bytes,
            partial,
            Some(&reader),
            reader_run_id.as_deref(),
        )?;
        return Ok(format!(
            "path: {}\nmode: chars\nbytes: {}\nchars: {} sha256: {} modifiedUnixMs: {} offset: {} limit: {}\n\n{}",
            full_path.display(),
            state.bytes,
            total_chars,
            state.sha256,
            state.modified_unix_ms,
            offset,
            limit,
            slice
        ) + loop_warning.as_deref().unwrap_or(""));
    }
    let offset = payload
        .get("lineOffset")
        .or_else(|| payload.get("line_offset"))
        .or_else(|| payload.get("offset"))
        .and_then(Value::as_u64)
        .unwrap_or(1)
        .max(1) as usize;
    let chat_config = store.config().map(|config| config.chat).unwrap_or_default();
    let max_lines = positive_or_default(chat_config.tool_output_max_lines, 2_000);
    let max_line_length = positive_or_default(chat_config.tool_output_max_line_length, 2_000);
    let limit = payload
        .get("lineLimit")
        .or_else(|| payload.get("line_limit"))
        .or_else(|| payload.get("limit"))
        .and_then(Value::as_u64)
        .unwrap_or(500)
        .clamp(1, max_lines as u64) as usize;
    let loop_warning = track_file_tool_loop(
        payload,
        format!("read:{}:lines:{offset}:{limit}", full_path.display()),
        "read_file",
    )?;
    let lines = content.split('\n').collect::<Vec<_>>();
    let total_lines = if content.is_empty() { 0 } else { lines.len() };
    let start = offset.saturating_sub(1).min(total_lines);
    let end = (start + limit).min(total_lines);
    let body = render_numbered_lines(&lines[start..end], offset, max_line_length);
    let truncated = end < total_lines;
    let next = if truncated {
        format!("\nnextOffset: {}", end + 1)
    } else {
        String::new()
    };
    let partial = offset > 1 || truncated;
    let (reader, reader_run_id) = file_state_actor(payload, "read_file");
    store.record_file_read_state(
        &full_path.to_string_lossy(),
        &state.sha256,
        state.modified_unix_ms,
        state.bytes,
        partial,
        Some(&reader),
        reader_run_id.as_deref(),
    )?;
    Ok(format!(
        "path: {}\nmode: lines\nbytes: {}\nlines: {} sha256: {} modifiedUnixMs: {} offset: {} limit: {} showing: {}-{} truncated: {}{}\n\n{}",
        full_path.display(),
        state.bytes,
        total_lines,
        state.sha256,
        state.modified_unix_ms,
        offset,
        limit,
        if total_lines == 0 { 0 } else { start + 1 },
        end,
        truncated,
        next,
        body
    ) + loop_warning.as_deref().unwrap_or(""))
}

fn render_numbered_lines(lines: &[&str], start_line: usize, max_line_chars: usize) -> String {
    lines
        .iter()
        .enumerate()
        .map(|(index, line)| {
            let mut text = line.trim_end_matches('\r').to_string();
            if text.chars().count() > max_line_chars {
                text = format!(
                    "{}... [truncated]",
                    text.chars().take(max_line_chars).collect::<String>()
                );
            }
            format!("{}|{}", start_line + index, text)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn has_pdf_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.eq_ignore_ascii_case("pdf"))
        .unwrap_or(false)
}

fn read_pdf_file_tool(store: &AppStore, full_path: &Path, payload: &Value) -> AppResult<String> {
    let bytes = fs::read(full_path)?;
    if !bytes.starts_with(b"%PDF") {
        return Err(AppError::BadRequest(format!(
            "read_file refused .pdf without a PDF header: {}",
            full_path.display()
        )));
    }
    let content = extract_pdf_text_best_effort(&bytes);
    if content.trim().is_empty() {
        return Err(AppError::BadRequest(format!(
            "read_file could not extract text from PDF: {}. The file may be scanned, encrypted, or compressed; use a PDF/OCR document workflow.",
            full_path.display()
        )));
    }
    let state = file_byte_state(full_path, &bytes)?;
    let mode = payload
        .get("mode")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let (reader, reader_run_id) = file_state_actor(payload, "read_file");
    if matches!(mode.as_str(), "raw" | "full" | "plain") {
        let max_chars = payload
            .get("maxChars")
            .or_else(|| payload.get("max_chars"))
            .and_then(Value::as_u64)
            .unwrap_or(80_000)
            .clamp(1_000, 500_000) as usize;
        let loop_warning = track_file_tool_loop(
            payload,
            format!("read:{}:pdf_raw:{max_chars}", full_path.display()),
            "read_file",
        )?;
        let total_chars = content.chars().count();
        if total_chars > max_chars {
            return Err(AppError::BadRequest(format!(
                "read_file PDF raw mode refused {} extracted chars from {}; pass a larger maxChars or use line pagination",
                total_chars,
                full_path.display()
            )));
        }
        store.record_file_read_state(
            &full_path.to_string_lossy(),
            &state.sha256,
            state.modified_unix_ms,
            state.bytes,
            false,
            Some(&reader),
            reader_run_id.as_deref(),
        )?;
        return Ok(format!(
            "path: {}\nmode: pdf_raw\nbytes: {}\nchars: {}\nsha256: {}\nmodifiedUnixMs: {}\nextractor: best_effort_pdf_text\n\n{}",
            full_path.display(),
            state.bytes,
            total_chars,
            state.sha256,
            state.modified_unix_ms,
            content
        ) + loop_warning.as_deref().unwrap_or(""));
    }
    let char_mode = matches!(mode.as_str(), "char" | "chars" | "characters")
        || payload.get("charOffset").is_some()
        || payload.get("char_offset").is_some()
        || payload.get("charLimit").is_some()
        || payload.get("char_limit").is_some();
    if char_mode {
        let limit = payload
            .get("charLimit")
            .or_else(|| payload.get("char_limit"))
            .or_else(|| payload.get("limit"))
            .and_then(Value::as_u64)
            .unwrap_or(12000)
            .min(80000) as usize;
        let offset = payload
            .get("charOffset")
            .or_else(|| payload.get("char_offset"))
            .or_else(|| payload.get("offset"))
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
        let loop_warning = track_file_tool_loop(
            payload,
            format!("read:{}:pdf_chars:{offset}:{limit}", full_path.display()),
            "read_file",
        )?;
        let total_chars = content.chars().count();
        let slice = content.chars().skip(offset).take(limit).collect::<String>();
        let partial = offset > 0 || offset + limit < total_chars;
        store.record_file_read_state(
            &full_path.to_string_lossy(),
            &state.sha256,
            state.modified_unix_ms,
            state.bytes,
            partial,
            Some(&reader),
            reader_run_id.as_deref(),
        )?;
        return Ok(format!(
            "path: {}\nmode: pdf_chars\nbytes: {}\nchars: {} sha256: {} modifiedUnixMs: {} offset: {} limit: {}\nextractor: best_effort_pdf_text\n\n{}",
            full_path.display(),
            state.bytes,
            total_chars,
            state.sha256,
            state.modified_unix_ms,
            offset,
            limit,
            slice
        ) + loop_warning.as_deref().unwrap_or(""));
    }

    let offset = payload
        .get("lineOffset")
        .or_else(|| payload.get("line_offset"))
        .or_else(|| payload.get("offset"))
        .and_then(Value::as_u64)
        .unwrap_or(1)
        .max(1) as usize;
    let chat_config = store.config().map(|config| config.chat).unwrap_or_default();
    let max_lines = positive_or_default(chat_config.tool_output_max_lines, 2_000);
    let max_line_length = positive_or_default(chat_config.tool_output_max_line_length, 2_000);
    let limit = payload
        .get("lineLimit")
        .or_else(|| payload.get("line_limit"))
        .or_else(|| payload.get("limit"))
        .and_then(Value::as_u64)
        .unwrap_or(500)
        .clamp(1, max_lines as u64) as usize;
    let loop_warning = track_file_tool_loop(
        payload,
        format!("read:{}:pdf_lines:{offset}:{limit}", full_path.display()),
        "read_file",
    )?;
    let lines = content.split('\n').collect::<Vec<_>>();
    let total_lines = if content.is_empty() { 0 } else { lines.len() };
    let start = offset.saturating_sub(1).min(total_lines);
    let end = (start + limit).min(total_lines);
    let body = render_numbered_lines(&lines[start..end], offset, max_line_length);
    let truncated = end < total_lines;
    let next = if truncated {
        format!("\nnextOffset: {}", end + 1)
    } else {
        String::new()
    };
    let partial = offset > 1 || truncated;
    store.record_file_read_state(
        &full_path.to_string_lossy(),
        &state.sha256,
        state.modified_unix_ms,
        state.bytes,
        partial,
        Some(&reader),
        reader_run_id.as_deref(),
    )?;
    Ok(format!(
        "path: {}\nmode: pdf_lines\nbytes: {}\nlines: {} sha256: {} modifiedUnixMs: {} offset: {} limit: {} showing: {}-{} truncated: {}\nextractor: best_effort_pdf_text{}\n\n{}",
        full_path.display(),
        state.bytes,
        total_lines,
        state.sha256,
        state.modified_unix_ms,
        offset,
        limit,
        if total_lines == 0 { 0 } else { start + 1 },
        end,
        truncated,
        next,
        body
    ) + loop_warning.as_deref().unwrap_or(""))
}

fn extract_pdf_text_best_effort(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut output = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.ends_with(" Tj") || trimmed.ends_with(" TJ") {
            output.extend(extract_pdf_strings_from_line(trimmed));
        }
    }
    output.join("\n")
}

fn extract_pdf_strings_from_line(line: &str) -> Vec<String> {
    let mut values = Vec::new();
    let chars = line.chars().collect::<Vec<_>>();
    let mut index = 0usize;
    while index < chars.len() {
        match chars[index] {
            '(' => {
                let (value, next) = parse_pdf_literal_string(&chars, index + 1);
                if !value.is_empty() {
                    values.push(value);
                }
                index = next;
            }
            '<' if index + 1 < chars.len() && chars[index + 1] != '<' => {
                let (value, next) = parse_pdf_hex_string(&chars, index + 1);
                if !value.is_empty() {
                    values.push(value);
                }
                index = next;
            }
            _ => index += 1,
        }
    }
    values
}

fn parse_pdf_literal_string(chars: &[char], mut index: usize) -> (String, usize) {
    let mut value = String::new();
    let mut depth = 1usize;
    while index < chars.len() {
        let ch = chars[index];
        if ch == '\\' {
            if index + 1 >= chars.len() {
                break;
            }
            let escaped = chars[index + 1];
            match escaped {
                'n' => value.push('\n'),
                'r' => value.push('\r'),
                't' => value.push('\t'),
                'b' => value.push('\u{0008}'),
                'f' => value.push('\u{000c}'),
                '(' | ')' | '\\' => value.push(escaped),
                _ => value.push(escaped),
            }
            index += 2;
            continue;
        }
        if ch == '(' {
            depth += 1;
            value.push(ch);
            index += 1;
            continue;
        }
        if ch == ')' {
            depth -= 1;
            index += 1;
            if depth == 0 {
                break;
            }
            value.push(ch);
            continue;
        }
        value.push(ch);
        index += 1;
    }
    (value, index)
}

fn parse_pdf_hex_string(chars: &[char], mut index: usize) -> (String, usize) {
    let mut hex = String::new();
    while index < chars.len() {
        let ch = chars[index];
        index += 1;
        if ch == '>' {
            break;
        }
        if ch.is_ascii_hexdigit() {
            hex.push(ch);
        }
    }
    if hex.len() % 2 == 1 {
        hex.push('0');
    }
    let bytes = hex
        .as_bytes()
        .chunks(2)
        .filter_map(|pair| std::str::from_utf8(pair).ok())
        .filter_map(|pair| u8::from_str_radix(pair, 16).ok())
        .collect::<Vec<_>>();
    (String::from_utf8_lossy(&bytes).to_string(), index)
}

struct FileState {
    sha256: String,
    modified_unix_ms: u128,
    bytes: usize,
}

fn file_state(path: &Path, content: &str) -> AppResult<FileState> {
    let metadata = fs::metadata(path)?;
    let modified_unix_ms = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    Ok(FileState {
        sha256: sha256_hex(content.as_bytes()),
        modified_unix_ms,
        bytes: content.len(),
    })
}

fn file_byte_state(path: &Path, content: &[u8]) -> AppResult<FileState> {
    let metadata = fs::metadata(path)?;
    let modified_unix_ms = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    Ok(FileState {
        sha256: sha256_hex(content),
        modified_unix_ms,
        bytes: content.len(),
    })
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn validate_expected_file_state(path: &Path, payload: &Value) -> AppResult<()> {
    let expected_sha = payload_string(
        payload,
        &[
            "expectedSha256",
            "expected_sha256",
            "ifMatchSha256",
            "if_match_sha256",
            "sha256",
        ],
    );
    let expected_modified = payload_u128(
        payload,
        &[
            "expectedModifiedUnixMs",
            "expected_modified_unix_ms",
            "ifMatchModifiedUnixMs",
            "if_match_modified_unix_ms",
            "modifiedUnixMs",
            "modified_unix_ms",
        ],
    );
    if expected_sha.is_none() && expected_modified.is_none() {
        return Ok(());
    }
    let content = fs::read_to_string(path).map_err(|error| {
        AppError::BadRequest(format!(
            "file state precondition failed for {}: cannot read current file ({error})",
            path.display()
        ))
    })?;
    let current = file_state(path, &content)?;
    if let Some(expected) = expected_sha {
        if current.sha256 != expected.trim().to_ascii_lowercase() {
            return Err(AppError::BadRequest(format!(
                "file state precondition failed for {}: expected sha256 {}, current sha256 {}. Re-read the file before modifying it.",
                path.display(),
                expected,
                current.sha256
            )));
        }
    }
    if let Some(expected) = expected_modified {
        if current.modified_unix_ms != expected {
            return Err(AppError::BadRequest(format!(
                "file state precondition failed for {}: expected modifiedUnixMs {}, current modifiedUnixMs {}. Re-read the file before modifying it.",
                path.display(),
                expected,
                current.modified_unix_ms
            )));
        }
    }
    Ok(())
}

fn payload_has_expected_file_state(payload: &Value) -> bool {
    payload_string(
        payload,
        &[
            "expectedSha256",
            "expected_sha256",
            "ifMatchSha256",
            "if_match_sha256",
            "sha256",
        ],
    )
    .is_some()
        || payload_u128(
            payload,
            &[
                "expectedModifiedUnixMs",
                "expected_modified_unix_ms",
                "ifMatchModifiedUnixMs",
                "if_match_modified_unix_ms",
                "modifiedUnixMs",
                "modified_unix_ms",
            ],
        )
        .is_some()
}

fn validate_registered_file_state(store: &AppStore, path: &Path, payload: &Value) -> AppResult<()> {
    if payload_has_expected_file_state(payload) {
        return Ok(());
    }
    let path_key = path.to_string_lossy().to_string();
    let Some(registered) = store.registered_file_state(&path_key)? else {
        return Ok(());
    };
    let content = fs::read_to_string(path).map_err(|error| {
        AppError::BadRequest(format!(
            "file registry stale check failed for {}: cannot read current file ({error}). Re-read the file before modifying it.",
            path.display()
        ))
    })?;
    let current = file_state(path, &content)?;
    if registered.sha256 != current.sha256
        || registered.modified_unix_ms != current.modified_unix_ms
        || registered.partial
    {
        return Err(AppError::BadRequest(format!(
            "file registry stale check failed for {}: lastReadSha256={} currentSha256={} lastReadModifiedUnixMs={} currentModifiedUnixMs={} partialRead={} lastReader={} lastReaderRunId={} lastWriter={} lastWriterRunId={}. Re-read the file before modifying it or pass explicit expectedSha256/expectedModifiedUnixMs from the latest read.",
            path.display(),
            registered.sha256,
            current.sha256,
            registered.modified_unix_ms,
            current.modified_unix_ms,
            registered.partial,
            registered.last_reader.as_deref().unwrap_or("<unknown>"),
            registered.last_reader_run_id.as_deref().unwrap_or("<none>"),
            registered.last_writer.as_deref().unwrap_or("<none>"),
            registered.last_writer_run_id.as_deref().unwrap_or("<none>")
        )));
    }
    Ok(())
}

fn record_registered_write(
    store: &AppStore,
    path: &Path,
    writer: &str,
    payload: &Value,
) -> AppResult<()> {
    let content = fs::read_to_string(path)?;
    let state = file_state(path, &content)?;
    let (actor, run_id) = file_state_actor(payload, writer);
    store.record_file_write_state(
        &path.to_string_lossy(),
        &state.sha256,
        state.modified_unix_ms,
        state.bytes,
        &actor,
        run_id.as_deref(),
    )
}

fn payload_string(payload: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| payload.get(*key))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn payload_u128(payload: &Value, keys: &[&str]) -> Option<u128> {
    keys.iter()
        .find_map(|key| payload.get(*key))
        .and_then(|value| {
            value
                .as_u64()
                .map(u128::from)
                .or_else(|| value.as_str()?.trim().parse::<u128>().ok())
        })
}

fn ensure_text_file_tool_path(path: &Path, tool_name: &str) -> AppResult<()> {
    if likely_binary(path) {
        return Err(AppError::BadRequest(format!(
            "{tool_name} refused binary or non-text file path: {}",
            path.display()
        )));
    }
    Ok(())
}

pub(super) fn search_files_tool(agent: &AgentDefinition, payload: &Value) -> AppResult<String> {
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
    let offset = payload
        .get("offset")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(10_000) as usize;
    let output_mode = payload
        .get("outputMode")
        .or_else(|| payload.get("output_mode"))
        .and_then(Value::as_str)
        .unwrap_or("content")
        .trim()
        .to_ascii_lowercase();
    let context_lines = payload
        .get("context")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(10) as usize;
    let file_glob = payload
        .get("fileGlob")
        .or_else(|| payload.get("file_glob"))
        .or_else(|| payload.get("glob"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
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
        return Err(AppError::BadRequest(
            "search_files requires a non-empty query".into(),
        ));
    }
    let loop_warning = track_file_tool_loop(
        payload,
        format!(
            "search:{query}:{target}:{}:{offset}:{limit}:{output_mode}:{context_lines}:{}",
            file_glob.as_deref().unwrap_or("<none>"),
            start.display()
        ),
        "search_files",
    )?;

    let mut checked = 0usize;
    let mut matches = Vec::new();
    let fetch_limit = limit.saturating_add(offset).min(10_000);
    search_recursive(
        &root,
        &start,
        &query,
        target,
        fetch_limit,
        max_files,
        file_glob.as_deref(),
        context_lines,
        &mut checked,
        &mut matches,
    )?;
    let rendered = render_search_matches(&matches, offset, limit, &output_mode);
    Ok(format!(
        "query: {query}\ntarget: {target}\nfileGlob: {}\noffset: {offset}\nlimit: {limit}\noutputMode: {output_mode}\ncontext: {context_lines}\ncheckedFiles: {checked}\nmatches: {}\n\n{}",
        file_glob.as_deref().unwrap_or("<none>"),
        matches.len(),
        rendered
    ) + loop_warning.as_deref().unwrap_or(""))
}

pub(super) fn write_file_tool(
    store: &AppStore,
    agent: &AgentDefinition,
    payload: &Value,
) -> AppResult<String> {
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
    ensure_text_file_tool_path(&full_path, "write_file")?;
    with_file_state_path_locks(&[full_path.as_path()], || {
        validate_expected_file_state(&full_path, payload)?;
        validate_registered_file_state(store, &full_path, payload)?;
        if let Some(parent) = full_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let pre_content = fs::read_to_string(&full_path).ok();
        let lsp_baseline = lsp_snapshot_baseline_blocking(&root, &full_path);
        let content = prepare_file_write_content(&full_path, content)?;
        fs::write(&full_path, &content)?;
        verify_text_write_landed(&full_path, &content)?;
        let mut post_write_warnings = Vec::new();
        if let Err(error) = record_registered_write(store, &full_path, "write_file", payload) {
            post_write_warnings.push(format!("file registry update failed after write landed: {error}"));
        }
        let edit_diagnostics = match edit_diagnostics_for_paths_with_baselines(
            agent,
            &root,
            &[full_path.clone()],
            |path| {
                if path == full_path.as_path() {
                    pre_content.as_deref()
                } else {
                    None
                }
            },
        ) {
            Ok(value) => value,
            Err(error) => {
                post_write_warnings
                    .push(format!("edit diagnostics failed after write landed: {error}"));
                json!({
                    "enabled": false,
                    "postWriteWarning": true,
                    "reason": error.to_string(),
                })
            }
        };
        let lsp_delta_diagnostics = lsp_delta_diagnostics_blocking(&root, &full_path);
        Ok(serde_json::to_string_pretty(&json!({
            "success": true,
            "tool": "write_file",
            "path": full_path.to_string_lossy(),
            "bytes_written": content.len(),
            "chars_written": content.chars().count(),
            "bytesWritten": content.len(),
            "charsWritten": content.chars().count(),
            "editDiagnostics": edit_diagnostics,
            "lspBaseline": lsp_baseline,
            "lspDeltaDiagnostics": lsp_delta_diagnostics,
            "postWriteWarnings": post_write_warnings
        }))?)
    })
}

pub(super) fn delete_file_tool(
    store: &AppStore,
    agent: &AgentDefinition,
    payload: &Value,
) -> AppResult<String> {
    let path = payload
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::BadRequest("delete_file requires payload.path".into()))?;
    let root = workspace_root(agent)?;
    let full_path = resolve_workspace_path(&root, path)?;
    ensure_text_file_tool_path(&full_path, "patch")?;
    with_file_state_path_locks(&[full_path.as_path()], || {
        validate_expected_file_state(&full_path, payload)?;
        validate_registered_file_state(store, &full_path, payload)?;
        if full_path.is_dir() {
            return Err(AppError::BadRequest(format!(
                "delete_file refuses to delete directories: {}",
                full_path.display()
            )));
        }
        let lsp_cleared_baseline = lsp_clear_baseline_for_path(&root, &full_path);
        fs::remove_file(&full_path)?;
        store.remove_file_state(&full_path.to_string_lossy())?;
        Ok(serde_json::to_string_pretty(&json!({
            "success": true,
            "tool": "delete_file",
            "path": full_path.to_string_lossy(),
            "deleted": true,
            "lspClearedBaseline": lsp_cleared_baseline
        }))?)
    })
}

pub(super) fn move_file_tool(
    store: &AppStore,
    agent: &AgentDefinition,
    payload: &Value,
) -> AppResult<String> {
    let src = payload
        .get("src")
        .or_else(|| payload.get("source"))
        .or_else(|| payload.get("from"))
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::BadRequest("move_file requires payload.src".into()))?;
    let dst = payload
        .get("dst")
        .or_else(|| payload.get("target"))
        .or_else(|| payload.get("to"))
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::BadRequest("move_file requires payload.dst".into()))?;
    let root = workspace_root(agent)?;
    let source = resolve_workspace_path(&root, src)?;
    let target = resolve_workspace_target_path(&root, dst)?;
    with_file_state_path_locks(&[source.as_path(), target.as_path()], || {
        validate_expected_file_state(&source, payload)?;
        validate_registered_file_state(store, &source, payload)?;
        if source.is_dir() {
            return Err(AppError::BadRequest(format!(
                "move_file refuses to move directories: {}",
                source.display()
            )));
        }
        let lsp_baseline = lsp_snapshot_baseline_blocking(&root, &source);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::rename(&source, &target)?;
        store.remove_file_state(&source.to_string_lossy())?;
        record_registered_write(store, &target, "move_file", payload)?;
        let lsp_cleared_baseline = lsp_clear_baseline_for_path(&root, &source);
        let lsp_delta_diagnostics = lsp_delta_diagnostics_blocking(&root, &target);
        Ok(serde_json::to_string_pretty(&json!({
            "success": true,
            "tool": "move_file",
            "src": source.to_string_lossy(),
            "dst": target.to_string_lossy(),
            "moved": true,
            "lspBaseline": lsp_baseline,
            "lspClearedBaseline": lsp_cleared_baseline,
            "lspDeltaDiagnostics": lsp_delta_diagnostics
        }))?)
    })
}

pub(super) fn patch_tool(
    store: &AppStore,
    agent: &AgentDefinition,
    payload: &Value,
) -> AppResult<String> {
    let mode = payload
        .get("mode")
        .and_then(Value::as_str)
        .unwrap_or("replace");
    if mode == "patch" || payload.get("patch").is_some() {
        return patch_v4a_tool(store, agent, payload);
    }
    let path = payload
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::BadRequest("patch requires payload.path".into()))?;
    let root = workspace_root(agent)?;
    let full_path = resolve_workspace_path(&root, path)?;
    with_file_state_path_locks(&[full_path.as_path()], || {
        validate_expected_file_state(&full_path, payload)?;
        validate_registered_file_state(store, &full_path, payload)?;
        let pre_content = fs::read_to_string(&full_path)?;
        let lsp_baseline = lsp_snapshot_baseline_blocking(&root, &full_path);
        let mut content = pre_content.clone();
        if content.starts_with('\u{feff}') {
            content = strip_utf8_bom_str(&content).to_string();
        }
        let replacements = normalized_replacements(payload)?;
        let mut applied = 0usize;
        for (search, replace, replace_all) in replacements {
            let matches = fuzzy_find_matches(&content, &search);
            if matches.is_empty() {
                let failure_count = record_patch_failure(payload, &full_path)?;
                return Err(AppError::BadRequest(format!(
                    "patch search text was not found in {}{}{}",
                    full_path.display(),
                    format_patch_no_match_hint(&search, &content),
                    patch_failure_escalation_hint(failure_count)
                )));
            }
            if matches.len() > 1 && !replace_all {
                let failure_count = record_patch_failure(payload, &full_path)?;
                return Err(AppError::BadRequest(format!(
                    "patch search text matched {} locations in {}; provide more context or set replaceAll=true{}",
                    matches.len(),
                    full_path.display(),
                    patch_failure_escalation_hint(failure_count)
                )));
            }
            content = apply_fuzzy_replacements(&content, &matches, &search, &replace);
            applied += matches.len();
        }
        let content = prepare_file_write_content(&full_path, &content)?;
        fs::write(&full_path, &content)?;
        verify_text_write_landed(&full_path, &content)?;
        record_registered_write(store, &full_path, "patch", payload)?;
        reset_patch_failures(payload, &[full_path.as_path()]);
        let edit_diagnostics = edit_diagnostics_for_paths_with_baselines(
            agent,
            &root,
            &[full_path.clone()],
            |path| {
                if path == full_path.as_path() {
                    Some(pre_content.as_str())
                } else {
                    None
                }
            },
        )?;
        let lsp_delta_diagnostics = lsp_delta_diagnostics_blocking(&root, &full_path);
        Ok(serde_json::to_string_pretty(&json!({
            "success": true,
            "tool": "patch",
            "path": full_path.to_string_lossy(),
            "replacementsApplied": applied,
            "editDiagnostics": edit_diagnostics,
            "lspBaseline": lsp_baseline,
            "lspDeltaDiagnostics": lsp_delta_diagnostics
        }))?)
    })
}

pub(super) fn normalized_replacements(payload: &Value) -> AppResult<Vec<(String, String, bool)>> {
    if let Some(items) = payload.get("replacements").and_then(Value::as_array) {
        let mut replacements = Vec::new();
        for item in items {
            let search = item
                .get("search")
                .or_else(|| item.get("old"))
                .or_else(|| item.get("old_string"))
                .or_else(|| item.get("oldString"))
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    AppError::BadRequest("each patch replacement requires search".into())
                })?;
            let replace = item
                .get("replace")
                .or_else(|| item.get("new"))
                .or_else(|| item.get("new_string"))
                .or_else(|| item.get("newString"))
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    AppError::BadRequest("each patch replacement requires replace".into())
                })?;
            let replace_all = item
                .get("replaceAll")
                .or_else(|| item.get("replace_all"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            replacements.push(normalized_replacement_tuple(search, replace, replace_all)?);
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
        .or_else(|| payload.get("old_string"))
        .or_else(|| payload.get("oldString"))
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::BadRequest("patch requires search/replace".into()))?;
    let replace = payload
        .get("replace")
        .or_else(|| payload.get("new"))
        .or_else(|| payload.get("new_string"))
        .or_else(|| payload.get("newString"))
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::BadRequest("patch requires search/replace".into()))?;
    let replace_all = payload
        .get("replaceAll")
        .or_else(|| payload.get("replace_all"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    Ok(vec![normalized_replacement_tuple(
        search,
        replace,
        replace_all,
    )?])
}

fn normalized_replacement_tuple(
    search: &str,
    replace: &str,
    replace_all: bool,
) -> AppResult<(String, String, bool)> {
    if search.is_empty() {
        return Err(AppError::BadRequest(
            "patch search text cannot be empty".into(),
        ));
    }
    if search == replace {
        return Err(AppError::BadRequest(
            "patch search and replace text are identical".into(),
        ));
    }
    Ok((search.to_string(), replace.to_string(), replace_all))
}

#[derive(Debug, Clone)]
pub(super) enum V4aPatchOp {
    Add {
        path: String,
        lines: Vec<String>,
    },
    Update {
        path: String,
        move_to: Option<String>,
        hunks: Vec<V4aHunk>,
    },
    Delete {
        path: String,
    },
    Move {
        path: String,
        to: String,
    },
}

#[derive(Debug, Clone)]
pub(super) struct V4aHunk {
    pub(super) hint: Option<String>,
    pub(super) lines: Vec<(char, String)>,
}

fn patch_v4a_tool(store: &AppStore, agent: &AgentDefinition, payload: &Value) -> AppResult<String> {
    let body = payload
        .get("patch")
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::BadRequest("patch mode requires payload.patch".into()))?;
    let operations = parse_v4a_patch(body)?;
    if operations.is_empty() {
        return Err(AppError::BadRequest(
            "patch mode received no operations".into(),
        ));
    }
    let root = workspace_root(agent)?;
    let mut report = V4aPatchReport::default();
    validate_v4a_operations(&root, &operations)?;
    validate_v4a_expected_file_states(&root, &operations, payload)?;
    validate_v4a_registered_file_states(store, &root, &operations, payload)?;
    for operation in operations {
        apply_v4a_operation(store, &root, operation, payload, &mut report)?;
    }
    let diagnostic_paths = report.diagnostic_paths(&root)?;
    let edit_diagnostics =
        edit_diagnostics_for_paths_with_baselines(agent, &root, &diagnostic_paths, |path| {
            report.baseline_for(path)
        })?;
    Ok(serde_json::to_string_pretty(&json!({
        "success": true,
        "tool": "patch",
        "mode": "patch",
        "filesModified": report.modified,
        "filesCreated": report.created,
        "filesDeleted": report.deleted,
        "filesMoved": report.moved,
        "operationsApplied": report.operations,
        "editDiagnostics": edit_diagnostics,
        "lspBaselines": report.lsp_baselines,
        "lspDeltaDiagnostics": report.lsp_delta_diagnostics,
        "lspClearedBaselines": report.lsp_cleared_baselines
    }))?)
}

#[derive(Default)]
struct V4aPatchReport {
    modified: Vec<String>,
    created: Vec<String>,
    deleted: Vec<String>,
    moved: Vec<String>,
    baselines: BTreeMap<String, String>,
    lsp_baselines: Vec<Value>,
    lsp_delta_diagnostics: Vec<Value>,
    lsp_cleared_baselines: Vec<Value>,
    operations: usize,
}

impl V4aPatchReport {
    fn diagnostic_paths(&self, root: &Path) -> AppResult<Vec<PathBuf>> {
        let mut paths = Vec::new();
        for path in self.modified.iter().chain(self.created.iter()) {
            paths.push(resolve_workspace_path(root, path)?);
        }
        for item in &self.moved {
            let target = item
                .split_once(" -> ")
                .map(|(_, to)| to)
                .unwrap_or(item.as_str());
            paths.push(resolve_workspace_path(root, target)?);
        }
        paths.sort();
        paths.dedup();
        Ok(paths)
    }

    fn record_baseline(&mut self, path: &Path, content: &str) {
        self.baselines
            .entry(normalize_diagnostic_path_key(path))
            .or_insert_with(|| content.to_string());
    }

    fn baseline_for(&self, path: &Path) -> Option<&str> {
        self.baselines
            .get(&normalize_diagnostic_path_key(path))
            .map(String::as_str)
    }

    fn record_lsp_baseline(&mut self, value: Value) {
        self.lsp_baselines.push(value);
    }

    fn record_lsp_delta(&mut self, value: Value) {
        self.lsp_delta_diagnostics.push(value);
    }

    fn record_lsp_clear(&mut self, value: Value) {
        self.lsp_cleared_baselines.push(value);
    }
}

fn normalize_diagnostic_path_key(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn prepare_file_write_content(path: &Path, content: &str) -> AppResult<String> {
    let existing = fs::read_to_string(path).ok();
    let mut output = content.to_string();
    if existing
        .as_deref()
        .and_then(detect_line_ending)
        .is_some_and(|ending| ending == "\r\n")
    {
        output = normalize_line_endings(&output, "\r\n");
    }
    if existing
        .as_deref()
        .is_some_and(|value| value.starts_with('\u{feff}'))
        && !output.starts_with('\u{feff}')
    {
        output.insert(0, '\u{feff}');
    }
    Ok(output)
}

fn verify_text_write_landed(path: &Path, intended: &str) -> AppResult<()> {
    let actual = fs::read_to_string(path).map_err(|error| {
        AppError::BadRequest(format!(
            "post-write verification failed: could not re-read {}: {error}",
            path.display()
        ))
    })?;
    let actual_normalized = normalize_text_for_write_verification(&actual);
    let intended_normalized = normalize_text_for_write_verification(intended);
    if actual_normalized == intended_normalized {
        Ok(())
    } else {
        Err(AppError::BadRequest(format!(
            "post-write verification failed for {}: on-disk content differs from intended write (wrote {} chars, read back {} chars after normalizing line endings and BOM). Re-read the file and try again.",
            path.display(),
            intended_normalized.chars().count(),
            actual_normalized.chars().count()
        )))
    }
}

fn normalize_text_for_write_verification(content: &str) -> String {
    normalize_line_endings(strip_utf8_bom_str(content), "\n")
}

fn detect_line_ending(content: &str) -> Option<&'static str> {
    if content.contains("\r\n") {
        Some("\r\n")
    } else if content.contains('\n') {
        Some("\n")
    } else {
        None
    }
}

fn normalize_line_endings(content: &str, ending: &str) -> String {
    let normalized = content.replace("\r\n", "\n").replace('\r', "\n");
    if ending == "\n" {
        normalized
    } else {
        normalized.replace('\n', ending)
    }
}

fn strip_utf8_bom_str(content: &str) -> &str {
    content.strip_prefix('\u{feff}').unwrap_or(content)
}

fn parse_v4a_patch(body: &str) -> AppResult<Vec<V4aPatchOp>> {
    let lines = body.lines().collect::<Vec<_>>();
    let start = lines
        .iter()
        .position(|line| line.trim().starts_with("*** Begin Patch"))
        .map(|index| index + 1)
        .unwrap_or(0);
    let end = lines
        .iter()
        .position(|line| line.trim().starts_with("*** End Patch"))
        .unwrap_or(lines.len());
    let mut ops = Vec::new();
    let mut current: Option<V4aPatchOp> = None;
    let mut current_hunk: Option<V4aHunk> = None;

    let flush_hunk = |operation: &mut Option<V4aPatchOp>, hunk: &mut Option<V4aHunk>| {
        if let (Some(V4aPatchOp::Update { hunks, .. }), Some(done)) =
            (operation.as_mut(), hunk.take())
        {
            if !done.lines.is_empty() {
                hunks.push(done);
            }
        }
    };
    let flush_op = |ops: &mut Vec<V4aPatchOp>,
                    operation: &mut Option<V4aPatchOp>,
                    hunk: &mut Option<V4aHunk>| {
        flush_hunk(operation, hunk);
        if let Some(done) = operation.take() {
            ops.push(done);
        }
    };

    for raw in lines.into_iter().take(end).skip(start) {
        let line = raw.trim_end_matches('\r');
        if let Some(path) = line.strip_prefix("*** Update File:") {
            flush_op(&mut ops, &mut current, &mut current_hunk);
            current = Some(V4aPatchOp::Update {
                path: path.trim().to_string(),
                move_to: None,
                hunks: Vec::new(),
            });
            continue;
        }
        if let Some(path) = line.strip_prefix("*** Add File:") {
            flush_op(&mut ops, &mut current, &mut current_hunk);
            current = Some(V4aPatchOp::Add {
                path: path.trim().to_string(),
                lines: Vec::new(),
            });
            continue;
        }
        if let Some(path) = line.strip_prefix("*** Delete File:") {
            flush_op(&mut ops, &mut current, &mut current_hunk);
            ops.push(V4aPatchOp::Delete {
                path: path.trim().to_string(),
            });
            continue;
        }
        if let Some(rest) = line.strip_prefix("*** Move File:") {
            flush_op(&mut ops, &mut current, &mut current_hunk);
            let Some((from, to)) = rest.split_once("->") else {
                return Err(AppError::BadRequest(
                    "Move File requires 'source -> destination'".into(),
                ));
            };
            ops.push(V4aPatchOp::Move {
                path: from.trim().to_string(),
                to: to.trim().to_string(),
            });
            continue;
        }
        if let Some(to) = line.strip_prefix("*** Move to:") {
            match current.as_mut() {
                Some(V4aPatchOp::Update { move_to, .. }) => *move_to = Some(to.trim().to_string()),
                _ => {
                    return Err(AppError::BadRequest(
                        "Move to must follow an Update File operation".into(),
                    ));
                }
            }
            continue;
        }
        if line.starts_with("@@") {
            flush_hunk(&mut current, &mut current_hunk);
            let hint = line
                .trim_matches('@')
                .trim()
                .is_empty()
                .then_some(None)
                .unwrap_or_else(|| Some(line.trim_matches('@').trim().to_string()));
            current_hunk = Some(V4aHunk {
                hint,
                lines: Vec::new(),
            });
            continue;
        }

        match current.as_mut() {
            Some(V4aPatchOp::Add { lines, .. }) => {
                if let Some(value) = line.strip_prefix('+') {
                    lines.push(value.to_string());
                } else if !line.starts_with('\\') && !line.is_empty() {
                    lines.push(line.to_string());
                }
            }
            Some(V4aPatchOp::Update { .. }) => {
                if current_hunk.is_none() {
                    current_hunk = Some(V4aHunk {
                        hint: None,
                        lines: Vec::new(),
                    });
                }
                if let Some(hunk) = current_hunk.as_mut() {
                    if let Some(value) = line.strip_prefix('+') {
                        hunk.lines.push(('+', value.to_string()));
                    } else if let Some(value) = line.strip_prefix('-') {
                        hunk.lines.push(('-', value.to_string()));
                    } else if let Some(value) = line.strip_prefix(' ') {
                        hunk.lines.push((' ', value.to_string()));
                    } else if !line.starts_with('\\') && !line.is_empty() {
                        hunk.lines.push((' ', line.to_string()));
                    }
                }
            }
            _ => {}
        }
    }
    flush_op(&mut ops, &mut current, &mut current_hunk);

    for op in &ops {
        match op {
            V4aPatchOp::Add { path, .. }
            | V4aPatchOp::Update { path, .. }
            | V4aPatchOp::Delete { path }
            | V4aPatchOp::Move { path, .. }
                if path.trim().is_empty() =>
            {
                return Err(AppError::BadRequest(
                    "patch operation has empty path".into(),
                ));
            }
            V4aPatchOp::Update { path, hunks, .. } if hunks.is_empty() => {
                return Err(AppError::BadRequest(format!(
                    "Update File {path} has no hunks"
                )));
            }
            V4aPatchOp::Move { to, .. } if to.trim().is_empty() => {
                return Err(AppError::BadRequest(
                    "Move File has empty destination".into(),
                ));
            }
            _ => {}
        }
    }
    Ok(ops)
}

fn validate_v4a_operations(root: &Path, operations: &[V4aPatchOp]) -> AppResult<()> {
    let mut errors = Vec::new();
    for op in operations {
        match op {
            V4aPatchOp::Add { path, .. } => {
                if let Err(error) = resolve_workspace_target_path(root, path) {
                    errors.push(format!("{path}: {error}"));
                }
            }
            V4aPatchOp::Update {
                path,
                move_to,
                hunks,
            } => {
                match resolve_workspace_path(root, path)
                    .and_then(|full| fs::read_to_string(full).map_err(AppError::from))
                    .and_then(|content| apply_v4a_hunks_to_content(&content, hunks))
                {
                    Ok(_) => {}
                    Err(error) => errors.push(format!("{path}: {error}")),
                }
                if let Some(to) = move_to {
                    if let Err(error) = resolve_workspace_target_path(root, to) {
                        errors.push(format!("{to}: {error}"));
                    }
                }
            }
            V4aPatchOp::Delete { path } => {
                if let Err(error) = resolve_workspace_path(root, path) {
                    errors.push(format!("{path}: {error}"));
                }
            }
            V4aPatchOp::Move { path, to } => {
                if let Err(error) = resolve_workspace_path(root, path) {
                    errors.push(format!("{path}: {error}"));
                }
                if let Err(error) = resolve_workspace_target_path(root, to) {
                    errors.push(format!("{to}: {error}"));
                }
            }
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(AppError::BadRequest(format!(
            "Patch validation failed (no files were modified):\n{}",
            errors.join("\n")
        )))
    }
}

fn validate_v4a_expected_file_states(
    root: &Path,
    operations: &[V4aPatchOp],
    payload: &Value,
) -> AppResult<()> {
    for op in operations {
        let Some(path) = v4a_existing_source_path(op) else {
            continue;
        };
        let full = resolve_workspace_path(root, path)?;
        if let Some(expected) = v4a_expected_state_payload(payload, path, &full, operations.len()) {
            validate_expected_file_state(&full, &expected)?;
        }
    }
    Ok(())
}

fn validate_v4a_registered_file_states(
    store: &AppStore,
    root: &Path,
    operations: &[V4aPatchOp],
    payload: &Value,
) -> AppResult<()> {
    if payload
        .get("expectedFileStates")
        .or_else(|| payload.get("expected_file_states"))
        .or_else(|| payload.get("ifMatchFileStates"))
        .or_else(|| payload.get("if_match_file_states"))
        .is_some()
    {
        return Ok(());
    }
    for op in operations {
        if let Some(path) = v4a_existing_source_path(op) {
            let full = resolve_workspace_path(root, path)?;
            validate_registered_file_state(store, &full, payload)?;
        }
    }
    Ok(())
}

fn v4a_existing_source_path(op: &V4aPatchOp) -> Option<&str> {
    match op {
        V4aPatchOp::Update { path, .. }
        | V4aPatchOp::Delete { path }
        | V4aPatchOp::Move { path, .. } => Some(path.as_str()),
        V4aPatchOp::Add { .. } => None,
    }
}

fn v4a_expected_state_payload(
    payload: &Value,
    patch_path: &str,
    full_path: &Path,
    operation_count: usize,
) -> Option<Value> {
    let states = payload
        .get("expectedFileStates")
        .or_else(|| payload.get("expected_file_states"))
        .or_else(|| payload.get("ifMatchFileStates"))
        .or_else(|| payload.get("if_match_file_states"))
        .and_then(Value::as_object);
    if let Some(states) = states {
        let normalized_patch_path = patch_path.replace('\\', "/");
        let full_string = full_path.to_string_lossy().to_string();
        let normalized_full = full_string.replace('\\', "/");
        for key in [
            patch_path,
            normalized_patch_path.as_str(),
            full_string.as_str(),
            normalized_full.as_str(),
        ] {
            if let Some(value) = states.get(key) {
                return Some(normalize_expected_state_entry(value));
            }
        }
    }
    if operation_count == 1
        && (payload_string(
            payload,
            &[
                "expectedSha256",
                "expected_sha256",
                "ifMatchSha256",
                "if_match_sha256",
                "sha256",
            ],
        )
        .is_some()
            || payload_u128(
                payload,
                &[
                    "expectedModifiedUnixMs",
                    "expected_modified_unix_ms",
                    "ifMatchModifiedUnixMs",
                    "if_match_modified_unix_ms",
                    "modifiedUnixMs",
                    "modified_unix_ms",
                ],
            )
            .is_some())
    {
        return Some(payload.clone());
    }
    None
}

fn normalize_expected_state_entry(value: &Value) -> Value {
    if let Some(sha) = value.as_str().map(str::trim).filter(|sha| !sha.is_empty()) {
        return json!({"expectedSha256": sha});
    }
    value.clone()
}

fn apply_v4a_operation(
    store: &AppStore,
    root: &Path,
    op: V4aPatchOp,
    payload: &Value,
    report: &mut V4aPatchReport,
) -> AppResult<()> {
    match op {
        V4aPatchOp::Add { path, lines } => {
            let full = resolve_workspace_target_path(root, &path)?;
            ensure_text_file_tool_path(&full, "patch")?;
            with_file_state_path_locks(&[full.as_path()], || {
                if let Some(parent) = full.parent() {
                    fs::create_dir_all(parent)?;
                }
                let next = prepare_file_write_content(&full, &lines.join("\n"))?;
                fs::write(&full, &next)?;
                verify_text_write_landed(&full, &next)?;
                record_registered_write(store, &full, "patch", payload)?;
                report.created.push(full.to_string_lossy().to_string());
                report.record_lsp_delta(lsp_delta_diagnostics_blocking(root, &full));
                Ok(())
            })?;
        }
        V4aPatchOp::Update {
            path,
            move_to,
            hunks,
        } => {
            let full = resolve_workspace_path(root, &path)?;
            ensure_text_file_tool_path(&full, "patch")?;
            if let Some(to) = move_to {
                let target = resolve_workspace_target_path(root, &to)?;
                ensure_text_file_tool_path(&target, "patch")?;
                with_file_state_path_locks(&[full.as_path(), target.as_path()], || {
                    let content = fs::read_to_string(&full)?;
                    report.record_lsp_baseline(lsp_snapshot_baseline_blocking(root, &full));
                    let match_content = strip_utf8_bom_str(&content);
                    let next = apply_v4a_hunks_to_content(match_content, &hunks)?;
                    let next = prepare_file_write_content(&full, &next)?;
                    fs::write(&full, &next)?;
                    verify_text_write_landed(&full, &next)?;
                    if let Some(parent) = target.parent() {
                        fs::create_dir_all(parent)?;
                    }
                    fs::rename(&full, &target)?;
                    store.remove_file_state(&full.to_string_lossy())?;
                    record_registered_write(store, &target, "patch", payload)?;
                    report.record_baseline(&target, &content);
                    report.record_lsp_clear(lsp_clear_baseline_for_path(root, &full));
                    report.record_lsp_delta(lsp_delta_diagnostics_blocking(root, &target));
                    report
                        .moved
                        .push(format!("{} -> {}", full.display(), target.display()));
                    Ok(())
                })?;
            } else {
                with_file_state_path_locks(&[full.as_path()], || {
                    let content = fs::read_to_string(&full)?;
                    report.record_lsp_baseline(lsp_snapshot_baseline_blocking(root, &full));
                    let match_content = strip_utf8_bom_str(&content);
                    let next = apply_v4a_hunks_to_content(match_content, &hunks)?;
                    let next = prepare_file_write_content(&full, &next)?;
                    fs::write(&full, &next)?;
                    verify_text_write_landed(&full, &next)?;
                    record_registered_write(store, &full, "patch", payload)?;
                    report.record_baseline(&full, &content);
                    report.record_lsp_delta(lsp_delta_diagnostics_blocking(root, &full));
                    report.modified.push(full.to_string_lossy().to_string());
                    Ok(())
                })?;
            }
        }
        V4aPatchOp::Delete { path } => {
            let full = resolve_workspace_path(root, &path)?;
            with_file_state_path_locks(&[full.as_path()], || {
                fs::remove_file(&full)?;
                store.remove_file_state(&full.to_string_lossy())?;
                report.record_lsp_clear(lsp_clear_baseline_for_path(root, &full));
                report.deleted.push(full.to_string_lossy().to_string());
                Ok(())
            })?;
        }
        V4aPatchOp::Move { path, to } => {
            let full = resolve_workspace_path(root, &path)?;
            let target = resolve_workspace_target_path(root, &to)?;
            ensure_text_file_tool_path(&full, "patch")?;
            ensure_text_file_tool_path(&target, "patch")?;
            with_file_state_path_locks(&[full.as_path(), target.as_path()], || {
                let content = fs::read_to_string(&full)?;
                report.record_lsp_baseline(lsp_snapshot_baseline_blocking(root, &full));
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::rename(&full, &target)?;
                store.remove_file_state(&full.to_string_lossy())?;
                record_registered_write(store, &target, "patch", payload)?;
                report.record_baseline(&target, &content);
                report.record_lsp_clear(lsp_clear_baseline_for_path(root, &full));
                report.record_lsp_delta(lsp_delta_diagnostics_blocking(root, &target));
                report
                    .moved
                    .push(format!("{} -> {}", full.display(), target.display()));
                Ok(())
            })?;
        }
    }
    report.operations += 1;
    Ok(())
}

pub(super) fn apply_v4a_hunks_to_content(content: &str, hunks: &[V4aHunk]) -> AppResult<String> {
    let mut next = content.to_string();
    for hunk in hunks {
        let search = hunk
            .lines
            .iter()
            .filter(|(prefix, _)| *prefix == ' ' || *prefix == '-')
            .map(|(_, line)| line.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let replacement = hunk
            .lines
            .iter()
            .filter(|(prefix, _)| *prefix == ' ' || *prefix == '+')
            .map(|(_, line)| line.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        if search.is_empty() {
            let insert = if replacement.ends_with('\n') {
                replacement
            } else {
                format!("{replacement}\n")
            };
            if let Some(hint) = hunk.hint.as_deref().filter(|hint| !hint.is_empty()) {
                let count = next.matches(hint).count();
                if count > 1 {
                    return Err(AppError::BadRequest(format!(
                        "addition-only hunk context hint '{hint}' is ambiguous"
                    )));
                }
                if let Some(pos) = next.find(hint) {
                    let insert_at = next[pos..]
                        .find('\n')
                        .map(|offset| pos + offset + 1)
                        .unwrap_or(next.len());
                    next.insert_str(insert_at, &insert);
                    continue;
                }
            }
            if !next.ends_with('\n') {
                next.push('\n');
            }
            next.push_str(&insert);
            continue;
        }
        let matches = fuzzy_find_matches(&next, &search);
        if matches.is_empty() {
            return Err(AppError::BadRequest(format!(
                "patch hunk not found{}{}",
                hunk.hint
                    .as_deref()
                    .map(|hint| format!(" near '{hint}'"))
                    .unwrap_or_default(),
                format_patch_no_match_hint(&search, &next)
            )));
        }
        if matches.len() > 1 {
            return Err(AppError::BadRequest(format!(
                "patch hunk matched {} locations{}; provide more context",
                matches.len(),
                hunk.hint
                    .as_deref()
                    .map(|hint| format!(" near '{hint}'"))
                    .unwrap_or_default()
            )));
        }
        next = apply_fuzzy_replacements(&next, &matches, &search, &replacement);
    }
    Ok(next)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FuzzyTextMatch {
    start: usize,
    end: usize,
    strategy: &'static str,
}

fn fuzzy_find_matches(content: &str, pattern: &str) -> Vec<FuzzyTextMatch> {
    if pattern.is_empty() {
        return Vec::new();
    }
    let strategies: [fn(&str, &str) -> Vec<FuzzyTextMatch>; 7] = [
        fuzzy_exact_matches,
        fuzzy_line_trimmed_matches,
        fuzzy_whitespace_normalized_matches,
        fuzzy_indentation_flexible_matches,
        fuzzy_escape_normalized_matches,
        fuzzy_trimmed_boundary_matches,
        fuzzy_unicode_normalized_matches,
    ];
    for strategy in strategies {
        let matches = strategy(content, pattern);
        if !matches.is_empty() {
            return matches;
        }
    }
    Vec::new()
}

fn format_patch_no_match_hint(search: &str, content: &str) -> String {
    let hint = find_closest_patch_lines(search, content, 2, 3);
    if hint.is_empty() {
        String::new()
    } else {
        format!("\n\nDid you mean one of these sections?\n{hint}")
    }
}

fn find_closest_patch_lines(
    search: &str,
    content: &str,
    context_lines: usize,
    max_results: usize,
) -> String {
    let anchor = search
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("");
    if anchor.is_empty() || content.trim().is_empty() {
        return String::new();
    }
    let lines = content.lines().collect::<Vec<_>>();
    if lines.is_empty() {
        return String::new();
    }
    let search_line_count = search.lines().count().max(1);
    let mut scored = lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| {
            let score = patch_line_similarity(anchor, line.trim());
            (score > 0.30).then_some((score, index))
        })
        .collect::<Vec<_>>();
    scored.sort_by(|left, right| {
        right
            .0
            .partial_cmp(&left.0)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut snippets = Vec::new();
    let mut seen = BTreeSet::new();
    for (_, line_index) in scored.into_iter().take(max_results) {
        let start = line_index.saturating_sub(context_lines);
        let end = (line_index + search_line_count + context_lines).min(lines.len());
        if !seen.insert((start, end)) {
            continue;
        }
        let snippet = (start..end)
            .map(|index| format!("{:4}| {}", index + 1, lines[index]))
            .collect::<Vec<_>>()
            .join("\n");
        snippets.push(snippet);
    }
    snippets.join("\n---\n")
}

fn patch_line_similarity(expected: &str, candidate: &str) -> f64 {
    if expected.is_empty() || candidate.is_empty() {
        return 0.0;
    }
    if expected == candidate {
        return 1.0;
    }
    if expected.contains(candidate) || candidate.contains(expected) {
        return 0.85;
    }
    let expected_tokens = patch_similarity_tokens(expected);
    let candidate_tokens = patch_similarity_tokens(candidate);
    if expected_tokens.is_empty() || candidate_tokens.is_empty() {
        return 0.0;
    }
    let common = expected_tokens.intersection(&candidate_tokens).count() as f64;
    let union = expected_tokens.union(&candidate_tokens).count() as f64;
    common / union
}

fn patch_similarity_tokens(value: &str) -> BTreeSet<String> {
    value
        .split(|ch: char| !(ch.is_alphanumeric() || ch == '_'))
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(|part| part.to_ascii_lowercase())
        .collect()
}

fn fuzzy_exact_matches(content: &str, pattern: &str) -> Vec<FuzzyTextMatch> {
    let mut matches = Vec::new();
    let mut start = 0usize;
    while let Some(pos) = content[start..].find(pattern) {
        let absolute = start + pos;
        matches.push(FuzzyTextMatch {
            start: absolute,
            end: absolute + pattern.len(),
            strategy: "exact",
        });
        start = absolute.saturating_add(1);
        if start >= content.len() {
            break;
        }
    }
    matches
}

fn apply_fuzzy_replacements(
    content: &str,
    matches: &[FuzzyTextMatch],
    old_string: &str,
    new_string: &str,
) -> String {
    let mut result = content.to_string();
    for matched in matches.iter().rev() {
        let replacement = if matched.strategy == "exact" {
            new_string.to_string()
        } else {
            reindent_fuzzy_replacement(&content[matched.start..matched.end], old_string, new_string)
        };
        result.replace_range(matched.start..matched.end, &replacement);
    }
    result
}

fn fuzzy_line_trimmed_matches(content: &str, pattern: &str) -> Vec<FuzzyTextMatch> {
    line_window_matches(content, pattern, "line_trimmed", |line| {
        line.trim().to_string()
    })
}

fn fuzzy_whitespace_normalized_matches(content: &str, pattern: &str) -> Vec<FuzzyTextMatch> {
    line_window_matches(content, pattern, "whitespace_normalized", |line| {
        collapse_inline_whitespace(line)
    })
}

fn fuzzy_indentation_flexible_matches(content: &str, pattern: &str) -> Vec<FuzzyTextMatch> {
    line_window_matches(content, pattern, "indentation_flexible", |line| {
        line.trim_start_matches([' ', '\t']).to_string()
    })
}

fn fuzzy_escape_normalized_matches(content: &str, pattern: &str) -> Vec<FuzzyTextMatch> {
    let unescaped = pattern
        .replace("\\n", "\n")
        .replace("\\t", "\t")
        .replace("\\r", "\r");
    if unescaped == pattern {
        return Vec::new();
    }
    fuzzy_exact_matches(content, &unescaped)
        .into_iter()
        .map(|mut matched| {
            matched.strategy = "escape_normalized";
            matched
        })
        .collect()
}

fn fuzzy_trimmed_boundary_matches(content: &str, pattern: &str) -> Vec<FuzzyTextMatch> {
    let mut pattern_lines = split_patch_lines(pattern);
    if pattern_lines.is_empty() {
        return Vec::new();
    }
    let content_lines = line_spans_without_newlines(content);
    if pattern_lines.len() > content_lines.len() {
        return Vec::new();
    }
    let last = pattern_lines.len().saturating_sub(1);
    pattern_lines[0] = pattern_lines[0].trim().to_string();
    pattern_lines[last] = pattern_lines[last].trim().to_string();
    let mut matches = Vec::new();
    for start_line in 0..=content_lines.len() - pattern_lines.len() {
        let mut block_lines = content_lines[start_line..start_line + pattern_lines.len()]
            .iter()
            .map(|(_, _, line)| line.to_string())
            .collect::<Vec<_>>();
        block_lines[0] = block_lines[0].trim().to_string();
        block_lines[last] = block_lines[last].trim().to_string();
        if block_lines == pattern_lines {
            matches.push(FuzzyTextMatch {
                start: content_lines[start_line].0,
                end: content_lines[start_line + pattern_lines.len() - 1].1,
                strategy: "trimmed_boundary",
            });
        }
    }
    matches
}

fn fuzzy_unicode_normalized_matches(content: &str, pattern: &str) -> Vec<FuzzyTextMatch> {
    let normalized_content = normalize_common_unicode(content);
    let normalized_pattern = normalize_common_unicode(pattern);
    if normalized_content == content && normalized_pattern == pattern {
        return Vec::new();
    }
    let normalized_matches = fuzzy_exact_matches(&normalized_content, &normalized_pattern);
    if normalized_matches.is_empty() {
        return Vec::new();
    }
    let index_map = normalized_to_original_index_map(content);
    normalized_matches
        .into_iter()
        .filter_map(|matched| {
            let start = *index_map.get(matched.start)?;
            let end = *index_map.get(matched.end)?;
            Some(FuzzyTextMatch {
                start,
                end,
                strategy: "unicode_normalized",
            })
        })
        .collect()
}

fn line_window_matches<F>(
    content: &str,
    pattern: &str,
    strategy: &'static str,
    normalize: F,
) -> Vec<FuzzyTextMatch>
where
    F: Fn(&str) -> String,
{
    let content_lines = line_spans_without_newlines(content);
    let pattern_lines = split_patch_lines(pattern);
    if pattern_lines.is_empty() || pattern_lines.len() > content_lines.len() {
        return Vec::new();
    }
    let normalized_pattern = pattern_lines
        .iter()
        .map(|line| normalize(line))
        .collect::<Vec<_>>();
    let normalized_content = content_lines
        .iter()
        .map(|(_, _, line)| normalize(line))
        .collect::<Vec<_>>();
    let mut matches = Vec::new();
    let count = normalized_pattern.len();
    for start_line in 0..=normalized_content.len() - count {
        if normalized_content[start_line..start_line + count] == normalized_pattern[..] {
            matches.push(FuzzyTextMatch {
                start: content_lines[start_line].0,
                end: content_lines[start_line + count - 1].1,
                strategy,
            });
        }
    }
    matches
}

fn split_patch_lines(value: &str) -> Vec<String> {
    value
        .split('\n')
        .map(|line| line.trim_end_matches('\r').to_string())
        .collect()
}

fn line_spans_without_newlines(content: &str) -> Vec<(usize, usize, &str)> {
    let mut spans = Vec::new();
    let mut start = 0usize;
    for (idx, ch) in content.char_indices() {
        if ch == '\n' {
            let mut end = idx;
            if end > start && content.as_bytes()[end - 1] == b'\r' {
                end -= 1;
            }
            spans.push((start, end, &content[start..end]));
            start = idx + 1;
        }
    }
    let mut end = content.len();
    if end > start && content.as_bytes()[end - 1] == b'\r' {
        end -= 1;
    }
    spans.push((start, end, &content[start..end]));
    spans
}

fn collapse_inline_whitespace(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut in_space = false;
    for ch in line.chars() {
        if ch == ' ' || ch == '\t' {
            if !in_space {
                out.push(' ');
                in_space = true;
            }
        } else {
            out.push(ch);
            in_space = false;
        }
    }
    out
}

fn normalize_common_unicode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\u{201c}' | '\u{201d}' => out.push('"'),
            '\u{2018}' | '\u{2019}' => out.push('\''),
            '\u{2014}' => out.push_str("--"),
            '\u{2013}' => out.push('-'),
            '\u{2026}' => out.push_str("..."),
            '\u{00a0}' => out.push(' '),
            _ => out.push(ch),
        }
    }
    out
}

fn normalized_to_original_index_map(original: &str) -> Vec<usize> {
    let mut map = Vec::new();
    for (idx, ch) in original.char_indices() {
        let width = match ch {
            '\u{2014}' => 2,
            '\u{2026}' => 3,
            _ => 1,
        };
        for _ in 0..width {
            map.push(idx);
        }
    }
    map.push(original.len());
    map
}

fn reindent_fuzzy_replacement(file_region: &str, old_string: &str, new_string: &str) -> String {
    let Some(old_first) = first_meaningful_line(old_string) else {
        return maybe_unescape_fuzzy_replacement(file_region, new_string);
    };
    let Some(file_first) = first_meaningful_line(file_region) else {
        return maybe_unescape_fuzzy_replacement(file_region, new_string);
    };
    let old_indent = leading_whitespace(old_first);
    let file_indent = leading_whitespace(file_first);
    let unescaped = maybe_unescape_fuzzy_replacement(file_region, new_string);
    if old_indent == file_indent || unescaped.is_empty() {
        return unescaped;
    }
    let old_lines = split_patch_lines(old_string);
    let file_lines = split_patch_lines(file_region);
    unescaped
        .split('\n')
        .enumerate()
        .map(|(index, line)| {
            if line.trim().is_empty() {
                return line.to_string();
            }
            let old_reference = old_lines
                .get(index)
                .or_else(|| old_lines.iter().rev().find(|item| !item.trim().is_empty()));
            let file_reference = file_lines
                .get(index)
                .or_else(|| file_lines.iter().rev().find(|item| !item.trim().is_empty()));
            if let (Some(old_reference), Some(file_reference)) = (old_reference, file_reference) {
                let old_line_indent = leading_whitespace(old_reference);
                let file_line_indent = leading_whitespace(file_reference);
                if let Some(rest) = line.strip_prefix(old_line_indent) {
                    return format!("{file_line_indent}{rest}");
                }
            }
            if let Some(rest) = line.strip_prefix(old_indent) {
                format!("{file_indent}{rest}")
            } else {
                format!("{}{}", file_indent, line.trim_start_matches([' ', '\t']))
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn maybe_unescape_fuzzy_replacement(file_region: &str, new_string: &str) -> String {
    let mut out = new_string.to_string();
    if file_region.contains('\t') {
        out = out.replace("\\t", "\t");
    }
    if file_region.contains('\r') {
        out = out.replace("\\r", "\r");
    }
    out
}

fn first_meaningful_line(value: &str) -> Option<&str> {
    value.split('\n').find(|line| !line.trim().is_empty())
}

fn leading_whitespace(value: &str) -> &str {
    let end = value
        .char_indices()
        .find(|(_, ch)| *ch != ' ' && *ch != '\t')
        .map(|(idx, _)| idx)
        .unwrap_or(value.len());
    &value[..end]
}

fn search_recursive(
    root: &Path,
    dir: &Path,
    query: &str,
    target: &str,
    limit: usize,
    max_files: usize,
    file_glob: Option<&str>,
    context_lines: usize,
    checked: &mut usize,
    matches: &mut Vec<SearchMatch>,
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
            search_recursive(
                root,
                &path,
                query,
                target,
                limit,
                max_files,
                file_glob,
                context_lines,
                checked,
                matches,
            )?;
            continue;
        }
        *checked += 1;
        let rel = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .display()
            .to_string();
        if file_glob.is_some_and(|pattern| {
            !simple_glob_matches(pattern, &rel) && !simple_glob_matches(pattern, &name)
        }) {
            continue;
        }
        if target == "files" || target == "path" || target == "files_only" {
            if rel.to_lowercase().contains(&query.to_lowercase()) {
                matches.push(SearchMatch {
                    path: rel,
                    line: None,
                    text: None,
                    context: Vec::new(),
                });
            }
            continue;
        }
        if likely_binary(&path) {
            continue;
        }
        let Ok(content) = fs::read_to_string(&path) else {
            continue;
        };
        let query_lower = query.to_lowercase();
        let lines = content.lines().collect::<Vec<_>>();
        for (index, line) in lines.iter().enumerate() {
            if matches.len() >= limit {
                break;
            }
            if line.to_lowercase().contains(&query_lower) {
                matches.push(SearchMatch {
                    path: rel.clone(),
                    line: Some(index + 1),
                    text: Some(line.trim().to_string()),
                    context: search_context_lines(&lines, index, context_lines),
                });
            }
        }
    }
    Ok(())
}

#[derive(Debug, Clone)]
struct SearchMatch {
    path: String,
    line: Option<usize>,
    text: Option<String>,
    context: Vec<(usize, String)>,
}

fn render_search_matches(
    matches: &[SearchMatch],
    offset: usize,
    limit: usize,
    output_mode: &str,
) -> String {
    let visible = matches.iter().skip(offset).take(limit).collect::<Vec<_>>();
    if visible.is_empty() {
        return "(none)".into();
    }
    match output_mode {
        "files_only" | "files" => {
            let mut files = visible
                .iter()
                .map(|item| item.path.clone())
                .collect::<Vec<_>>();
            files.sort();
            files.dedup();
            files.join("\n")
        }
        "count" => {
            let mut counts: BTreeMap<String, usize> = BTreeMap::new();
            for item in visible {
                *counts.entry(item.path.clone()).or_insert(0) += 1;
            }
            counts
                .into_iter()
                .map(|(path, count)| format!("{path}: {count}"))
                .collect::<Vec<_>>()
                .join("\n")
        }
        _ => visible
            .into_iter()
            .map(render_search_match)
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

fn render_search_match(item: &SearchMatch) -> String {
    let Some(line) = item.line else {
        return item.path.clone();
    };
    if item.context.is_empty() {
        return format!(
            "{}: line {}: {}",
            item.path,
            line,
            item.text.as_deref().unwrap_or("")
        );
    }
    let block = item
        .context
        .iter()
        .map(|(line_no, text)| {
            let marker = if *line_no == line { ">" } else { " " };
            format!("{marker} {:4}| {}", line_no, text)
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!("{}:\n{}", item.path, block)
}

fn search_context_lines(
    lines: &[&str],
    match_index: usize,
    context_lines: usize,
) -> Vec<(usize, String)> {
    if context_lines == 0 {
        return Vec::new();
    }
    let start = match_index.saturating_sub(context_lines);
    let end = (match_index + context_lines + 1).min(lines.len());
    (start..end)
        .map(|index| (index + 1, lines[index].to_string()))
        .collect()
}

fn simple_glob_matches(pattern: &str, value: &str) -> bool {
    simple_glob_matches_bytes(
        pattern.to_ascii_lowercase().as_bytes(),
        value.to_ascii_lowercase().replace('\\', "/").as_bytes(),
    )
}

fn simple_glob_matches_bytes(pattern: &[u8], value: &[u8]) -> bool {
    if pattern.is_empty() {
        return value.is_empty();
    }
    match pattern[0] {
        b'*' => {
            simple_glob_matches_bytes(&pattern[1..], value)
                || (!value.is_empty() && simple_glob_matches_bytes(pattern, &value[1..]))
        }
        b'?' => !value.is_empty() && simple_glob_matches_bytes(&pattern[1..], &value[1..]),
        byte => {
            !value.is_empty()
                && value[0] == byte
                && simple_glob_matches_bytes(&pattern[1..], &value[1..])
        }
    }
}
