use std::{
    collections::HashMap,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::error::{AppError, AppResult};
use crate::models::{new_id, LlmProvider};
use crate::process_utils::CommandWindowExt;

#[cfg(test)]
pub(crate) static HERMES_AUTH_TEST_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[derive(Debug, Clone)]
pub struct HermesRuntimeCredential {
    pub provider_id: String,
    pub api_key: String,
    pub base_url: Option<String>,
    pub source: String,
    pub expires_at: Option<String>,
}

#[derive(Debug, Clone)]
pub struct HermesExternalCredentialStatus {
    pub provider_id: String,
    pub source: String,
    pub state: &'static str,
    pub expires_at: Option<String>,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HermesCredentialPoolEntryStatus {
    pub provider_id: String,
    pub index: usize,
    pub id: Option<String>,
    pub label: String,
    pub auth_type: Option<String>,
    pub source: Option<String>,
    pub state: String,
    pub expires_at: Option<String>,
    pub base_url: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CodexDeviceCodeStart {
    pub user_code: String,
    pub device_auth_id: String,
    pub verification_uri: String,
    pub interval_seconds: u64,
}

#[derive(Debug, Clone)]
pub struct NousDeviceCodeStart {
    pub user_code: String,
    pub device_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: String,
    pub expires_in: u64,
    pub interval_seconds: u64,
}

#[derive(Debug, Clone)]
pub struct MiniMaxOauthStart {
    pub user_code: String,
    pub verification_uri: String,
    pub code_verifier: String,
    pub expired_in: i64,
    pub interval_ms: Option<u64>,
    pub region: String,
}

#[derive(Debug, Clone)]
pub struct XaiOauthStart {
    pub authorize_url: String,
    pub redirect_uri: String,
    pub state: String,
    pub code_verifier: String,
    pub code_challenge: String,
}

#[derive(Debug, Clone)]
pub struct GoogleGeminiOauthStart {
    pub authorize_url: String,
    pub redirect_uri: String,
    pub state: String,
    pub code_verifier: String,
}

#[derive(Debug, Clone)]
pub struct AnthropicOauthStart {
    pub authorize_url: String,
    pub redirect_uri: String,
    pub state: String,
    pub code_verifier: String,
    pub code_challenge: String,
}

#[derive(Debug, Clone)]
pub struct SpotifyOauthStart {
    pub authorize_url: String,
    pub redirect_uri: String,
    pub state: String,
    pub code_verifier: String,
    pub code_challenge: String,
    pub client_id: String,
    pub scope: String,
}

const QWEN_OAUTH_CLIENT_ID: &str = "f0304373b74a44d2b584a3fb70ca9e56";
const QWEN_OAUTH_TOKEN_URL: &str = "https://chat.qwen.ai/api/v1/oauth2/token";
const MINIMAX_OAUTH_CLIENT_ID: &str = "78257093-7e40-4613-99e0-527b14b39113";
const MINIMAX_OAUTH_GLOBAL_BASE: &str = "https://api.minimax.io";
const MINIMAX_OAUTH_GLOBAL_INFERENCE: &str = "https://api.minimax.io/anthropic";
const MINIMAX_OAUTH_SCOPE: &str = "group_id profile model.completion";
const XAI_OAUTH_CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";
const XAI_OAUTH_TOKEN_ENDPOINT: &str = "https://auth.x.ai/oauth/token";
const XAI_OAUTH_BASE_URL: &str = "https://api.x.ai/v1";
const XAI_OAUTH_DISCOVERY_URL: &str = "https://auth.x.ai/.well-known/openid-configuration";
const XAI_OAUTH_SCOPE: &str = "openid profile email offline_access grok-cli:access api:access";
const XAI_OAUTH_REDIRECT_URI: &str = "http://127.0.0.1:56121/callback";
const CODEX_OAUTH_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const CODEX_OAUTH_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const CODEX_OAUTH_ISSUER_URL: &str = "https://auth.openai.com";
const DEFAULT_CODEX_BASE_URL: &str = "https://chatgpt.com/backend-api/codex";
const GOOGLE_GEMINI_OAUTH_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const GOOGLE_GEMINI_OAUTH_AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const GOOGLE_GEMINI_OAUTH_REDIRECT_URI: &str = "http://127.0.0.1:8085/oauth2callback";
const GOOGLE_GEMINI_OAUTH_SCOPES: &str = "https://www.googleapis.com/auth/cloud-platform https://www.googleapis.com/auth/userinfo.email https://www.googleapis.com/auth/userinfo.profile";
const ANTHROPIC_OAUTH_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const ANTHROPIC_OAUTH_AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";
const ANTHROPIC_OAUTH_TOKEN_URL: &str = "https://console.anthropic.com/v1/oauth/token";
const ANTHROPIC_OAUTH_REDIRECT_URI: &str = "https://console.anthropic.com/oauth/code/callback";
const ANTHROPIC_OAUTH_SCOPES: &str = "org:create_api_key user:profile user:inference";
const DEFAULT_NOUS_PORTAL_URL: &str = "https://portal.nousresearch.com";
const DEFAULT_NOUS_INFERENCE_URL: &str = "https://inference-api.nousresearch.com/v1";
const DEFAULT_NOUS_CLIENT_ID: &str = "hermes-cli";
const NOUS_INFERENCE_INVOKE_SCOPE: &str = "inference:invoke";
const NOUS_INVOKE_JWT_MIN_TTL_SECONDS: u64 = 120;
const NOUS_SHARED_STORE_FILENAME: &str = "nous_auth.json";
const DEFAULT_SPOTIFY_ACCOUNTS_BASE_URL: &str = "https://accounts.spotify.com";
const DEFAULT_SPOTIFY_API_BASE_URL: &str = "https://api.spotify.com/v1";
const DEFAULT_SPOTIFY_REDIRECT_URI: &str = "http://127.0.0.1:43827/spotify/callback";
const DEFAULT_SPOTIFY_SCOPE: &str = "user-read-playback-state user-modify-playback-state user-read-currently-playing user-read-recently-played playlist-read-private playlist-modify-public playlist-modify-private user-library-read user-library-modify";
const BITWARDEN_CACHE_BASENAME: &str = "bws_cache.json";
const BITWARDEN_DEFAULT_CACHE_TTL_SECONDS: u64 = 300;
const BITWARDEN_RUN_TIMEOUT_SECONDS: u64 = 30;

pub fn resolve_hermes_runtime_credential(
    provider: &LlmProvider,
) -> Option<HermesRuntimeCredential> {
    let provider_ids = hermes_provider_id_candidates(provider);
    if provider_ids.is_empty() {
        return None;
    }
    hermes_auth_store_candidates()
        .into_iter()
        .find_map(|path| {
            let payload = std::fs::read_to_string(&path).ok()?;
            let mut store = serde_json::from_str::<Value>(&payload).ok()?;
            for provider_id in &provider_ids {
                if let Some(credential) = provider_state_credential(provider_id, &store) {
                    return Some(credential);
                }
                let (credential, changed) =
                    credential_pool_credential_select(provider_id, &mut store);
                if changed {
                    let _ = write_hermes_auth_store_path(&path, &store);
                }
                if credential.is_some() {
                    return credential;
                }
            }
            None
        })
        .or_else(|| {
            provider_ids
                .iter()
                .find_map(|provider_id| external_provider_credential(provider_id))
        })
}

pub fn hermes_auth_store_status(provider: &LlmProvider) -> Option<HermesRuntimeCredential> {
    resolve_hermes_runtime_credential(provider)
}

pub fn resolve_bitwarden_secret(env_names: &[&str]) -> Option<String> {
    let secrets = load_bitwarden_secrets().ok()?;
    env_names.iter().find_map(|name| {
        secrets
            .get(*name)
            .map(|value| value.trim().to_string())
            .filter(|value| usable_credential_secret(value))
    })
}

pub fn hermes_auth_store_credential_status(
    provider: &LlmProvider,
) -> Option<HermesExternalCredentialStatus> {
    let provider_ids = hermes_provider_id_candidates(provider);
    if provider_ids.is_empty() {
        return None;
    }
    hermes_auth_store_candidates().into_iter().find_map(|path| {
        let payload = std::fs::read_to_string(path).ok()?;
        let store = serde_json::from_str::<Value>(&payload).ok()?;
        provider_ids
            .iter()
            .find_map(|provider_id| credential_status_from_store(provider_id, &store))
    })
}

pub fn hermes_external_credential_status(
    provider: &LlmProvider,
) -> Option<HermesExternalCredentialStatus> {
    hermes_provider_id_candidates(provider)
        .into_iter()
        .find_map(external_provider_status)
}

pub fn list_hermes_credential_pool(
    provider_filter: Option<&str>,
) -> AppResult<Vec<HermesCredentialPoolEntryStatus>> {
    let store = read_primary_hermes_auth_store()?;
    let Some(pool) = store.get("credential_pool").and_then(Value::as_object) else {
        return Ok(Vec::new());
    };
    let filter = provider_filter
        .map(normalize_credential_pool_provider)
        .filter(|value| !value.is_empty());
    let mut providers = pool.keys().cloned().collect::<Vec<_>>();
    providers.sort();
    let mut entries = Vec::new();
    for provider_id in providers {
        if let Some(filter) = filter.as_deref() {
            if !credential_pool_provider_matches(&provider_id, filter) {
                continue;
            }
        }
        if let Some(value) = pool.get(&provider_id) {
            push_credential_pool_entry_statuses(&mut entries, &provider_id, value);
        }
    }
    Ok(entries)
}

pub fn list_hermes_oauth_provider_statuses() -> AppResult<Value> {
    let store = read_primary_hermes_auth_store()?;
    let providers = hermes_oauth_provider_catalog()
        .iter()
        .map(|entry| {
            let status = hermes_oauth_provider_status_value(entry, &store);
            json!({
                "id": entry.id,
                "name": entry.name,
                "flow": entry.flow,
                "cli_command": entry.cli_command,
                "docs_url": entry.docs_url,
                "status": status
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "providers": providers,
        "schema": "hermes_dashboard_oauth_providers_desktop_v1",
        "desktopAdaptation": true,
        "authStore": primary_hermes_auth_store_path()?.display().to_string()
    }))
}

pub fn clear_hermes_oauth_provider_auth(provider: &str) -> AppResult<usize> {
    let provider_id = normalize_credential_pool_provider(provider);
    if !hermes_oauth_provider_catalog()
        .iter()
        .any(|entry| entry.id == provider_id)
    {
        return Err(AppError::BadRequest(format!(
            "Unknown provider: {provider}. Available: {}",
            hermes_oauth_provider_catalog()
                .iter()
                .map(|entry| entry.id)
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }
    let path = primary_hermes_auth_store_path()?;
    let mut store = read_hermes_auth_store_path(&path)?;
    let mut cleared = 0usize;
    if let Some(providers) = store.get_mut("providers").and_then(Value::as_object_mut) {
        if providers.remove(&provider_id).is_some() {
            cleared += 1;
        }
        if provider_id == "claude-code" && providers.remove("anthropic").is_some() {
            cleared += 1;
        }
    }
    if let Some(pool) = store
        .get_mut("credential_pool")
        .and_then(Value::as_object_mut)
    {
        if pool.remove(&provider_id).is_some() {
            cleared += 1;
        }
        if provider_id == "claude-code" && pool.remove("anthropic").is_some() {
            cleared += 1;
        }
    }
    if provider_id == "anthropic" || provider_id == "claude-code" {
        for path in anthropic_oauth_file_candidates() {
            if path.exists() {
                std::fs::remove_file(&path)?;
                cleared += 1;
            }
        }
    }
    write_hermes_auth_store_path(&path, &store)?;
    Ok(cleared)
}

#[derive(Clone, Copy)]
struct HermesOauthProviderCatalogEntry {
    id: &'static str,
    name: &'static str,
    flow: &'static str,
    cli_command: &'static str,
    docs_url: &'static str,
}

fn hermes_oauth_provider_catalog() -> &'static [HermesOauthProviderCatalogEntry] {
    &[
        HermesOauthProviderCatalogEntry {
            id: "anthropic",
            name: "Anthropic (Claude API)",
            flow: "pkce",
            cli_command: "hermes auth add anthropic",
            docs_url: "https://docs.claude.com/en/api/getting-started",
        },
        HermesOauthProviderCatalogEntry {
            id: "claude-code",
            name: "Claude Code (subscription)",
            flow: "external",
            cli_command: "claude setup-token",
            docs_url: "https://docs.claude.com/en/docs/claude-code",
        },
        HermesOauthProviderCatalogEntry {
            id: "nous",
            name: "Nous Portal",
            flow: "device_code",
            cli_command: "hermes auth add nous",
            docs_url: "https://portal.nousresearch.com",
        },
        HermesOauthProviderCatalogEntry {
            id: "openai-codex",
            name: "OpenAI Codex (ChatGPT)",
            flow: "device_code",
            cli_command: "hermes auth add openai-codex",
            docs_url: "https://platform.openai.com/docs",
        },
        HermesOauthProviderCatalogEntry {
            id: "qwen-oauth",
            name: "Qwen (via Qwen CLI)",
            flow: "external",
            cli_command: "hermes auth add qwen-oauth",
            docs_url: "https://github.com/QwenLM/qwen-code",
        },
        HermesOauthProviderCatalogEntry {
            id: "minimax-oauth",
            name: "MiniMax (OAuth)",
            flow: "device_code",
            cli_command: "hermes auth add minimax-oauth",
            docs_url: "https://www.minimax.io",
        },
        HermesOauthProviderCatalogEntry {
            id: "xai-oauth",
            name: "xAI OAuth",
            flow: "pkce",
            cli_command: "hermes auth add xai-oauth",
            docs_url: "https://x.ai",
        },
        HermesOauthProviderCatalogEntry {
            id: "google-gemini-cli",
            name: "Google Gemini CLI",
            flow: "pkce",
            cli_command: "hermes auth add google-gemini-cli",
            docs_url: "https://ai.google.dev/gemini-api/docs/oauth",
        },
        HermesOauthProviderCatalogEntry {
            id: "spotify",
            name: "Spotify",
            flow: "pkce",
            cli_command: "hermes auth spotify",
            docs_url: "https://hermes-agent.nousresearch.com/docs/user-guide/features/spotify",
        },
    ]
}

fn hermes_oauth_provider_status_value(
    entry: &HermesOauthProviderCatalogEntry,
    store: &Value,
) -> Value {
    let status = credential_status_from_store(entry.id, store)
        .or_else(|| external_provider_status(entry.id));
    let runtime = provider_state_credential(entry.id, store)
        .or_else(|| external_provider_credential(entry.id));
    let logged_in = runtime.is_some()
        || status
            .as_ref()
            .is_some_and(|status| matches!(status.state, "present" | "active"));
    json!({
        "logged_in": logged_in,
        "source": status.as_ref().map(|status| status.source.clone()).or_else(|| runtime.as_ref().map(|credential| credential.source.clone())),
        "source_label": status.as_ref().and_then(|status| status.note.clone()).or_else(|| runtime.as_ref().map(|credential| credential.source.clone())),
        "token_preview": runtime.as_ref().map(|credential| truncate_oauth_token_preview(&credential.api_key)),
        "expires_at": status.as_ref().and_then(|status| status.expires_at.clone()).or_else(|| runtime.as_ref().and_then(|credential| credential.expires_at.clone())),
        "has_refresh_token": hermes_oauth_provider_has_refresh_token(entry.id, store),
        "state": status.as_ref().map(|status| status.state).unwrap_or(if logged_in { "present" } else { "missing" }),
        "error": status.as_ref().and_then(|status| status.note.clone()).filter(|_| !logged_in),
        "desktopAdaptation": true
    })
}

fn hermes_oauth_provider_has_refresh_token(provider_id: &str, store: &Value) -> bool {
    let state = store
        .get("providers")
        .and_then(|providers| providers.get(provider_id));
    state.is_some_and(|state| {
        first_string(state, &["refresh_token", "refreshToken"]).is_some()
            || state.get("tokens").is_some_and(|tokens| {
                first_string(tokens, &["refresh_token", "refreshToken"]).is_some()
            })
    })
}

fn truncate_oauth_token_preview(value: &str) -> String {
    let mut token = value.trim();
    if token.matches('.').count() >= 2 {
        token = token.rsplit('.').next().unwrap_or(token);
    }
    let chars = token.chars().collect::<Vec<_>>();
    if chars.len() <= 6 {
        return token.to_string();
    }
    format!(
        "...{}",
        chars[chars.len().saturating_sub(6)..]
            .iter()
            .collect::<String>()
    )
}

fn anthropic_oauth_file_candidates() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(home) = std::env::var_os("HERMES_HOME").filter(|value| !value.is_empty()) {
        push_unique_path(
            &mut paths,
            PathBuf::from(home).join(".anthropic_oauth.json"),
        );
    }
    if let Some(home) = home_dir() {
        push_unique_path(
            &mut paths,
            home.join(".hermes").join(".anthropic_oauth.json"),
        );
    }
    paths
}

fn claude_code_credentials_file_candidates() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(home) = home_dir() {
        push_unique_path(&mut paths, home.join(".claude").join(".credentials.json"));
        push_unique_path(&mut paths, home.join(".claude").join("credentials.json"));
    }
    paths
}

pub fn add_hermes_credential_pool_entry(
    provider: &str,
    label: Option<&str>,
    api_key: &str,
    base_url: Option<&str>,
    auth_type: Option<&str>,
    expires_at: Option<&str>,
) -> AppResult<HermesCredentialPoolEntryStatus> {
    let provider_id = normalize_credential_pool_provider(provider);
    if provider_id.is_empty() {
        return Err(AppError::BadRequest("provider is required".into()));
    }
    let token = api_key.trim();
    if token.is_empty() {
        return Err(AppError::BadRequest("apiKey is required".into()));
    }
    let auth_type = auth_type
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("api_key")
        .replace('-', "_")
        .to_ascii_lowercase();
    if auth_type != "api_key" {
        return Err(AppError::BadRequest(
            "only api_key credential pool entries can be added through this safe settings API; use provider OAuth login for OAuth credentials".into(),
        ));
    }

    let path = primary_hermes_auth_store_path()?;
    let mut store = read_hermes_auth_store_path(&path)?;
    if !store.is_object() {
        store = json!({});
    }
    if store.get("credential_pool").is_none() || !store["credential_pool"].is_object() {
        store["credential_pool"] = json!({});
    }
    if store["credential_pool"].get(&provider_id).is_none() {
        store["credential_pool"][&provider_id] = json!([]);
    }

    let existing_count = credential_pool_entry_count(&store["credential_pool"][&provider_id]);
    let mut entry = json!({
        "id": short_credential_pool_id(),
        "label": label
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| format!("api-key-{}", existing_count + 1)),
        "auth_type": "api_key",
        "priority": 0,
        "source": "manual",
        "access_token": token,
    });
    if let Some(base_url) = base_url.map(str::trim).filter(|value| !value.is_empty()) {
        if provider_id == "nous" {
            entry["inference_base_url"] = json!(base_url.trim_end_matches('/'));
        } else {
            entry["base_url"] = json!(base_url.trim_end_matches('/'));
        }
    }
    if let Some(expires_at) = expires_at.map(str::trim).filter(|value| !value.is_empty()) {
        entry["expires_at"] = json!(expires_at);
    }

    let value = store
        .get_mut("credential_pool")
        .and_then(Value::as_object_mut)
        .and_then(|pool| pool.get_mut(&provider_id))
        .ok_or_else(|| AppError::NotFound(format!("credential_pool.{provider_id}")))?;
    let index = append_pool_entry_value(value, entry)?;
    write_hermes_auth_store_path(&path, &store)?;

    let added = read_hermes_auth_store_path(&path)?
        .get("credential_pool")
        .and_then(|pool| pool.get(&provider_id))
        .and_then(|value| credential_pool_entry_at(&provider_id, value, index))
        .ok_or_else(|| AppError::NotFound(format!("credential_pool.{provider_id}.{index}")))?;
    Ok(added)
}

pub fn remove_hermes_credential_pool_entry(
    provider: &str,
    target: &str,
) -> AppResult<HermesCredentialPoolEntryStatus> {
    let provider_id = normalize_credential_pool_provider(provider);
    if provider_id.is_empty() {
        return Err(AppError::BadRequest("provider is required".into()));
    }
    if target.trim().is_empty() {
        return Err(AppError::BadRequest(
            "credential target is required; use 1-based index, id, or label prefix".into(),
        ));
    }
    let path = primary_hermes_auth_store_path()?;
    let mut store = read_hermes_auth_store_path(&path)?;
    let pool = store
        .get_mut("credential_pool")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| AppError::NotFound("Hermes credential_pool".into()))?;
    let value = pool
        .get_mut(&provider_id)
        .ok_or_else(|| AppError::NotFound(format!("credential_pool.{provider_id}")))?;
    let removed = remove_pool_entry_value(&provider_id, value, target)?;
    write_hermes_auth_store_path(&path, &store)?;
    Ok(removed)
}

pub fn reset_hermes_credential_pool_statuses(provider: &str) -> AppResult<usize> {
    let provider_id = normalize_credential_pool_provider(provider);
    if provider_id.is_empty() {
        return Err(AppError::BadRequest("provider is required".into()));
    }
    let path = primary_hermes_auth_store_path()?;
    let mut store = read_hermes_auth_store_path(&path)?;
    let pool = store
        .get_mut("credential_pool")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| AppError::NotFound("Hermes credential_pool".into()))?;
    let value = pool
        .get_mut(&provider_id)
        .ok_or_else(|| AppError::NotFound(format!("credential_pool.{provider_id}")))?;
    let count = reset_pool_entry_status_values(value);
    if count > 0 {
        write_hermes_auth_store_path(&path, &store)?;
    }
    Ok(count)
}

pub fn mark_hermes_credential_pool_failure(
    provider: &LlmProvider,
    kind: &str,
    message: &str,
) -> AppResult<Option<HermesExternalCredentialStatus>> {
    mark_hermes_credential_pool_failure_inner(provider, None, kind, message)
}

pub fn mark_hermes_credential_pool_failure_for_source(
    provider: &LlmProvider,
    credential_source: &str,
    kind: &str,
    message: &str,
) -> AppResult<Option<HermesExternalCredentialStatus>> {
    mark_hermes_credential_pool_failure_inner(provider, Some(credential_source), kind, message)
}

fn mark_hermes_credential_pool_failure_inner(
    provider: &LlmProvider,
    credential_source: Option<&str>,
    kind: &str,
    message: &str,
) -> AppResult<Option<HermesExternalCredentialStatus>> {
    let ttl = credential_pool_failure_ttl_seconds(kind);
    if ttl == 0 || provider_has_configured_credential(provider) {
        return Ok(None);
    }
    let provider_ids = hermes_provider_id_candidates(provider);
    if provider_ids.is_empty() {
        return Ok(None);
    }

    let path = primary_hermes_auth_store_path()?;
    let mut store = read_hermes_auth_store_path(&path)?;
    for provider_id in provider_ids {
        let Some(pool) = store
            .get_mut("credential_pool")
            .and_then(Value::as_object_mut)
            .and_then(|pool| pool.get_mut(provider_id))
        else {
            continue;
        };
        if let Some(status) = mark_credential_pool_value_failure(
            provider_id,
            pool,
            credential_source,
            kind,
            message,
            ttl,
        ) {
            write_hermes_auth_store_path(&path, &store)?;
            return Ok(Some(status));
        }
    }
    Ok(None)
}

pub async fn refresh_qwen_cli_oauth_credentials() -> AppResult<HermesRuntimeCredential> {
    let path = qwen_cli_auth_path()?;
    let payload = std::fs::read_to_string(&path)?;
    let tokens = serde_json::from_str::<Value>(&payload)
        .map_err(|error| AppError::BadRequest(format!("invalid Qwen CLI credentials: {error}")))?;
    let refresh_token = first_string(&tokens, &["refresh_token"]).ok_or_else(|| {
        AppError::BadRequest("Qwen OAuth refresh_token missing; re-run qwen auth qwen-oauth".into())
    })?;
    let client = reqwest::Client::new();
    let response = client
        .post(QWEN_OAUTH_TOKEN_URL)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Accept", "application/json")
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token.as_str()),
            ("client_id", QWEN_OAUTH_CLIENT_ID),
        ])
        .send()
        .await
        .map_err(|error| AppError::BadRequest(format!("Qwen OAuth refresh failed: {error}")))?;
    let status = response.status();
    let body = response.text().await.map_err(|error| {
        AppError::BadRequest(format!("Qwen OAuth refresh read failed: {error}"))
    })?;
    if !status.is_success() {
        return Err(AppError::BadRequest(format!(
            "Qwen OAuth refresh failed with HTTP {status}: {body}"
        )));
    }
    let response = serde_json::from_str::<Value>(&body).map_err(|error| {
        AppError::BadRequest(format!("Qwen OAuth refresh returned invalid JSON: {error}"))
    })?;
    let refreshed = qwen_refreshed_tokens_from_response(&tokens, &response)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, serde_json::to_string_pretty(&refreshed)?)?;
    qwen_runtime_credential_from_tokens(&refreshed).ok_or_else(|| {
        AppError::BadRequest("Qwen OAuth refresh did not produce usable credentials".into())
    })
}

pub async fn refresh_minimax_oauth_credentials() -> AppResult<HermesRuntimeCredential> {
    let (path, mut store, state) = minimax_oauth_state_from_auth_store()?;
    let refreshed = refresh_minimax_oauth_state_value(&state).await?;
    if let Some(providers) = store.get_mut("providers").and_then(Value::as_object_mut) {
        providers.insert("minimax-oauth".into(), refreshed.clone());
    } else {
        store["providers"] = json!({"minimax-oauth": refreshed.clone()});
    }
    std::fs::write(&path, serde_json::to_string_pretty(&store)?)?;
    minimax_runtime_credential_from_state(&refreshed).ok_or_else(|| {
        AppError::BadRequest("MiniMax OAuth refresh did not produce usable credentials".into())
    })
}

pub async fn start_minimax_oauth_login() -> AppResult<MiniMaxOauthStart> {
    let (code_verifier, code_challenge, state) = minimax_pkce_pair();
    let portal_base_url = minimax_portal_base_url();
    let response = reqwest::Client::new()
        .post(format!("{portal_base_url}/oauth/code"))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Accept", "application/json")
        .header("x-request-id", uuid::Uuid::new_v4().to_string())
        .form(&[
            ("response_type", "code"),
            ("client_id", MINIMAX_OAUTH_CLIENT_ID),
            ("scope", MINIMAX_OAUTH_SCOPE),
            ("code_challenge", code_challenge.as_str()),
            ("code_challenge_method", "S256"),
            ("state", state.as_str()),
        ])
        .send()
        .await
        .map_err(|error| {
            AppError::BadRequest(format!("MiniMax OAuth authorization failed: {error}"))
        })?;
    let status = response.status();
    let body = response.text().await.map_err(|error| {
        AppError::BadRequest(format!(
            "MiniMax OAuth authorization response read failed: {error}"
        ))
    })?;
    if !status.is_success() {
        return Err(AppError::BadRequest(format!(
            "MiniMax OAuth authorization failed with HTTP {status}: {body}"
        )));
    }
    let data = serde_json::from_str::<Value>(&body).map_err(|error| {
        AppError::BadRequest(format!(
            "MiniMax OAuth authorization returned invalid JSON: {error}"
        ))
    })?;
    minimax_oauth_start_from_response(&data, &state, &code_verifier)
}

pub async fn complete_minimax_oauth_login(
    user_code: &str,
    code_verifier: &str,
) -> AppResult<HermesRuntimeCredential> {
    let portal_base_url = minimax_portal_base_url();
    let response = reqwest::Client::new()
        .post(format!("{portal_base_url}/oauth/token"))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Accept", "application/json")
        .form(&[
            ("grant_type", "authorization_code"),
            ("client_id", MINIMAX_OAUTH_CLIENT_ID),
            ("code_verifier", code_verifier),
            ("user_code", user_code),
        ])
        .send()
        .await
        .map_err(|error| AppError::BadRequest(format!("MiniMax OAuth poll failed: {error}")))?;
    let status = response.status();
    let body = response.text().await.map_err(|error| {
        AppError::BadRequest(format!("MiniMax OAuth poll response read failed: {error}"))
    })?;
    if !status.is_success() {
        return Err(minimax_oauth_poll_error(status.as_u16(), &body));
    }
    let data = serde_json::from_str::<Value>(&body).map_err(|error| {
        AppError::BadRequest(format!("MiniMax OAuth poll returned invalid JSON: {error}"))
    })?;
    let state = minimax_oauth_state_from_token_response(&data)?;
    persist_minimax_oauth_state(&state)
}

pub async fn refresh_xai_oauth_credentials() -> AppResult<HermesRuntimeCredential> {
    let (path, mut store, state) = provider_state_from_auth_store("xai-oauth")?;
    let refreshed = refresh_xai_oauth_state_value(&state).await?;
    if let Some(providers) = store.get_mut("providers").and_then(Value::as_object_mut) {
        providers.insert("xai-oauth".into(), refreshed.clone());
    } else {
        store["providers"] = json!({"xai-oauth": refreshed.clone()});
    }
    std::fs::write(&path, serde_json::to_string_pretty(&store)?)?;
    xai_runtime_credential_from_state(&refreshed).ok_or_else(|| {
        AppError::BadRequest("xAI OAuth refresh did not produce usable credentials".into())
    })
}

pub async fn refresh_spotify_oauth_credentials() -> AppResult<HermesRuntimeCredential> {
    let (path, mut store, state) = provider_state_from_auth_store("spotify")?;
    let refreshed = refresh_spotify_oauth_state_value(&state).await?;
    if let Some(providers) = store.get_mut("providers").and_then(Value::as_object_mut) {
        providers.insert("spotify".into(), refreshed.clone());
    } else {
        store["providers"] = json!({"spotify": refreshed.clone()});
    }
    std::fs::write(&path, serde_json::to_string_pretty(&store)?)?;
    provider_state_credential("spotify", &json!({"providers": {"spotify": refreshed}})).ok_or_else(
        || AppError::BadRequest("Spotify OAuth refresh did not produce usable credentials".into()),
    )
}

pub async fn refresh_anthropic_oauth_credentials() -> AppResult<HermesRuntimeCredential> {
    let path = anthropic_oauth_file_candidates()
        .into_iter()
        .find(|path| path.exists())
        .ok_or_else(|| {
            AppError::BadRequest(
                "Anthropic OAuth credentials not found; run /auth login anthropic".into(),
            )
        })?;
    let payload = std::fs::read_to_string(&path)?;
    let state = serde_json::from_str::<Value>(&payload).map_err(|error| {
        AppError::BadRequest(format!("invalid Anthropic OAuth credentials: {error}"))
    })?;
    let refreshed = refresh_anthropic_oauth_state_value(&state).await?;
    persist_anthropic_oauth_credentials(&refreshed)
}

pub fn start_anthropic_oauth_login() -> AppResult<AnthropicOauthStart> {
    let (code_verifier, code_challenge, _) = minimax_pkce_pair();
    let state = code_verifier.clone();
    anthropic_oauth_start_from_parts(&state, &code_verifier, &code_challenge)
}

pub fn start_spotify_oauth_login() -> AppResult<SpotifyOauthStart> {
    let existing_state = provider_auth_state_or_empty("spotify");
    let client_id = spotify_oauth_client_id(&existing_state)?;
    let redirect_uri = spotify_oauth_redirect_uri(&existing_state);
    let scope = spotify_oauth_scope(&existing_state);
    let accounts_base_url = spotify_oauth_accounts_base_url(&existing_state);
    let (code_verifier, code_challenge, state) = minimax_pkce_pair();
    spotify_oauth_start_from_parts(
        &accounts_base_url,
        &client_id,
        &redirect_uri,
        &scope,
        &state,
        &code_verifier,
        &code_challenge,
    )
}

pub async fn complete_spotify_oauth_login(
    callback_or_code: &str,
    expected_state: &str,
    code_verifier: &str,
) -> AppResult<HermesRuntimeCredential> {
    let existing_state = provider_auth_state_or_empty("spotify");
    let client_id = spotify_oauth_client_id(&existing_state)?;
    let redirect_uri = spotify_oauth_redirect_uri(&existing_state);
    let accounts_base_url = spotify_oauth_accounts_base_url(&existing_state);
    let api_base_url = spotify_oauth_api_base_url(&existing_state);
    let scope = spotify_oauth_scope(&existing_state);
    let code = spotify_authorization_code_from_callback(callback_or_code, expected_state)?;
    let token_response = exchange_spotify_authorization_code(
        &accounts_base_url,
        &client_id,
        &redirect_uri,
        &code,
        code_verifier,
    )
    .await?;
    let state = spotify_oauth_state_from_token_response(
        &token_response,
        &existing_state,
        &client_id,
        &redirect_uri,
        &scope,
        &accounts_base_url,
        &api_base_url,
    )?;
    persist_spotify_oauth_state(&state)
}

