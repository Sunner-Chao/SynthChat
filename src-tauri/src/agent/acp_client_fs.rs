use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::error::{AppError, AppResult};

use super::redact_sensitive_text;
pub(super) async fn acp_read_text_file_response(message: &Value, cwd: &Path) -> Value {
    let id = message.get("id").cloned().unwrap_or(Value::Null);
    let params = message.get("params").unwrap_or(&Value::Null);
    let path_text = params.get("path").and_then(Value::as_str).unwrap_or("");
    match acp_path_within_cwd(cwd, path_text) {
        Ok(path) => {
            if let Some(reason) = acp_sensitive_path_reason(&path, false) {
                return json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32602, "message": reason}});
            }
            let mut content = tokio::fs::read_to_string(&path).await.unwrap_or_default();
            let line = params.get("line").and_then(Value::as_u64).unwrap_or(0);
            let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(0);
            if line > 1 {
                let start = line.saturating_sub(1) as usize;
                let take = if limit > 0 {
                    limit as usize
                } else {
                    usize::MAX
                };
                content = content
                    .split_inclusive('\n')
                    .skip(start)
                    .take(take)
                    .collect::<Vec<_>>()
                    .join("");
            }
            if !content.is_empty() {
                content = redact_sensitive_text(&content);
            }
            json!({"jsonrpc": "2.0", "id": id, "result": {"content": content}})
        }
        Err(error) => {
            json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32602, "message": error.to_string()}})
        }
    }
}

pub(super) async fn acp_write_text_file_response(message: &Value, cwd: &Path) -> Value {
    let id = message.get("id").cloned().unwrap_or(Value::Null);
    let params = message.get("params").unwrap_or(&Value::Null);
    let path_text = params.get("path").and_then(Value::as_str).unwrap_or("");
    let content = params.get("content").and_then(Value::as_str).unwrap_or("");
    match acp_path_within_cwd(cwd, path_text) {
        Ok(path) => {
            if let Some(reason) = acp_sensitive_path_reason(&path, true) {
                return json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32602, "message": reason}});
            }
            let result = async {
                if let Some(parent) = path.parent() {
                    tokio::fs::create_dir_all(parent).await?;
                }
                tokio::fs::write(&path, content).await
            }
            .await;
            match result {
                Ok(()) => json!({"jsonrpc": "2.0", "id": id, "result": Value::Null}),
                Err(error) => {
                    json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32602, "message": error.to_string()}})
                }
            }
        }
        Err(error) => {
            json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32602, "message": error.to_string()}})
        }
    }
}

pub(super) fn acp_sensitive_path_reason(path: &Path, writing: bool) -> Option<&'static str> {
    let normalized_path = path
        .to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase();
    let blocked_components = [
        ".ssh", ".aws", ".gnupg", ".kube", ".docker", ".azure", ".gcloud",
    ];
    if path.components().any(|component| {
        component
            .as_os_str()
            .to_str()
            .map(|value| {
                blocked_components
                    .iter()
                    .any(|blocked| value.eq_ignore_ascii_case(blocked))
            })
            .unwrap_or(false)
    }) {
        return Some("ACP file access denied: sensitive credential directory");
    }
    for blocked in [
        "/.config/gh",
        "/.hermes/skills/.hub",
        "/skills/.hub",
        "/.ssh/config",
    ] {
        if normalized_path.contains(blocked) {
            return Some("ACP file access denied: sensitive credential directory");
        }
    }
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    let blocked_files = [
        ".netrc",
        ".pgpass",
        ".npmrc",
        ".pypirc",
        "id_rsa",
        "id_ed25519",
        "authorized_keys",
    ];
    if blocked_files
        .iter()
        .any(|blocked| file_name.eq_ignore_ascii_case(blocked))
    {
        return Some("ACP file access denied: sensitive credential file");
    }
    if writing {
        let lower_name = file_name.to_ascii_lowercase();
        if lower_name == ".env"
            || lower_name.starts_with(".env.")
            || lower_name == "config.yaml"
            || lower_name == "config.yml"
        {
            return Some("ACP write denied: protected env/config file");
        }
    }
    None
}

pub(super) fn acp_path_within_cwd(cwd: &Path, path_text: &str) -> AppResult<PathBuf> {
    let raw = PathBuf::from(path_text);
    let joined = if raw.is_absolute() {
        raw
    } else {
        cwd.join(raw)
    };
    let base = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
    let candidate = joined.canonicalize().unwrap_or_else(|_| {
        joined
            .parent()
            .and_then(|parent| parent.canonicalize().ok())
            .and_then(|parent| joined.file_name().map(|name| parent.join(name)))
            .unwrap_or_else(|| joined.clone())
    });
    if candidate.starts_with(&base) {
        Ok(candidate)
    } else {
        Err(AppError::BadRequest(format!(
            "ACP file path '{}' is outside cwd {}",
            path_text,
            base.display()
        )))
    }
}
