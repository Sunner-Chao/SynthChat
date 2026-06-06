use std::{
    fs,
    path::{Path, PathBuf},
};

use serde_json::{json, Value};

const ACP_RESOURCE_INLINE_MAX_BYTES: u64 = 1_000_000;

pub(super) fn acp_prompt_text_from_params(params: &Value) -> String {
    if let Some(text) = params.get("content").and_then(Value::as_str) {
        return text.trim().to_string();
    }
    let Some(prompt) = params.get("prompt") else {
        return String::new();
    };
    if let Some(text) = prompt.as_str() {
        return text.trim().to_string();
    }
    let Some(items) = prompt.as_array() else {
        return String::new();
    };
    items
        .iter()
        .filter_map(acp_prompt_item_text)
        .collect::<Vec<_>>()
        .join("\n")
}

pub(super) fn acp_idle_steer_prompt_text(text: &str) -> Option<String> {
    let trimmed = text.trim();
    let body = trimmed
        .strip_prefix("/steer")
        .or_else(|| trimmed.strip_prefix("／steer"))?;
    if !body.is_empty() && !body.starts_with(char::is_whitespace) {
        return None;
    }
    let steer_text = body.trim();
    (!steer_text.is_empty()).then(|| steer_text.to_string())
}

pub(super) fn acp_queue_prompt_text(text: &str) -> Option<String> {
    let trimmed = text.trim();
    let body = trimmed
        .strip_prefix("/queue")
        .or_else(|| trimmed.strip_prefix("／queue"))?;
    if !body.is_empty() && !body.starts_with(char::is_whitespace) {
        return None;
    }
    Some(body.trim().to_string())
}

pub(super) fn acp_prompt_is_local_queue_command(text: &str) -> bool {
    let trimmed = text.trim();
    for prefix in ["/queue", "／queue", "/agent-queue", "／agent-queue"] {
        let Some(body) = trimmed.strip_prefix(prefix) else {
            continue;
        };
        return body.is_empty() || body.starts_with(char::is_whitespace);
    }
    false
}

pub(super) fn acp_prompt_provider_data_from_params(params: &Value) -> Option<Value> {
    let prompt = params.get("prompt")?;
    let items = prompt.as_array()?;
    let mut parts = Vec::new();
    let mut has_media = false;
    for item in items {
        acp_prompt_item_openai_parts(item, &mut parts, &mut has_media);
    }
    if has_media && !parts.is_empty() {
        Some(json!({
            "openai": {
                "content": parts
            }
        }))
    } else {
        None
    }
}