pub async fn complete_anthropic_oauth_login(
    callback_or_code: &str,
    expected_state: &str,
    code_verifier: &str,
) -> AppResult<HermesRuntimeCredential> {
    let (code, callback_state) =
        anthropic_authorization_code_from_callback(callback_or_code, expected_state)?;
    let exchange_state = callback_state.unwrap_or_else(|| expected_state.to_string());
    let token_response =
        exchange_anthropic_authorization_code(&code, &exchange_state, code_verifier).await?;
    let state = anthropic_oauth_state_from_token_response(&token_response)?;
    persist_anthropic_oauth_credentials(&state)
}

pub async fn start_xai_oauth_login() -> AppResult<XaiOauthStart> {
    let discovery = fetch_xai_oauth_discovery().await?;
    let authorization_endpoint =
        first_string(&discovery, &["authorization_endpoint"]).ok_or_else(|| {
            AppError::BadRequest("xAI discovery missing authorization_endpoint".into())
        })?;
    let (code_verifier, code_challenge, state) = minimax_pkce_pair();
    xai_oauth_start_from_parts(
        &authorization_endpoint,
        XAI_OAUTH_REDIRECT_URI,
        &state,
        &code_verifier,
        &code_challenge,
    )
}

pub async fn complete_xai_oauth_login(
    callback_or_code: &str,
    state: &str,
    code_verifier: &str,
    code_challenge: &str,
) -> AppResult<HermesRuntimeCredential> {
    let code = xai_authorization_code_from_callback(callback_or_code, state)?;
    let token_response =
        exchange_xai_authorization_code(&code, code_verifier, code_challenge).await?;
    let state = xai_oauth_state_from_token_response(&token_response)?;
    persist_xai_oauth_state(&state)
}

pub async fn refresh_codex_oauth_credentials() -> AppResult<HermesRuntimeCredential> {
    let (path, mut store, state) = provider_state_from_auth_store("openai-codex")?;
    let refreshed = refresh_codex_oauth_state_value(&state).await?;
    sync_codex_pool_entries(&mut store, &refreshed);
    if let Some(providers) = store.get_mut("providers").and_then(Value::as_object_mut) {
        providers.insert("openai-codex".into(), refreshed.clone());
    } else {
        store["providers"] = json!({"openai-codex": refreshed.clone()});
    }
    std::fs::write(&path, serde_json::to_string_pretty(&store)?)?;
    provider_state_credential(
        "openai-codex",
        &json!({"providers": {"openai-codex": refreshed}}),
    )
    .ok_or_else(|| {
        AppError::BadRequest("Codex OAuth refresh did not produce usable credentials".into())
    })
}

pub async fn start_codex_device_code_login() -> AppResult<CodexDeviceCodeStart> {
    let response = reqwest::Client::new()
        .post(format!(
            "{CODEX_OAUTH_ISSUER_URL}/api/accounts/deviceauth/usercode"
        ))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .json(&json!({"client_id": CODEX_OAUTH_CLIENT_ID}))
        .send()
        .await
        .map_err(|error| {
            AppError::BadRequest(format!("Codex device-code request failed: {error}"))
        })?;
    let status = response.status();
    let body = response.text().await.map_err(|error| {
        AppError::BadRequest(format!("Codex device-code response read failed: {error}"))
    })?;
    if !status.is_success() {
        return Err(AppError::BadRequest(format!(
            "Codex device-code request failed with HTTP {status}: {body}"
        )));
    }
    let data = serde_json::from_str::<Value>(&body).map_err(|error| {
        AppError::BadRequest(format!(
            "Codex device-code response is invalid JSON: {error}"
        ))
    })?;
    codex_device_code_start_from_response(&data)
}

pub async fn complete_codex_device_code_login(
    device_auth_id: &str,
    user_code: &str,
) -> AppResult<HermesRuntimeCredential> {
    let response = reqwest::Client::new()
        .post(format!(
            "{CODEX_OAUTH_ISSUER_URL}/api/accounts/deviceauth/token"
        ))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .json(&json!({
            "device_auth_id": device_auth_id,
            "user_code": user_code,
        }))
        .send()
        .await
        .map_err(|error| AppError::BadRequest(format!("Codex device-code poll failed: {error}")))?;
    let status = response.status();
    let body = response.text().await.map_err(|error| {
        AppError::BadRequest(format!(
            "Codex device-code poll response read failed: {error}"
        ))
    })?;
    if matches!(status.as_u16(), 403 | 404) {
        return Err(AppError::BadRequest(
            "Codex device-code authorization is still pending; finish browser login and retry `/auth poll openai-codex <device_auth_id> <user_code>`.".into(),
        ));
    }
    if !status.is_success() {
        return Err(AppError::BadRequest(format!(
            "Codex device-code poll failed with HTTP {status}: {body}"
        )));
    }
    let data = serde_json::from_str::<Value>(&body).map_err(|error| {
        AppError::BadRequest(format!(
            "Codex device-code poll returned invalid JSON: {error}"
        ))
    })?;
    let authorization_code = first_string(&data, &["authorization_code"]).ok_or_else(|| {
        AppError::BadRequest("Codex device-code poll response missing authorization_code".into())
    })?;
    let code_verifier = first_string(&data, &["code_verifier"]).ok_or_else(|| {
        AppError::BadRequest("Codex device-code poll response missing code_verifier".into())
    })?;
    let token_response =
        exchange_codex_device_authorization_code(&authorization_code, &code_verifier).await?;
    let state = codex_device_code_state_from_token_response(&token_response)?;
    persist_codex_oauth_state(&state)
}

pub async fn refresh_google_gemini_oauth_credentials() -> AppResult<HermesRuntimeCredential> {
    let path = google_gemini_oauth_credentials_path()
        .ok_or_else(|| AppError::BadRequest("HOME/USERPROFILE is not available".into()))?;
    let payload = std::fs::read_to_string(&path)?;
    let credentials = serde_json::from_str::<Value>(&payload).map_err(|error| {
        AppError::BadRequest(format!("invalid Google Gemini OAuth credentials: {error}"))
    })?;
    let refreshed = refresh_google_gemini_oauth_credentials_value(&credentials).await?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, serde_json::to_string_pretty(&refreshed)?)?;
    google_gemini_runtime_credential_from_value(&refreshed).ok_or_else(|| {
        AppError::BadRequest(
            "Google Gemini OAuth refresh did not produce usable credentials".into(),
        )
    })
}

pub fn start_google_gemini_oauth_login() -> AppResult<GoogleGeminiOauthStart> {
    let (code_verifier, code_challenge, state) = minimax_pkce_pair();
    google_gemini_oauth_start_from_parts(&state, &code_verifier, &code_challenge)
}

pub async fn complete_google_gemini_oauth_login(
    callback_or_code: &str,
    state: &str,
    code_verifier: &str,
) -> AppResult<HermesRuntimeCredential> {
    let code = xai_authorization_code_from_callback(callback_or_code, state)?;
    let token_response = exchange_google_gemini_authorization_code(&code, code_verifier).await?;
    let creds = google_gemini_credentials_from_token_response(&token_response)?;
    persist_google_gemini_oauth_credentials(&creds)
}

pub async fn refresh_nous_oauth_credentials() -> AppResult<HermesRuntimeCredential> {
    let (path, mut store, mut state) = provider_state_from_auth_store("nous")?;
    merge_shared_nous_oauth_state(&mut state);
    let refreshed = match refresh_nous_oauth_state_value(&state).await {
        Ok(refreshed) => refreshed,
        Err(error) => {
            if let Some(reason) = terminal_nous_refresh_error_reason(&error) {
                quarantine_nous_oauth_state(&mut state, &error, reason);
                quarantine_nous_pool_entries(&mut store, &error, reason);
                if let Some(providers) = store.get_mut("providers").and_then(Value::as_object_mut) {
                    providers.insert("nous".into(), state);
                } else {
                    store["providers"] = json!({"nous": state});
                }
                let _ = std::fs::write(&path, serde_json::to_string_pretty(&store)?);
                clear_shared_nous_state();
            }
            return Err(error);
        }
    };
    sync_nous_pool_entries(&mut store, &refreshed);
    if let Some(providers) = store.get_mut("providers").and_then(Value::as_object_mut) {
        providers.insert("nous".into(), refreshed.clone());
    } else {
        store["providers"] = json!({"nous": refreshed.clone()});
    }
    std::fs::write(&path, serde_json::to_string_pretty(&store)?)?;
    write_shared_nous_state(&refreshed);
    provider_state_credential("nous", &json!({"providers": {"nous": refreshed}})).ok_or_else(|| {
        AppError::BadRequest("Nous OAuth refresh did not produce usable credentials".into())
    })
}

pub async fn start_nous_device_code_login() -> AppResult<NousDeviceCodeStart> {
    let portal_base_url = nous_portal_base_url();
    let response = reqwest::Client::new()
        .post(format!("{portal_base_url}/api/oauth/device/code"))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Accept", "application/json")
        .form(&[
            ("client_id", DEFAULT_NOUS_CLIENT_ID),
            ("scope", NOUS_INFERENCE_INVOKE_SCOPE),
        ])
        .send()
        .await
        .map_err(|error| {
            AppError::BadRequest(format!("Nous device-code request failed: {error}"))
        })?;
    let status = response.status();
    let body = response.text().await.map_err(|error| {
        AppError::BadRequest(format!("Nous device-code response read failed: {error}"))
    })?;
    if !status.is_success() {
        return Err(AppError::BadRequest(format!(
            "Nous device-code request failed with HTTP {status}: {body}"
        )));
    }
    let data = serde_json::from_str::<Value>(&body).map_err(|error| {
        AppError::BadRequest(format!(
            "Nous device-code response is invalid JSON: {error}"
        ))
    })?;
    nous_device_code_start_from_response(&data)
}

pub async fn complete_nous_device_code_login(
    device_code: &str,
) -> AppResult<HermesRuntimeCredential> {
    let portal_base_url = nous_portal_base_url();
    let response = reqwest::Client::new()
        .post(format!("{portal_base_url}/api/oauth/token"))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Accept", "application/json")
        .form(&[
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ("client_id", DEFAULT_NOUS_CLIENT_ID),
            ("device_code", device_code),
        ])
        .send()
        .await
        .map_err(|error| AppError::BadRequest(format!("Nous device-code poll failed: {error}")))?;
    let status = response.status();
    let body = response.text().await.map_err(|error| {
        AppError::BadRequest(format!(
            "Nous device-code poll response read failed: {error}"
        ))
    })?;
    if !status.is_success() {
        return Err(nous_device_code_poll_error(status.as_u16(), &body));
    }
    let data = serde_json::from_str::<Value>(&body).map_err(|error| {
        AppError::BadRequest(format!(
            "Nous device-code poll returned invalid JSON: {error}"
        ))
    })?;
    let state = nous_device_code_state_from_token_response(&data)?;
    persist_nous_oauth_state(&state)
}

fn credential_status_from_store(
    provider_id: &str,
    store: &Value,
) -> Option<HermesExternalCredentialStatus> {
    provider_state_credential_status(provider_id, store)
        .or_else(|| credential_pool_credential_status(provider_id, store))
}

fn external_provider_credential(provider_id: &str) -> Option<HermesRuntimeCredential> {
    match provider_id {
        "anthropic" => anthropic_oauth_credential(),
        "claude-code" => claude_code_oauth_credential(),
        "qwen-oauth" => qwen_cli_credential(),
        "google-gemini-cli" => google_gemini_oauth_credential(),
        _ => None,
    }
}

fn external_provider_status(provider_id: &str) -> Option<HermesExternalCredentialStatus> {
    match provider_id {
        "anthropic" => anthropic_oauth_status(),
        "claude-code" => claude_code_oauth_status(),
        "qwen-oauth" => qwen_cli_status(),
        "google-gemini-cli" => google_gemini_oauth_status(),
        _ => None,
    }
}

fn qwen_cli_status() -> Option<HermesExternalCredentialStatus> {
    let path = qwen_cli_auth_path().ok()?;
    let payload = std::fs::read_to_string(path).ok()?;
    let tokens = serde_json::from_str::<Value>(&payload).ok()?;
    let token = first_string(&tokens, &["access_token"])?;
    let expired = credential_expired(&tokens, &token);
    Some(HermesExternalCredentialStatus {
        provider_id: "qwen-oauth".into(),
        source: "qwen-cli".into(),
        state: if expired { "expired" } else { "present" },
        expires_at: first_string(&tokens, &["expiry_date", "expires_at_ms", "expires_at"]),
        note: if expired {
            Some("Qwen CLI token is expired; run /auth refresh qwen-oauth".into())
        } else {
            Some("Qwen CLI OAuth credential detected".into())
        },
    })
}

fn anthropic_oauth_status() -> Option<HermesExternalCredentialStatus> {
    let credential = anthropic_oauth_credential()?;
    Some(HermesExternalCredentialStatus {
        provider_id: "anthropic".into(),
        source: credential.source,
        state: "present",
        expires_at: credential.expires_at,
        note: Some("Hermes Anthropic dashboard PKCE credentials".into()),
    })
}

fn claude_code_oauth_status() -> Option<HermesExternalCredentialStatus> {
    let credential = claude_code_oauth_credential()?;
    Some(HermesExternalCredentialStatus {
        provider_id: "claude-code".into(),
        source: credential.source,
        state: "present",
        expires_at: credential.expires_at,
        note: Some("Claude Code setup-token credential detected".into()),
    })
}

fn anthropic_oauth_credential() -> Option<HermesRuntimeCredential> {
    let state = anthropic_oauth_file_candidates()
        .into_iter()
        .find_map(|path| {
            std::fs::read_to_string(path)
                .ok()
                .and_then(|payload| serde_json::from_str::<Value>(&payload).ok())
        })?;
    let token_source = provider_runtime_token_source("anthropic", &state)?;
    if credential_expired(token_source.expiry_value, &token_source.token) {
        return None;
    }
    Some(HermesRuntimeCredential {
        provider_id: "anthropic".into(),
        api_key: token_source.token,
        base_url: None,
        source: "hermes-oauth-file:anthropic".into(),
        expires_at: expiry_label(token_source.expiry_value),
    })
}

fn claude_code_oauth_credential() -> Option<HermesRuntimeCredential> {
    claude_code_credentials_file_credential().or_else(claude_code_env_credential)
}

fn claude_code_credentials_file_credential() -> Option<HermesRuntimeCredential> {
    let state = claude_code_credentials_file_candidates()
        .into_iter()
        .find_map(|path| {
            std::fs::read_to_string(path)
                .ok()
                .and_then(|payload| serde_json::from_str::<Value>(&payload).ok())
        })?;
    let token_source = provider_runtime_token_source("claude-code", &state)?;
    if credential_expired(token_source.expiry_value, &token_source.token) {
        return None;
    }
    Some(HermesRuntimeCredential {
        provider_id: "claude-code".into(),
        api_key: token_source.token,
        base_url: None,
        source: "claude_code_cli".into(),
        expires_at: expiry_label(token_source.expiry_value),
    })
}

fn claude_code_env_credential() -> Option<HermesRuntimeCredential> {
    let (env_name, token) = ["ANTHROPIC_TOKEN", "CLAUDE_CODE_OAUTH_TOKEN"]
        .into_iter()
        .find_map(|name| {
            std::env::var(name)
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
                .map(|value| (name, value))
        })?;
    Some(HermesRuntimeCredential {
        provider_id: "claude-code".into(),
        api_key: token,
        base_url: None,
        source: format!("env:{env_name}"),
        expires_at: None,
    })
}

fn qwen_cli_credential() -> Option<HermesRuntimeCredential> {
    let path = qwen_cli_auth_path().ok()?;
    let payload = std::fs::read_to_string(path).ok()?;
    let tokens = serde_json::from_str::<Value>(&payload).ok()?;
    qwen_runtime_credential_from_tokens(&tokens)
}

fn qwen_cli_auth_path() -> AppResult<PathBuf> {
    home_dir()
        .map(|home| home.join(".qwen").join("oauth_creds.json"))
        .ok_or_else(|| AppError::BadRequest("HOME/USERPROFILE is not available".into()))
}

fn qwen_runtime_credential_from_tokens(tokens: &Value) -> Option<HermesRuntimeCredential> {
    let token = first_string(tokens, &["access_token"])?;
    if credential_expired(&tokens, &token) {
        return None;
    }
    let base_url = std::env::var("HERMES_QWEN_BASE_URL")
        .ok()
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "https://portal.qwen.ai/v1".into());
    Some(HermesRuntimeCredential {
        provider_id: "qwen-oauth".into(),
        api_key: token,
        base_url: Some(base_url),
        source: "qwen-cli".into(),
        expires_at: first_string(&tokens, &["expiry_date", "expires_at_ms", "expires_at"]),
    })
}

fn qwen_refreshed_tokens_from_response(existing: &Value, response: &Value) -> AppResult<Value> {
    let access_token = first_string(response, &["access_token"]).ok_or_else(|| {
        AppError::BadRequest("Qwen OAuth refresh response missing access_token".into())
    })?;
    let refresh_token = first_string(response, &["refresh_token"])
        .or_else(|| first_string(existing, &["refresh_token"]))
        .ok_or_else(|| {
            AppError::BadRequest("Qwen OAuth refresh response missing refresh_token".into())
        })?;
    let expires_in = response
        .get("expires_in")
        .and_then(|value| {
            value
                .as_i64()
                .or_else(|| value.as_str()?.trim().parse::<i64>().ok())
        })
        .unwrap_or(6 * 60 * 60)
        .max(1);
    let token_type = first_string(response, &["token_type"])
        .or_else(|| first_string(existing, &["token_type"]))
        .unwrap_or_else(|| "Bearer".into());
    let resource_url = first_string(response, &["resource_url"])
        .or_else(|| first_string(existing, &["resource_url"]))
        .unwrap_or_else(|| "portal.qwen.ai".into());
    Ok(json!({
        "access_token": access_token,
        "refresh_token": refresh_token,
        "token_type": token_type,
        "resource_url": resource_url,
        "expiry_date": unix_now_seconds().saturating_mul(1000).saturating_add(expires_in as u64 * 1000),
    }))
}

fn minimax_pkce_pair() -> (String, String, String) {
    let verifier = format!(
        "{}{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let state = uuid::Uuid::new_v4().simple().to_string();
    (verifier, challenge, state)
}

fn minimax_oauth_start_from_response(
    response: &Value,
    expected_state: &str,
    code_verifier: &str,
) -> AppResult<MiniMaxOauthStart> {
    if first_string(response, &["state"]).as_deref() != Some(expected_state) {
        return Err(AppError::BadRequest(
            "MiniMax OAuth state mismatch (possible CSRF)".into(),
        ));
    }
    let user_code = first_string(response, &["user_code"]).ok_or_else(|| {
        AppError::BadRequest("MiniMax OAuth authorization response missing user_code".into())
    })?;
    let verification_uri = first_string(response, &["verification_uri"]).ok_or_else(|| {
        AppError::BadRequest("MiniMax OAuth authorization response missing verification_uri".into())
    })?;
    let expired_in = response
        .get("expired_in")
        .and_then(value_as_i64)
        .ok_or_else(|| {
            AppError::BadRequest("MiniMax OAuth authorization response missing expired_in".into())
        })?;
    let interval_ms = response.get("interval").and_then(value_as_u64);
    Ok(MiniMaxOauthStart {
        user_code,
        verification_uri,
        code_verifier: code_verifier.to_string(),
        expired_in,
        interval_ms,
        region: "global".into(),
    })
}

fn minimax_oauth_poll_error(status: u16, body: &str) -> AppError {
    let parsed = serde_json::from_str::<Value>(body).ok();
    let code = parsed
        .as_ref()
        .and_then(|value| value.get("error"))
        .and_then(Value::as_str)
        .unwrap_or("token_exchange_failed");
    let description = parsed
        .as_ref()
        .and_then(|value| {
            value
                .get("error_description")
                .or_else(|| value.get("message"))
        })
        .and_then(Value::as_str)
        .unwrap_or(body);
    if code == "authorization_pending" {
        return AppError::BadRequest(
            "MiniMax OAuth authorization is still pending; finish browser login and retry `/auth poll minimax-oauth <user_code> <code_verifier>`.".into(),
        );
    }
    AppError::BadRequest(format!(
        "MiniMax OAuth poll failed with HTTP {status}: {description} code={code}"
    ))
}

fn minimax_oauth_state_from_token_response(response: &Value) -> AppResult<Value> {
    if response.get("status").and_then(Value::as_str) != Some("success") {
        return Err(AppError::BadRequest(
            "MiniMax OAuth token response did not return success".into(),
        ));
    }
    let access_token = first_string(response, &["access_token"]).ok_or_else(|| {
        AppError::BadRequest("MiniMax OAuth token response missing access_token".into())
    })?;
    let refresh_token = first_string(response, &["refresh_token"]).ok_or_else(|| {
        AppError::BadRequest("MiniMax OAuth token response missing refresh_token".into())
    })?;
    let expired_in = response
        .get("expired_in")
        .and_then(value_as_i64)
        .ok_or_else(|| {
            AppError::BadRequest("MiniMax OAuth token response missing expired_in".into())
        })?;
    let now = unix_now_seconds();
    let expires_at = minimax_resolve_expiry_seconds(expired_in, now);
    Ok(json!({
        "provider": "minimax-oauth",
        "region": "global",
        "portal_base_url": minimax_portal_base_url(),
        "inference_base_url": MINIMAX_OAUTH_GLOBAL_INFERENCE,
        "client_id": MINIMAX_OAUTH_CLIENT_ID,
        "scope": MINIMAX_OAUTH_SCOPE,
        "token_type": first_string(response, &["token_type"]).unwrap_or_else(|| "Bearer".into()),
        "access_token": access_token,
        "refresh_token": refresh_token,
        "resource_url": first_string(response, &["resource_url"]),
        "obtained_at": chrono::Utc::now().to_rfc3339(),
        "expires_at": iso_from_unix_seconds(expires_at),
        "expires_in": expires_at.saturating_sub(now),
    }))
}

fn persist_minimax_oauth_state(state: &Value) -> AppResult<HermesRuntimeCredential> {
    let path = primary_hermes_auth_store_path()?;
    let mut store = if let Ok(payload) = std::fs::read_to_string(&path) {
        serde_json::from_str::<Value>(&payload).map_err(|error| {
            AppError::BadRequest(format!(
                "invalid Hermes auth store {}: {error}",
                path.display()
            ))
        })?
    } else {
        json!({})
    };
    if let Some(providers) = store.get_mut("providers").and_then(Value::as_object_mut) {
        providers.insert("minimax-oauth".into(), state.clone());
    } else {
        store["providers"] = json!({"minimax-oauth": state.clone()});
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, serde_json::to_string_pretty(&store)?)?;
    minimax_runtime_credential_from_state(state).ok_or_else(|| {
        AppError::BadRequest("MiniMax OAuth login did not produce usable credentials".into())
    })
}

fn minimax_portal_base_url() -> String {
    std::env::var("HERMES_MINIMAX_PORTAL_BASE_URL")
        .ok()
        .or_else(|| std::env::var("MINIMAX_PORTAL_BASE_URL").ok())
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| MINIMAX_OAUTH_GLOBAL_BASE.into())
}

fn minimax_oauth_state_from_auth_store() -> AppResult<(PathBuf, Value, Value)> {
    provider_state_from_auth_store("minimax-oauth")
}

fn provider_state_from_auth_store(provider_id: &str) -> AppResult<(PathBuf, Value, Value)> {
    for path in hermes_auth_store_candidates() {
        let Ok(payload) = std::fs::read_to_string(&path) else {
            continue;
        };
        let store = serde_json::from_str::<Value>(&payload).map_err(|error| {
            AppError::BadRequest(format!(
                "invalid Hermes auth store {}: {error}",
                path.display()
            ))
        })?;
        if let Some(state) = store
            .get("providers")
            .and_then(|providers| providers.get(provider_id))
            .filter(|state| state.is_object())
            .cloned()
        {
            return Ok((path, store, state));
        }
    }
    Err(AppError::BadRequest(format!(
        "{provider_id} credentials not found in Hermes auth store"
    )))
}

async fn refresh_minimax_oauth_state_value(state: &Value) -> AppResult<Value> {
    let refresh_token = first_string(state, &["refresh_token"]).ok_or_else(|| {
        AppError::BadRequest("MiniMax OAuth refresh_token missing; re-login required".into())
    })?;
    let portal_base = first_string(state, &["portal_base_url"])
        .unwrap_or_else(|| MINIMAX_OAUTH_GLOBAL_BASE.into())
        .trim_end_matches('/')
        .to_string();
    let client_id =
        first_string(state, &["client_id"]).unwrap_or_else(|| MINIMAX_OAUTH_CLIENT_ID.into());
    let response = reqwest::Client::new()
        .post(format!("{portal_base}/oauth/token"))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Accept", "application/json")
        .form(&[
            ("grant_type", "refresh_token"),
            ("client_id", client_id.as_str()),
            ("refresh_token", refresh_token.as_str()),
        ])
        .send()
        .await
        .map_err(|error| AppError::BadRequest(format!("MiniMax OAuth refresh failed: {error}")))?;
    let status = response.status();
    let body = response.text().await.map_err(|error| {
        AppError::BadRequest(format!("MiniMax OAuth refresh read failed: {error}"))
    })?;
    if !status.is_success() {
        let lower = body.to_ascii_lowercase();
        let relogin = lower.contains("invalid_grant")
            || lower.contains("refresh_token_reused")
            || lower.contains("invalid_refresh_token");
        return Err(AppError::BadRequest(format!(
            "MiniMax OAuth refresh failed with HTTP {status}: {body}{}",
            if relogin { " (re-login required)" } else { "" }
        )));
    }
    let response = serde_json::from_str::<Value>(&body).map_err(|error| {
        AppError::BadRequest(format!(
            "MiniMax OAuth refresh returned invalid JSON: {error}"
        ))
    })?;
    minimax_refreshed_state_from_response(state, &response)
}

fn minimax_refreshed_state_from_response(existing: &Value, response: &Value) -> AppResult<Value> {
    if response.get("status").and_then(Value::as_str) != Some("success") {
        return Err(AppError::BadRequest(
            "MiniMax OAuth refresh did not return success".into(),
        ));
    }
    let access_token = first_string(response, &["access_token"]).ok_or_else(|| {
        AppError::BadRequest("MiniMax OAuth refresh response missing access_token".into())
    })?;
    let refresh_token = first_string(response, &["refresh_token"])
        .or_else(|| first_string(existing, &["refresh_token"]))
        .ok_or_else(|| {
            AppError::BadRequest("MiniMax OAuth refresh response missing refresh_token".into())
        })?;
    let expired_in = response
        .get("expired_in")
        .and_then(|value| {
            value
                .as_i64()
                .or_else(|| value.as_str()?.trim().parse::<i64>().ok())
        })
        .ok_or_else(|| {
            AppError::BadRequest("MiniMax OAuth refresh response missing expired_in".into())
        })?;
    let now = unix_now_seconds();
    let expires_at = minimax_resolve_expiry_seconds(expired_in, now);
    let mut next = existing.clone();
    next["access_token"] = json!(access_token);
    next["refresh_token"] = json!(refresh_token);
    next["token_type"] = json!(first_string(response, &["token_type"])
        .or_else(|| first_string(existing, &["token_type"]))
        .unwrap_or_else(|| "Bearer".into()));
    next["obtained_at"] = json!(chrono::Utc::now().to_rfc3339());
    next["expires_at"] = json!(iso_from_unix_seconds(expires_at));
    next["expires_in"] = json!(expires_at.saturating_sub(now));
    if next.get("provider").is_none() {
        next["provider"] = json!("minimax-oauth");
    }
    if next.get("portal_base_url").is_none() {
        next["portal_base_url"] = json!(MINIMAX_OAUTH_GLOBAL_BASE);
    }
    if next.get("inference_base_url").is_none() {
        next["inference_base_url"] = json!(MINIMAX_OAUTH_GLOBAL_INFERENCE);
    }
    if next.get("client_id").is_none() {
        next["client_id"] = json!(MINIMAX_OAUTH_CLIENT_ID);
    }
    Ok(next)
}

fn minimax_runtime_credential_from_state(state: &Value) -> Option<HermesRuntimeCredential> {
    let token = first_string(state, &["access_token"])?;
    if credential_expired(state, &token) {
        return None;
    }
    Some(HermesRuntimeCredential {
        provider_id: "minimax-oauth".into(),
        api_key: token,
        base_url: runtime_base_url("minimax-oauth", state)
            .or_else(|| Some(MINIMAX_OAUTH_GLOBAL_INFERENCE.into())),
        source: "hermes-auth:minimax-oauth".into(),
        expires_at: expiry_label(state),
    })
}

fn minimax_resolve_expiry_seconds(expired_in: i64, now_seconds: u64) -> u64 {
    let raw = expired_in.max(1) as u64;
    let now_ms = now_seconds.saturating_mul(1000);
    if raw > now_ms.saturating_sub(60_000) {
        raw / 1000
    } else {
        now_seconds.saturating_add(raw)
    }
}

fn iso_from_unix_seconds(seconds: u64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp(seconds as i64, 0)
        .unwrap_or_else(chrono::Utc::now)
        .to_rfc3339()
}

async fn refresh_xai_oauth_state_value(state: &Value) -> AppResult<Value> {
    let tokens = state
        .get("tokens")
        .filter(|value| value.is_object())
        .ok_or_else(|| {
            AppError::BadRequest("xAI OAuth auth store state is missing tokens".into())
        })?;
    let refresh_token = first_string(tokens, &["refresh_token"]).ok_or_else(|| {
        AppError::BadRequest("xAI OAuth refresh_token missing; re-login required".into())
    })?;
    let token_endpoint = state
        .get("discovery")
        .and_then(|discovery| discovery.get("token_endpoint"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(XAI_OAUTH_TOKEN_ENDPOINT);
    validate_xai_oauth_endpoint(token_endpoint)?;
    let response = reqwest::Client::new()
        .post(token_endpoint)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Accept", "application/json")
        .form(&[
            ("grant_type", "refresh_token"),
            ("client_id", XAI_OAUTH_CLIENT_ID),
            ("refresh_token", refresh_token.as_str()),
        ])
        .send()
        .await
        .map_err(|error| AppError::BadRequest(format!("xAI OAuth refresh failed: {error}")))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|error| AppError::BadRequest(format!("xAI OAuth refresh read failed: {error}")))?;
    if !status.is_success() {
        let reauth = matches!(status.as_u16(), 400 | 401);
        return Err(AppError::BadRequest(format!(
            "xAI OAuth refresh failed with HTTP {status}: {body}{}",
            if reauth { " (re-login required)" } else { "" }
        )));
    }
    let response = serde_json::from_str::<Value>(&body).map_err(|error| {
        AppError::BadRequest(format!("xAI OAuth refresh returned invalid JSON: {error}"))
    })?;
    xai_refreshed_state_from_response(state, &response)
}

async fn fetch_xai_oauth_discovery() -> AppResult<Value> {
    let response = reqwest::Client::new()
        .get(XAI_OAUTH_DISCOVERY_URL)
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|error| AppError::BadRequest(format!("xAI OIDC discovery failed: {error}")))?;
    let status = response.status();
    let body = response.text().await.map_err(|error| {
        AppError::BadRequest(format!("xAI OIDC discovery response read failed: {error}"))
    })?;
    if !status.is_success() {
        return Err(AppError::BadRequest(format!(
            "xAI OIDC discovery failed with HTTP {status}: {body}"
        )));
    }
    let discovery = serde_json::from_str::<Value>(&body).map_err(|error| {
        AppError::BadRequest(format!("xAI OIDC discovery returned invalid JSON: {error}"))
    })?;
    let authorization_endpoint =
        first_string(&discovery, &["authorization_endpoint"]).ok_or_else(|| {
            AppError::BadRequest("xAI discovery missing authorization_endpoint".into())
        })?;
    let token_endpoint = first_string(&discovery, &["token_endpoint"])
        .ok_or_else(|| AppError::BadRequest("xAI discovery missing token_endpoint".into()))?;
    validate_xai_oauth_endpoint(&authorization_endpoint)?;
    validate_xai_oauth_endpoint(&token_endpoint)?;
    Ok(discovery)
}

fn xai_oauth_start_from_parts(
    authorization_endpoint: &str,
    redirect_uri: &str,
    state: &str,
    code_verifier: &str,
    code_challenge: &str,
) -> AppResult<XaiOauthStart> {
    validate_xai_oauth_endpoint(authorization_endpoint)?;
    let mut url = reqwest::Url::parse(authorization_endpoint).map_err(|error| {
        AppError::BadRequest(format!("invalid xAI authorization endpoint: {error}"))
    })?;
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", XAI_OAUTH_CLIENT_ID)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("scope", XAI_OAUTH_SCOPE)
        .append_pair("code_challenge", code_challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", state)
        .append_pair("nonce", uuid::Uuid::new_v4().simple().to_string().as_str())
        .append_pair("plan", "generic")
        .append_pair("referrer", "hermes-agent");
    Ok(XaiOauthStart {
        authorize_url: url.to_string(),
        redirect_uri: redirect_uri.to_string(),
        state: state.to_string(),
        code_verifier: code_verifier.to_string(),
        code_challenge: code_challenge.to_string(),
    })
}

fn anthropic_oauth_start_from_parts(
    state: &str,
    code_verifier: &str,
    code_challenge: &str,
) -> AppResult<AnthropicOauthStart> {
    if state.trim().is_empty()
        || code_verifier.trim().is_empty()
        || code_challenge.trim().is_empty()
    {
        return Err(AppError::BadRequest(
            "Anthropic OAuth PKCE state/verifier/challenge cannot be empty".into(),
        ));
    }
    let mut url = reqwest::Url::parse(ANTHROPIC_OAUTH_AUTHORIZE_URL).map_err(|error| {
        AppError::BadRequest(format!("invalid Anthropic authorize URL: {error}"))
    })?;
    url.query_pairs_mut()
        .append_pair("code", "true")
        .append_pair("client_id", ANTHROPIC_OAUTH_CLIENT_ID)
        .append_pair("response_type", "code")
        .append_pair("redirect_uri", ANTHROPIC_OAUTH_REDIRECT_URI)
        .append_pair("scope", ANTHROPIC_OAUTH_SCOPES)
        .append_pair("code_challenge", code_challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", state);
    Ok(AnthropicOauthStart {
        authorize_url: url.to_string(),
        redirect_uri: ANTHROPIC_OAUTH_REDIRECT_URI.into(),
        state: state.to_string(),
        code_verifier: code_verifier.to_string(),
        code_challenge: code_challenge.to_string(),
    })
}

fn anthropic_authorization_code_from_callback(
    callback_or_code: &str,
    expected_state: &str,
) -> AppResult<(String, Option<String>)> {
    let raw = callback_or_code.trim();
    if raw.is_empty() {
        return Err(AppError::BadRequest(
            "Anthropic callback/code is empty".into(),
        ));
    }
    if !raw.contains("code=") && !raw.contains("state=") && !raw.contains("error=") {
        let mut parts = raw.splitn(2, '#');
        let code = parts.next().unwrap_or_default().trim().to_string();
        let state = parts
            .next()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        if code.is_empty() {
            return Err(AppError::BadRequest(
                "Anthropic callback missing authorization code".into(),
            ));
        }
        if let Some(state) = state.as_deref() {
            if state != expected_state {
                return Err(AppError::BadRequest(
                    "Anthropic authorization failed: state mismatch".into(),
                ));
            }
        }
        return Ok((code, state));
    }
    let parsed = if raw.starts_with('?') {
        reqwest::Url::parse(&format!("{ANTHROPIC_OAUTH_REDIRECT_URI}{raw}"))
    } else {
        reqwest::Url::parse(raw)
    }
    .map_err(|error| AppError::BadRequest(format!("invalid Anthropic callback URL: {error}")))?;
    let pairs = parsed
        .query_pairs()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect::<std::collections::BTreeMap<_, _>>();
    if let Some(error) = pairs.get("error") {
        return Err(AppError::BadRequest(format!(
            "Anthropic authorization failed: {}",
            pairs.get("error_description").unwrap_or(error)
        )));
    }
    let callback_state = pairs
        .get("state")
        .map(String::as_str)
        .ok_or_else(|| AppError::BadRequest("Anthropic callback missing state".into()))?;
    if callback_state != expected_state {
        return Err(AppError::BadRequest(
            "Anthropic authorization failed: state mismatch".into(),
        ));
    }
    let code = pairs
        .get("code")
        .cloned()
        .filter(|code| !code.trim().is_empty())
        .ok_or_else(|| {
            AppError::BadRequest("Anthropic callback missing authorization code".into())
        })?;
    Ok((code, Some(callback_state.to_string())))
}

async fn exchange_anthropic_authorization_code(
    code: &str,
    state: &str,
    code_verifier: &str,
) -> AppResult<Value> {
    if code_verifier.trim().is_empty() {
        return Err(AppError::BadRequest(
            "Anthropic PKCE code_verifier is empty".into(),
        ));
    }
    let response = reqwest::Client::new()
        .post(ANTHROPIC_OAUTH_TOKEN_URL)
        .header("Content-Type", "application/json")
        .header("User-Agent", "hermes-dashboard/1.0")
        .json(&json!({
            "grant_type": "authorization_code",
            "client_id": ANTHROPIC_OAUTH_CLIENT_ID,
            "code": code,
            "state": state,
            "redirect_uri": ANTHROPIC_OAUTH_REDIRECT_URI,
            "code_verifier": code_verifier,
        }))
        .send()
        .await
        .map_err(|error| {
            AppError::BadRequest(format!("Anthropic token exchange failed: {error}"))
        })?;
    let status = response.status();
    let body = response.text().await.map_err(|error| {
        AppError::BadRequest(format!(
            "Anthropic token exchange response read failed: {error}"
        ))
    })?;
    if !status.is_success() {
        return Err(AppError::BadRequest(format!(
            "Anthropic token exchange failed with HTTP {status}: {body}"
        )));
    }
    serde_json::from_str::<Value>(&body).map_err(|error| {
        AppError::BadRequest(format!(
            "Anthropic token exchange returned invalid JSON: {error}"
        ))
    })
}

fn anthropic_oauth_state_from_token_response(response: &Value) -> AppResult<Value> {
    let access_token =
        first_string(response, &["access_token", "accessToken"]).ok_or_else(|| {
            AppError::BadRequest("Anthropic token exchange response missing access_token".into())
        })?;
    let refresh_token =
        first_string(response, &["refresh_token", "refreshToken"]).unwrap_or_default();
    let expires_in = response
        .get("expires_in")
        .and_then(value_as_u64)
        .unwrap_or(3600);
    Ok(json!({
        "accessToken": access_token,
        "refreshToken": refresh_token,
        "expiresAt": unix_now_seconds().saturating_mul(1000).saturating_add(expires_in.saturating_mul(1000)),
    }))
}

async fn refresh_anthropic_oauth_state_value(existing: &Value) -> AppResult<Value> {
    let refresh_token =
        first_string(existing, &["refreshToken", "refresh_token"]).ok_or_else(|| {
            AppError::BadRequest("Anthropic OAuth refreshToken missing; re-login required".into())
        })?;
    let response = reqwest::Client::new()
        .post(ANTHROPIC_OAUTH_TOKEN_URL)
        .header("Content-Type", "application/json")
        .header("User-Agent", "hermes-dashboard/1.0")
        .json(&json!({
            "grant_type": "refresh_token",
            "client_id": ANTHROPIC_OAUTH_CLIENT_ID,
            "refresh_token": refresh_token,
        }))
        .send()
        .await
        .map_err(|error| {
            AppError::BadRequest(format!("Anthropic OAuth refresh failed: {error}"))
        })?;
    let status = response.status();
    let body = response.text().await.map_err(|error| {
        AppError::BadRequest(format!(
            "Anthropic OAuth refresh response read failed: {error}"
        ))
    })?;
    if !status.is_success() {
        let reauth = matches!(status.as_u16(), 400 | 401);
        return Err(AppError::BadRequest(format!(
            "Anthropic OAuth refresh failed with HTTP {status}: {body}{}",
            if reauth { " (re-login required)" } else { "" }
        )));
    }
    let response = serde_json::from_str::<Value>(&body).map_err(|error| {
        AppError::BadRequest(format!(
            "Anthropic OAuth refresh returned invalid JSON: {error}"
        ))
    })?;
    anthropic_refreshed_state_from_response(existing, &response)
}

fn anthropic_refreshed_state_from_response(existing: &Value, response: &Value) -> AppResult<Value> {
    let access_token =
        first_string(response, &["access_token", "accessToken"]).ok_or_else(|| {
            AppError::BadRequest("Anthropic OAuth refresh response missing access_token".into())
        })?;
    let refresh_token = first_string(response, &["refresh_token", "refreshToken"])
        .or_else(|| first_string(existing, &["refreshToken", "refresh_token"]))
        .ok_or_else(|| {
            AppError::BadRequest("Anthropic OAuth refresh response missing refresh_token".into())
        })?;
    let expires_in = response
        .get("expires_in")
        .and_then(value_as_u64)
        .unwrap_or(3600);
    Ok(json!({
        "accessToken": access_token,
        "refreshToken": refresh_token,
        "expiresAt": unix_now_seconds().saturating_mul(1000).saturating_add(expires_in.saturating_mul(1000)),
    }))
}

fn persist_anthropic_oauth_credentials(state: &Value) -> AppResult<HermesRuntimeCredential> {
    let access_token = first_string(state, &["accessToken", "access_token"])
        .ok_or_else(|| AppError::BadRequest("Anthropic OAuth state missing access token".into()))?;
    let refresh_token = first_string(state, &["refreshToken", "refresh_token"]).unwrap_or_default();
    let expires_at_ms = state
        .get("expiresAt")
        .or_else(|| state.get("expires_at_ms"))
        .cloned()
        .unwrap_or_else(|| json!(unix_now_seconds().saturating_add(3600).saturating_mul(1000)));
    let file_state = json!({
        "accessToken": access_token,
        "refreshToken": refresh_token,
        "expiresAt": expires_at_ms,
    });
    let oauth_path = anthropic_oauth_file_candidates()
        .into_iter()
        .next()
        .ok_or_else(|| AppError::BadRequest("HOME/USERPROFILE is not available".into()))?;
    if let Some(parent) = oauth_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&oauth_path, serde_json::to_string_pretty(&file_state)?)?;

    let auth_store_path = primary_hermes_auth_store_path()?;
    let mut store = read_hermes_auth_store_path(&auth_store_path)?;
    if !store.is_object() {
        store = json!({});
    }
    if store.get("credential_pool").is_none() || !store["credential_pool"].is_object() {
        store["credential_pool"] = json!({});
    }
    let pool = store
        .get_mut("credential_pool")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| AppError::BadRequest("Hermes credential_pool is not an object".into()))?;
    let entries = pool.entry("anthropic").or_insert_with(|| json!([]));
    if !entries.is_array() {
        *entries = json!([]);
    }
    if let Some(items) = entries.as_array_mut() {
        items.retain(|entry| {
            !first_string(entry, &["source"])
                .is_some_and(|source| source.starts_with("manual:dashboard_pkce"))
        });
        items.insert(
            0,
            json!({
                "id": short_credential_pool_id(),
                "label": "dashboard PKCE",
                "auth_type": "oauth",
                "priority": 0,
                "source": "manual:dashboard_pkce",
                "access_token": file_state["accessToken"],
                "refresh_token": file_state["refreshToken"],
                "expires_at_ms": file_state["expiresAt"],
            }),
        );
    }
    write_hermes_auth_store_path(&auth_store_path, &store)?;
    Ok(HermesRuntimeCredential {
        provider_id: "anthropic".into(),
        api_key: file_state["accessToken"]
            .as_str()
            .unwrap_or_default()
            .into(),
        base_url: None,
        source: "hermes-auth:anthropic-dashboard-pkce".into(),
        expires_at: expiry_label(&file_state),
    })
}

fn xai_authorization_code_from_callback(
    callback_or_code: &str,
    expected_state: &str,
) -> AppResult<String> {
    let raw = callback_or_code.trim();
    if raw.is_empty() {
        return Err(AppError::BadRequest("xAI callback/code is empty".into()));
    }
    if !raw.contains("code=") && !raw.contains("state=") && !raw.contains("error=") {
        return Ok(raw.to_string());
    }
    let parsed = if raw.starts_with('?') {
        reqwest::Url::parse(&format!("{XAI_OAUTH_REDIRECT_URI}{raw}"))
    } else {
        reqwest::Url::parse(raw)
    }
    .map_err(|error| AppError::BadRequest(format!("invalid xAI callback URL: {error}")))?;
    let pairs = parsed
        .query_pairs()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect::<std::collections::BTreeMap<_, _>>();
    if let Some(error) = pairs.get("error") {
        return Err(AppError::BadRequest(format!(
            "xAI authorization failed: {}",
            pairs.get("error_description").unwrap_or(error)
        )));
    }
    let callback_state = pairs
        .get("state")
        .map(String::as_str)
        .ok_or_else(|| AppError::BadRequest("xAI callback missing state".into()))?;
    if callback_state != expected_state {
        return Err(AppError::BadRequest(
            "xAI authorization failed: state mismatch".into(),
        ));
    }
    pairs
        .get("code")
        .cloned()
        .filter(|code| !code.trim().is_empty())
        .ok_or_else(|| AppError::BadRequest("xAI callback missing authorization code".into()))
}

fn spotify_oauth_start_from_parts(
    accounts_base_url: &str,
    client_id: &str,
    redirect_uri: &str,
    scope: &str,
    state: &str,
    code_verifier: &str,
    code_challenge: &str,
) -> AppResult<SpotifyOauthStart> {
    if client_id.trim().is_empty()
        || redirect_uri.trim().is_empty()
        || scope.trim().is_empty()
        || state.trim().is_empty()
        || code_verifier.trim().is_empty()
        || code_challenge.trim().is_empty()
    {
        return Err(AppError::BadRequest(
            "Spotify OAuth PKCE client_id/redirect/scope/state/verifier/challenge cannot be empty"
                .into(),
        ));
    }
    let mut url = reqwest::Url::parse(&format!(
        "{}/authorize",
        accounts_base_url.trim().trim_end_matches('/')
    ))
    .map_err(|error| AppError::BadRequest(format!("invalid Spotify authorize URL: {error}")))?;
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", client_id)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("scope", scope)
        .append_pair("code_challenge_method", "S256")
        .append_pair("code_challenge", code_challenge)
        .append_pair("state", state);
    Ok(SpotifyOauthStart {
        authorize_url: url.to_string(),
        redirect_uri: redirect_uri.to_string(),
        state: state.to_string(),
        code_verifier: code_verifier.to_string(),
        code_challenge: code_challenge.to_string(),
        client_id: client_id.to_string(),
        scope: scope.to_string(),
    })
}

fn spotify_authorization_code_from_callback(
    callback_or_code: &str,
    expected_state: &str,
) -> AppResult<String> {
    let raw = callback_or_code.trim();
    if raw.is_empty() {
        return Err(AppError::BadRequest(
            "Spotify callback/code is empty".into(),
        ));
    }
    if !raw.contains("code=") && !raw.contains("state=") && !raw.contains("error=") {
        return Ok(raw.to_string());
    }
    let parsed = if raw.starts_with('?') {
        reqwest::Url::parse(&format!("{DEFAULT_SPOTIFY_REDIRECT_URI}{raw}"))
    } else {
        reqwest::Url::parse(raw)
    }
    .map_err(|error| AppError::BadRequest(format!("invalid Spotify callback URL: {error}")))?;
    let pairs = parsed
        .query_pairs()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect::<std::collections::BTreeMap<_, _>>();
    if let Some(error) = pairs.get("error") {
        return Err(AppError::BadRequest(format!(
            "Spotify authorization failed: {}",
            pairs.get("error_description").unwrap_or(error)
        )));
    }
    let callback_state = pairs
        .get("state")
        .map(String::as_str)
        .ok_or_else(|| AppError::BadRequest("Spotify callback missing state".into()))?;
    if callback_state != expected_state {
        return Err(AppError::BadRequest(
            "Spotify authorization failed: state mismatch".into(),
        ));
    }
    pairs
        .get("code")
        .cloned()
        .filter(|code| !code.trim().is_empty())
        .ok_or_else(|| AppError::BadRequest("Spotify callback missing authorization code".into()))
}

async fn exchange_spotify_authorization_code(
    accounts_base_url: &str,
    client_id: &str,
    redirect_uri: &str,
    code: &str,
    code_verifier: &str,
) -> AppResult<Value> {
    let token_url = format!(
        "{}/api/token",
        accounts_base_url.trim().trim_end_matches('/')
    );
    let form = vec![
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("client_id", client_id),
        ("code_verifier", code_verifier),
    ];
    let response = reqwest::Client::new()
        .post(&token_url)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Accept", "application/json")
        .form(&form)
        .send()
        .await
        .map_err(|error| {
            AppError::BadRequest(format!("Spotify OAuth token exchange failed: {error}"))
        })?;
    let status = response.status();
    let body = response.text().await.map_err(|error| {
        AppError::BadRequest(format!(
            "Spotify OAuth token exchange response read failed: {error}"
        ))
    })?;
    if !status.is_success() {
        return Err(AppError::BadRequest(format!(
            "Spotify OAuth token exchange failed with HTTP {status}: {body}"
        )));
    }
    serde_json::from_str::<Value>(&body).map_err(|error| {
        AppError::BadRequest(format!(
            "Spotify OAuth token exchange returned invalid JSON: {error}"
        ))
    })
}

async fn refresh_spotify_oauth_state_value(state: &Value) -> AppResult<Value> {
    let refresh_token =
        first_string(state, &["refresh_token", "refreshToken"]).ok_or_else(|| {
            AppError::BadRequest("Spotify OAuth refresh_token missing; re-login required".into())
        })?;
    let client_id = spotify_oauth_client_id(state)?;
    let accounts_base_url = spotify_oauth_accounts_base_url(state);
    let token_url = format!(
        "{}/api/token",
        accounts_base_url.trim().trim_end_matches('/')
    );
    let mut form = vec![
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token.as_str()),
        ("client_id", client_id.as_str()),
    ];
    let client_secret = std::env::var("HERMES_SPOTIFY_CLIENT_SECRET")
        .ok()
        .or_else(|| std::env::var("SPOTIFY_CLIENT_SECRET").ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .or_else(|| first_string(state, &["client_secret", "clientSecret"]));
    if let Some(secret) = client_secret.as_deref() {
        form.push(("client_secret", secret));
    }
    let response = reqwest::Client::new()
        .post(&token_url)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Accept", "application/json")
        .form(&form)
        .send()
        .await
        .map_err(|error| AppError::BadRequest(format!("Spotify OAuth refresh failed: {error}")))?;
    let status = response.status();
    let body = response.text().await.map_err(|error| {
        AppError::BadRequest(format!(
            "Spotify OAuth refresh response read failed: {error}"
        ))
    })?;
    if !status.is_success() {
        let lower = body.to_ascii_lowercase();
        let relogin = lower.contains("invalid_grant")
            || lower.contains("invalid_token")
            || lower.contains("invalid_refresh_token");
        return Err(AppError::BadRequest(format!(
            "Spotify OAuth refresh failed with HTTP {status}: {body}{}",
            if relogin { " (re-login required)" } else { "" }
        )));
    }
    let response = serde_json::from_str::<Value>(&body).map_err(|error| {
        AppError::BadRequest(format!(
            "Spotify OAuth refresh returned invalid JSON: {error}"
        ))
    })?;
    spotify_refreshed_state_from_response(state, &response)
}

fn spotify_oauth_state_from_token_response(
    response: &Value,
    existing_state: &Value,
    client_id: &str,
    redirect_uri: &str,
    scope: &str,
    accounts_base_url: &str,
    api_base_url: &str,
) -> AppResult<Value> {
    let access_token = first_string(response, &["access_token"]).ok_or_else(|| {
        AppError::BadRequest("Spotify token exchange response missing access_token".into())
    })?;
    let refresh_token = first_string(response, &["refresh_token"])
        .or_else(|| first_string(existing_state, &["refresh_token", "refreshToken"]))
        .ok_or_else(|| {
            AppError::BadRequest("Spotify token exchange response missing refresh_token".into())
        })?;
    let expires_in = response
        .get("expires_in")
        .and_then(value_as_u64)
        .unwrap_or(3600);
    let expires_at =
        chrono::Utc::now() + chrono::Duration::seconds(i64::try_from(expires_in).unwrap_or(3600));
    Ok(json!({
        "access_token": access_token,
        "refresh_token": refresh_token,
        "token_type": first_string(response, &["token_type"]).unwrap_or_else(|| "Bearer".into()),
        "scope": first_string(response, &["scope"]).unwrap_or_else(|| scope.to_string()),
        "expires_in": expires_in,
        "expires_at": expires_at.to_rfc3339(),
        "obtained_at": chrono::Utc::now().to_rfc3339(),
        "client_id": client_id,
        "redirect_uri": redirect_uri,
        "accounts_base_url": accounts_base_url.trim().trim_end_matches('/'),
        "api_base_url": api_base_url.trim().trim_end_matches('/'),
        "base_url": api_base_url.trim().trim_end_matches('/'),
        "auth_type": "oauth_pkce",
        "source": "oauth-loopback",
    }))
}

fn spotify_refreshed_state_from_response(existing: &Value, response: &Value) -> AppResult<Value> {
    let access_token = first_string(response, &["access_token"]).ok_or_else(|| {
        AppError::BadRequest("Spotify OAuth refresh response missing access_token".into())
    })?;
    let refresh_token = first_string(response, &["refresh_token"])
        .or_else(|| first_string(existing, &["refresh_token", "refreshToken"]))
        .ok_or_else(|| {
            AppError::BadRequest("Spotify OAuth refresh response missing refresh_token".into())
        })?;
    let expires_in = response
        .get("expires_in")
        .and_then(value_as_u64)
        .or_else(|| existing.get("expires_in").and_then(value_as_u64))
        .unwrap_or(3600);
    let expires_at =
        chrono::Utc::now() + chrono::Duration::seconds(i64::try_from(expires_in).unwrap_or(3600));
    let mut next = existing.clone();
    next["access_token"] = json!(access_token);
    next["refresh_token"] = json!(refresh_token);
    next["token_type"] = json!(first_string(response, &["token_type"])
        .or_else(|| first_string(existing, &["token_type"]))
        .unwrap_or_else(|| "Bearer".into()));
    next["scope"] = json!(first_string(response, &["scope"])
        .or_else(|| first_string(existing, &["scope"]))
        .unwrap_or_else(|| DEFAULT_SPOTIFY_SCOPE.into()));
    next["expires_in"] = json!(expires_in);
    next["expires_at"] = json!(expires_at.to_rfc3339());
    next["obtained_at"] = json!(chrono::Utc::now().to_rfc3339());
    if next.get("client_id").is_none() {
        next["client_id"] = json!(spotify_oauth_client_id(existing)?);
    }
    if next.get("redirect_uri").is_none() {
        next["redirect_uri"] = json!(spotify_oauth_redirect_uri(existing));
    }
    if next.get("accounts_base_url").is_none() {
        next["accounts_base_url"] = json!(spotify_oauth_accounts_base_url(existing));
    }
    if next.get("api_base_url").is_none() {
        next["api_base_url"] = json!(spotify_oauth_api_base_url(existing));
    }
    if next.get("base_url").is_none() {
        next["base_url"] = json!(spotify_oauth_api_base_url(existing));
    }
    if next.get("auth_type").is_none() {
        next["auth_type"] = json!("oauth_pkce");
    }
    if next.get("source").is_none() {
        next["source"] = json!("oauth-refresh");
    }
    Ok(next)
}

fn persist_spotify_oauth_state(state: &Value) -> AppResult<HermesRuntimeCredential> {
    let path = primary_hermes_auth_store_path()?;
    let mut store = if let Ok(payload) = std::fs::read_to_string(&path) {
        serde_json::from_str::<Value>(&payload).map_err(|error| {
            AppError::BadRequest(format!(
                "invalid Hermes auth store {}: {error}",
                path.display()
            ))
        })?
    } else {
        json!({})
    };
    if let Some(providers) = store.get_mut("providers").and_then(Value::as_object_mut) {
        providers.insert("spotify".into(), state.clone());
    } else {
        store["providers"] = json!({"spotify": state.clone()});
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, serde_json::to_string_pretty(&store)?)?;
    provider_state_credential("spotify", &json!({"providers": {"spotify": state.clone()}}))
        .ok_or_else(|| {
            AppError::BadRequest("Spotify OAuth login did not produce usable credentials".into())
        })
}

async fn exchange_xai_authorization_code(
    code: &str,
    code_verifier: &str,
    code_challenge: &str,
) -> AppResult<Value> {
    if code_verifier.trim().is_empty() {
        return Err(AppError::BadRequest(
            "xAI PKCE code_verifier is empty".into(),
        ));
    }
    let response = reqwest::Client::new()
        .post(XAI_OAUTH_TOKEN_ENDPOINT)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Accept", "application/json")
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", XAI_OAUTH_REDIRECT_URI),
            ("client_id", XAI_OAUTH_CLIENT_ID),
            ("code_verifier", code_verifier),
            ("code_challenge", code_challenge),
            ("code_challenge_method", "S256"),
        ])
        .send()
        .await
        .map_err(|error| AppError::BadRequest(format!("xAI token exchange failed: {error}")))?;
    let status = response.status();
    let body = response.text().await.map_err(|error| {
        AppError::BadRequest(format!("xAI token exchange response read failed: {error}"))
    })?;
    if !status.is_success() {
        return Err(AppError::BadRequest(format!(
            "xAI token exchange failed with HTTP {status}: {body}"
        )));
    }
    serde_json::from_str::<Value>(&body).map_err(|error| {
        AppError::BadRequest(format!("xAI token exchange returned invalid JSON: {error}"))
    })
}

fn xai_oauth_state_from_token_response(response: &Value) -> AppResult<Value> {
    let access_token = first_string(response, &["access_token"]).ok_or_else(|| {
        AppError::BadRequest("xAI token exchange response missing access_token".into())
    })?;
    let refresh_token = first_string(response, &["refresh_token"]).ok_or_else(|| {
        AppError::BadRequest("xAI token exchange response missing refresh_token".into())
    })?;
    let mut tokens = json!({
        "access_token": access_token,
        "refresh_token": refresh_token,
        "token_type": first_string(response, &["token_type"]).unwrap_or_else(|| "Bearer".into()),
    });
    if let Some(id_token) = first_string(response, &["id_token"]) {
        tokens["id_token"] = json!(id_token);
    }
    if let Some(expires_in) = response.get("expires_in") {
        tokens["expires_in"] = expires_in.clone();
    }
    Ok(json!({
        "tokens": tokens,
        "last_refresh": chrono::Utc::now().to_rfc3339(),
        "auth_mode": "oauth_pkce",
        "redirect_uri": XAI_OAUTH_REDIRECT_URI,
        "base_url": XAI_OAUTH_BASE_URL,
        "source": "oauth-loopback",
        "discovery": {
            "authorization_endpoint": "https://auth.x.ai/oauth/authorize",
            "token_endpoint": XAI_OAUTH_TOKEN_ENDPOINT,
        }
    }))
}

fn persist_xai_oauth_state(state: &Value) -> AppResult<HermesRuntimeCredential> {
    let path = primary_hermes_auth_store_path()?;
    let mut store = if let Ok(payload) = std::fs::read_to_string(&path) {
        serde_json::from_str::<Value>(&payload).map_err(|error| {
            AppError::BadRequest(format!(
                "invalid Hermes auth store {}: {error}",
                path.display()
            ))
        })?
    } else {
        json!({})
    };
    if let Some(providers) = store.get_mut("providers").and_then(Value::as_object_mut) {
        providers.insert("xai-oauth".into(), state.clone());
    } else {
        store["providers"] = json!({"xai-oauth": state.clone()});
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, serde_json::to_string_pretty(&store)?)?;
    xai_runtime_credential_from_state(state).ok_or_else(|| {
        AppError::BadRequest("xAI OAuth login did not produce usable credentials".into())
    })
}

fn xai_refreshed_state_from_response(existing: &Value, response: &Value) -> AppResult<Value> {
    let access_token = first_string(response, &["access_token"]).ok_or_else(|| {
        AppError::BadRequest("xAI OAuth refresh response missing access_token".into())
    })?;
    let existing_tokens = existing.get("tokens").unwrap_or(&Value::Null);
    let refresh_token = first_string(response, &["refresh_token"])
        .or_else(|| first_string(existing_tokens, &["refresh_token"]))
        .ok_or_else(|| {
            AppError::BadRequest("xAI OAuth refresh response missing refresh_token".into())
        })?;
    let mut next = existing.clone();
    let mut tokens = existing_tokens.clone();
    if !tokens.is_object() {
        tokens = json!({});
    }
    tokens["access_token"] = json!(access_token);
    tokens["refresh_token"] = json!(refresh_token);
    if let Some(id_token) = first_string(response, &["id_token"]) {
        tokens["id_token"] = json!(id_token);
    }
    if let Some(expires_in) = response.get("expires_in") {
        tokens["expires_in"] = expires_in.clone();
    }
    tokens["token_type"] = json!(first_string(response, &["token_type"])
        .or_else(|| first_string(existing_tokens, &["token_type"]))
        .unwrap_or_else(|| "Bearer".into()));
    next["tokens"] = tokens;
    next["last_refresh"] = json!(chrono::Utc::now().to_rfc3339());
    if next.get("discovery").is_none() {
        next["discovery"] = json!({"token_endpoint": XAI_OAUTH_TOKEN_ENDPOINT});
    }
    Ok(next)
}

fn xai_runtime_credential_from_state(state: &Value) -> Option<HermesRuntimeCredential> {
    let tokens = state.get("tokens").filter(|value| value.is_object())?;
    let token = first_string(tokens, &["access_token"])?;
    if credential_expired(tokens, &token) {
        return None;
    }
    Some(HermesRuntimeCredential {
        provider_id: "xai-oauth".into(),
        api_key: token,
        base_url: runtime_base_url("xai-oauth", state).or_else(|| Some(XAI_OAUTH_BASE_URL.into())),
        source: "hermes-auth:xai-oauth".into(),
        expires_at: expiry_label(tokens).or_else(|| expiry_label(state)),
    })
}

fn validate_xai_oauth_endpoint(endpoint: &str) -> AppResult<()> {
    let url = reqwest::Url::parse(endpoint)
        .map_err(|error| AppError::BadRequest(format!("invalid xAI OAuth endpoint: {error}")))?;
    if url.scheme() != "https" {
        return Err(AppError::BadRequest(
            "xAI OAuth endpoint must use https".into(),
        ));
    }
    let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    if host == "x.ai" || host.ends_with(".x.ai") {
        Ok(())
    } else {
        Err(AppError::BadRequest(format!(
            "refusing non-xAI OAuth endpoint host: {host}"
        )))
    }
}

async fn refresh_codex_oauth_state_value(state: &Value) -> AppResult<Value> {
    let tokens = state.get("tokens").unwrap_or(state);
    let refresh_token = first_string(tokens, &["refresh_token"]).ok_or_else(|| {
        AppError::BadRequest("Codex OAuth refresh_token missing; re-login required".into())
    })?;
    let response = reqwest::Client::new()
        .post(CODEX_OAUTH_TOKEN_URL)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Accept", "application/json")
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token.as_str()),
            ("client_id", CODEX_OAUTH_CLIENT_ID),
        ])
        .send()
        .await
        .map_err(|error| AppError::BadRequest(format!("Codex OAuth refresh failed: {error}")))?;
    let status = response.status();
    let body = response.text().await.map_err(|error| {
        AppError::BadRequest(format!("Codex OAuth refresh read failed: {error}"))
    })?;
    if status.as_u16() == 429 {
        return Err(AppError::BadRequest(format!(
            "Codex OAuth refresh hit HTTP 429 quota/rate limit; credentials are still valid: {body}"
        )));
    }
    if !status.is_success() {
        return Err(codex_refresh_error(status.as_u16(), &body));
    }
    let response = serde_json::from_str::<Value>(&body).map_err(|error| {
        AppError::BadRequest(format!(
            "Codex OAuth refresh returned invalid JSON: {error}"
        ))
    })?;
    codex_refreshed_state_from_response(state, &response)
}