fn acp_prompt_item_openai_parts(item: &Value, parts: &mut Vec<Value>, has_media: &mut bool) {
    let kind = item
        .get("type")
        .or_else(|| item.get("kind"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let mime = acp_prompt_mime_type(item);
    if kind.contains("image") || mime.starts_with("image/") {
        if let Some(text) = acp_prompt_image_text(item) {
            parts.push(json!({"type": "text", "text": text}));
        }
        if let Some(url) = acp_prompt_image_url(item, &mime) {
            parts.push(json!({"type": "image_url", "image_url": {"url": url}}));
            *has_media = true;
        }
        return;
    }
    if let Some(items) = item.get("content").and_then(Value::as_array) {
        for child in items {
            acp_prompt_item_openai_parts(child, parts, has_media);
        }
        return;
    }
    let resource = item.get("resource").unwrap_or(item);
    let resource_mime = acp_prompt_mime_type(resource);
    if resource_mime.starts_with("image/") {
        if let Some(text) = acp_prompt_resource_text(item) {
            parts.push(json!({"type": "text", "text": text}));
        }
        if let Some(url) = acp_prompt_image_url(resource, &resource_mime) {
            parts.push(json!({"type": "image_url", "image_url": {"url": url}}));
            *has_media = true;
        }
        return;
    }
    if let Some(text) = item
        .get("text")
        .or_else(|| item.get("content").and_then(|content| content.get("text")))
        .or_else(|| item.get("content"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        parts.push(json!({"type": "text", "text": text}));
    }
}

fn acp_prompt_image_url(item: &Value, mime: &str) -> Option<String> {
    if let Some(data) = acp_prompt_string(item, &["data", "blob"]) {
        if data.starts_with("data:") {
            return Some(data);
        }
        let mime = if mime.is_empty() { "image/png" } else { mime };
        return Some(format!("data:{mime};base64,{data}"));
    }
    if let Some(contents) = item.get("contents") {
        if let Some(data) = acp_prompt_string(contents, &["data", "blob"]) {
            if data.starts_with("data:") {
                return Some(data);
            }
            let mime = if mime.is_empty() { "image/png" } else { mime };
            return Some(format!("data:{mime};base64,{data}"));
        }
    }
    let uri = acp_prompt_string(item, &["uri", "url"])?;
    if uri.starts_with("file://") {
        if let Some(data_url) = acp_prompt_file_image_data_url(&uri, mime) {
            return Some(data_url);
        }
    }
    Some(uri)
}

fn acp_prompt_file_image_data_url(uri: &str, mime: &str) -> Option<String> {
    let mime = if mime.is_empty() {
        acp_prompt_image_mime_from_uri(uri)?
    } else {
        mime.to_string()
    };
    if !mime.starts_with("image/") {
        return None;
    }
    let path = acp_file_path_from_uri(uri)?;
    let metadata = fs::metadata(&path).ok()?;
    if metadata.len() > ACP_RESOURCE_INLINE_MAX_BYTES {
        return None;
    }
    let bytes = fs::read(path).ok()?;
    use base64::Engine;
    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
    Some(format!("data:{mime};base64,{encoded}"))
}

fn acp_prompt_item_text(item: &Value) -> Option<String> {
    if let Some(text) = item
        .get("text")
        .or_else(|| item.get("content").and_then(|content| content.get("text")))
        .or_else(|| item.get("content"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        return Some(text.to_string());
    }
    if let Some(items) = item.get("content").and_then(Value::as_array) {
        let text = items
            .iter()
            .filter_map(acp_prompt_item_text)
            .collect::<Vec<_>>()
            .join("\n");
        if !text.trim().is_empty() {
            return Some(text);
        }
    }
    let kind = item
        .get("type")
        .or_else(|| item.get("kind"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if kind.contains("image") || acp_prompt_mime_type(item).starts_with("image/") {
        return acp_prompt_image_text(item);
    }
    if kind.contains("resource") || item.get("uri").is_some() || item.get("resource").is_some() {
        return acp_prompt_resource_text(item);
    }
    None
}

fn acp_prompt_image_text(item: &Value) -> Option<String> {
    let uri = acp_prompt_string(item, &["uri", "url", "data"]).unwrap_or_default();
    let mime = acp_prompt_mime_type(item);
    let display = acp_prompt_display_name(item, &uri, "image");
    let mut lines = vec![format!("[Attached image: {display}]")];
    if !uri.is_empty() {
        lines.push(format!("URI: {}", acp_prompt_uri_preview(&uri)));
    }
    if !mime.is_empty() {
        lines.push(format!("MIME: {mime}"));
    }
    Some(lines.join("\n"))
}

fn acp_prompt_resource_text(item: &Value) -> Option<String> {
    let resource = item.get("resource").unwrap_or(item);
    let uri = acp_prompt_string(resource, &["uri", "url"])
        .or_else(|| acp_prompt_string(item, &["uri", "url"]))
        .unwrap_or_default();
    let text = acp_prompt_string(resource, &["text"])
        .or_else(|| {
            resource
                .get("contents")
                .and_then(|contents| acp_prompt_string(contents, &["text"]))
        })
        .unwrap_or_default();
    let mime = acp_prompt_mime_type(resource);
    let display = acp_prompt_display_name(resource, &uri, "resource");
    let mut lines = vec![format!("[Attached file: {display}]")];
    if !uri.is_empty() {
        lines.push(format!("URI: {uri}"));
    }
    if !mime.is_empty() {
        lines.push(format!("MIME: {mime}"));
    }
    if !text.trim().is_empty() {
        lines.push(String::new());
        lines.push(text);
    } else if let Some(file_text) = acp_prompt_file_text_from_uri(&uri, &mime) {
        lines.push(String::new());
        lines.push(file_text);
    } else if resource.get("blob").is_some()
        || resource
            .get("contents")
            .and_then(|contents| contents.get("blob"))
            .is_some()
    {
        lines.push(String::new());
        lines.push("[Binary embedded resource omitted.]".into());
    }
    Some(lines.join("\n"))
}

fn acp_prompt_file_text_from_uri(uri: &str, mime: &str) -> Option<String> {
    if !uri.starts_with("file://") || !acp_prompt_resource_looks_text(uri, mime) {
        return None;
    }
    let path = acp_file_path_from_uri(uri)?;
    let metadata = fs::metadata(&path).ok()?;
    let size = metadata.len();
    let read_len = size.min(ACP_RESOURCE_INLINE_MAX_BYTES) as usize;
    let bytes = fs::read(path).ok()?;
    let mut text = String::from_utf8_lossy(&bytes[..bytes.len().min(read_len)]).to_string();
    if size > ACP_RESOURCE_INLINE_MAX_BYTES {
        text.push_str(&format!(
            "\n\n[Truncated to {ACP_RESOURCE_INLINE_MAX_BYTES} of {size} bytes]"
        ));
    }
    Some(text)
}

fn acp_prompt_resource_looks_text(uri: &str, mime: &str) -> bool {
    if mime.starts_with("text/") {
        return true;
    }
    let lower_mime = mime.to_ascii_lowercase();
    if ["json", "xml", "yaml", "toml", "javascript", "typescript"]
        .iter()
        .any(|token| lower_mime.contains(token))
    {
        return true;
    }
    let lower_uri = uri.to_ascii_lowercase();
    [
        ".txt", ".md", ".json", ".jsonl", ".yaml", ".yml", ".toml", ".rs", ".py", ".js", ".ts",
        ".tsx", ".jsx", ".html", ".css", ".xml", ".csv", ".log",
    ]
    .iter()
    .any(|extension| lower_uri.ends_with(extension))
}

fn acp_prompt_image_mime_from_uri(uri: &str) -> Option<String> {
    let lower = uri.to_ascii_lowercase();
    let mime = if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        "image/jpeg"
    } else if lower.ends_with(".png") {
        "image/png"
    } else if lower.ends_with(".gif") {
        "image/gif"
    } else if lower.ends_with(".webp") {
        "image/webp"
    } else if lower.ends_with(".bmp") {
        "image/bmp"
    } else {
        return None;
    };
    Some(mime.into())
}

fn acp_file_path_from_uri(uri: &str) -> Option<PathBuf> {
    let parsed = reqwest::Url::parse(uri).ok()?;
    if parsed.scheme() != "file" {
        return None;
    }
    parsed.to_file_path().ok()
}

fn acp_prompt_display_name(item: &Value, uri: &str, fallback: &str) -> String {
    for key in ["title", "name"] {
        if let Some(text) = item
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
        {
            return text.to_string();
        }
    }
    Path::new(uri)
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(fallback)
        .to_string()
}

fn acp_prompt_mime_type(item: &Value) -> String {
    acp_prompt_string(item, &["mimeType", "mime_type", "mime"])
        .unwrap_or_default()
        .to_lowercase()
}

fn acp_prompt_string(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(ToString::to_string)
}

fn acp_prompt_uri_preview(uri: &str) -> String {
    let uri = uri.trim();
    if uri.starts_with("data:") && uri.len() > 96 {
        format!("{}...[truncated]", &uri[..96])
    } else {
        uri.to_string()
    }
}

pub(super) fn acp_prompt_usage_delta(before: &Value, after: &Value) -> Option<Value> {
    let input_tokens = acp_usage_counter(after, "promptTokens", "prompt_tokens")
        .saturating_sub(acp_usage_counter(before, "promptTokens", "prompt_tokens"));
    let output_tokens =
        acp_usage_counter(after, "completionTokens", "completion_tokens").saturating_sub(
            acp_usage_counter(before, "completionTokens", "completion_tokens"),
        );
    let thought_tokens =
        acp_usage_counter(after, "reasoningTokens", "reasoning_tokens").saturating_sub(
            acp_usage_counter(before, "reasoningTokens", "reasoning_tokens"),
        );
    let cached_read_tokens =
        acp_usage_counter(after, "cacheReadTokens", "cache_read_tokens").saturating_sub(
            acp_usage_counter(before, "cacheReadTokens", "cache_read_tokens"),
        );
    let cached_write_tokens =
        acp_usage_counter(after, "cacheWriteTokens", "cache_write_tokens").saturating_sub(
            acp_usage_counter(before, "cacheWriteTokens", "cache_write_tokens"),
        );
    let total_tokens = input_tokens + output_tokens;
    if input_tokens == 0
        && output_tokens == 0
        && thought_tokens == 0
        && cached_read_tokens == 0
        && cached_write_tokens == 0
    {
        return None;
    }
    Some(json!({
        "inputTokens": input_tokens,
        "outputTokens": output_tokens,
        "totalTokens": total_tokens,
        "thoughtTokens": thought_tokens,
        "cachedReadTokens": cached_read_tokens,
        "cachedWriteTokens": cached_write_tokens
    }))
}

fn acp_usage_counter(value: &Value, camel: &str, snake: &str) -> u64 {
    value
        .get(camel)
        .or_else(|| value.get(snake))
        .and_then(Value::as_u64)
        .unwrap_or(0)
}