fn codex_device_code_start_from_response(response: &Value) -> AppResult<CodexDeviceCodeStart> {
    let user_code = first_string(response, &["user_code"]).ok_or_else(|| {
        AppError::BadRequest("Codex device-code response missing user_code".into())
    })?;
    let device_auth_id = first_string(response, &["device_auth_id"]).ok_or_else(|| {
        AppError::BadRequest("Codex device-code response missing device_auth_id".into())
    })?;
    let interval_seconds = response
        .get("interval")
        .and_then(|value| {
            value
                .as_u64()
                .or_else(|| value.as_str()?.trim().parse::<u64>().ok())
        })
        .unwrap_or(5)
        .max(3);
    Ok(CodexDeviceCodeStart {
        user_code,
        device_auth_id,
        verification_uri: format!("{CODEX_OAUTH_ISSUER_URL}/codex/device"),
        interval_seconds,
    })
}

async fn exchange_codex_device_authorization_code(
    authorization_code: &str,
    code_verifier: &str,
) -> AppResult<Value> {
    let redirect_uri = format!("{CODEX_OAUTH_ISSUER_URL}/deviceauth/callback");
    let response = reqwest::Client::new()
        .post(CODEX_OAUTH_TOKEN_URL)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Accept", "application/json")
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", authorization_code),
            ("redirect_uri", redirect_uri.as_str()),
            ("client_id", CODEX_OAUTH_CLIENT_ID),
            ("code_verifier", code_verifier),
        ])
        .send()
        .await
        .map_err(|error| AppError::BadRequest(format!("Codex token exchange failed: {error}")))?;
    let status = response.status();
    let body = response.text().await.map_err(|error| {
        AppError::BadRequest(format!(
            "Codex token exchange response read failed: {error}"
        ))
    })?;
    if !status.is_success() {
        return Err(AppError::BadRequest(format!(
            "Codex token exchange failed with HTTP {status}: {body}"
        )));
    }
    serde_json::from_str::<Value>(&body).map_err(|error| {
        AppError::BadRequest(format!(
            "Codex token exchange returned invalid JSON: {error}"
        ))
    })
}

fn codex_device_code_state_from_token_response(response: &Value) -> AppResult<Value> {
    let access_token = first_string(response, &["access_token"]).ok_or_else(|| {
        AppError::BadRequest("Codex token exchange response missing access_token".into())
    })?;
    let refresh_token = first_string(response, &["refresh_token"]).ok_or_else(|| {
        AppError::BadRequest("Codex token exchange response missing refresh_token".into())
    })?;
    let mut tokens = json!({
        "access_token": access_token,
        "refresh_token": refresh_token,
        "token_type": first_string(response, &["token_type"]).unwrap_or_else(|| "Bearer".into()),
    });
    if let Some(id_token) = first_string(response, &["id_token"]) {
        tokens["id_token"] = json!(id_token);
    }
    if let Some(expires_in) = response.get("expires_in") {
        tokens["expires_in"] = expires_in.clone();
    }
    Ok(json!({
        "tokens": tokens,
        "base_url": std::env::var("HERMES_CODEX_BASE_URL")
            .ok()
            .map(|value| value.trim().trim_end_matches('/').to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| DEFAULT_CODEX_BASE_URL.into()),
        "last_refresh": chrono::Utc::now().to_rfc3339(),
        "auth_mode": "chatgpt",
        "source": "device-code",
    }))
}

fn persist_codex_oauth_state(state: &Value) -> AppResult<HermesRuntimeCredential> {
    let path = primary_hermes_auth_store_path()?;
    let mut store = if let Ok(payload) = std::fs::read_to_string(&path) {
        serde_json::from_str::<Value>(&payload).map_err(|error| {
            AppError::BadRequest(format!(
                "invalid Hermes auth store {}: {error}",
                path.display()
            ))
        })?
    } else {
        json!({})
    };
    upsert_codex_device_code_pool_entry(&mut store, state);
    if let Some(providers) = store.get_mut("providers").and_then(Value::as_object_mut) {
        providers.insert("openai-codex".into(), state.clone());
    } else {
        store["providers"] = json!({"openai-codex": state.clone()});
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, serde_json::to_string_pretty(&store)?)?;
    provider_state_credential(
        "openai-codex",
        &json!({"providers": {"openai-codex": state.clone()}}),
    )
    .ok_or_else(|| {
        AppError::BadRequest("Codex device-code login did not produce usable credentials".into())
    })
}

fn codex_refreshed_state_from_response(existing: &Value, response: &Value) -> AppResult<Value> {
    let access_token = first_string(response, &["access_token"]).ok_or_else(|| {
        AppError::BadRequest("Codex OAuth refresh response missing access_token".into())
    })?;
    let existing_tokens = existing.get("tokens").unwrap_or(existing);
    let refresh_token = first_string(response, &["refresh_token"])
        .or_else(|| first_string(existing_tokens, &["refresh_token"]))
        .ok_or_else(|| {
            AppError::BadRequest("Codex OAuth refresh response missing refresh_token".into())
        })?;
    let mut next = existing.clone();
    let mut tokens = existing_tokens.clone();
    if !tokens.is_object() {
        tokens = json!({});
    }
    tokens["access_token"] = json!(access_token);
    tokens["refresh_token"] = json!(refresh_token);
    if let Some(id_token) = first_string(response, &["id_token"]) {
        tokens["id_token"] = json!(id_token);
    }
    if let Some(expires_in) = response.get("expires_in") {
        tokens["expires_in"] = expires_in.clone();
    }
    tokens["token_type"] = json!(first_string(response, &["token_type"])
        .or_else(|| first_string(existing_tokens, &["token_type"]))
        .unwrap_or_else(|| "Bearer".into()));
    next["tokens"] = tokens;
    next["last_refresh"] = json!(chrono::Utc::now().to_rfc3339());
    next["auth_mode"] = json!("chatgpt");
    Ok(next)
}

fn codex_refresh_error(status: u16, body: &str) -> AppError {
    let parsed = serde_json::from_str::<Value>(body).ok();
    let mut code = String::from("codex_refresh_failed");
    let mut message = format!("Codex token refresh failed with HTTP {status}.");
    if let Some(error) = parsed.as_ref().and_then(|value| value.get("error")) {
        if let Some(error_obj) = error.as_object() {
            if let Some(nested_code) = error_obj
                .get("code")
                .or_else(|| error_obj.get("type"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                code = nested_code.to_string();
            }
            if let Some(nested_message) = error_obj
                .get("message")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                message = format!("Codex token refresh failed: {nested_message}");
            }
        } else if let Some(error_code) = error
            .as_str()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            code = error_code.to_string();
            if let Some(description) = parsed
                .as_ref()
                .and_then(|value| {
                    value
                        .get("error_description")
                        .or_else(|| value.get("message"))
                })
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                message = format!("Codex token refresh failed: {description}");
            }
        }
    }
    let reauth = matches!(status, 400 | 401 | 403)
        || matches!(
            code.as_str(),
            "invalid_grant" | "invalid_token" | "invalid_request" | "refresh_token_reused"
        );
    if code == "refresh_token_reused" {
        message =
            "Codex refresh token was already consumed by another client; re-login required".into();
    }
    AppError::BadRequest(format!(
        "{message} code={code}{}",
        if reauth { " (re-login required)" } else { "" }
    ))
}

fn sync_codex_pool_entries(store: &mut Value, refreshed: &Value) {
    let Some(access_token) = refreshed
        .get("tokens")
        .and_then(|tokens| first_string(tokens, &["access_token"]))
    else {
        return;
    };
    let refresh_token = refreshed
        .get("tokens")
        .and_then(|tokens| first_string(tokens, &["refresh_token"]));
    let last_refresh = first_string(refreshed, &["last_refresh"]);
    let Some(pool) = store
        .get_mut("credential_pool")
        .and_then(|pool| pool.get_mut("openai-codex"))
    else {
        return;
    };
    if let Some(entries) = pool.get_mut("entries").and_then(Value::as_array_mut) {
        sync_codex_pool_entry_list(
            entries,
            &access_token,
            refresh_token.as_deref(),
            last_refresh.as_deref(),
        );
    } else if let Some(entries) = pool.as_array_mut() {
        sync_codex_pool_entry_list(
            entries,
            &access_token,
            refresh_token.as_deref(),
            last_refresh.as_deref(),
        );
    }
}

fn upsert_codex_device_code_pool_entry(store: &mut Value, state: &Value) {
    sync_codex_pool_entries(store, state);
    let Some(access_token) = state
        .get("tokens")
        .and_then(|tokens| first_string(tokens, &["access_token"]))
    else {
        return;
    };
    let refresh_token = state
        .get("tokens")
        .and_then(|tokens| first_string(tokens, &["refresh_token"]));
    let last_refresh = first_string(state, &["last_refresh"]);
    let entry = json!({
        "label": "openai-codex-device-code",
        "source": "device_code",
        "access_token": access_token,
        "refresh_token": refresh_token,
        "last_refresh": last_refresh,
    });
    if store.get("credential_pool").is_none() || !store["credential_pool"].is_object() {
        store["credential_pool"] = json!({});
    }
    if store["credential_pool"].get("openai-codex").is_none() {
        store["credential_pool"]["openai-codex"] = json!([entry]);
        return;
    }
    if let Some(entries) = store["credential_pool"]["openai-codex"]
        .get_mut("entries")
        .and_then(Value::as_array_mut)
    {
        upsert_codex_device_code_entry_list(entries, entry);
    } else if let Some(entries) = store["credential_pool"]["openai-codex"].as_array_mut() {
        upsert_codex_device_code_entry_list(entries, entry);
    }
}

fn upsert_codex_device_code_entry_list(entries: &mut Vec<Value>, entry: Value) {
    if entries.iter().any(|existing| {
        first_string(existing, &["source"]).is_some_and(|source| source == "device_code")
    }) {
        sync_codex_pool_entry_list(
            entries,
            first_string(&entry, &["access_token"])
                .unwrap_or_default()
                .as_str(),
            first_string(&entry, &["refresh_token"]).as_deref(),
            first_string(&entry, &["last_refresh"]).as_deref(),
        );
    } else {
        entries.push(entry);
    }
}

fn sync_codex_pool_entry_list(
    entries: &mut [Value],
    access_token: &str,
    refresh_token: Option<&str>,
    last_refresh: Option<&str>,
) {
    for entry in entries {
        let Some(source) = first_string(entry, &["source"]) else {
            continue;
        };
        if source != "device_code" && source != "manual:device_code" {
            continue;
        }
        entry["access_token"] = json!(access_token);
        if let Some(refresh_token) = refresh_token {
            entry["refresh_token"] = json!(refresh_token);
        }
        if let Some(last_refresh) = last_refresh {
            entry["last_refresh"] = json!(last_refresh);
        }
        for key in [
            "last_status",
            "last_status_at",
            "last_error_code",
            "last_error_reason",
            "last_error_message",
            "last_error_reset_at",
        ] {
            entry[key] = Value::Null;
        }
    }
}

async fn refresh_nous_oauth_state_value(state: &Value) -> AppResult<Value> {
    let refresh_token = first_string(state, &["refresh_token"]).ok_or_else(|| {
        AppError::BadRequest("Nous OAuth refresh_token missing; re-login required".into())
    })?;
    let portal_base_url = first_string(state, &["portal_base_url"])
        .unwrap_or_else(|| DEFAULT_NOUS_PORTAL_URL.into())
        .trim_end_matches('/')
        .to_string();
    let client_id =
        first_string(state, &["client_id"]).unwrap_or_else(|| DEFAULT_NOUS_CLIENT_ID.into());
    let response = reqwest::Client::new()
        .post(format!("{portal_base_url}/api/oauth/token"))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Accept", "application/json")
        .header("x-nous-refresh-token", refresh_token.as_str())
        .form(&[
            ("grant_type", "refresh_token"),
            ("client_id", client_id.as_str()),
        ])
        .send()
        .await
        .map_err(|error| AppError::BadRequest(format!("Nous OAuth refresh failed: {error}")))?;
    let status = response.status();
    let body = response.text().await.map_err(|error| {
        AppError::BadRequest(format!("Nous OAuth refresh read failed: {error}"))
    })?;
    if !status.is_success() {
        return Err(nous_refresh_error(status.as_u16(), &body));
    }
    let response = serde_json::from_str::<Value>(&body).map_err(|error| {
        AppError::BadRequest(format!("Nous OAuth refresh returned invalid JSON: {error}"))
    })?;
    nous_refreshed_state_from_response(state, &response)
}

fn nous_device_code_start_from_response(response: &Value) -> AppResult<NousDeviceCodeStart> {
    let user_code = first_string(response, &["user_code"]).ok_or_else(|| {
        AppError::BadRequest("Nous device-code response missing user_code".into())
    })?;
    let device_code = first_string(response, &["device_code"]).ok_or_else(|| {
        AppError::BadRequest("Nous device-code response missing device_code".into())
    })?;
    let verification_uri = first_string(response, &["verification_uri"]).ok_or_else(|| {
        AppError::BadRequest("Nous device-code response missing verification_uri".into())
    })?;
    let verification_uri_complete = first_string(response, &["verification_uri_complete"])
        .ok_or_else(|| {
            AppError::BadRequest(
                "Nous device-code response missing verification_uri_complete".into(),
            )
        })?;
    let expires_in = response
        .get("expires_in")
        .and_then(value_as_u64)
        .ok_or_else(|| {
            AppError::BadRequest("Nous device-code response missing expires_in".into())
        })?;
    let interval_seconds = response
        .get("interval")
        .and_then(value_as_u64)
        .unwrap_or(5)
        .max(1);
    Ok(NousDeviceCodeStart {
        user_code,
        device_code,
        verification_uri,
        verification_uri_complete,
        expires_in,
        interval_seconds,
    })
}

fn nous_device_code_poll_error(status: u16, body: &str) -> AppError {
    let parsed = serde_json::from_str::<Value>(body).ok();
    let code = parsed
        .as_ref()
        .and_then(|value| value.get("error"))
        .and_then(Value::as_str)
        .unwrap_or("device_code_poll_failed");
    let description = parsed
        .as_ref()
        .and_then(|value| value.get("error_description"))
        .and_then(Value::as_str)
        .unwrap_or(body);
    if code == "authorization_pending" {
        return AppError::BadRequest(
            "Nous device-code authorization is still pending; finish browser login and retry `/auth poll nous <device_code>`.".into(),
        );
    }
    if code == "slow_down" {
        return AppError::BadRequest(
            "Nous device-code polling should slow down; wait a little longer and retry `/auth poll nous <device_code>`.".into(),
        );
    }
    AppError::BadRequest(format!(
        "Nous device-code poll failed with HTTP {status}: {description} code={code}"
    ))
}

fn nous_device_code_state_from_token_response(response: &Value) -> AppResult<Value> {
    let access_token = first_string(response, &["access_token"]).ok_or_else(|| {
        AppError::BadRequest("Nous device-code token response missing access_token".into())
    })?;
    let refresh_token = first_string(response, &["refresh_token"]).ok_or_else(|| {
        AppError::BadRequest("Nous device-code token response missing refresh_token".into())
    })?;
    validate_nous_invoke_jwt(
        &access_token,
        response.get("scope"),
        response.get("expires_at"),
    )?;
    let now = chrono::Utc::now();
    let expires_in = response
        .get("expires_in")
        .and_then(value_as_i64)
        .unwrap_or(3600)
        .max(1);
    let inference_base_url = first_string(response, &["inference_base_url"])
        .and_then(|url| validate_nous_inference_url(&url))
        .unwrap_or_else(|| DEFAULT_NOUS_INFERENCE_URL.into());
    let mut state = json!({
        "portal_base_url": nous_portal_base_url(),
        "inference_base_url": inference_base_url,
        "client_id": DEFAULT_NOUS_CLIENT_ID,
        "scope": first_string(response, &["scope"]).unwrap_or_else(|| NOUS_INFERENCE_INVOKE_SCOPE.into()),
        "token_type": first_string(response, &["token_type"]).unwrap_or_else(|| "Bearer".into()),
        "access_token": access_token,
        "refresh_token": refresh_token,
        "obtained_at": now.to_rfc3339(),
        "expires_in": expires_in,
        "expires_at": (now + chrono::Duration::seconds(expires_in)).to_rfc3339(),
    });
    set_nous_agent_key_from_invoke_jwt(&mut state);
    Ok(state)
}

fn persist_nous_oauth_state(state: &Value) -> AppResult<HermesRuntimeCredential> {
    let path = primary_hermes_auth_store_path()?;
    let mut store = if let Ok(payload) = std::fs::read_to_string(&path) {
        serde_json::from_str::<Value>(&payload).map_err(|error| {
            AppError::BadRequest(format!(
                "invalid Hermes auth store {}: {error}",
                path.display()
            ))
        })?
    } else {
        json!({})
    };
    upsert_nous_device_code_pool_entry(&mut store, state);
    if let Some(providers) = store.get_mut("providers").and_then(Value::as_object_mut) {
        providers.insert("nous".into(), state.clone());
    } else {
        store["providers"] = json!({"nous": state.clone()});
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, serde_json::to_string_pretty(&store)?)?;
    write_shared_nous_state(state);
    provider_state_credential("nous", &json!({"providers": {"nous": state.clone()}})).ok_or_else(
        || AppError::BadRequest("Nous device-code login did not produce usable credentials".into()),
    )
}

fn nous_refreshed_state_from_response(existing: &Value, response: &Value) -> AppResult<Value> {
    let access_token = first_string(response, &["access_token"]).ok_or_else(|| {
        AppError::BadRequest("Nous OAuth refresh response missing access_token".into())
    })?;
    let refresh_token = first_string(response, &["refresh_token"])
        .or_else(|| first_string(existing, &["refresh_token"]))
        .ok_or_else(|| {
            AppError::BadRequest("Nous OAuth refresh response missing refresh_token".into())
        })?;
    validate_nous_invoke_jwt(
        &access_token,
        response.get("scope").or_else(|| existing.get("scope")),
        response
            .get("expires_at")
            .or_else(|| existing.get("expires_at")),
    )?;
    let now = chrono::Utc::now();
    let expires_in = response
        .get("expires_in")
        .and_then(|value| {
            value
                .as_i64()
                .or_else(|| value.as_str()?.trim().parse::<i64>().ok())
        })
        .unwrap_or(3600)
        .max(1) as u64;
    let mut next = existing.clone();
    next["access_token"] = json!(access_token);
    next["refresh_token"] = json!(refresh_token);
    next["token_type"] = json!(first_string(response, &["token_type"])
        .or_else(|| first_string(existing, &["token_type"]))
        .unwrap_or_else(|| "Bearer".into()));
    next["scope"] = json!(first_string(response, &["scope"])
        .or_else(|| first_string(existing, &["scope"]))
        .unwrap_or_else(|| NOUS_INFERENCE_INVOKE_SCOPE.into()));
    if let Some(url) = first_string(response, &["inference_base_url"])
        .and_then(|url| validate_nous_inference_url(&url))
        .or_else(|| first_string(existing, &["inference_base_url"]))
    {
        next["inference_base_url"] = json!(url.trim_end_matches('/').to_string());
    } else {
        next["inference_base_url"] = json!(DEFAULT_NOUS_INFERENCE_URL);
    }
    if next.get("portal_base_url").is_none() {
        next["portal_base_url"] = json!(DEFAULT_NOUS_PORTAL_URL);
    }
    if next.get("client_id").is_none() {
        next["client_id"] = json!(DEFAULT_NOUS_CLIENT_ID);
    }
    next["obtained_at"] = json!(now.to_rfc3339());
    next["expires_in"] = json!(expires_in);
    next["expires_at"] = json!((now + chrono::Duration::seconds(expires_in as i64)).to_rfc3339());
    set_nous_agent_key_from_invoke_jwt(&mut next);
    Ok(next)
}

fn validate_nous_invoke_jwt(
    token: &str,
    scope_value: Option<&Value>,
    expires_at_value: Option<&Value>,
) -> AppResult<()> {
    let Some(claims) = jwt_claims(token) else {
        return Err(AppError::BadRequest(
            "Nous Portal access token is not a usable inference JWT (access_token_not_jwt); re-login required".into(),
        ));
    };
    let scopes = scope_values(scope_value)
        .into_iter()
        .chain(scope_values(claims.get("scope")))
        .chain(scope_values(claims.get("scp")))
        .collect::<Vec<_>>();
    if !scopes
        .iter()
        .any(|scope| scope == NOUS_INFERENCE_INVOKE_SCOPE)
    {
        return Err(AppError::BadRequest(
            "Nous Portal access token is not a usable inference JWT (missing_inference_invoke_scope); re-login required".into(),
        ));
    }
    let now = unix_now_seconds().saturating_add(NOUS_INVOKE_JWT_MIN_TTL_SECONDS);
    if let Some(exp) = claims.get("exp").and_then(Value::as_u64) {
        if exp <= now {
            return Err(AppError::BadRequest(
                "Nous Portal access token is not a usable inference JWT (invoke_jwt_expiring); re-login required".into(),
            ));
        }
        return Ok(());
    }
    if let Some(expires_at) = expires_at_value
        .and_then(Value::as_str)
        .and_then(|text| chrono::DateTime::parse_from_rfc3339(text).ok())
    {
        if expires_at.timestamp() > now as i64 {
            return Ok(());
        }
    }
    Err(AppError::BadRequest(
        "Nous Portal access token is not a usable inference JWT (invoke_jwt_expiry_unknown_or_expiring); re-login required".into(),
    ))
}

fn set_nous_agent_key_from_invoke_jwt(state: &mut Value) {
    let Some(access_token) = first_string(state, &["access_token"]) else {
        return;
    };
    let expires_at = jwt_exp_seconds(&access_token)
        .map(iso_from_unix_seconds)
        .or_else(|| first_string(state, &["expires_at"]));
    let expires_in = expires_at
        .as_deref()
        .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
        .map(|dt| {
            dt.timestamp()
                .saturating_sub(unix_now_seconds() as i64)
                .max(0)
        })
        .unwrap_or_else(|| {
            state
                .get("expires_in")
                .and_then(Value::as_i64)
                .unwrap_or_default()
                .max(0)
        });
    state["agent_key"] = json!(access_token);
    state["agent_key_id"] = Value::Null;
    state["agent_key_expires_at"] = expires_at.map(Value::String).unwrap_or(Value::Null);
    state["agent_key_expires_in"] = json!(expires_in);
    state["agent_key_reused"] = json!(false);
    if state.get("agent_key_obtained_at").is_none() {
        state["agent_key_obtained_at"] = json!(chrono::Utc::now().to_rfc3339());
    }
}

fn nous_refresh_error(status: u16, body: &str) -> AppError {
    let parsed = serde_json::from_str::<Value>(body).ok();
    let code = parsed
        .as_ref()
        .and_then(|value| value.get("error"))
        .and_then(Value::as_str)
        .unwrap_or("invalid_grant");
    let description = parsed
        .as_ref()
        .and_then(|value| value.get("error_description"))
        .and_then(Value::as_str)
        .unwrap_or(body);
    let lower = description.to_ascii_lowercase();
    let relogin = matches!(
        code,
        "invalid_grant" | "invalid_token" | "refresh_token_reused"
    ) || lower.contains("reuse");
    AppError::BadRequest(format!(
        "Nous OAuth refresh failed with HTTP {status}: {description} code={code}{}",
        if relogin { " (re-login required)" } else { "" }
    ))
}

fn terminal_nous_refresh_error_reason(error: &AppError) -> Option<&'static str> {
    let message = error.to_string().to_ascii_lowercase();
    if !message.contains("re-login required") {
        return None;
    }
    [
        "code=invalid_grant",
        "code=invalid_token",
        "code=refresh_token_reused",
    ]
    .iter()
    .any(|needle| message.contains(needle))
    .then_some("oauth_refresh_terminal_failure")
}

fn quarantine_nous_oauth_state(state: &mut Value, error: &AppError, reason: &str) {
    if let Some(map) = state.as_object_mut() {
        for key in [
            "access_token",
            "refresh_token",
            "expires_at",
            "expires_in",
            "obtained_at",
            "agent_key",
            "agent_key_id",
            "agent_key_expires_at",
            "agent_key_expires_in",
            "agent_key_reused",
            "agent_key_obtained_at",
        ] {
            map.remove(key);
        }
    }
    state["last_auth_error"] = json!({
        "provider": "nous",
        "code": nous_error_code_from_message(error).unwrap_or("oauth_refresh_failed"),
        "message": error.to_string(),
        "reason": reason,
        "relogin_required": true,
        "at": chrono::Utc::now().to_rfc3339(),
    });
}

fn nous_error_code_from_message(error: &AppError) -> Option<&'static str> {
    let message = error.to_string().to_ascii_lowercase();
    if message.contains("code=refresh_token_reused") {
        Some("refresh_token_reused")
    } else if message.contains("code=invalid_token") {
        Some("invalid_token")
    } else if message.contains("code=invalid_grant") {
        Some("invalid_grant")
    } else {
        None
    }
}

fn validate_nous_inference_url(url: &str) -> Option<String> {
    let parsed = reqwest::Url::parse(url).ok()?;
    if parsed.scheme() != "https" {
        return None;
    }
    let host = parsed.host_str()?.to_ascii_lowercase();
    let allowed = host == "inference-api.nousresearch.com"
        || host.ends_with(".inference-api.nousresearch.com")
        || host == "api.nousresearch.com"
        || host.ends_with(".nousresearch.com");
    allowed.then(|| url.trim_end_matches('/').to_string())
}

fn nous_portal_base_url() -> String {
    std::env::var("HERMES_PORTAL_BASE_URL")
        .ok()
        .or_else(|| std::env::var("NOUS_PORTAL_BASE_URL").ok())
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_NOUS_PORTAL_URL.into())
}

fn sync_nous_pool_entries(store: &mut Value, refreshed: &Value) {
    let Some(pool) = store
        .get_mut("credential_pool")
        .and_then(|pool| pool.get_mut("nous"))
    else {
        return;
    };
    if let Some(entries) = pool.get_mut("entries").and_then(Value::as_array_mut) {
        sync_nous_pool_entry_list(entries, refreshed);
    } else if let Some(entries) = pool.as_array_mut() {
        sync_nous_pool_entry_list(entries, refreshed);
    }
}

fn upsert_nous_device_code_pool_entry(store: &mut Value, state: &Value) {
    sync_nous_pool_entries(store, state);
    let Some(agent_key) = first_string(state, &["agent_key"]) else {
        return;
    };
    let entry = json!({
        "label": "nous-device-code",
        "source": "device_code",
        "agent_key": agent_key,
        "access_token": first_string(state, &["access_token"]),
        "refresh_token": first_string(state, &["refresh_token"]),
        "token_type": first_string(state, &["token_type"]).unwrap_or_else(|| "Bearer".into()),
        "scope": first_string(state, &["scope"]).unwrap_or_else(|| NOUS_INFERENCE_INVOKE_SCOPE.into()),
        "expires_at": first_string(state, &["expires_at"]),
        "expires_in": state.get("expires_in").cloned().unwrap_or(Value::Null),
        "agent_key_expires_at": first_string(state, &["agent_key_expires_at"]),
        "agent_key_expires_in": state.get("agent_key_expires_in").cloned().unwrap_or(Value::Null),
        "portal_base_url": first_string(state, &["portal_base_url"]).unwrap_or_else(|| DEFAULT_NOUS_PORTAL_URL.into()),
        "inference_base_url": first_string(state, &["inference_base_url"]).unwrap_or_else(|| DEFAULT_NOUS_INFERENCE_URL.into()),
        "client_id": first_string(state, &["client_id"]).unwrap_or_else(|| DEFAULT_NOUS_CLIENT_ID.into()),
    });
    if store.get("credential_pool").is_none() || !store["credential_pool"].is_object() {
        store["credential_pool"] = json!({});
    }
    if store["credential_pool"].get("nous").is_none() {
        store["credential_pool"]["nous"] = json!([entry]);
        return;
    }
    if let Some(entries) = store["credential_pool"]["nous"]
        .get_mut("entries")
        .and_then(Value::as_array_mut)
    {
        upsert_nous_device_code_entry_list(entries, entry);
    } else if let Some(entries) = store["credential_pool"]["nous"].as_array_mut() {
        upsert_nous_device_code_entry_list(entries, entry);
    }
}

fn upsert_nous_device_code_entry_list(entries: &mut Vec<Value>, entry: Value) {
    if entries.iter().any(|existing| {
        first_string(existing, &["source"]).is_some_and(|source| source == "device_code")
    }) {
        sync_nous_pool_entry_list(entries, &entry);
    } else {
        entries.push(entry);
    }
}

fn quarantine_nous_pool_entries(store: &mut Value, error: &AppError, reason: &str) -> bool {
    let Some(pool) = store
        .get_mut("credential_pool")
        .and_then(|pool| pool.get_mut("nous"))
    else {
        return false;
    };
    if let Some(entries) = pool.get_mut("entries").and_then(Value::as_array_mut) {
        return quarantine_nous_pool_entry_list(entries, error, reason);
    }
    if let Some(entries) = pool.as_array_mut() {
        return quarantine_nous_pool_entry_list(entries, error, reason);
    }
    false
}

fn quarantine_nous_pool_entry_list(
    entries: &mut Vec<Value>,
    _error: &AppError,
    _reason: &str,
) -> bool {
    let before = entries.len();
    entries.retain(|entry| {
        let Some(source) = first_string(entry, &["source"]) else {
            return true;
        };
        source != "device_code" && source != "manual:device_code"
    });
    before != entries.len()
}

fn sync_nous_pool_entry_list(entries: &mut [Value], refreshed: &Value) {
    for entry in entries {
        let Some(source) = first_string(entry, &["source"]) else {
            continue;
        };
        if source != "device_code" && source != "manual:device_code" {
            continue;
        }
        for key in [
            "access_token",
            "refresh_token",
            "token_type",
            "scope",
            "obtained_at",
            "expires_in",
            "expires_at",
            "agent_key",
            "agent_key_id",
            "agent_key_expires_at",
            "agent_key_expires_in",
            "agent_key_reused",
            "agent_key_obtained_at",
            "portal_base_url",
            "inference_base_url",
            "client_id",
        ] {
            if let Some(value) = refreshed.get(key) {
                entry[key] = value.clone();
            }
        }
        for key in [
            "last_status",
            "last_status_at",
            "last_error_code",
            "last_error_reason",
            "last_error_message",
            "last_error_reset_at",
        ] {
            entry[key] = Value::Null;
        }
    }
}

fn write_shared_nous_state(state: &Value) {
    let Some(shared) = nous_shared_state_from_provider_state(state) else {
        return;
    };
    let Some(path) = nous_shared_store_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
    }
    let _ = std::fs::write(
        path,
        serde_json::to_string_pretty(&shared).unwrap_or_default(),
    );
}

fn read_shared_nous_state() -> Option<Value> {
    let path = nous_shared_store_path()?;
    let payload = std::fs::read_to_string(path).ok()?;
    serde_json::from_str::<Value>(&payload).ok()
}

fn clear_shared_nous_state() {
    let Some(path) = nous_shared_store_path() else {
        return;
    };
    let _ = std::fs::remove_file(path);
}

fn merge_shared_nous_oauth_state(state: &mut Value) -> bool {
    let Some(shared) = read_shared_nous_state() else {
        return false;
    };
    merge_shared_nous_oauth_state_value(state, &shared)
}

fn merge_shared_nous_oauth_state_value(state: &mut Value, shared: &Value) -> bool {
    let Some(shared_refresh) = first_string(shared, &["refresh_token"]) else {
        return false;
    };
    let local_refresh = first_string(state, &["refresh_token"]).unwrap_or_default();
    let refresh_changed = shared_refresh.trim() != local_refresh.trim();
    let shared_access_exp = nous_access_expiry_seconds(shared).unwrap_or(0);
    let local_access_exp = nous_access_expiry_seconds(state).unwrap_or(0);
    let fresher_access = shared_access_exp > local_access_exp;
    if !refresh_changed && !fresher_access {
        return false;
    }

    for key in [
        "access_token",
        "refresh_token",
        "token_type",
        "scope",
        "client_id",
        "portal_base_url",
        "inference_base_url",
        "obtained_at",
        "expires_at",
    ] {
        if let Some(value) = shared.get(key).filter(|value| !value_is_blank(value)) {
            state[key] = value.clone();
        }
    }
    true
}

fn nous_shared_state_from_provider_state(state: &Value) -> Option<Value> {
    let access_token = first_string(state, &["access_token"])?;
    let refresh_token = first_string(state, &["refresh_token"])?;
    Some(json!({
        "_schema": 1,
        "access_token": access_token,
        "refresh_token": refresh_token,
        "token_type": first_string(state, &["token_type"]).unwrap_or_else(|| "Bearer".into()),
        "scope": first_string(state, &["scope"]).unwrap_or_else(|| NOUS_INFERENCE_INVOKE_SCOPE.into()),
        "client_id": first_string(state, &["client_id"]).unwrap_or_else(|| DEFAULT_NOUS_CLIENT_ID.into()),
        "portal_base_url": first_string(state, &["portal_base_url"]).unwrap_or_else(|| DEFAULT_NOUS_PORTAL_URL.into()),
        "inference_base_url": first_string(state, &["inference_base_url"]).unwrap_or_else(|| DEFAULT_NOUS_INFERENCE_URL.into()),
        "obtained_at": first_string(state, &["obtained_at"]),
        "expires_at": first_string(state, &["expires_at"]),
        "updated_at": chrono::Utc::now().to_rfc3339(),
    }))
}

fn nous_access_expiry_seconds(state: &Value) -> Option<u64> {
    let raw = state.get("expires_at")?;
    if let Some(number) = raw.as_u64() {
        return Some(normalize_epoch_seconds(number));
    }
    let text = raw.as_str()?.trim();
    if text.is_empty() {
        return None;
    }
    if let Ok(number) = text.parse::<u64>() {
        return Some(normalize_epoch_seconds(number));
    }
    chrono::DateTime::parse_from_rfc3339(text)
        .ok()
        .and_then(|dt| (dt.timestamp() > 0).then_some(dt.timestamp() as u64))
}

fn value_is_blank(value: &Value) -> bool {
    matches!(value, Value::Null) || value.as_str().is_some_and(|text| text.trim().is_empty())
}

fn nous_shared_store_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("HERMES_SHARED_AUTH_DIR").filter(|value| !value.is_empty())
    {
        return Some(PathBuf::from(path).join(NOUS_SHARED_STORE_FILENAME));
    }
    if let Some(home) = std::env::var_os("HERMES_HOME").filter(|value| !value.is_empty()) {
        return Some(
            PathBuf::from(home)
                .join("shared")
                .join(NOUS_SHARED_STORE_FILENAME),
        );
    }
    home_dir().map(|home| {
        home.join(".hermes")
            .join("shared")
            .join(NOUS_SHARED_STORE_FILENAME)
    })
}

fn google_gemini_oauth_start_from_parts(
    state: &str,
    code_verifier: &str,
    code_challenge: &str,
) -> AppResult<GoogleGeminiOauthStart> {
    let client_id = google_gemini_oauth_client_id()?;
    let mut url = reqwest::Url::parse(GOOGLE_GEMINI_OAUTH_AUTH_URL).map_err(|error| {
        AppError::BadRequest(format!("invalid Google OAuth authorization URL: {error}"))
    })?;
    url.query_pairs_mut()
        .append_pair("client_id", client_id.as_str())
        .append_pair("redirect_uri", GOOGLE_GEMINI_OAUTH_REDIRECT_URI)
        .append_pair("response_type", "code")
        .append_pair("scope", GOOGLE_GEMINI_OAUTH_SCOPES)
        .append_pair("code_challenge", code_challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", state)
        .append_pair("access_type", "offline")
        .append_pair("prompt", "consent");
    Ok(GoogleGeminiOauthStart {
        authorize_url: url.to_string(),
        redirect_uri: GOOGLE_GEMINI_OAUTH_REDIRECT_URI.into(),
        state: state.to_string(),
        code_verifier: code_verifier.to_string(),
    })
}

async fn exchange_google_gemini_authorization_code(
    code: &str,
    code_verifier: &str,
) -> AppResult<Value> {
    let mut form = vec![
        ("grant_type", "authorization_code".to_string()),
        ("code", code.to_string()),
        ("redirect_uri", GOOGLE_GEMINI_OAUTH_REDIRECT_URI.to_string()),
        ("client_id", google_gemini_oauth_client_id()?),
        ("code_verifier", code_verifier.to_string()),
    ];
    if let Some(client_secret) = google_gemini_oauth_client_secret() {
        form.push(("client_secret", client_secret));
    }
    let response = reqwest::Client::new()
        .post(GOOGLE_GEMINI_OAUTH_TOKEN_URL)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Accept", "application/json")
        .form(&form)
        .send()
        .await
        .map_err(|error| {
            AppError::BadRequest(format!(
                "Google Gemini OAuth token exchange failed: {error}"
            ))
        })?;
    let status = response.status();
    let body = response.text().await.map_err(|error| {
        AppError::BadRequest(format!(
            "Google Gemini OAuth token exchange response read failed: {error}"
        ))
    })?;
    if !status.is_success() {
        return Err(AppError::BadRequest(format!(
            "Google Gemini OAuth token exchange failed with HTTP {status}: {body}"
        )));
    }
    serde_json::from_str::<Value>(&body).map_err(|error| {
        AppError::BadRequest(format!(
            "Google Gemini OAuth token exchange returned invalid JSON: {error}"
        ))
    })
}

fn google_gemini_credentials_from_token_response(response: &Value) -> AppResult<Value> {
    let access_token = first_string(response, &["access_token"]).ok_or_else(|| {
        AppError::BadRequest("Google Gemini OAuth token response missing access_token".into())
    })?;
    let refresh_token = first_string(response, &["refresh_token"]).ok_or_else(|| {
        AppError::BadRequest("Google Gemini OAuth token response missing refresh_token".into())
    })?;
    let expires_in = response
        .get("expires_in")
        .and_then(value_as_u64)
        .unwrap_or(3600);
    Ok(json!({
        "access": access_token,
        "refresh": pack_google_gemini_refresh(&refresh_token, "", ""),
        "expires": unix_now_seconds().saturating_add(expires_in).saturating_mul(1000),
        "email": first_string(response, &["email"]).unwrap_or_default(),
    }))
}

fn persist_google_gemini_oauth_credentials(creds: &Value) -> AppResult<HermesRuntimeCredential> {
    let path = google_gemini_oauth_credentials_path()
        .ok_or_else(|| AppError::BadRequest("HOME/USERPROFILE is not available".into()))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, serde_json::to_string_pretty(creds)?)?;
    google_gemini_runtime_credential_from_value(creds).ok_or_else(|| {
        AppError::BadRequest("Google Gemini OAuth login did not produce usable credentials".into())
    })
}

fn google_gemini_oauth_status() -> Option<HermesExternalCredentialStatus> {
    if let Some(credential) = google_gemini_oauth_credential() {
        return Some(HermesExternalCredentialStatus {
            provider_id: credential.provider_id,
            source: credential.source,
            state: "present",
            expires_at: credential.expires_at,
            note: Some("Cloud Code Assist transport enabled".into()),
        });
    }
    let path = google_gemini_oauth_credentials_path()?;
    let payload = std::fs::read_to_string(path).ok()?;
    let creds = serde_json::from_str::<Value>(&payload).ok()?;
    let access = first_string(&creds, &["access"])?;
    let expired = credential_expired(&creds, &access);
    Some(HermesExternalCredentialStatus {
        provider_id: "google-gemini-cli".into(),
        source: "google-oauth".into(),
        state: if expired { "expired" } else { "present" },
        expires_at: first_string(&creds, &["expires"]),
        note: Some("Cloud Code Assist transport enabled".into()),
    })
}

fn google_gemini_oauth_credential() -> Option<HermesRuntimeCredential> {
    let path = google_gemini_oauth_credentials_path()?;
    let payload = std::fs::read_to_string(path).ok()?;
    let creds = serde_json::from_str::<Value>(&payload).ok()?;
    google_gemini_runtime_credential_from_value(&creds)
}

fn google_gemini_runtime_credential_from_value(creds: &Value) -> Option<HermesRuntimeCredential> {
    let token = first_string(&creds, &["access"])?;
    if credential_expired(&creds, &token) {
        return None;
    }
    Some(HermesRuntimeCredential {
        provider_id: "google-gemini-cli".into(),
        api_key: token,
        base_url: Some("cloudcode-pa://google".into()),
        source: "google-oauth".into(),
        expires_at: first_string(&creds, &["expires"]),
    })
}

async fn refresh_google_gemini_oauth_credentials_value(existing: &Value) -> AppResult<Value> {
    let refresh_packed = first_string(existing, &["refresh"]).ok_or_else(|| {
        AppError::BadRequest("Google Gemini OAuth refresh token missing; re-login required".into())
    })?;
    let (refresh_token, _, _) = split_google_gemini_refresh(&refresh_packed);
    if refresh_token.is_empty() {
        return Err(AppError::BadRequest(
            "Google Gemini OAuth refresh token missing; re-login required".into(),
        ));
    }
    let mut form = vec![
        ("grant_type", "refresh_token".to_string()),
        ("refresh_token", refresh_token),
        ("client_id", google_gemini_oauth_client_id()?),
    ];
    if let Some(client_secret) = google_gemini_oauth_client_secret() {
        form.push(("client_secret", client_secret));
    }
    let response = reqwest::Client::new()
        .post(GOOGLE_GEMINI_OAUTH_TOKEN_URL)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Accept", "application/json")
        .form(&form)
        .send()
        .await
        .map_err(|error| {
            AppError::BadRequest(format!("Google Gemini OAuth refresh failed: {error}"))
        })?;
    let status = response.status();
    let body = response.text().await.map_err(|error| {
        AppError::BadRequest(format!("Google Gemini OAuth refresh read failed: {error}"))
    })?;
    if !status.is_success() {
        return Err(google_gemini_refresh_error(status.as_u16(), &body));
    }
    let response = serde_json::from_str::<Value>(&body).map_err(|error| {
        AppError::BadRequest(format!(
            "Google Gemini OAuth refresh returned invalid JSON: {error}"
        ))
    })?;
    google_gemini_refreshed_credentials_from_response(existing, &response)
}

fn google_gemini_refreshed_credentials_from_response(
    existing: &Value,
    response: &Value,
) -> AppResult<Value> {
    let access_token = first_string(response, &["access_token"]).ok_or_else(|| {
        AppError::BadRequest("Google Gemini OAuth refresh response missing access_token".into())
    })?;
    let existing_refresh = first_string(existing, &["refresh"]).unwrap_or_default();
    let (existing_refresh_token, project_id, managed_project_id) =
        split_google_gemini_refresh(&existing_refresh);
    let refresh_token = first_string(response, &["refresh_token"])
        .or_else(|| (!existing_refresh_token.is_empty()).then_some(existing_refresh_token))
        .ok_or_else(|| {
            AppError::BadRequest(
                "Google Gemini OAuth refresh response missing refresh_token".into(),
            )
        })?;
    let expires_in = response
        .get("expires_in")
        .and_then(|value| {
            value
                .as_i64()
                .or_else(|| value.as_str()?.trim().parse::<i64>().ok())
        })
        .unwrap_or(60)
        .max(60) as u64;
    let mut next = existing.clone();
    next["access"] = json!(access_token);
    next["refresh"] = json!(pack_google_gemini_refresh(
        &refresh_token,
        &project_id,
        &managed_project_id
    ));
    next["expires"] = json!(unix_now_seconds()
        .saturating_add(expires_in)
        .saturating_mul(1000));
    Ok(next)
}

fn google_gemini_refresh_error(status: u16, body: &str) -> AppError {
    let relogin = body.to_ascii_lowercase().contains("invalid_grant");
    AppError::BadRequest(format!(
        "Google Gemini OAuth refresh failed with HTTP {status}: {body}{}",
        if relogin { " (re-login required)" } else { "" }
    ))
}

fn split_google_gemini_refresh(packed: &str) -> (String, String, String) {
    let mut parts = packed.splitn(3, '|');
    (
        parts.next().unwrap_or_default().trim().to_string(),
        parts.next().unwrap_or_default().trim().to_string(),
        parts.next().unwrap_or_default().trim().to_string(),
    )
}

fn pack_google_gemini_refresh(
    refresh_token: &str,
    project_id: &str,
    managed_project_id: &str,
) -> String {
    if project_id.is_empty() && managed_project_id.is_empty() {
        refresh_token.to_string()
    } else {
        format!("{refresh_token}|{project_id}|{managed_project_id}")
    }
}

fn google_gemini_oauth_client_id() -> AppResult<String> {
    std::env::var("HERMES_GEMINI_CLIENT_ID")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            AppError::BadRequest(
                "Google Gemini OAuth client_id is required. Set HERMES_GEMINI_CLIENT_ID.".into(),
            )
        })
}

fn google_gemini_oauth_client_secret() -> Option<String> {
    std::env::var("HERMES_GEMINI_CLIENT_SECRET")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn provider_auth_state_or_empty(provider_id: &str) -> Value {
    read_primary_hermes_auth_store()
        .ok()
        .and_then(|store| {
            store
                .get("providers")
                .and_then(|providers| providers.get(provider_id))
                .cloned()
        })
        .unwrap_or_else(|| json!({}))
}

fn spotify_oauth_client_id(state: &Value) -> AppResult<String> {
    std::env::var("HERMES_SPOTIFY_CLIENT_ID")
        .ok()
        .or_else(|| std::env::var("SPOTIFY_CLIENT_ID").ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .or_else(|| first_string(state, &["client_id", "clientId"]))
        .ok_or_else(|| {
            AppError::BadRequest(
                "Spotify client_id is required. Set HERMES_SPOTIFY_CLIENT_ID or use an existing providers.spotify auth state.".into(),
            )
        })
}

fn spotify_oauth_redirect_uri(state: &Value) -> String {
    std::env::var("HERMES_SPOTIFY_REDIRECT_URI")
        .ok()
        .or_else(|| std::env::var("SPOTIFY_REDIRECT_URI").ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .or_else(|| first_string(state, &["redirect_uri", "redirectUri"]))
        .unwrap_or_else(|| DEFAULT_SPOTIFY_REDIRECT_URI.into())
}

fn spotify_oauth_scope(state: &Value) -> String {
    std::env::var("HERMES_SPOTIFY_SCOPE")
        .ok()
        .or_else(|| std::env::var("SPOTIFY_SCOPE").ok())
        .map(|value| value.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|value| !value.is_empty())
        .or_else(|| first_string(state, &["scope"]))
        .unwrap_or_else(|| DEFAULT_SPOTIFY_SCOPE.into())
}

fn spotify_oauth_accounts_base_url(state: &Value) -> String {
    std::env::var("HERMES_SPOTIFY_ACCOUNTS_BASE_URL")
        .ok()
        .or_else(|| std::env::var("SPOTIFY_ACCOUNTS_BASE_URL").ok())
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
        .or_else(|| first_string(state, &["accounts_base_url", "accountsBaseUrl"]))
        .unwrap_or_else(|| DEFAULT_SPOTIFY_ACCOUNTS_BASE_URL.into())
}

fn spotify_oauth_api_base_url(state: &Value) -> String {
    std::env::var("HERMES_SPOTIFY_API_BASE_URL")
        .ok()
        .or_else(|| std::env::var("SPOTIFY_API_BASE_URL").ok())
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
        .or_else(|| {
            first_string(
                state,
                &["api_base_url", "apiBaseUrl", "base_url", "baseUrl"],
            )
        })
        .unwrap_or_else(|| DEFAULT_SPOTIFY_API_BASE_URL.into())
}

fn provider_state_credential(provider_id: &str, store: &Value) -> Option<HermesRuntimeCredential> {
    let state = store.get("providers")?.get(provider_id)?;
    let token_source = provider_runtime_token_source(provider_id, state)?;
    if credential_expired(token_source.expiry_value, &token_source.token) {
        return None;
    }
    Some(HermesRuntimeCredential {
        provider_id: provider_id.to_string(),
        api_key: token_source.token,
        base_url: runtime_base_url(provider_id, state),
        source: format!("hermes-auth:{provider_id}"),
        expires_at: expiry_label(token_source.expiry_value),
    })
}

fn provider_state_credential_status(
    provider_id: &str,
    store: &Value,
) -> Option<HermesExternalCredentialStatus> {
    let state = store.get("providers")?.get(provider_id)?;
    credential_status_from_value(provider_id, state, format!("hermes-auth:{provider_id}"))
}

fn credential_pool_credential_select(
    provider_id: &str,
    store: &mut Value,
) -> (Option<HermesRuntimeCredential>, bool) {
    let strategy = credential_pool_strategy(provider_id, store);
    let Some(pool) = store
        .get_mut("credential_pool")
        .and_then(|pool| pool.get_mut(provider_id))
    else {
        return (None, false);
    };

    if let Some(entries) = pool.as_array_mut() {
        return select_credential_pool_array_entry(provider_id, entries, &strategy);
    }
    if let Some(entries) = pool.get_mut("entries").and_then(Value::as_array_mut) {
        return select_credential_pool_array_entry(provider_id, entries, &strategy);
    }
    if pool.is_object() {
        return (
            credential_pool_entry_runtime_credential(provider_id, pool),
            false,
        );
    }
    (None, false)
}

fn select_credential_pool_array_entry(
    provider_id: &str,
    entries: &mut Vec<Value>,
    strategy: &str,
) -> (Option<HermesRuntimeCredential>, bool) {
    let available = sorted_available_pool_entry_indices(provider_id, entries);
    let Some(selected_index) = selected_pool_entry_index(entries, &available, strategy) else {
        return (None, false);
    };
    let mut changed = false;
    match strategy {
        "least_used" if available.len() > 1 => {
            if let Some(map) = entries[selected_index].as_object_mut() {
                let next = map
                    .get("request_count")
                    .and_then(value_as_u64)
                    .unwrap_or(0)
                    .saturating_add(1);
                map.insert("request_count".into(), json!(next));
                changed = true;
            }
        }
        "round_robin" if available.len() > 1 => {
            let selected = entries.remove(selected_index);
            entries.push(selected);
            normalize_pool_priorities(entries);
            changed = true;
        }
        _ => {}
    }
    let selected_index = if strategy == "round_robin" && changed {
        entries.len().saturating_sub(1)
    } else {
        selected_index
    };
    (
        entries
            .get(selected_index)
            .and_then(|entry| credential_pool_entry_runtime_credential(provider_id, entry)),
        changed,
    )
}

fn sorted_available_pool_entry_indices(provider_id: &str, entries: &[Value]) -> Vec<usize> {
    let mut indices = entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| {
            provider_runtime_token_source(provider_id, entry).is_some_and(|source| {
                !credential_expired(source.expiry_value, &source.token)
                    && !credential_in_cooldown(entry)
            })
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    indices.sort_by(|left, right| {
        pool_entry_priority(entries.get(*left))
            .cmp(&pool_entry_priority(entries.get(*right)))
            .then_with(|| left.cmp(right))
    });
    indices
}

fn selected_pool_entry_index(
    entries: &[Value],
    available: &[usize],
    strategy: &str,
) -> Option<usize> {
    match strategy {
        "least_used" if available.len() > 1 => available.iter().copied().min_by(|left, right| {
            pool_entry_request_count(entries.get(*left))
                .cmp(&pool_entry_request_count(entries.get(*right)))
                .then_with(|| {
                    pool_entry_priority(entries.get(*left))
                        .cmp(&pool_entry_priority(entries.get(*right)))
                })
                .then_with(|| left.cmp(right))
        }),
        "random" if available.len() > 1 => {
            let offset = random_pool_offset(available.len());
            available.get(offset).copied()
        }
        _ => available.first().copied(),
    }
}

fn credential_pool_entry_runtime_credential(
    provider_id: &str,
    entry: &Value,
) -> Option<HermesRuntimeCredential> {
    let token_source = provider_runtime_token_source(provider_id, entry)?;
    if credential_expired(token_source.expiry_value, &token_source.token)
        || credential_in_cooldown(entry)
    {
        return None;
    }
    let label = first_string(entry, &["label", "name"]).unwrap_or_else(|| "default".into());
    Some(HermesRuntimeCredential {
        provider_id: provider_id.to_string(),
        api_key: token_source.token,
        base_url: runtime_base_url(provider_id, entry),
        source: format!("hermes-pool:{provider_id}:{label}"),
        expires_at: expiry_label(token_source.expiry_value),
    })
}

fn pool_entry_priority(entry: Option<&Value>) -> u64 {
    entry
        .and_then(|entry| entry.get("priority"))
        .and_then(value_as_u64)
        .unwrap_or(u64::MAX)
}

fn pool_entry_request_count(entry: Option<&Value>) -> u64 {
    entry
        .and_then(|entry| entry.get("request_count"))
        .and_then(value_as_u64)
        .unwrap_or(0)
}

fn credential_pool_strategy(provider_id: &str, store: &Value) -> String {
    let raw = store
        .get("credential_pool_strategies")
        .and_then(|strategies| strategies.get(provider_id))
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| std::env::var("SYNTHCHAT_LLM_CREDENTIAL_POOL_STRATEGY").ok())
        .or_else(|| std::env::var("HERMES_CREDENTIAL_POOL_STRATEGY").ok());
    raw.as_deref()
        .map(normalize_pool_strategy)
        .unwrap_or("fill_first")
        .to_string()
}

fn normalize_pool_strategy(strategy: &str) -> &'static str {
    match strategy.trim().to_ascii_lowercase().as_str() {
        "round_robin" | "round-robin" | "roundrobin" => "round_robin",
        "least_used" | "least-used" | "leastused" => "least_used",
        "random" | "shuffle" => "random",
        _ => "fill_first",
    }
}

fn random_pool_offset(len: usize) -> usize {
    if len <= 1 {
        return 0;
    }
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    nanos.hash(&mut hasher);
    new_id("pool").hash(&mut hasher);
    (hasher.finish() as usize) % len
}

fn credential_pool_credential_status(
    provider_id: &str,
    store: &Value,
) -> Option<HermesExternalCredentialStatus> {
    let pool = store.get("credential_pool")?.get(provider_id)?;
    if let Some(entries) = pool.get("entries").and_then(Value::as_array) {
        return credential_pool_entries_status(provider_id, entries);
    }
    if let Some(entries) = pool.as_array() {
        return credential_pool_entries_status(provider_id, entries);
    }
    let label = first_string(pool, &["label", "name"]).unwrap_or_else(|| "default".into());
    credential_status_from_value(
        provider_id,
        pool,
        format!("hermes-pool:{provider_id}:{label}"),
    )
}

fn credential_pool_entries_status(
    provider_id: &str,
    entries: &[Value],
) -> Option<HermesExternalCredentialStatus> {
    let statuses = entries
        .iter()
        .filter_map(|entry| {
            let label = first_string(entry, &["label", "name"]).unwrap_or_else(|| "default".into());
            credential_status_from_value(
                provider_id,
                entry,
                format!("hermes-pool:{provider_id}:{label}"),
            )
        })
        .collect::<Vec<_>>();
    statuses
        .iter()
        .find(|status| status.state == "present")
        .cloned()
        .or_else(|| statuses.into_iter().next())
}

fn push_credential_pool_entry_statuses(
    entries: &mut Vec<HermesCredentialPoolEntryStatus>,
    provider_id: &str,
    value: &Value,
) {
    if let Some(items) = value.as_array() {
        for (offset, entry) in items.iter().enumerate() {
            entries.push(credential_pool_entry_status(provider_id, offset + 1, entry));
        }
    } else if let Some(items) = value.get("entries").and_then(Value::as_array) {
        for (offset, entry) in items.iter().enumerate() {
            entries.push(credential_pool_entry_status(provider_id, offset + 1, entry));
        }
    } else if value.is_object() {
        entries.push(credential_pool_entry_status(provider_id, 1, value));
    }
}

fn credential_pool_entry_status(
    provider_id: &str,
    index: usize,
    entry: &Value,
) -> HermesCredentialPoolEntryStatus {
    let credential_status = credential_status_from_value(
        provider_id,
        entry,
        format!(
            "hermes-pool:{provider_id}:{}",
            first_string(entry, &["label", "name"]).unwrap_or_else(|| "default".into())
        ),
    );
    let fallback_state = if credential_in_cooldown(entry) {
        "cooldown".to_string()
    } else if provider_runtime_token_source(provider_id, entry).is_some() {
        "present".to_string()
    } else {
        "unusable".to_string()
    };
    HermesCredentialPoolEntryStatus {
        provider_id: provider_id.to_string(),
        index,
        id: first_string(entry, &["id"]),
        label: first_string(entry, &["label", "name"]).unwrap_or_else(|| "default".into()),
        auth_type: first_string(entry, &["auth_type", "authType"]),
        source: first_string(entry, &["source"]),
        state: credential_status
            .as_ref()
            .map(|status| status.state.to_string())
            .unwrap_or(fallback_state),
        expires_at: credential_status
            .and_then(|status| status.expires_at)
            .or_else(|| expiry_label(entry)),
        base_url: runtime_base_url(provider_id, entry),
    }
}

fn remove_pool_entry_value(
    provider_id: &str,
    value: &mut Value,
    target: &str,
) -> AppResult<HermesCredentialPoolEntryStatus> {
    if let Some(entries) = value.as_array_mut() {
        return remove_pool_entry_from_array(provider_id, entries, target);
    }
    if let Some(entries) = value.get_mut("entries").and_then(Value::as_array_mut) {
        return remove_pool_entry_from_array(provider_id, entries, target);
    }
    if value.is_object() && credential_pool_entry_matches(value, 1, target) {
        let removed = credential_pool_entry_status(provider_id, 1, value);
        *value = Value::Null;
        return Ok(removed);
    }
    Err(AppError::NotFound(format!(
        "credential {target} for provider {provider_id}"
    )))
}

fn append_pool_entry_value(value: &mut Value, entry: Value) -> AppResult<usize> {
    if let Some(entries) = value.as_array_mut() {
        entries.push(entry);
        normalize_pool_priorities(entries);
        return Ok(entries.len());
    }
    if let Some(entries) = value.get_mut("entries").and_then(Value::as_array_mut) {
        entries.push(entry);
        normalize_pool_priorities(entries);
        return Ok(entries.len());
    }
    if value.is_object() {
        let existing = std::mem::replace(value, Value::Null);
        *value = json!([existing, entry]);
        let entries = value
            .as_array_mut()
            .ok_or_else(|| AppError::BadRequest("credential_pool entry is not an array".into()))?;
        normalize_pool_priorities(entries);
        return Ok(entries.len());
    }
    *value = json!([entry]);
    Ok(1)
}

fn credential_pool_entry_count(value: &Value) -> usize {
    if let Some(entries) = value.as_array() {
        return entries.len();
    }
    if let Some(entries) = value.get("entries").and_then(Value::as_array) {
        return entries.len();
    }
    usize::from(value.is_object())
}

fn credential_pool_entry_at(
    provider_id: &str,
    value: &Value,
    index: usize,
) -> Option<HermesCredentialPoolEntryStatus> {
    if index == 0 {
        return None;
    }
    if let Some(entries) = value.as_array() {
        return entries
            .get(index - 1)
            .map(|entry| credential_pool_entry_status(provider_id, index, entry));
    }
    if let Some(entries) = value.get("entries").and_then(Value::as_array) {
        return entries
            .get(index - 1)
            .map(|entry| credential_pool_entry_status(provider_id, index, entry));
    }
    (index == 1 && value.is_object())
        .then(|| credential_pool_entry_status(provider_id, index, value))
}

fn remove_pool_entry_from_array(
    provider_id: &str,
    entries: &mut Vec<Value>,
    target: &str,
) -> AppResult<HermesCredentialPoolEntryStatus> {
    let matches = entries
        .iter()
        .enumerate()
        .filter(|(offset, entry)| credential_pool_entry_matches(entry, offset + 1, target))
        .map(|(offset, _)| offset)
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [] => Err(AppError::NotFound(format!(
            "credential {target} for provider {provider_id}"
        ))),
        [_first, _second, ..] => Err(AppError::BadRequest(format!(
            "credential target is ambiguous for provider {provider_id}: {target}"
        ))),
        [offset] => {
            let removed = entries.remove(*offset);
            normalize_pool_priorities(entries);
            Ok(credential_pool_entry_status(
                provider_id,
                *offset + 1,
                &removed,
            ))
        }
    }
}

fn credential_pool_entry_matches(entry: &Value, index: usize, target: &str) -> bool {
    let target = target.trim();
    if target.is_empty() {
        return false;
    }
    if target
        .parse::<usize>()
        .ok()
        .is_some_and(|target_index| target_index == index)
    {
        return true;
    }
    let target = target.to_ascii_lowercase();
    ["id", "label", "name"]
        .iter()
        .filter_map(|key| first_string(entry, &[*key]))
        .any(|value| {
            let value = value.to_ascii_lowercase();
            value == target || value.starts_with(&target)
        })
}

fn normalize_pool_priorities(entries: &mut [Value]) {
    for (priority, entry) in entries.iter_mut().enumerate() {
        if let Some(map) = entry.as_object_mut() {
            map.insert("priority".into(), json!(priority));
        }
    }
}

fn short_credential_pool_id() -> String {
    new_id("cred")
        .rsplit_once('-')
        .map(|(_, value)| value.chars().take(6).collect())
        .unwrap_or_else(|| new_id("cred").chars().take(6).collect())
}

fn reset_pool_entry_status_values(value: &mut Value) -> usize {
    if let Some(entries) = value.as_array_mut() {
        return entries.iter_mut().map(reset_pool_entry_status_value).sum();
    }
    if let Some(entries) = value.get_mut("entries").and_then(Value::as_array_mut) {
        return entries.iter_mut().map(reset_pool_entry_status_value).sum();
    }
    reset_pool_entry_status_value(value)
}

fn reset_pool_entry_status_value(value: &mut Value) -> usize {
    let Some(map) = value.as_object_mut() else {
        return 0;
    };
    let keys = [
        "last_status",
        "last_status_at",
        "last_error_code",
        "last_error_reason",
        "last_error_message",
        "last_error_reset_at",
        "last_error",
    ];
    let mut changed = false;
    for key in keys {
        if map.get(key).is_some_and(|value| !value.is_null()) {
            changed = true;
        }
        map.insert(key.into(), Value::Null);
    }
    usize::from(changed)
}

fn credential_pool_failure_ttl_seconds(kind: &str) -> u64 {
    match kind {
        "terminal_auth" => 10 * 365 * 24 * 60 * 60,
        "auth" => 5 * 60,
        "rate_limit" | "quota" | "long_context_tier" | "oauth_long_context_beta_forbidden" => {
            60 * 60
        }
        _ => 0,
    }
}

fn provider_has_configured_credential(provider: &LlmProvider) -> bool {
    if provider
        .api_key
        .as_deref()
        .map(str::trim)
        .is_some_and(usable_credential_secret)
    {
        return true;
    }
    let env_name = provider.api_key_env.trim();
    if env_name.is_empty() {
        return false;
    }
    usable_credential_secret(env_name)
        || std::env::var(env_name)
            .ok()
            .map(|value| value.trim().to_string())
            .is_some_and(|value| usable_credential_secret(&value))
}

fn mark_credential_pool_value_failure(
    provider_id: &str,
    value: &mut Value,
    credential_source: Option<&str>,
    kind: &str,
    message: &str,
    ttl: u64,
) -> Option<HermesExternalCredentialStatus> {
    if let Some(entries) = value.as_array_mut() {
        let selected = selected_pool_entry_for_failure(provider_id, entries, credential_source)?;
        mark_credential_pool_entry_failure(entries.get_mut(selected)?, kind, message, ttl);
        let label = first_string(entries.get(selected)?, &["label", "name"])
            .unwrap_or_else(|| "default".into());
        return credential_status_from_value(
            provider_id,
            entries.get(selected)?,
            format!("hermes-pool:{provider_id}:{label}"),
        );
    }
    if let Some(entries) = value.get_mut("entries").and_then(Value::as_array_mut) {
        let selected = selected_pool_entry_for_failure(provider_id, entries, credential_source)?;
        mark_credential_pool_entry_failure(entries.get_mut(selected)?, kind, message, ttl);
        let label = first_string(entries.get(selected)?, &["label", "name"])
            .unwrap_or_else(|| "default".into());
        return credential_status_from_value(
            provider_id,
            entries.get(selected)?,
            format!("hermes-pool:{provider_id}:{label}"),
        );
    }
    if credential_source
        .is_some_and(|source| !credential_pool_entry_source_matches(provider_id, value, source))
    {
        return None;
    }
    credential_pool_entry_runtime_credential(provider_id, value)?;
    mark_credential_pool_entry_failure(value, kind, message, ttl);
    let label = first_string(value, &["label", "name"]).unwrap_or_else(|| "default".into());
    credential_status_from_value(
        provider_id,
        value,
        format!("hermes-pool:{provider_id}:{label}"),
    )
}

fn selected_pool_entry_for_failure(
    provider_id: &str,
    entries: &[Value],
    credential_source: Option<&str>,
) -> Option<usize> {
    if let Some(source) = credential_source {
        return credential_pool_entry_index_for_source(provider_id, entries, source);
    }
    let available = sorted_available_pool_entry_indices(provider_id, entries);
    available.first().copied()
}

fn credential_pool_entry_index_for_source(
    provider_id: &str,
    entries: &[Value],
    credential_source: &str,
) -> Option<usize> {
    entries.iter().position(|entry| {
        credential_pool_entry_source_matches(provider_id, entry, credential_source)
    })
}

fn credential_pool_entry_source_matches(
    provider_id: &str,
    entry: &Value,
    credential_source: &str,
) -> bool {
    let label = first_string(entry, &["label", "name"]).unwrap_or_else(|| "default".into());
    credential_source == format!("hermes-pool:{provider_id}:{label}")
}

fn mark_credential_pool_entry_failure(value: &mut Value, kind: &str, message: &str, ttl: u64) {
    let now = unix_now_seconds();
    value["last_status"] = json!(if kind == "terminal_auth" {
        "dead"
    } else {
        "exhausted"
    });
    value["last_status_at"] = json!(now);
    value["last_error_code"] = json!(kind);
    value["last_error_reason"] = json!(kind);
    value["last_error_message"] = json!(message.chars().take(500).collect::<String>());
    value["last_error_reset_at"] = json!(now.saturating_add(ttl));
}

fn credential_status_from_value(
    provider_id: &str,
    value: &Value,
    source: String,
) -> Option<HermesExternalCredentialStatus> {
    let token_source = provider_runtime_token_source(provider_id, value)?;
    let cooldown = credential_in_cooldown(value);
    Some(HermesExternalCredentialStatus {
        provider_id: provider_id.to_string(),
        source,
        state: if cooldown {
            "cooldown"
        } else if credential_expired(token_source.expiry_value, &token_source.token) {
            "expired"
        } else {
            "present"
        },
        expires_at: expiry_label(token_source.expiry_value).or_else(|| expiry_label(value)),
        note: cooldown.then_some("credential_pool entry is in exhaustion cooldown".into()),
    })
}

struct RuntimeTokenSource<'a> {
    token: String,
    expiry_value: &'a Value,
}

fn provider_runtime_token_source<'a>(
    provider_id: &str,
    value: &'a Value,
) -> Option<RuntimeTokenSource<'a>> {
    if provider_id == "nous" {
        if let Some(token) = first_string(value, &["agent_key"]) {
            return Some(RuntimeTokenSource {
                token,
                expiry_value: value,
            });
        }
    }
    if let Some(tokens) = value.get("tokens").filter(|tokens| tokens.is_object()) {
        if let Some(token) = first_string(
            tokens,
            &["runtime_api_key", "api_key", "access_token", "accessToken"],
        ) {
            return Some(RuntimeTokenSource {
                token,
                expiry_value: tokens,
            });
        }
    }
    first_string(
        value,
        &[
            "runtime_api_key",
            "api_key",
            "agent_key",
            "access_token",
            "accessToken",
            "token",
        ],
    )
    .map(|token| RuntimeTokenSource {
        token,
        expiry_value: value,
    })
}

fn runtime_base_url(provider_id: &str, value: &Value) -> Option<String> {
    if provider_id == "nous" {
        return first_string(value, &["inference_base_url", "base_url", "api_base_url"]);
    }
    first_string(
        value,
        &[
            "base_url",
            "inference_base_url",
            "portal_base_url",
            "api_base_url",
        ],
    )
}

fn expiry_label(value: &Value) -> Option<String> {
    first_string(
        value,
        &[
            "agent_key_expires_at",
            "expires_at",
            "expiresAt",
            "expires_at_ms",
            "access_expires_at",
            "expiry_date",
            "expires",
        ],
    )
}

fn hermes_provider_id_candidates(provider: &LlmProvider) -> Vec<&'static str> {
    let haystack = format!(
        "{} {} {} {} {}",
        provider.id,
        provider.provider_type,
        provider.preset.as_deref().unwrap_or_default(),
        provider.base_url,
        provider.model
    )
    .to_ascii_lowercase();
    if haystack.contains("openai-codex") || haystack.contains("codex") {
        return vec!["openai-codex"];
    }
    if haystack.contains("openrouter") {
        return vec!["openrouter"];
    }
    if haystack.contains("claude-code")
        || haystack.contains("claude_code")
        || haystack.contains("claude setup-token")
    {
        return vec!["claude-code"];
    }
    if haystack.contains("anthropic") || haystack.contains("claude") {
        return vec!["anthropic", "claude-code"];
    }
    if haystack.contains("xai-oauth")
        || haystack.contains("x-ai-oauth")
        || haystack.contains("grok-oauth")
    {
        return vec!["xai-oauth"];
    }
    if haystack.contains("xai") || haystack.contains("x.ai") || haystack.contains("grok") {
        return vec!["xai"];
    }
    if haystack.contains("qwen-oauth") {
        return vec!["qwen-oauth"];
    }
    if haystack.contains("alibaba-coding-plan")
        || haystack.contains("alibaba_coding")
        || haystack.contains("alibaba-coding")
        || haystack.contains("alibaba_coding_plan")
    {
        return vec!["alibaba-coding-plan"];
    }
    if haystack.contains("dashscope") || haystack.contains("alibaba") || haystack.contains("qwen") {
        return vec!["alibaba"];
    }
    if haystack.contains("google-gemini-cli")
        || haystack.contains("gemini-cli")
        || haystack.contains("gemini-oauth")
    {
        return vec!["google-gemini-cli"];
    }
    if haystack.contains("spotify") {
        return vec!["spotify"];
    }
    if haystack.contains("minimax-oauth") {
        return vec!["minimax-oauth"];
    }
    if haystack.contains("minimax-cn")
        || haystack.contains("minimax-china")
        || haystack.contains("minimax_cn")
    {
        return vec!["minimax-cn"];
    }
    if haystack.contains("minimax") {
        return vec!["minimax"];
    }
    if haystack.contains("nous") {
        return vec!["nous"];
    }
    if haystack.contains("zai")
        || haystack.contains("z-ai")
        || haystack.contains("z.ai")
        || haystack.contains("zhipu")
        || haystack.contains("glm")
    {
        return vec!["zai"];
    }
    if haystack.contains("kimi")
        || haystack.contains("moonshot")
        || haystack.contains("kimi-for-coding")
    {
        return vec!["kimi-for-coding"];
    }
    if haystack.contains("deepseek") || haystack.contains("deep-seek") {
        return vec!["deepseek"];
    }
    if haystack.contains("stepfun")
        || haystack.contains("step-plan")
        || haystack.contains("stepfun-coding-plan")
    {
        return vec!["stepfun"];
    }
    if haystack.contains("lmstudio") || haystack.contains("lm-studio") {
        return vec!["lmstudio"];
    }
    if haystack.contains("copilot-acp") {
        return vec!["copilot-acp"];
    }
    if haystack.contains("github-copilot")
        || haystack.contains("copilot")
        || haystack.contains("github")
    {
        return vec!["github-copilot"];
    }
    if haystack.contains("opencode-go")
        || haystack.contains("opencode-go-sub")
        || contains_provider_word(&haystack, "go")
    {
        return vec!["opencode-go"];
    }
    if haystack.contains("opencode")
        || haystack.contains("opencode-zen")
        || contains_provider_word(&haystack, "zen")
    {
        return vec!["opencode"];
    }
    if haystack.contains("kilo") || haystack.contains("kilocode") || haystack.contains("kilo-code")
    {
        return vec!["kilo"];
    }
    if haystack.contains("huggingface")
        || haystack.contains("hugging-face")
        || haystack.contains("huggingface-hub")
        || contains_provider_word(&haystack, "hf")
    {
        return vec!["huggingface"];
    }
    if haystack.contains("novita")
        || haystack.contains("novita-ai")
        || haystack.contains("novitaai")
    {
        return vec!["novita"];
    }
    if haystack.contains("nvidia")
        || haystack.contains("nemotron")
        || contains_provider_word(&haystack, "nim")
        || haystack.contains("build-nvidia")
    {
        return vec!["nvidia"];
    }
    if haystack.contains("xiaomi") || haystack.contains("mimo") || haystack.contains("xiaomi-mimo")
    {
        return vec!["xiaomi"];
    }
    if haystack.contains("tencent")
        || haystack.contains("tokenhub")
        || haystack.contains("tencentmaas")
    {
        return vec!["tencent-tokenhub"];
    }
    if haystack.contains("arcee") || haystack.contains("arcee-ai") || haystack.contains("arceeai") {
        return vec!["arcee"];
    }
    if haystack.contains("gmi") || haystack.contains("gmi-cloud") || haystack.contains("gmicloud") {
        return vec!["gmi"];
    }
    if haystack.contains("ollama-cloud") {
        return vec!["ollama-cloud"];
    }
    if haystack.contains("azure-foundry") {
        return vec!["azure-foundry"];
    }
    if haystack.contains("bedrock")
        || contains_provider_word(&haystack, "aws")
        || haystack.contains("amazon-bedrock")
        || contains_provider_word(&haystack, "amazon")
    {
        return vec!["bedrock"];
    }
    if haystack.contains("groq") {
        return vec!["groq"];
    }
    if haystack.contains("mistral") {
        return vec!["mistral"];
    }
    if haystack.contains("cohere") {
        return vec!["cohere"];
    }
    if haystack.contains("openai-api") {
        return vec!["openai-api", "openai"];
    }
    if haystack.contains("openai") {
        return vec!["openai-api", "openai"];
    }
    Vec::new()
}

fn contains_provider_word(haystack: &str, needle: &str) -> bool {
    haystack
        .split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '.' || ch == '_'))
        .any(|part| part == needle)
}

fn hermes_auth_store_candidates() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(home) = std::env::var_os("HERMES_HOME").filter(|value| !value.is_empty()) {
        push_unique_path(&mut paths, PathBuf::from(home).join("auth.json"));
    }
    if let Some(home) = home_dir() {
        push_unique_path(&mut paths, home.join(".hermes").join("auth.json"));
    }
    paths
}

fn primary_hermes_auth_store_path() -> AppResult<PathBuf> {
    hermes_auth_store_candidates()
        .into_iter()
        .next()
        .ok_or_else(|| AppError::BadRequest("HOME/USERPROFILE is not available".into()))
}

fn read_primary_hermes_auth_store() -> AppResult<Value> {
    read_hermes_auth_store_path(&primary_hermes_auth_store_path()?)
}

fn read_hermes_auth_store_path(path: &PathBuf) -> AppResult<Value> {
    match std::fs::read_to_string(path) {
        Ok(payload) => serde_json::from_str::<Value>(&payload).map_err(|error| {
            AppError::BadRequest(format!(
                "invalid Hermes auth store {}: {error}",
                path.display()
            ))
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(json!({})),
        Err(error) => Err(AppError::Io(error)),
    }
}

fn write_hermes_auth_store_path(path: &PathBuf, store: &Value) -> AppResult<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(store)?)?;
    Ok(())
}

fn load_bitwarden_secrets() -> AppResult<HashMap<String, String>> {
    let Some(config) = bitwarden_config() else {
        return Ok(HashMap::new());
    };
    let cache_key =
        bitwarden_cache_key(&config.access_token, &config.project_id, &config.server_url);
    if let Some(cached) = read_bitwarden_disk_cache(&cache_key, config.cache_ttl_seconds) {
        return Ok(cached);
    }
    let bws = bitwarden_binary_path().ok_or_else(|| {
        AppError::BadRequest(
            "Bitwarden Secrets Manager is configured but `bws` is not available on PATH".into(),
        )
    })?;
    let secrets = run_bws_secret_list(&bws, &config)?;
    write_bitwarden_disk_cache(&cache_key, &secrets);
    Ok(secrets)
}

#[derive(Debug, Clone)]
struct BitwardenConfig {
    access_token: String,
    project_id: String,
    server_url: String,
    cache_ttl_seconds: u64,
}

fn bitwarden_config() -> Option<BitwardenConfig> {
    let access_token = env_string("BWS_ACCESS_TOKEN")
        .or_else(|| env_string("BITWARDEN_ACCESS_TOKEN"))
        .or_else(|| env_string("SYNTHCHAT_BWS_ACCESS_TOKEN"))?;
    let project_id = env_string("BWS_PROJECT_ID")
        .or_else(|| env_string("BITWARDEN_PROJECT_ID"))
        .or_else(|| env_string("HERMES_BWS_PROJECT_ID"))
        .or_else(|| env_string("SYNTHCHAT_BWS_PROJECT_ID"))?;
    let server_url = env_string("BWS_SERVER_URL")
        .or_else(|| env_string("BITWARDEN_SERVER_URL"))
        .unwrap_or_default();
    let cache_ttl_seconds = env_string("BWS_CACHE_TTL_SECONDS")
        .or_else(|| env_string("SYNTHCHAT_BWS_CACHE_TTL_SECONDS"))
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(BITWARDEN_DEFAULT_CACHE_TTL_SECONDS);
    Some(BitwardenConfig {
        access_token,
        project_id,
        server_url,
        cache_ttl_seconds,
    })
}

fn run_bws_secret_list(
    binary: &Path,
    config: &BitwardenConfig,
) -> AppResult<HashMap<String, String>> {
    let mut command = Command::new(binary);
    command.hide_window();
    command
        .args(["secret", "list", &config.project_id, "--output", "json"])
        .env("BWS_ACCESS_TOKEN", &config.access_token)
        .env("NO_COLOR", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if !config.server_url.trim().is_empty() {
        command.env("BWS_SERVER_URL", config.server_url.trim());
    }
    let mut child = command.spawn().map_err(|error| {
        AppError::BadRequest(format!(
            "failed to invoke Bitwarden Secrets Manager `bws`: {error}"
        ))
    })?;
    let deadline = std::time::Instant::now()
        .checked_add(std::time::Duration::from_secs(
            BITWARDEN_RUN_TIMEOUT_SECONDS,
        ))
        .unwrap_or_else(std::time::Instant::now);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if std::time::Instant::now() < deadline => {
                thread::sleep(std::time::Duration::from_millis(50));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(AppError::BadRequest(format!(
                    "Bitwarden Secrets Manager `bws` timed out after {BITWARDEN_RUN_TIMEOUT_SECONDS}s"
                )));
            }
            Err(error) => {
                let _ = child.kill();
                return Err(AppError::BadRequest(format!(
                    "failed while waiting for Bitwarden Secrets Manager `bws`: {error}"
                )));
            }
        }
    }
    let output = child.wait_with_output().map_err(|error| {
        AppError::BadRequest(format!(
            "failed to collect Bitwarden Secrets Manager `bws` output: {error}"
        ))
    })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let detail = redact_bitwarden_error(if stderr.trim().is_empty() {
            stdout.trim()
        } else {
            stderr.trim()
        });
        return Err(AppError::BadRequest(format!(
            "Bitwarden Secrets Manager `bws secret list` failed: {detail}"
        )));
    }
    let payload = serde_json::from_slice::<Value>(&output.stdout).map_err(|error| {
        AppError::BadRequest(format!("invalid Bitwarden Secrets Manager JSON: {error}"))
    })?;
    Ok(bitwarden_secrets_from_payload(&payload).0)
}

fn bitwarden_secrets_from_payload(payload: &Value) -> (HashMap<String, String>, Vec<String>) {
    let mut secrets = HashMap::new();
    let mut warnings = Vec::new();
    let Some(items) = payload.as_array() else {
        warnings.push("Bitwarden secret list returned a non-array payload".into());
        return (secrets, warnings);
    };
    for item in items {
        let Some(key) = first_string(item, &["key", "name"]) else {
            continue;
        };
        let Some(value) = first_string(item, &["value"]) else {
            continue;
        };
        if !valid_env_name(&key) {
            warnings.push(format!("skipped invalid env secret name: {key}"));
            continue;
        }
        secrets.insert(key, value);
    }
    (secrets, warnings)
}

fn bitwarden_binary_path() -> Option<PathBuf> {
    if let Some(path) = env_string("BWS_BINARY").or_else(|| env_string("SYNTHCHAT_BWS_BINARY")) {
        let path = PathBuf::from(path);
        if path.exists() {
            return Some(path);
        }
    }
    find_on_path(if cfg!(windows) { "bws.exe" } else { "bws" })
}

fn find_on_path(binary: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    std::env::split_paths(&path_var)
        .map(|path| path.join(binary))
        .find(|path| path.exists())
}

fn bitwarden_cache_key(access_token: &str, project_id: &str, server_url: &str) -> String {
    let token_fp = Sha256::digest(access_token.as_bytes());
    format!("{:x}|{}|{}", token_fp, project_id.trim(), server_url.trim())
}

fn read_bitwarden_disk_cache(cache_key: &str, ttl_seconds: u64) -> Option<HashMap<String, String>> {
    if ttl_seconds == 0 {
        return None;
    }
    let path = bitwarden_disk_cache_path()?;
    let payload = std::fs::read_to_string(path).ok()?;
    let value = serde_json::from_str::<Value>(&payload).ok()?;
    if value.get("key").and_then(Value::as_str) != Some(cache_key) {
        return None;
    }
    let fetched_at = value.get("fetched_at").and_then(Value::as_u64)?;
    if unix_now_seconds().saturating_sub(fetched_at) >= ttl_seconds {
        return None;
    }
    let secrets = value.get("secrets")?.as_object()?;
    Some(
        secrets
            .iter()
            .filter_map(|(key, value)| {
                Some((key.clone(), value.as_str()?.trim().to_string()))
                    .filter(|(_, value)| usable_credential_secret(value))
            })
            .collect(),
    )
}

fn write_bitwarden_disk_cache(cache_key: &str, secrets: &HashMap<String, String>) {
    let Some(path) = bitwarden_disk_cache_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
    }
    let payload = json!({
        "key": cache_key,
        "secrets": secrets,
        "fetched_at": unix_now_seconds()
    });
    let _ = std::fs::write(
        path,
        serde_json::to_string_pretty(&payload).unwrap_or_default(),
    );
}

fn bitwarden_disk_cache_path() -> Option<PathBuf> {
    hermes_home_dir().map(|home| home.join("cache").join(BITWARDEN_CACHE_BASENAME))
}

fn env_string(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn valid_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first == '_' || first.is_ascii_alphabetic()) {
        return false;
    }
    chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

fn usable_credential_secret(value: &str) -> bool {
    let trimmed = value.trim();
    !trimmed.is_empty()
        && !matches!(
            trimmed.to_ascii_lowercase().as_str(),
            "none"
                | "null"
                | "undefined"
                | "changeme"
                | "placeholder"
                | "your_api_key"
                | "your_api_key_here"
        )
}

fn redact_bitwarden_error(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return "no error text".into();
    }
    let mut redacted = trimmed.to_string();
    for key in [
        "BWS_ACCESS_TOKEN",
        "access_token",
        "token",
        "secret",
        "password",
    ] {
        redacted = redacted.replace(key, "[redacted]");
    }
    redacted.chars().take(240).collect()
}

fn hermes_home_dir() -> Option<PathBuf> {
    std::env::var_os("HERMES_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| home_dir().map(|home| home.join(".hermes")))
}

fn normalize_credential_pool_provider(provider: &str) -> String {
    match provider.trim().to_ascii_lowercase().as_str() {
        "openai" => "openrouter".into(),
        "codex" | "openai_codex" => "openai-codex".into(),
        "nous-oauth" | "nous_portal" | "nous-portal" => "nous".into(),
        "minimax" | "minimax_oauth" => "minimax-oauth".into(),
        "minimax-china" | "minimax_cn" => "minimax-cn".into(),
        "xai" | "x-ai" | "x.ai" | "grok" | "x-ai-oauth" | "grok-oauth" | "xai-grok-oauth" => {
            "xai-oauth".into()
        }
        "glm" | "z-ai" | "z.ai" | "zhipu" => "zai".into(),
        "kimi" | "kimi-coding" | "kimi-coding-cn" | "moonshot" => "kimi-for-coding".into(),
        "step" | "stepfun-coding-plan" => "stepfun".into(),
        "claude" | "claude-oauth" | "anthropic-oauth" => "anthropic".into(),
        "dashscope" | "aliyun" | "qwen" | "alibaba-cloud" => "alibaba".into(),
        "alibaba_coding" | "alibaba-coding" | "alibaba_coding_plan" => "alibaba-coding-plan".into(),
        "gemini" | "gemini-cli" | "gemini-oauth" => "google-gemini-cli".into(),
        "copilot" | "github" => "github-copilot".into(),
        "github-copilot-acp" => "copilot-acp".into(),
        "opencode-zen" | "zen" => "opencode".into(),
        "go" | "opencode-go-sub" => "opencode-go".into(),
        "kilocode" | "kilo-code" | "kilo-gateway" => "kilo".into(),
        "deep-seek" => "deepseek".into(),
        "hf" | "hugging-face" | "huggingface-hub" => "huggingface".into(),
        "novita-ai" | "novitaai" => "novita".into(),
        "mimo" | "xiaomi-mimo" => "xiaomi".into(),
        "tencent" | "tokenhub" | "tencent-cloud" | "tencentmaas" => "tencent-tokenhub".into(),
        "aws" | "aws-bedrock" => "bedrock".into(),
        "amazon-bedrock" | "amazon" => "bedrock".into(),
        "nim" | "nvidia-nim" | "build-nvidia" | "nemotron" => "nvidia".into(),
        "arcee-ai" | "arceeai" => "arcee".into(),
        "gmi-cloud" | "gmicloud" => "gmi".into(),
        "lm_studio" => "lmstudio".into(),
        "ollama" => "custom".into(),
        "vllm" | "llamacpp" | "llama.cpp" | "llama-cpp" => "local".into(),
        other => other.to_string(),
    }
}

fn credential_pool_provider_matches(provider_id: &str, filter: &str) -> bool {
    let normalized = normalize_credential_pool_provider(filter);
    provider_id == normalized || provider_id.starts_with(&normalized)
}

fn push_unique_path(paths: &mut Vec<PathBuf>, path: PathBuf) {
    if !paths.iter().any(|existing| existing == &path) {
        paths.push(path);
    }
}

fn google_gemini_oauth_credentials_path() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("HERMES_HOME").filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(home).join("auth").join("google_oauth.json"));
    }
    home_dir().map(|home| home.join(".hermes").join("auth").join("google_oauth.json"))
}

fn first_string(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        value
            .get(*key)
            .and_then(|item| {
                item.as_str()
                    .map(str::to_string)
                    .or_else(|| item.as_i64().map(|number| number.to_string()))
                    .or_else(|| item.as_u64().map(|number| number.to_string()))
            })
            .map(|text| text.trim().to_string())
            .filter(|text| !text.is_empty())
    })
}

fn value_as_u64(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_str()?.trim().parse::<u64>().ok())
}

fn value_as_i64(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_str()?.trim().parse::<i64>().ok())
}

fn credential_expired(state: &Value, token: &str) -> bool {
    let now = unix_now_seconds();
    if let Some(expiry) = expiry_seconds_from_state(state) {
        return expiry <= now.saturating_add(60);
    }
    if let Some(expiry) = jwt_exp_seconds(token) {
        return expiry <= now.saturating_add(60);
    }
    false
}

fn credential_in_cooldown(value: &Value) -> bool {
    let Some(raw) = value.get("last_error_reset_at") else {
        return false;
    };
    let reset_at = raw.as_f64().or_else(|| {
        raw.as_str()
            .and_then(|text| text.trim().parse::<f64>().ok())
    });
    reset_at.is_some_and(|reset_at| reset_at > unix_now_seconds() as f64)
}

fn expiry_seconds_from_state(state: &Value) -> Option<u64> {
    for key in [
        "agent_key_expires_at",
        "expires_at",
        "expiresAt",
        "access_expires_at",
        "expiry_date",
        "expires",
    ] {
        let Some(raw) = state.get(key) else {
            continue;
        };
        if let Some(number) = raw.as_u64() {
            return Some(normalize_epoch_seconds(number));
        }
        if let Some(text) = raw.as_str().map(str::trim).filter(|text| !text.is_empty()) {
            if let Ok(number) = text.parse::<u64>() {
                return Some(normalize_epoch_seconds(number));
            }
            if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(text) {
                return (dt.timestamp() > 0).then_some(dt.timestamp() as u64);
            }
        }
    }
    None
}

fn normalize_epoch_seconds(value: u64) -> u64 {
    if value > 10_000_000_000 {
        value / 1000
    } else {
        value
    }
}

fn jwt_exp_seconds(token: &str) -> Option<u64> {
    jwt_claims(token)?.get("exp").and_then(Value::as_u64)
}

fn jwt_claims(token: &str) -> Option<Value> {
    let payload = token.split('.').nth(1)?;
    let decoded = URL_SAFE_NO_PAD.decode(payload.as_bytes()).ok()?;
    serde_json::from_slice::<Value>(&decoded).ok()
}

fn scope_values(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::String(text)) => text
            .split_whitespace()
            .map(str::trim)
            .filter(|scope| !scope.is_empty())
            .map(str::to_string)
            .collect(),
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .flat_map(str::split_whitespace)
            .map(str::trim)
            .filter(|scope| !scope.is_empty())
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

fn unix_now_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(u64::MAX)
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("USERPROFILE")
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider(id: &str) -> LlmProvider {
        LlmProvider {
            id: id.into(),
            name: id.into(),
            provider_type: "openai".into(),
            preset: Some(id.into()),
            base_url: String::new(),
            append_chat_path: true,
            api_key_env: String::new(),
            api_key: None,
            model: "gpt-5".into(),
            enabled: true,
            timeout_seconds: 60,
            request_timeout_seconds: None,
            stale_timeout_seconds: None,
            models: json!({}),
            prompt_cache_mode: "auto".into(),
            prompt_cache_ttl: "5m".into(),
            prompt_cache_layout: "ephemeral-last".into(),
        }
    }

    #[test]
    fn hermes_auth_store_reads_provider_state_tokens_without_leaking_values() {
        let _guard = HERMES_AUTH_TEST_ENV_LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join(format!(
            "synthchat-hermes-auth-{}",
            crate::models::new_id("test")
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("auth.json"),
            serde_json::json!({
                "version": 1,
                "providers": {
                    "openai-codex": {
                        "access_token": "codex-token",
                        "base_url": "https://chatgpt.com/backend-api/codex",
                        "expires_at": "2999-01-01T00:00:00Z"
                    }
                }
            })
            .to_string(),
        )
        .unwrap();
        std::env::set_var("HERMES_HOME", &dir);

        let credential = resolve_hermes_runtime_credential(&provider("openai-codex")).unwrap();
        assert_eq!(credential.provider_id, "openai-codex");
        assert_eq!(credential.api_key, "codex-token");
        assert_eq!(
            credential.base_url.as_deref(),
            Some("https://chatgpt.com/backend-api/codex")
        );
        assert_eq!(credential.source, "hermes-auth:openai-codex");

        std::env::remove_var("HERMES_HOME");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn hermes_auth_store_reads_codex_singleton_tokens_shape() {
        let _guard = HERMES_AUTH_TEST_ENV_LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join(format!(
            "synthchat-hermes-auth-codex-tokens-{}",
            crate::models::new_id("test")
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("auth.json"),
            serde_json::json!({
                "providers": {
                    "openai-codex": {
                        "tokens": {
                            "access_token": "codex-nested-token",
                            "refresh_token": "refresh-token"
                        },
                        "last_refresh": "2999-01-01T00:00:00Z"
                    }
                }
            })
            .to_string(),
        )
        .unwrap();
        std::env::set_var("HERMES_HOME", &dir);

        let credential = resolve_hermes_runtime_credential(&provider("openai-codex")).unwrap();
        assert_eq!(credential.api_key, "codex-nested-token");
        assert_eq!(credential.source, "hermes-auth:openai-codex");

        std::env::remove_var("HERMES_HOME");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn hermes_auth_store_maps_common_provider_pool_aliases() {
        let _guard = HERMES_AUTH_TEST_ENV_LOCK.lock().unwrap();
        let old_hermes_home = std::env::var_os("HERMES_HOME");
        let dir = std::env::temp_dir().join(format!(
            "synthchat-hermes-auth-provider-aliases-{}",
            crate::models::new_id("test")
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("auth.json"),
            json!({
                "credential_pool": {
                    "openai-api": [{ "label": "api", "access_token": "openai-token" }],
                    "minimax-cn": [{ "label": "api", "access_token": "minimax-token" }],
                    "alibaba-coding-plan": [{ "label": "api", "access_token": "alibaba-coding-token" }],
                    "github-copilot": [{ "label": "api", "access_token": "copilot-token" }],
                    "opencode-go": [{ "label": "api", "access_token": "opencode-go-token" }],
                    "huggingface": [{ "label": "api", "access_token": "huggingface-token" }],
                    "nvidia": [{ "label": "api", "access_token": "nvidia-token" }],
                    "tencent-tokenhub": [{ "label": "api", "access_token": "tencent-token" }],
                    "bedrock": [{ "label": "api", "access_token": "bedrock-token" }],
                    "arcee": [{ "label": "api", "access_token": "arcee-token" }],
                    "gmi": [{ "label": "api", "access_token": "gmi-token" }],
                    "deepseek": [{ "label": "api", "access_token": "deepseek-token" }],
                    "zai": [{ "label": "api", "access_token": "zai-token" }],
                    "kimi-for-coding": [{ "label": "api", "access_token": "kimi-token" }],
                    "groq": [{ "label": "api", "access_token": "groq-token" }],
                    "mistral": [{ "label": "api", "access_token": "mistral-token" }]
                }
            })
            .to_string(),
        )
        .unwrap();
        std::env::set_var("HERMES_HOME", &dir);

        for (provider_id, expected_provider, expected_token) in [
            ("openai", "openai-api", "openai-token"),
            ("minimax-china", "minimax-cn", "minimax-token"),
            (
                "alibaba_coding_plan",
                "alibaba-coding-plan",
                "alibaba-coding-token",
            ),
            ("github", "github-copilot", "copilot-token"),
            ("opencode-go-sub", "opencode-go", "opencode-go-token"),
            ("hf", "huggingface", "huggingface-token"),
            ("nvidia-nim", "nvidia", "nvidia-token"),
            ("tencentmaas", "tencent-tokenhub", "tencent-token"),
            ("amazon-bedrock", "bedrock", "bedrock-token"),
            ("arcee-ai", "arcee", "arcee-token"),
            ("gmi-cloud", "gmi", "gmi-token"),
            ("deep-seek", "deepseek", "deepseek-token"),
            ("glm", "zai", "zai-token"),
            ("moonshot", "kimi-for-coding", "kimi-token"),
            ("groq", "groq", "groq-token"),
            ("mistral", "mistral", "mistral-token"),
        ] {
            let credential = resolve_hermes_runtime_credential(&provider(provider_id)).unwrap();
            assert_eq!(credential.provider_id, expected_provider);
            assert_eq!(credential.api_key, expected_token);
        }

        restore_env("HERMES_HOME", old_hermes_home);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn hermes_auth_store_prefers_nous_agent_key_from_pool() {
        let _guard = HERMES_AUTH_TEST_ENV_LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join(format!(
            "synthchat-hermes-auth-nous-pool-{}",
            crate::models::new_id("test")
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("auth.json"),
            serde_json::json!({
                "credential_pool": {
                    "nous": [{
                        "label": "portal",
                        "access_token": "portal-access-token",
                        "agent_key": "nas-agent-key",
                        "agent_key_expires_at": "2999-01-01T00:00:00Z",
                        "inference_base_url": "https://inference-api.nousresearch.com/v1"
                    }]
                }
            })
            .to_string(),
        )
        .unwrap();
        std::env::set_var("HERMES_HOME", &dir);

        let credential = resolve_hermes_runtime_credential(&provider("nous")).unwrap();
        assert_eq!(credential.api_key, "nas-agent-key");
        assert_eq!(
            credential.base_url.as_deref(),
            Some("https://inference-api.nousresearch.com/v1")
        );
        assert_eq!(credential.source, "hermes-pool:nous:portal");

        std::env::remove_var("HERMES_HOME");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn hermes_auth_store_skips_pool_entries_in_cooldown() {
        let _guard = HERMES_AUTH_TEST_ENV_LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join(format!(
            "synthchat-hermes-auth-pool-cooldown-{}",
            crate::models::new_id("test")
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("auth.json"),
            serde_json::json!({
                "credential_pool": {
                    "openai-codex": [{
                        "label": "cooling",
                        "access_token": "cooldown-token",
                        "last_error_reset_at": unix_now_seconds() + 3600
                    }, {
                        "label": "usable",
                        "access_token": "usable-token"
                    }]
                }
            })
            .to_string(),
        )
        .unwrap();
        std::env::set_var("HERMES_HOME", &dir);

        let credential = resolve_hermes_runtime_credential(&provider("openai-codex")).unwrap();
        assert_eq!(credential.api_key, "usable-token");
        assert_eq!(credential.source, "hermes-pool:openai-codex:usable");
        let status = hermes_auth_store_credential_status(&provider("openai-codex")).unwrap();
        assert_eq!(status.state, "present");
        assert_eq!(status.source, "hermes-pool:openai-codex:usable");

        std::env::remove_var("HERMES_HOME");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn hermes_auth_store_prefers_profile_over_global_auth_store() {
        let _guard = HERMES_AUTH_TEST_ENV_LOCK.lock().unwrap();
        let old_hermes_home = std::env::var_os("HERMES_HOME");
        let old_home = std::env::var_os("HOME");
        let old_userprofile = std::env::var_os("USERPROFILE");
        let root = std::env::temp_dir().join(format!(
            "synthchat-hermes-auth-profile-global-{}",
            crate::models::new_id("test")
        ));
        let profile = root.join("profile");
        let global = root.join("global");
        std::fs::create_dir_all(&profile).unwrap();
        std::fs::create_dir_all(global.join(".hermes")).unwrap();
        std::fs::write(
            profile.join("auth.json"),
            serde_json::json!({
                "providers": {
                    "openai-codex": {
                        "access_token": "profile-token",
                        "expires_at": "2999-01-01T00:00:00Z"
                    }
                }
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(
            global.join(".hermes").join("auth.json"),
            serde_json::json!({
                "providers": {
                    "openai-codex": {
                        "access_token": "global-token",
                        "expires_at": "2999-01-01T00:00:00Z"
                    }
                }
            })
            .to_string(),
        )
        .unwrap();
        std::env::set_var("HERMES_HOME", &profile);
        std::env::set_var("HOME", &global);
        std::env::set_var("USERPROFILE", &global);

        let credential = resolve_hermes_runtime_credential(&provider("openai-codex")).unwrap();
        assert_eq!(credential.api_key, "profile-token");
        assert_eq!(credential.source, "hermes-auth:openai-codex");

        restore_env("HERMES_HOME", old_hermes_home);
        restore_env("HOME", old_home);
        restore_env("USERPROFILE", old_userprofile);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn hermes_auth_store_falls_back_to_global_pool_when_profile_provider_missing() {
        let _guard = HERMES_AUTH_TEST_ENV_LOCK.lock().unwrap();
        let old_hermes_home = std::env::var_os("HERMES_HOME");
        let old_home = std::env::var_os("HOME");
        let old_userprofile = std::env::var_os("USERPROFILE");
        let root = std::env::temp_dir().join(format!(
            "synthchat-hermes-auth-profile-global-pool-{}",
            crate::models::new_id("test")
        ));
        let profile = root.join("profile");
        let global = root.join("global");
        std::fs::create_dir_all(&profile).unwrap();
        std::fs::create_dir_all(global.join(".hermes")).unwrap();
        std::fs::write(
            profile.join("auth.json"),
            serde_json::json!({
                "credential_pool": {
                    "nous": [{
                        "label": "profile-nous",
                        "agent_key": "profile-nous-key"
                    }]
                }
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(
            global.join(".hermes").join("auth.json"),
            serde_json::json!({
                "credential_pool": {
                    "openai-codex": [{
                        "label": "global-codex",
                        "access_token": "global-codex-token"
                    }]
                }
            })
            .to_string(),
        )
        .unwrap();
        std::env::set_var("HERMES_HOME", &profile);
        std::env::set_var("HOME", &global);
        std::env::set_var("USERPROFILE", &global);

        let credential = resolve_hermes_runtime_credential(&provider("openai-codex")).unwrap();
        assert_eq!(credential.api_key, "global-codex-token");
        assert_eq!(credential.source, "hermes-pool:openai-codex:global-codex");

        restore_env("HERMES_HOME", old_hermes_home);
        restore_env("HOME", old_home);
        restore_env("USERPROFILE", old_userprofile);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn spotify_pkce_state_persists_as_runtime_credential() {
        let _guard = HERMES_AUTH_TEST_ENV_LOCK.lock().unwrap();
        let old_hermes_home = std::env::var_os("HERMES_HOME");
        let root = std::env::temp_dir().join(format!(
            "synthchat-spotify-pkce-auth-{}",
            crate::models::new_id("test")
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::env::set_var("HERMES_HOME", &root);

        let state = spotify_oauth_state_from_token_response(
            &json!({
                "access_token": "spotify-access-token",
                "refresh_token": "spotify-refresh-token",
                "expires_in": 3600,
                "token_type": "Bearer",
                "scope": "user-read-playback-state"
            }),
            &json!({}),
            "spotify-client-id",
            DEFAULT_SPOTIFY_REDIRECT_URI,
            "user-read-playback-state",
            DEFAULT_SPOTIFY_ACCOUNTS_BASE_URL,
            DEFAULT_SPOTIFY_API_BASE_URL,
        )
        .unwrap();
        let credential = persist_spotify_oauth_state(&state).unwrap();
        assert_eq!(credential.provider_id, "spotify");
        assert_eq!(credential.api_key, "spotify-access-token");
        assert_eq!(credential.source, "hermes-auth:spotify");
        assert_eq!(
            credential.base_url.as_deref(),
            Some(DEFAULT_SPOTIFY_API_BASE_URL)
        );

        let store = serde_json::from_str::<Value>(
            &std::fs::read_to_string(root.join("auth.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            store["providers"]["spotify"]["refresh_token"],
            "spotify-refresh-token"
        );
        assert_eq!(store["providers"]["spotify"]["auth_type"], "oauth_pkce");
        assert_eq!(
            resolve_hermes_runtime_credential(&provider("spotify"))
                .unwrap()
                .api_key,
            "spotify-access-token"
        );

        restore_env("HERMES_HOME", old_hermes_home);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn spotify_refresh_response_preserves_refresh_token_and_runtime_shape() {
        let state = spotify_refreshed_state_from_response(
            &json!({
                "access_token": "old-access",
                "refresh_token": "old-refresh",
                "client_id": "spotify-client-id",
                "redirect_uri": DEFAULT_SPOTIFY_REDIRECT_URI,
                "scope": "user-read-playback-state",
                "accounts_base_url": DEFAULT_SPOTIFY_ACCOUNTS_BASE_URL,
                "api_base_url": DEFAULT_SPOTIFY_API_BASE_URL
            }),
            &json!({
                "access_token": "new-access",
                "expires_in": 1800,
                "token_type": "Bearer"
            }),
        )
        .unwrap();

        assert_eq!(state["access_token"], "new-access");
        assert_eq!(state["refresh_token"], "old-refresh");
        assert_eq!(state["auth_type"], "oauth_pkce");
        assert_eq!(state["source"], "oauth-refresh");
        assert_eq!(state["base_url"], DEFAULT_SPOTIFY_API_BASE_URL);
        assert!(
            provider_state_credential("spotify", &json!({"providers": {"spotify": state}}))
                .is_some()
        );
    }

    #[test]
    fn anthropic_refresh_response_preserves_refresh_token_and_runtime_shape() {
        let state = anthropic_refreshed_state_from_response(
            &json!({
                "accessToken": "old-access",
                "refreshToken": "old-refresh",
                "expiresAt": 1000
            }),
            &json!({
                "access_token": "new-access",
                "expires_in": 1800
            }),
        )
        .unwrap();

        assert_eq!(state["accessToken"], "new-access");
        assert_eq!(state["refreshToken"], "old-refresh");
        assert!(state["expiresAt"].as_u64().unwrap() > 1000);
    }

    #[test]
    fn hermes_auth_reads_qwen_cli_oauth_credentials() {
        let _guard = HERMES_AUTH_TEST_ENV_LOCK.lock().unwrap();
        let old_home = std::env::var_os("HOME");
        let old_userprofile = std::env::var_os("USERPROFILE");
        let old_base = std::env::var_os("HERMES_QWEN_BASE_URL");
        let dir = std::env::temp_dir().join(format!(
            "synthchat-qwen-cli-auth-{}",
            crate::models::new_id("test")
        ));
        let qwen_dir = dir.join(".qwen");
        std::fs::create_dir_all(&qwen_dir).unwrap();
        std::fs::write(
            qwen_dir.join("oauth_creds.json"),
            serde_json::json!({
                "access_token": "qwen-access-token",
                "refresh_token": "qwen-refresh-token",
                "expiry_date": 32503680000000u64
            })
            .to_string(),
        )
        .unwrap();
        std::env::set_var("HOME", &dir);
        std::env::set_var("USERPROFILE", &dir);
        std::env::set_var("HERMES_QWEN_BASE_URL", "https://qwen.example/v1/");

        let credential = resolve_hermes_runtime_credential(&provider("qwen-oauth")).unwrap();
        assert_eq!(credential.api_key, "qwen-access-token");
        assert_eq!(credential.source, "qwen-cli");
        assert_eq!(
            credential.base_url.as_deref(),
            Some("https://qwen.example/v1")
        );
        let status = hermes_external_credential_status(&provider("qwen-oauth")).unwrap();
        assert_eq!(status.provider_id, "qwen-oauth");
        assert_eq!(status.source, "qwen-cli");
        assert_eq!(status.state, "present");

        restore_env("HOME", old_home);
        restore_env("USERPROFILE", old_userprofile);
        restore_env("HERMES_QWEN_BASE_URL", old_base);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn claude_code_oauth_status_reads_env_setup_token() {
        let _guard = HERMES_AUTH_TEST_ENV_LOCK.lock().unwrap();
        let old_anthropic_token = std::env::var_os("ANTHROPIC_TOKEN");
        let old_claude_code_token = std::env::var_os("CLAUDE_CODE_OAUTH_TOKEN");
        std::env::set_var("ANTHROPIC_TOKEN", "sk-ant-oat-env-token");
        std::env::remove_var("CLAUDE_CODE_OAUTH_TOKEN");

        let credential = resolve_hermes_runtime_credential(&provider("claude-code")).unwrap();
        assert_eq!(credential.provider_id, "claude-code");
        assert_eq!(credential.api_key, "sk-ant-oat-env-token");
        assert_eq!(credential.source, "env:ANTHROPIC_TOKEN");

        let status = hermes_external_credential_status(&provider("claude-code")).unwrap();
        assert_eq!(status.provider_id, "claude-code");
        assert_eq!(status.source, "env:ANTHROPIC_TOKEN");
        assert_eq!(status.state, "present");

        let catalog = list_hermes_oauth_provider_statuses().unwrap();
        let claude_code = catalog["providers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|provider| provider["id"] == "claude-code")
            .unwrap();
        assert_eq!(claude_code["status"]["logged_in"], true);
        assert_eq!(claude_code["status"]["source"], "env:ANTHROPIC_TOKEN");
        assert_eq!(claude_code["status"]["has_refresh_token"], false);

        restore_env("ANTHROPIC_TOKEN", old_anthropic_token);
        restore_env("CLAUDE_CODE_OAUTH_TOKEN", old_claude_code_token);
    }

    #[test]
    fn claude_code_oauth_status_reads_local_credentials_file() {
        let _guard = HERMES_AUTH_TEST_ENV_LOCK.lock().unwrap();
        let old_home = std::env::var_os("HOME");
        let old_userprofile = std::env::var_os("USERPROFILE");
        let old_anthropic_token = std::env::var_os("ANTHROPIC_TOKEN");
        let old_claude_code_token = std::env::var_os("CLAUDE_CODE_OAUTH_TOKEN");
        let dir = std::env::temp_dir().join(format!(
            "synthchat-claude-code-auth-{}",
            crate::models::new_id("test")
        ));
        std::fs::create_dir_all(dir.join(".claude")).unwrap();
        std::fs::write(
            dir.join(".claude").join(".credentials.json"),
            serde_json::json!({
                "accessToken": "claude-code-access-token",
                "refreshToken": "claude-code-refresh-token",
                "expiresAt": "2999-01-01T00:00:00Z"
            })
            .to_string(),
        )
        .unwrap();
        std::env::set_var("HOME", &dir);
        std::env::set_var("USERPROFILE", &dir);
        std::env::remove_var("ANTHROPIC_TOKEN");
        std::env::remove_var("CLAUDE_CODE_OAUTH_TOKEN");

        let credential = resolve_hermes_runtime_credential(&provider("claude-code")).unwrap();
        assert_eq!(credential.api_key, "claude-code-access-token");
        assert_eq!(credential.source, "claude_code_cli");
        assert_eq!(
            credential.expires_at.as_deref(),
            Some("2999-01-01T00:00:00Z")
        );

        let status = hermes_external_credential_status(&provider("claude-code")).unwrap();
        assert_eq!(status.provider_id, "claude-code");
        assert_eq!(status.source, "claude_code_cli");
        assert_eq!(status.state, "present");

        restore_env("HOME", old_home);
        restore_env("USERPROFILE", old_userprofile);
        restore_env("ANTHROPIC_TOKEN", old_anthropic_token);
        restore_env("CLAUDE_CODE_OAUTH_TOKEN", old_claude_code_token);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn anthropic_dashboard_pkce_persists_hermes_file_and_pool_entry() {
        let _guard = HERMES_AUTH_TEST_ENV_LOCK.lock().unwrap();
        let old_hermes_home = std::env::var_os("HERMES_HOME");
        let old_home = std::env::var_os("HOME");
        let old_userprofile = std::env::var_os("USERPROFILE");
        let dir = std::env::temp_dir().join(format!(
            "synthchat-anthropic-dashboard-pkce-{}",
            crate::models::new_id("test")
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("HERMES_HOME", &dir);
        std::env::set_var("HOME", &dir);
        std::env::set_var("USERPROFILE", &dir);

        let start =
            anthropic_oauth_start_from_parts("state-1", "verifier-1", "challenge-1").unwrap();
        assert!(start
            .authorize_url
            .starts_with("https://claude.ai/oauth/authorize?"));
        assert!(start
            .authorize_url
            .contains("client_id=9d1c250a-e61b-44d9-88ed-5944d1962f5e"));
        assert!(start.authorize_url.contains("code_challenge=challenge-1"));
        assert_eq!(start.redirect_uri, ANTHROPIC_OAUTH_REDIRECT_URI);

        let (code, state) =
            anthropic_authorization_code_from_callback("auth-code#state-1", "state-1").unwrap();
        assert_eq!(code, "auth-code");
        assert_eq!(state.as_deref(), Some("state-1"));
        assert!(anthropic_authorization_code_from_callback("auth-code#wrong", "state-1").is_err());

        let state = anthropic_oauth_state_from_token_response(&json!({
            "access_token": "anthropic-access-token",
            "refresh_token": "anthropic-refresh-token",
            "expires_in": 3600
        }))
        .unwrap();
        let credential = persist_anthropic_oauth_credentials(&state).unwrap();
        assert_eq!(credential.provider_id, "anthropic");
        assert_eq!(credential.api_key, "anthropic-access-token");

        let file_state: Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join(".anthropic_oauth.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(file_state["accessToken"], "anthropic-access-token");
        assert_eq!(file_state["refreshToken"], "anthropic-refresh-token");

        let store: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("auth.json")).unwrap()).unwrap();
        let entry = &store["credential_pool"]["anthropic"][0];
        assert_eq!(entry["auth_type"], "oauth");
        assert_eq!(entry["source"], "manual:dashboard_pkce");
        assert_eq!(entry["access_token"], "anthropic-access-token");
        assert_eq!(entry["refresh_token"], "anthropic-refresh-token");

        let resolved = resolve_hermes_runtime_credential(&provider("anthropic")).unwrap();
        assert_eq!(resolved.api_key, "anthropic-access-token");

        restore_env("HERMES_HOME", old_hermes_home);
        restore_env("HOME", old_home);
        restore_env("USERPROFILE", old_userprofile);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn hermes_auth_reports_expired_qwen_cli_oauth_status() {
        let _guard = HERMES_AUTH_TEST_ENV_LOCK.lock().unwrap();
        let old_home = std::env::var_os("HOME");
        let old_userprofile = std::env::var_os("USERPROFILE");
        let dir = std::env::temp_dir().join(format!(
            "synthchat-qwen-cli-auth-expired-{}",
            crate::models::new_id("test")
        ));
        let qwen_dir = dir.join(".qwen");
        std::fs::create_dir_all(&qwen_dir).unwrap();
        std::fs::write(
            qwen_dir.join("oauth_creds.json"),
            serde_json::json!({
                "access_token": "qwen-expired-token",
                "refresh_token": "qwen-refresh-token",
                "expiry_date": 946684800000u64
            })
            .to_string(),
        )
        .unwrap();
        std::env::set_var("HOME", &dir);
        std::env::set_var("USERPROFILE", &dir);

        assert!(resolve_hermes_runtime_credential(&provider("qwen-oauth")).is_none());
        let status = hermes_external_credential_status(&provider("qwen-oauth")).unwrap();
        assert_eq!(status.state, "expired");
        assert!(status
            .note
            .as_deref()
            .unwrap_or_default()
            .contains("/auth refresh qwen-oauth"));

        restore_env("HOME", old_home);
        restore_env("USERPROFILE", old_userprofile);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn qwen_refresh_response_merges_hermes_cli_token_shape() {
        let existing = json!({
            "access_token": "old-access",
            "refresh_token": "old-refresh",
            "token_type": "Bearer",
            "resource_url": "portal.qwen.ai",
            "expiry_date": 1
        });
        let response = json!({
            "access_token": "new-access",
            "expires_in": "3600"
        });

        let refreshed = qwen_refreshed_tokens_from_response(&existing, &response).unwrap();

        assert_eq!(refreshed["access_token"], "new-access");
        assert_eq!(refreshed["refresh_token"], "old-refresh");
        assert_eq!(refreshed["token_type"], "Bearer");
        assert_eq!(refreshed["resource_url"], "portal.qwen.ai");
        assert!(refreshed["expiry_date"].as_u64().unwrap() > unix_now_seconds() * 1000);
    }

    #[test]
    fn minimax_refresh_response_merges_hermes_auth_state() {
        let existing = json!({
            "provider": "minimax-oauth",
            "portal_base_url": "https://api.minimax.io",
            "inference_base_url": "https://api.minimax.io/anthropic",
            "client_id": "client-1",
            "access_token": "old-access",
            "refresh_token": "old-refresh",
            "expires_at": "2000-01-01T00:00:00Z"
        });
        let response = json!({
            "status": "success",
            "access_token": "new-access",
            "expired_in": 3600
        });

        let refreshed = minimax_refreshed_state_from_response(&existing, &response).unwrap();

        assert_eq!(refreshed["access_token"], "new-access");
        assert_eq!(refreshed["refresh_token"], "old-refresh");
        assert_eq!(refreshed["provider"], "minimax-oauth");
        assert_eq!(refreshed["portal_base_url"], "https://api.minimax.io");
        assert_eq!(
            refreshed["inference_base_url"],
            "https://api.minimax.io/anthropic"
        );
        assert!(refreshed["expires_in"].as_u64().unwrap() > 0);
        assert!(refreshed["expires_at"].as_str().unwrap().contains('T'));
    }

    #[test]
    fn minimax_oauth_start_parses_hermes_response() {
        let start = minimax_oauth_start_from_response(
            &json!({
                "user_code": "MM-1234",
                "verification_uri": "https://api.minimax.io/oauth/verify",
                "expired_in": "900",
                "interval": "2000",
                "state": "state-1"
            }),
            "state-1",
            "verifier-1",
        )
        .unwrap();

        assert_eq!(start.user_code, "MM-1234");
        assert_eq!(
            start.verification_uri,
            "https://api.minimax.io/oauth/verify"
        );
        assert_eq!(start.code_verifier, "verifier-1");
        assert_eq!(start.expired_in, 900);
        assert_eq!(start.interval_ms, Some(2000));
    }

    #[test]
    fn minimax_oauth_start_rejects_state_mismatch() {
        let error = minimax_oauth_start_from_response(
            &json!({
                "user_code": "MM-1234",
                "verification_uri": "https://api.minimax.io/oauth/verify",
                "expired_in": 900,
                "state": "wrong"
            }),
            "expected",
            "verifier-1",
        )
        .unwrap_err();

        assert!(error.to_string().contains("state mismatch"));
    }

    #[test]
    fn minimax_oauth_token_response_builds_login_state() {
        let state = minimax_oauth_state_from_token_response(&json!({
            "status": "success",
            "access_token": "access-token",
            "refresh_token": "refresh-token",
            "expired_in": 3600,
            "token_type": "Bearer",
            "resource_url": "https://api.minimax.io"
        }))
        .unwrap();

        assert_eq!(state["provider"], "minimax-oauth");
        assert_eq!(state["access_token"], "access-token");
        assert_eq!(state["refresh_token"], "refresh-token");
        assert_eq!(state["scope"], MINIMAX_OAUTH_SCOPE);
        assert_eq!(state["portal_base_url"], MINIMAX_OAUTH_GLOBAL_BASE);
        assert_eq!(state["inference_base_url"], MINIMAX_OAUTH_GLOBAL_INFERENCE);
    }

    #[test]
    fn xai_refresh_response_merges_nested_token_state() {
        let existing = json!({
            "provider": "xai-oauth",
            "tokens": {
                "access_token": "old-access",
                "refresh_token": "old-refresh",
                "token_type": "Bearer"
            },
            "discovery": {
                "token_endpoint": "https://auth.x.ai/oauth/token"
            }
        });
        let response = json!({
            "access_token": "new-access",
            "expires_in": 3600
        });

        let refreshed = xai_refreshed_state_from_response(&existing, &response).unwrap();

        assert_eq!(refreshed["tokens"]["access_token"], "new-access");
        assert_eq!(refreshed["tokens"]["refresh_token"], "old-refresh");
        assert_eq!(refreshed["tokens"]["token_type"], "Bearer");
        assert_eq!(refreshed["tokens"]["expires_in"], 3600);
        assert_eq!(
            refreshed["discovery"]["token_endpoint"],
            "https://auth.x.ai/oauth/token"
        );
        assert!(refreshed["last_refresh"].as_str().unwrap().contains('T'));
    }

    #[test]
    fn credential_pool_provider_matches_hermes_xai_aliases() {
        for alias in [
            "xai",
            "x-ai",
            "x.ai",
            "grok",
            "x-ai-oauth",
            "grok-oauth",
            "xai-grok-oauth",
        ] {
            assert!(
                credential_pool_provider_matches("xai-oauth", alias),
                "{alias} should match xai-oauth"
            );
        }
    }

    #[test]
    fn credential_pool_provider_matches_hermes_model_provider_aliases() {
        for (provider_id, aliases) in [
            ("openrouter", vec!["openai"]),
            ("zai", vec!["glm", "z-ai", "z.ai", "zhipu"]),
            (
                "kimi-for-coding",
                vec!["kimi", "kimi-coding", "kimi-coding-cn", "moonshot"],
            ),
            ("stepfun", vec!["step", "stepfun-coding-plan"]),
            ("minimax-cn", vec!["minimax-china", "minimax_cn"]),
            (
                "alibaba",
                vec!["dashscope", "aliyun", "qwen", "alibaba-cloud"],
            ),
            (
                "alibaba-coding-plan",
                vec!["alibaba_coding", "alibaba-coding", "alibaba_coding_plan"],
            ),
            ("google-gemini-cli", vec!["gemini-cli", "gemini-oauth"]),
            ("github-copilot", vec!["copilot", "github"]),
            ("copilot-acp", vec!["github-copilot-acp"]),
            ("opencode", vec!["opencode-zen", "zen"]),
            ("opencode-go", vec!["go", "opencode-go-sub"]),
            ("kilo", vec!["kilocode", "kilo-code", "kilo-gateway"]),
            ("deepseek", vec!["deep-seek"]),
            ("huggingface", vec!["hf", "hugging-face", "huggingface-hub"]),
            ("novita", vec!["novita-ai", "novitaai"]),
            ("xiaomi", vec!["mimo", "xiaomi-mimo"]),
            (
                "tencent-tokenhub",
                vec!["tencent", "tokenhub", "tencent-cloud", "tencentmaas"],
            ),
            (
                "bedrock",
                vec!["aws", "aws-bedrock", "amazon-bedrock", "amazon"],
            ),
            (
                "nvidia",
                vec!["nim", "nvidia-nim", "build-nvidia", "nemotron"],
            ),
            ("arcee", vec!["arcee-ai", "arceeai"]),
            ("gmi", vec!["gmi-cloud", "gmicloud"]),
            ("lmstudio", vec!["lm_studio"]),
            ("custom", vec!["ollama"]),
            ("local", vec!["vllm", "llamacpp", "llama.cpp", "llama-cpp"]),
        ] {
            for alias in aliases {
                assert!(
                    credential_pool_provider_matches(provider_id, alias),
                    "{alias} should match {provider_id}"
                );
            }
        }
    }

    #[test]
    fn xai_oauth_start_builds_authorize_url() {
        let start = xai_oauth_start_from_parts(
            "https://auth.x.ai/oauth/authorize",
            XAI_OAUTH_REDIRECT_URI,
            "state-1",
            "verifier-1",
            "challenge-1",
        )
        .unwrap();

        assert_eq!(start.state, "state-1");
        assert_eq!(start.code_verifier, "verifier-1");
        assert_eq!(start.code_challenge, "challenge-1");
        assert!(start.authorize_url.contains("client_id="));
        assert!(start.authorize_url.contains("code_challenge=challenge-1"));
        assert!(start.authorize_url.contains("plan=generic"));
        assert!(start.authorize_url.contains("referrer=hermes-agent"));
    }

    #[test]
    fn xai_authorization_code_parses_callback_and_bare_code() {
        let code = xai_authorization_code_from_callback(
            "http://127.0.0.1:56121/callback?code=auth-code&state=state-1",
            "state-1",
        )
        .unwrap();
        assert_eq!(code, "auth-code");

        let bare = xai_authorization_code_from_callback("bare-code", "state-1").unwrap();
        assert_eq!(bare, "bare-code");
    }

    #[test]
    fn xai_authorization_code_rejects_state_mismatch() {
        let error = xai_authorization_code_from_callback(
            "http://127.0.0.1:56121/callback?code=auth-code&state=wrong",
            "state-1",
        )
        .unwrap_err();

        assert!(error.to_string().contains("state mismatch"));
    }

    #[test]
    fn xai_oauth_token_response_builds_login_state() {
        let state = xai_oauth_state_from_token_response(&json!({
            "access_token": "access-token",
            "refresh_token": "refresh-token",
            "id_token": "id-token",
            "expires_in": 3600,
            "token_type": "Bearer"
        }))
        .unwrap();

        assert_eq!(state["tokens"]["access_token"], "access-token");
        assert_eq!(state["tokens"]["refresh_token"], "refresh-token");
        assert_eq!(state["auth_mode"], "oauth_pkce");
        assert_eq!(state["redirect_uri"], XAI_OAUTH_REDIRECT_URI);
        assert_eq!(state["base_url"], XAI_OAUTH_BASE_URL);
    }

    #[test]
    fn codex_refresh_response_merges_nested_token_state() {
        let existing = json!({
            "tokens": {
                "access_token": "old-access",
                "refresh_token": "old-refresh",
                "token_type": "Bearer"
            },
            "last_refresh": "2000-01-01T00:00:00Z"
        });
        let response = json!({
            "access_token": "new-access",
            "expires_in": 3600
        });

        let refreshed = codex_refreshed_state_from_response(&existing, &response).unwrap();

        assert_eq!(refreshed["tokens"]["access_token"], "new-access");
        assert_eq!(refreshed["tokens"]["refresh_token"], "old-refresh");
        assert_eq!(refreshed["tokens"]["token_type"], "Bearer");
        assert_eq!(refreshed["tokens"]["expires_in"], 3600);
        assert_eq!(refreshed["auth_mode"], "chatgpt");
        assert!(refreshed["last_refresh"].as_str().unwrap().contains('T'));
    }

    #[test]
    fn codex_refresh_syncs_device_code_pool_entries() {
        let refreshed = json!({
            "tokens": {
                "access_token": "new-access",
                "refresh_token": "new-refresh"
            },
            "last_refresh": "2026-01-01T00:00:00Z"
        });
        let mut store = json!({
            "credential_pool": {
                "openai-codex": [{
                    "label": "default",
                    "source": "device_code",
                    "access_token": "old-access",
                    "refresh_token": "old-refresh",
                    "last_status": "failed",
                    "last_error_code": "token_invalidated",
                    "last_error_reset_at": 9999999999u64
                }, {
                    "label": "manual",
                    "source": "manual:device_code",
                    "access_token": "manual-old",
                    "refresh_token": "manual-refresh"
                }, {
                    "label": "api-key",
                    "source": "manual:api_key",
                    "access_token": "api-key-token"
                }]
            }
        });

        sync_codex_pool_entries(&mut store, &refreshed);

        let entries = store["credential_pool"]["openai-codex"].as_array().unwrap();
        assert_eq!(entries[0]["access_token"], "new-access");
        assert_eq!(entries[0]["refresh_token"], "new-refresh");
        assert_eq!(entries[0]["last_refresh"], "2026-01-01T00:00:00Z");
        assert!(entries[0]["last_status"].is_null());
        assert!(entries[0]["last_error_code"].is_null());
        assert!(entries[0]["last_error_reset_at"].is_null());
        assert_eq!(entries[1]["access_token"], "new-access");
        assert_eq!(entries[1]["refresh_token"], "new-refresh");
        assert_eq!(entries[2]["access_token"], "api-key-token");
    }

    #[test]
    fn codex_device_code_start_parses_hermes_response() {
        let start = codex_device_code_start_from_response(&json!({
            "user_code": "ABCD-EFGH",
            "device_auth_id": "device-auth-id",
            "interval": 2
        }))
        .unwrap();

        assert_eq!(start.user_code, "ABCD-EFGH");
        assert_eq!(start.device_auth_id, "device-auth-id");
        assert_eq!(
            start.verification_uri,
            "https://auth.openai.com/codex/device"
        );
        assert_eq!(start.interval_seconds, 3);
    }

    #[test]
    fn codex_device_code_token_response_builds_login_state() {
        let state = codex_device_code_state_from_token_response(&json!({
            "access_token": "access-token",
            "refresh_token": "refresh-token",
            "token_type": "Bearer",
            "expires_in": 3600
        }))
        .unwrap();

        assert_eq!(state["tokens"]["access_token"], "access-token");
        assert_eq!(state["tokens"]["refresh_token"], "refresh-token");
        assert_eq!(state["base_url"], DEFAULT_CODEX_BASE_URL);
        assert_eq!(state["auth_mode"], "chatgpt");
        assert_eq!(state["source"], "device-code");
    }

    #[test]
    fn codex_device_code_login_upserts_pool_entry() {
        let state = json!({
            "tokens": {
                "access_token": "access-token",
                "refresh_token": "refresh-token"
            },
            "last_refresh": "2026-01-01T00:00:00Z"
        });
        let mut store = json!({});

        upsert_codex_device_code_pool_entry(&mut store, &state);

        let entries = store["credential_pool"]["openai-codex"].as_array().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["source"], "device_code");
        assert_eq!(entries[0]["access_token"], "access-token");
        assert_eq!(entries[0]["refresh_token"], "refresh-token");
    }

    #[test]
    fn google_gemini_refresh_response_preserves_packed_project_ids() {
        let existing = json!({
            "access": "old-access",
            "refresh": "old-refresh|project-1|managed-1",
            "expires": 1,
            "email": "user@example.com"
        });
        let response = json!({
            "access_token": "new-access",
            "expires_in": "3600"
        });

        let refreshed =
            google_gemini_refreshed_credentials_from_response(&existing, &response).unwrap();

        assert_eq!(refreshed["access"], "new-access");
        assert_eq!(refreshed["refresh"], "old-refresh|project-1|managed-1");
        assert_eq!(refreshed["email"], "user@example.com");
        assert!(refreshed["expires"].as_u64().unwrap() > unix_now_seconds() * 1000);
    }

    #[test]
    fn google_gemini_refresh_response_uses_rotated_refresh_token() {
        let existing = json!({
            "access": "old-access",
            "refresh": "old-refresh|project-1|managed-1",
            "expires": 1
        });
        let response = json!({
            "access_token": "new-access",
            "refresh_token": "rotated-refresh",
            "expires_in": 3600
        });

        let refreshed =
            google_gemini_refreshed_credentials_from_response(&existing, &response).unwrap();

        assert_eq!(refreshed["access"], "new-access");
        assert_eq!(refreshed["refresh"], "rotated-refresh|project-1|managed-1");
    }

    #[test]
    fn google_gemini_oauth_start_builds_authorize_url() {
        let _guard = HERMES_AUTH_TEST_ENV_LOCK.lock().unwrap();
        let old_client_id = std::env::var_os("HERMES_GEMINI_CLIENT_ID");
        std::env::set_var(
            "HERMES_GEMINI_CLIENT_ID",
            "desktop-client-id.apps.googleusercontent.com",
        );
        let start =
            google_gemini_oauth_start_from_parts("state-1", "verifier-1", "challenge-1").unwrap();

        assert_eq!(start.state, "state-1");
        assert_eq!(start.code_verifier, "verifier-1");
        assert_eq!(start.redirect_uri, GOOGLE_GEMINI_OAUTH_REDIRECT_URI);
        assert!(start.authorize_url.contains("accounts.google.com"));
        assert!(start.authorize_url.contains("code_challenge=challenge-1"));
        assert!(start.authorize_url.contains("access_type=offline"));
        assert!(start.authorize_url.contains("prompt=consent"));
        restore_env("HERMES_GEMINI_CLIENT_ID", old_client_id);
    }

    #[test]
    fn google_gemini_token_response_builds_credentials_file_shape() {
        let creds = google_gemini_credentials_from_token_response(&json!({
            "access_token": "access-token",
            "refresh_token": "refresh-token",
            "expires_in": 3600,
            "email": "user@example.com"
        }))
        .unwrap();

        assert_eq!(creds["access"], "access-token");
        assert_eq!(creds["refresh"], "refresh-token");
        assert_eq!(creds["email"], "user@example.com");
        assert!(creds["expires"].as_u64().unwrap() > unix_now_seconds() * 1000);
    }

    #[test]
    fn nous_refresh_response_selects_invoke_jwt_as_agent_key() {
        let access_token = test_jwt(json!({
            "scope": "profile inference:invoke",
            "exp": unix_now_seconds() + 3600
        }));
        let existing = json!({
            "access_token": "old-access",
            "refresh_token": "old-refresh",
            "client_id": "hermes-cli",
            "portal_base_url": "https://portal.nousresearch.com",
            "inference_base_url": "https://inference-api.nousresearch.com/v1",
            "scope": "inference:invoke"
        });
        let response = json!({
            "access_token": access_token,
            "refresh_token": "new-refresh",
            "expires_in": 3600,
            "scope": "inference:invoke",
            "inference_base_url": "https://inference-api.nousresearch.com/v1/"
        });

        let refreshed = nous_refreshed_state_from_response(&existing, &response).unwrap();

        assert_eq!(refreshed["refresh_token"], "new-refresh");
        assert_eq!(refreshed["agent_key"], refreshed["access_token"]);
        assert_eq!(refreshed["agent_key_id"], Value::Null);
        assert_eq!(refreshed["agent_key_reused"], false);
        assert_eq!(
            refreshed["inference_base_url"],
            "https://inference-api.nousresearch.com/v1"
        );
        assert!(refreshed["agent_key_expires_at"]
            .as_str()
            .unwrap()
            .contains('T'));
    }

    #[test]
    fn nous_refresh_syncs_device_code_pool_entries() {
        let access_token = test_jwt(json!({
            "scope": "inference:invoke",
            "exp": unix_now_seconds() + 3600
        }));
        let refreshed = nous_refreshed_state_from_response(
            &json!({
                "access_token": "old-access",
                "refresh_token": "old-refresh",
                "scope": "inference:invoke"
            }),
            &json!({
                "access_token": access_token,
                "refresh_token": "new-refresh",
                "expires_in": 3600,
                "scope": "inference:invoke"
            }),
        )
        .unwrap();
        let mut store = json!({
            "credential_pool": {
                "nous": [{
                    "label": "oauth",
                    "source": "device_code",
                    "access_token": "old-access",
                    "refresh_token": "old-refresh",
                    "last_status": "failed",
                    "last_error_code": "invalid_token"
                }, {
                    "label": "manual-key",
                    "source": "manual:api_key",
                    "agent_key": "manual-key"
                }]
            }
        });

        sync_nous_pool_entries(&mut store, &refreshed);

        let entries = store["credential_pool"]["nous"].as_array().unwrap();
        assert_eq!(entries[0]["access_token"], refreshed["access_token"]);
        assert_eq!(entries[0]["refresh_token"], "new-refresh");
        assert_eq!(entries[0]["agent_key"], refreshed["agent_key"]);
        assert!(entries[0]["last_status"].is_null());
        assert!(entries[0]["last_error_code"].is_null());
        assert_eq!(entries[1]["agent_key"], "manual-key");
    }

    #[test]
    fn nous_device_code_start_parses_hermes_response() {
        let start = nous_device_code_start_from_response(&json!({
            "device_code": "device-code",
            "user_code": "ABCD-EFGH",
            "verification_uri": "https://portal.nousresearch.com/device",
            "verification_uri_complete": "https://portal.nousresearch.com/device?user_code=ABCD-EFGH",
            "expires_in": "900",
            "interval": "5"
        }))
        .unwrap();

        assert_eq!(start.device_code, "device-code");
        assert_eq!(start.user_code, "ABCD-EFGH");
        assert_eq!(
            start.verification_uri_complete,
            "https://portal.nousresearch.com/device?user_code=ABCD-EFGH"
        );
        assert_eq!(start.expires_in, 900);
        assert_eq!(start.interval_seconds, 5);
    }

    #[test]
    fn nous_device_code_token_response_builds_agent_key_state() {
        let access_token = test_jwt(json!({
            "scope": "inference:invoke",
            "exp": unix_now_seconds() + 3600
        }));
        let state = nous_device_code_state_from_token_response(&json!({
            "access_token": access_token,
            "refresh_token": "refresh-token",
            "expires_in": 3600,
            "scope": "inference:invoke",
            "inference_base_url": "https://inference-api.nousresearch.com/v1/"
        }))
        .unwrap();

        assert_eq!(state["refresh_token"], "refresh-token");
        assert_eq!(state["agent_key"], state["access_token"]);
        assert_eq!(
            state["inference_base_url"],
            "https://inference-api.nousresearch.com/v1"
        );
        assert_eq!(state["client_id"], DEFAULT_NOUS_CLIENT_ID);
    }

    #[test]
    fn nous_device_code_login_upserts_pool_entry() {
        let access_token = test_jwt(json!({
            "scope": "inference:invoke",
            "exp": unix_now_seconds() + 3600
        }));
        let mut state = json!({
            "access_token": access_token,
            "refresh_token": "refresh-token",
            "scope": "inference:invoke",
            "inference_base_url": "https://inference-api.nousresearch.com/v1",
            "portal_base_url": "https://portal.nousresearch.com",
            "client_id": DEFAULT_NOUS_CLIENT_ID,
            "expires_at": "2999-01-01T00:00:00Z",
            "expires_in": 3600
        });
        set_nous_agent_key_from_invoke_jwt(&mut state);
        let mut store = json!({});

        upsert_nous_device_code_pool_entry(&mut store, &state);

        let entries = store["credential_pool"]["nous"].as_array().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["source"], "device_code");
        assert_eq!(entries[0]["agent_key"], state["agent_key"]);
        assert_eq!(entries[0]["refresh_token"], "refresh-token");
    }

    #[test]
    fn nous_shared_state_omits_runtime_agent_key() {
        let shared = nous_shared_state_from_provider_state(&json!({
            "access_token": "access-token",
            "refresh_token": "refresh-token",
            "token_type": "Bearer",
            "scope": "inference:invoke",
            "client_id": "hermes-cli",
            "portal_base_url": "https://portal.nousresearch.com",
            "inference_base_url": "https://inference-api.nousresearch.com/v1",
            "obtained_at": "2026-01-01T00:00:00Z",
            "expires_at": "2026-01-01T01:00:00Z",
            "agent_key": "runtime-agent-key"
        }))
        .unwrap();

        assert_eq!(shared["_schema"], 1);
        assert_eq!(shared["access_token"], "access-token");
        assert_eq!(shared["refresh_token"], "refresh-token");
        assert!(shared.get("agent_key").is_none());
        assert!(shared["updated_at"].as_str().unwrap().contains('T'));
    }

    #[test]
    fn nous_shared_state_writes_to_configured_shared_auth_dir() {
        let _guard = HERMES_AUTH_TEST_ENV_LOCK.lock().unwrap();
        let old_shared = std::env::var_os("HERMES_SHARED_AUTH_DIR");
        let dir = std::env::temp_dir().join(format!(
            "synthchat-nous-shared-auth-{}",
            crate::models::new_id("test")
        ));
        std::env::set_var("HERMES_SHARED_AUTH_DIR", &dir);

        write_shared_nous_state(&json!({
            "access_token": "access-token",
            "refresh_token": "refresh-token",
            "scope": "inference:invoke"
        }));

        let payload = std::fs::read_to_string(dir.join("nous_auth.json")).unwrap();
        let shared: Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(shared["access_token"], "access-token");
        assert_eq!(shared["refresh_token"], "refresh-token");
        assert_eq!(shared["client_id"], DEFAULT_NOUS_CLIENT_ID);
        assert_eq!(shared["portal_base_url"], DEFAULT_NOUS_PORTAL_URL);

        restore_env("HERMES_SHARED_AUTH_DIR", old_shared);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn nous_shared_state_merges_changed_refresh_token() {
        let mut state = json!({
            "access_token": "local-access",
            "refresh_token": "local-refresh",
            "token_type": "Bearer",
            "scope": "inference:invoke",
            "client_id": "hermes-cli",
            "portal_base_url": "https://old.portal.example",
            "inference_base_url": "https://old.inference.example/v1",
            "obtained_at": "2026-01-01T00:00:00Z",
            "expires_at": "2026-01-01T01:00:00Z",
            "agent_key": "local-runtime-key"
        });
        let shared = json!({
            "access_token": "shared-access",
            "refresh_token": "shared-refresh",
            "token_type": "Bearer",
            "scope": "inference:invoke",
            "client_id": "hermes-cli",
            "portal_base_url": "https://portal.nousresearch.com",
            "inference_base_url": "https://inference-api.nousresearch.com/v1",
            "obtained_at": "2026-01-01T00:30:00Z",
            "expires_at": "2026-01-01T02:00:00Z",
            "agent_key": "shared-runtime-key"
        });

        assert!(merge_shared_nous_oauth_state_value(&mut state, &shared));

        assert_eq!(state["access_token"], "shared-access");
        assert_eq!(state["refresh_token"], "shared-refresh");
        assert_eq!(state["portal_base_url"], "https://portal.nousresearch.com");
        assert_eq!(state["expires_at"], "2026-01-01T02:00:00Z");
        assert_eq!(state["agent_key"], "local-runtime-key");
    }

    #[test]
    fn nous_shared_state_ignores_missing_refresh_token() {
        let mut state = json!({
            "access_token": "local-access",
            "refresh_token": "local-refresh",
            "expires_at": "2026-01-01T01:00:00Z"
        });
        let shared = json!({
            "access_token": "shared-access",
            "expires_at": "2026-01-01T02:00:00Z"
        });

        assert!(!merge_shared_nous_oauth_state_value(&mut state, &shared));
        assert_eq!(state["access_token"], "local-access");
        assert_eq!(state["refresh_token"], "local-refresh");
    }

    #[test]
    fn nous_shared_state_reads_and_merges_configured_store() {
        let _guard = HERMES_AUTH_TEST_ENV_LOCK.lock().unwrap();
        let old_shared = std::env::var_os("HERMES_SHARED_AUTH_DIR");
        let dir = std::env::temp_dir().join(format!(
            "synthchat-nous-shared-auth-read-{}",
            crate::models::new_id("test")
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("HERMES_SHARED_AUTH_DIR", &dir);
        std::fs::write(
            dir.join("nous_auth.json"),
            serde_json::to_string_pretty(&json!({
                "access_token": "shared-access",
                "refresh_token": "shared-refresh",
                "expires_at": "2026-01-01T02:00:00Z"
            }))
            .unwrap(),
        )
        .unwrap();
        let mut state = json!({
            "access_token": "local-access",
            "refresh_token": "local-refresh",
            "expires_at": "2026-01-01T01:00:00Z"
        });

        assert!(merge_shared_nous_oauth_state(&mut state));
        assert_eq!(state["access_token"], "shared-access");
        assert_eq!(state["refresh_token"], "shared-refresh");

        restore_env("HERMES_SHARED_AUTH_DIR", old_shared);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn nous_terminal_refresh_error_quarantines_oauth_state() {
        let error = nous_refresh_error(
            400,
            r#"{"error":"refresh_token_reused","error_description":"token already used"}"#,
        );
        let mut state = json!({
            "access_token": "access-token",
            "refresh_token": "refresh-token",
            "expires_at": "2026-01-01T01:00:00Z",
            "agent_key": "runtime-key",
            "portal_base_url": "https://portal.nousresearch.com",
            "client_id": "hermes-cli"
        });

        assert_eq!(
            terminal_nous_refresh_error_reason(&error),
            Some("oauth_refresh_terminal_failure")
        );
        quarantine_nous_oauth_state(&mut state, &error, "oauth_refresh_terminal_failure");

        assert!(state.get("access_token").is_none());
        assert!(state.get("refresh_token").is_none());
        assert!(state.get("agent_key").is_none());
        assert_eq!(state["portal_base_url"], "https://portal.nousresearch.com");
        assert_eq!(state["last_auth_error"]["code"], "refresh_token_reused");
        assert_eq!(state["last_auth_error"]["relogin_required"], true);
    }

    #[test]
    fn nous_quarantine_removes_device_code_pool_entries_only() {
        let error = nous_refresh_error(
            401,
            r#"{"error":"invalid_grant","error_description":"revoked"}"#,
        );
        let mut store = json!({
            "credential_pool": {
                "nous": [{
                    "label": "oauth",
                    "source": "device_code",
                    "refresh_token": "dead-refresh"
                }, {
                    "label": "manual-oauth",
                    "source": "manual:device_code",
                    "refresh_token": "dead-refresh"
                }, {
                    "label": "manual-key",
                    "source": "manual:api_key",
                    "agent_key": "manual-key"
                }]
            }
        });

        assert!(quarantine_nous_pool_entries(
            &mut store,
            &error,
            "oauth_refresh_terminal_failure"
        ));

        let entries = store["credential_pool"]["nous"].as_array().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["source"], "manual:api_key");
        assert_eq!(entries[0]["agent_key"], "manual-key");
    }

    #[test]
    fn nous_clear_shared_state_removes_configured_store() {
        let _guard = HERMES_AUTH_TEST_ENV_LOCK.lock().unwrap();
        let old_shared = std::env::var_os("HERMES_SHARED_AUTH_DIR");
        let dir = std::env::temp_dir().join(format!(
            "synthchat-nous-shared-auth-clear-{}",
            crate::models::new_id("test")
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("HERMES_SHARED_AUTH_DIR", &dir);
        let path = dir.join("nous_auth.json");
        std::fs::write(&path, "{}").unwrap();

        clear_shared_nous_state();

        assert!(!path.exists());
        restore_env("HERMES_SHARED_AUTH_DIR", old_shared);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn xai_oauth_endpoint_validation_rejects_non_xai_hosts() {
        assert!(validate_xai_oauth_endpoint("https://auth.x.ai/oauth/token").is_ok());
        assert!(validate_xai_oauth_endpoint("https://evil.example/oauth/token").is_err());
        assert!(validate_xai_oauth_endpoint("http://auth.x.ai/oauth/token").is_err());
    }

    #[test]
    fn hermes_auth_reads_google_gemini_oauth_runtime_credentials() {
        let _guard = HERMES_AUTH_TEST_ENV_LOCK.lock().unwrap();
        let old_hermes_home = std::env::var_os("HERMES_HOME");
        let dir = std::env::temp_dir().join(format!(
            "synthchat-google-oauth-{}",
            crate::models::new_id("test")
        ));
        let auth_dir = dir.join("auth");
        std::fs::create_dir_all(&auth_dir).unwrap();
        std::fs::write(
            auth_dir.join("google_oauth.json"),
            serde_json::json!({
                "access": "google-access-token",
                "refresh": "google-refresh-token|project-1|managed-1",
                "expires": 32503680000000u64,
                "email": "user@example.com"
            })
            .to_string(),
        )
        .unwrap();
        std::env::set_var("HERMES_HOME", &dir);
        let provider = provider("google-gemini-cli");

        let status = hermes_external_credential_status(&provider).unwrap();
        assert_eq!(status.provider_id, "google-gemini-cli");
        assert_eq!(status.source, "google-oauth");
        assert_eq!(status.state, "present");
        let credential = resolve_hermes_runtime_credential(&provider).unwrap();
        assert_eq!(credential.api_key, "google-access-token");
        assert_eq!(
            credential.base_url.as_deref(),
            Some("cloudcode-pa://google")
        );

        restore_env("HERMES_HOME", old_hermes_home);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn hermes_auth_store_ignores_expired_tokens() {
        let _guard = HERMES_AUTH_TEST_ENV_LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join(format!(
            "synthchat-hermes-auth-expired-{}",
            crate::models::new_id("test")
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("auth.json"),
            serde_json::json!({
                "providers": {
                    "minimax-oauth": {
                        "access_token": "expired-token",
                        "expires_at": "2000-01-01T00:00:00Z"
                    }
                }
            })
            .to_string(),
        )
        .unwrap();
        std::env::set_var("HERMES_HOME", &dir);

        assert!(resolve_hermes_runtime_credential(&provider("minimax-oauth")).is_none());
        let status = hermes_auth_store_credential_status(&provider("minimax-oauth")).unwrap();
        assert_eq!(status.provider_id, "minimax-oauth");
        assert_eq!(status.source, "hermes-auth:minimax-oauth");
        assert_eq!(status.state, "expired");
        assert_eq!(status.expires_at.as_deref(), Some("2000-01-01T00:00:00Z"));

        std::env::remove_var("HERMES_HOME");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn add_credential_pool_entry_creates_redacted_api_key_entry() {
        let _guard = HERMES_AUTH_TEST_ENV_LOCK.lock().unwrap();
        let old_hermes_home = std::env::var_os("HERMES_HOME");
        let dir = std::env::temp_dir().join(format!(
            "synthchat-hermes-auth-add-pool-{}",
            crate::models::new_id("test")
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("HERMES_HOME", &dir);

        let status = add_hermes_credential_pool_entry(
            "openai_codex",
            Some("primary"),
            "secret-token",
            Some("https://example.test/v1/"),
            None,
            None,
        )
        .unwrap();

        assert_eq!(status.provider_id, "openai-codex");
        assert_eq!(status.index, 1);
        assert_eq!(status.label, "primary");
        assert_eq!(status.auth_type.as_deref(), Some("api_key"));
        assert_eq!(status.source.as_deref(), Some("manual"));
        assert_eq!(status.state, "present");
        assert_eq!(status.base_url.as_deref(), Some("https://example.test/v1"));
        let rendered = serde_json::to_string(&status).unwrap();
        assert!(!rendered.contains("secret-token"));

        let store = read_primary_hermes_auth_store().unwrap();
        let entry = &store["credential_pool"]["openai-codex"][0];
        assert_eq!(entry["access_token"], "secret-token");
        assert_eq!(entry["priority"], 0);
        let credential = resolve_hermes_runtime_credential(&provider("openai-codex")).unwrap();
        assert_eq!(credential.api_key, "secret-token");
        assert_eq!(credential.source, "hermes-pool:openai-codex:primary");

        restore_env("HERMES_HOME", old_hermes_home);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn add_credential_pool_entry_appends_to_entries_shape_and_normalizes_priority() {
        let _guard = HERMES_AUTH_TEST_ENV_LOCK.lock().unwrap();
        let old_hermes_home = std::env::var_os("HERMES_HOME");
        let dir = std::env::temp_dir().join(format!(
            "synthchat-hermes-auth-add-pool-entries-{}",
            crate::models::new_id("test")
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("auth.json"),
            json!({
                "credential_pool": {
                    "openrouter": {
                        "entries": [{
                            "id": "existing",
                            "label": "existing",
                            "auth_type": "api_key",
                            "priority": 9,
                            "source": "manual",
                            "access_token": "old-token"
                        }]
                    }
                }
            })
            .to_string(),
        )
        .unwrap();
        std::env::set_var("HERMES_HOME", &dir);

        let status = add_hermes_credential_pool_entry(
            "openrouter",
            None,
            "new-token",
            None,
            Some("api-key"),
            Some("2999-01-01T00:00:00Z"),
        )
        .unwrap();

        assert_eq!(status.index, 2);
        assert_eq!(status.label, "api-key-2");
        assert_eq!(status.expires_at.as_deref(), Some("2999-01-01T00:00:00Z"));
        let store = read_primary_hermes_auth_store().unwrap();
        let entries = store["credential_pool"]["openrouter"]["entries"]
            .as_array()
            .unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0]["priority"], 0);
        assert_eq!(entries[1]["priority"], 1);
        assert_eq!(entries[1]["access_token"], "new-token");

        restore_env("HERMES_HOME", old_hermes_home);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn add_credential_pool_entry_rejects_oauth_payloads() {
        let _guard = HERMES_AUTH_TEST_ENV_LOCK.lock().unwrap();
        let old_hermes_home = std::env::var_os("HERMES_HOME");
        let dir = std::env::temp_dir().join(format!(
            "synthchat-hermes-auth-add-pool-oauth-{}",
            crate::models::new_id("test")
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("HERMES_HOME", &dir);

        let error = add_hermes_credential_pool_entry(
            "openai-codex",
            None,
            "secret-token",
            None,
            Some("oauth"),
            None,
        )
        .unwrap_err();

        assert!(error.to_string().contains("only api_key"));
        assert!(!dir.join("auth.json").exists());

        restore_env("HERMES_HOME", old_hermes_home);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn hermes_credential_pool_round_robin_rotates_priority() {
        let _guard = HERMES_AUTH_TEST_ENV_LOCK.lock().unwrap();
        let old_hermes_home = std::env::var_os("HERMES_HOME");
        let old_strategy = std::env::var_os("SYNTHCHAT_LLM_CREDENTIAL_POOL_STRATEGY");
        let dir = std::env::temp_dir().join(format!(
            "synthchat-hermes-auth-round-robin-{}",
            crate::models::new_id("test")
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("auth.json"),
            json!({
                "credential_pool": {
                    "openrouter": [{
                        "label": "first",
                        "auth_type": "api_key",
                        "priority": 0,
                        "source": "manual",
                        "access_token": "first-token"
                    }, {
                        "label": "second",
                        "auth_type": "api_key",
                        "priority": 1,
                        "source": "manual",
                        "access_token": "second-token"
                    }]
                }
            })
            .to_string(),
        )
        .unwrap();
        std::env::set_var("HERMES_HOME", &dir);
        std::env::set_var("SYNTHCHAT_LLM_CREDENTIAL_POOL_STRATEGY", "round_robin");

        let first = resolve_hermes_runtime_credential(&provider("openrouter")).unwrap();
        let second = resolve_hermes_runtime_credential(&provider("openrouter")).unwrap();

        assert_eq!(first.api_key, "first-token");
        assert_eq!(second.api_key, "second-token");
        let store = read_primary_hermes_auth_store().unwrap();
        let entries = store["credential_pool"]["openrouter"].as_array().unwrap();
        assert_eq!(entries[0]["label"], "first");
        assert_eq!(entries[0]["priority"], 0);
        assert_eq!(entries[1]["label"], "second");
        assert_eq!(entries[1]["priority"], 1);

        restore_env("SYNTHCHAT_LLM_CREDENTIAL_POOL_STRATEGY", old_strategy);
        restore_env("HERMES_HOME", old_hermes_home);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn hermes_credential_pool_least_used_increments_request_count() {
        let _guard = HERMES_AUTH_TEST_ENV_LOCK.lock().unwrap();
        let old_hermes_home = std::env::var_os("HERMES_HOME");
        let old_strategy = std::env::var_os("SYNTHCHAT_LLM_CREDENTIAL_POOL_STRATEGY");
        let dir = std::env::temp_dir().join(format!(
            "synthchat-hermes-auth-least-used-{}",
            crate::models::new_id("test")
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("auth.json"),
            json!({
                "credential_pool": {
                    "openrouter": [{
                        "label": "busy",
                        "auth_type": "api_key",
                        "priority": 0,
                        "source": "manual",
                        "access_token": "busy-token",
                        "request_count": 9
                    }, {
                        "label": "idle",
                        "auth_type": "api_key",
                        "priority": 1,
                        "source": "manual",
                        "access_token": "idle-token",
                        "request_count": 1
                    }]
                }
            })
            .to_string(),
        )
        .unwrap();
        std::env::set_var("HERMES_HOME", &dir);
        std::env::set_var("SYNTHCHAT_LLM_CREDENTIAL_POOL_STRATEGY", "least_used");

        let credential = resolve_hermes_runtime_credential(&provider("openrouter")).unwrap();

        assert_eq!(credential.api_key, "idle-token");
        assert_eq!(credential.source, "hermes-pool:openrouter:idle");
        let store = read_primary_hermes_auth_store().unwrap();
        let entries = store["credential_pool"]["openrouter"].as_array().unwrap();
        assert_eq!(entries[0]["request_count"], 9);
        assert_eq!(entries[1]["request_count"], 2);

        restore_env("SYNTHCHAT_LLM_CREDENTIAL_POOL_STRATEGY", old_strategy);
        restore_env("HERMES_HOME", old_hermes_home);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn hermes_credential_pool_failure_marks_entry_exhausted_and_skips_it() {
        let _guard = HERMES_AUTH_TEST_ENV_LOCK.lock().unwrap();
        let old_hermes_home = std::env::var_os("HERMES_HOME");
        let dir = std::env::temp_dir().join(format!(
            "synthchat-hermes-pool-failure-{}",
            crate::models::new_id("test")
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("auth.json"),
            json!({
                "credential_pool": {
                    "openrouter": [{
                        "label": "primary",
                        "access_token": "sk-primary",
                        "priority": 0
                    }, {
                        "label": "backup",
                        "access_token": "sk-backup",
                        "priority": 1
                    }]
                }
            })
            .to_string(),
        )
        .unwrap();
        std::env::set_var("HERMES_HOME", &dir);

        let marked = mark_hermes_credential_pool_failure(
            &provider("openrouter"),
            "rate_limit",
            "provider returned 429",
        )
        .unwrap()
        .unwrap();
        assert_eq!(marked.source, "hermes-pool:openrouter:primary");
        assert_eq!(marked.state, "cooldown");

        let store = read_primary_hermes_auth_store().unwrap();
        let primary = &store["credential_pool"]["openrouter"][0];
        assert_eq!(primary["last_status"], "exhausted");
        assert_eq!(primary["last_error_code"], "rate_limit");
        assert_eq!(primary["last_error_reason"], "rate_limit");
        assert_eq!(primary["last_error_message"], "provider returned 429");
        assert!(primary["last_error_reset_at"].as_u64().unwrap() > unix_now_seconds());

        let credential = resolve_hermes_runtime_credential(&provider("openrouter")).unwrap();
        assert_eq!(credential.api_key, "sk-backup");
        assert_eq!(credential.source, "hermes-pool:openrouter:backup");

        restore_env("HERMES_HOME", old_hermes_home);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn hermes_credential_pool_failure_marks_terminal_auth_dead() {
        let _guard = HERMES_AUTH_TEST_ENV_LOCK.lock().unwrap();
        let old_hermes_home = std::env::var_os("HERMES_HOME");
        let dir = std::env::temp_dir().join(format!(
            "synthchat-hermes-pool-terminal-{}",
            crate::models::new_id("test")
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("auth.json"),
            json!({
                "credential_pool": {
                    "openrouter": [{
                        "label": "primary",
                        "access_token": "sk-primary"
                    }]
                }
            })
            .to_string(),
        )
        .unwrap();
        std::env::set_var("HERMES_HOME", &dir);

        mark_hermes_credential_pool_failure(&provider("openrouter"), "terminal_auth", "401")
            .unwrap()
            .unwrap();
        let store = read_primary_hermes_auth_store().unwrap();
        let primary = &store["credential_pool"]["openrouter"][0];
        assert_eq!(primary["last_status"], "dead");
        assert_eq!(primary["last_error_code"], "terminal_auth");
        assert!(primary["last_error_reset_at"].as_u64().unwrap() > unix_now_seconds());

        restore_env("HERMES_HOME", old_hermes_home);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn hermes_credential_pool_failure_marks_matching_source_only() {
        let _guard = HERMES_AUTH_TEST_ENV_LOCK.lock().unwrap();
        let old_hermes_home = std::env::var_os("HERMES_HOME");
        let dir = std::env::temp_dir().join(format!(
            "synthchat-hermes-pool-source-failure-{}",
            crate::models::new_id("test")
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("auth.json"),
            json!({
                "credential_pool": {
                    "openrouter": [{
                        "label": "primary",
                        "access_token": "sk-primary",
                        "priority": 0
                    }, {
                        "label": "backup",
                        "access_token": "sk-backup",
                        "priority": 1
                    }]
                }
            })
            .to_string(),
        )
        .unwrap();
        std::env::set_var("HERMES_HOME", &dir);

        let marked = mark_hermes_credential_pool_failure_for_source(
            &provider("openrouter"),
            "hermes-pool:openrouter:backup",
            "quota",
            "quota exhausted",
        )
        .unwrap()
        .unwrap();
        assert_eq!(marked.source, "hermes-pool:openrouter:backup");
        assert_eq!(marked.state, "cooldown");

        let store = read_primary_hermes_auth_store().unwrap();
        let primary = &store["credential_pool"]["openrouter"][0];
        let backup = &store["credential_pool"]["openrouter"][1];
        assert!(primary["last_status"].is_null());
        assert_eq!(backup["last_status"], "exhausted");
        assert_eq!(backup["last_error_code"], "quota");

        restore_env("HERMES_HOME", old_hermes_home);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn bitwarden_secret_cache_resolves_env_named_credentials() {
        let _guard = HERMES_AUTH_TEST_ENV_LOCK.lock().unwrap();
        let old_hermes_home = std::env::var_os("HERMES_HOME");
        let old_access = std::env::var_os("BWS_ACCESS_TOKEN");
        let old_project = std::env::var_os("BWS_PROJECT_ID");
        let old_server = std::env::var_os("BWS_SERVER_URL");
        let dir = std::env::temp_dir().join(format!(
            "synthchat-bws-cache-{}",
            crate::models::new_id("test")
        ));
        let cache_dir = dir.join("cache");
        std::fs::create_dir_all(&cache_dir).unwrap();
        std::env::set_var("HERMES_HOME", &dir);
        std::env::set_var("BWS_ACCESS_TOKEN", "bws-test-token");
        std::env::set_var("BWS_PROJECT_ID", "project-1");
        std::env::remove_var("BWS_SERVER_URL");
        let cache_key = bitwarden_cache_key("bws-test-token", "project-1", "");
        std::fs::write(
            cache_dir.join(BITWARDEN_CACHE_BASENAME),
            json!({
                "key": cache_key,
                "fetched_at": unix_now_seconds(),
                "secrets": {
                    "OPENROUTER_API_KEY": "sk-openrouter-from-bws",
                    "invalid-name": "ignored"
                }
            })
            .to_string(),
        )
        .unwrap();

        let secret = resolve_bitwarden_secret(&["MISSING_KEY", "OPENROUTER_API_KEY"]).unwrap();
        assert_eq!(secret, "sk-openrouter-from-bws");
        let parsed = bitwarden_secrets_from_payload(&json!([
            {"key": "VALID_ENV", "value": "value"},
            {"key": "bad-name", "value": "ignored"}
        ]));
        assert_eq!(parsed.0.get("VALID_ENV").map(String::as_str), Some("value"));
        assert_eq!(parsed.1.len(), 1);

        restore_env("BWS_SERVER_URL", old_server);
        restore_env("BWS_PROJECT_ID", old_project);
        restore_env("BWS_ACCESS_TOKEN", old_access);
        restore_env("HERMES_HOME", old_hermes_home);
        let _ = std::fs::remove_dir_all(dir);
    }

    fn restore_env(name: &str, value: Option<std::ffi::OsString>) {
        if let Some(value) = value {
            std::env::set_var(name, value);
        } else {
            std::env::remove_var(name);
        }
    }

    fn test_jwt(claims: Value) -> String {
        let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"none","typ":"JWT"}"#);
        let payload = URL_SAFE_NO_PAD.encode(serde_json::to_string(&claims).unwrap());
        format!("{header}.{payload}.sig")
    }
}
