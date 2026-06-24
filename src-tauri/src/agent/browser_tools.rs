use std::{
    collections::HashMap,
    fs,
    process::Command,
    sync::{Mutex, OnceLock},
    time::Duration,
};

use futures::{SinkExt, StreamExt};
use reqwest::StatusCode;
use serde_json::{json, Value};
use tokio::task::AbortHandle;
use tokio_tungstenite::{connect_async, tungstenite::Message};

use crate::{
    error::{AppError, AppResult},
    models::{new_id, AgentDefinition, BrowserProvider},
    process_utils::CommandWindowExt,
    store::{summarize_browser_supervisor_state, AppStore},
};

use super::{
    build_browser_snapshot, extract_images, fetch_url_text_for_store, format_list,
    openai_compatible_vision_analyze, provider_api_key, resolve_vision_provider, string_arg,
    truncate_for_prompt, truncate_output, validate_web_url,
};

static LAST_BROWSER_URLS: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
static BROWSER_HISTORIES: OnceLock<Mutex<HashMap<String, Vec<String>>>> = OnceLock::new();
static BROWSER_RECORDERS: OnceLock<Mutex<HashMap<String, AbortHandle>>> = OnceLock::new();
static BROWSER_USE_PENDING_CREATE_KEYS: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
const HERMES_BROWSER_LEGACY_PREFERENCE: [&str; 2] = ["browser-use", "browserbase"];

pub(super) async fn browser_provider_tool(store: &AppStore, payload: &Value) -> AppResult<String> {
    let action = string_arg(payload, &["action"])
        .unwrap_or_else(|| "status".into())
        .trim()
        .to_ascii_lowercase();
    let configured = string_arg(
        payload,
        &[
            "provider",
            "providerId",
            "provider_id",
            "cloudProvider",
            "cloud_provider",
        ],
    );
    let providers = store.browser_providers()?;
    let status = browser_provider_registry_status(&providers, configured.as_deref());
    let value = match action.as_str() {
        "" | "status" | "list" | "resolve" => status,
        "setup_schema" | "setup-schema" | "schema" => json!({
            "setupSchema": providers.iter().map(browser_provider_setup_schema).collect::<Vec<_>>(),
            "status": status
        }),
        "lifecycle" | "health" | "health_schema" | "health-schema" => json!({
            "healthSchema": browser_provider_health_schema(),
            "status": status,
            "lifecycle": browser_provider_lifecycle_status(&providers, configured.as_deref()),
        }),
        other => {
            return Err(AppError::BadRequest(format!(
                "unsupported browser_provider action: {other}"
            )));
        }
    };
    Ok(serde_json::to_string_pretty(&value)?)
}

fn browser_provider_registry_status(
    providers: &[BrowserProvider],
    configured: Option<&str>,
) -> Value {
    let configured = configured.map(str::trim).filter(|value| !value.is_empty());
    let synthchat_enabled = providers
        .iter()
        .find(|provider| {
            provider.enabled
                && !provider.provider_type.trim().is_empty()
                && !provider.base_url.trim().is_empty()
        })
        .map(browser_provider_summary);
    let (hermes_resolved, hermes_reason) = resolve_hermes_browser_provider(providers, configured);
    let active_provider = hermes_resolved.map(browser_provider_summary);
    json!({
        "configured": configured.unwrap_or(""),
        "legacyPreference": HERMES_BROWSER_LEGACY_PREFERENCE,
        "providers": providers.iter().map(browser_provider_summary).collect::<Vec<_>>(),
        "activeProvider": active_provider.clone(),
        "synthchatEnabledProvider": synthchat_enabled,
        "hermesResolvedProvider": active_provider,
        "hermesResolutionReason": hermes_reason,
        "notes": [
            "Hermes auto-detect only considers browser-use then browserbase.",
            "SynthChat session creation uses the same explicit-provider and legacy-preference resolution."
        ]
    })
}

fn resolve_hermes_browser_provider<'a>(
    providers: &'a [BrowserProvider],
    configured: Option<&str>,
) -> (Option<&'a BrowserProvider>, &'static str) {
    let configured = configured.unwrap_or("").trim();
    if configured.eq_ignore_ascii_case("local") {
        return (None, "explicit_local");
    }
    if !configured.is_empty() {
        if let Some(provider) = providers
            .iter()
            .find(|provider| browser_provider_matches(provider, configured))
        {
            return (Some(provider), "explicit_config");
        }
    }
    for legacy in HERMES_BROWSER_LEGACY_PREFERENCE {
        if let Some(provider) = providers.iter().find(|provider| {
            browser_provider_matches(provider, legacy) && browser_provider_available(provider)
        }) {
            return (Some(provider), "legacy_available");
        }
    }
    if configured.is_empty() {
        (None, "no_available_legacy_provider")
    } else {
        (
            None,
            "explicit_not_registered_then_no_available_legacy_provider",
        )
    }
}

fn configured_browser_provider_from_payload(payload: &Value) -> Option<String> {
    string_arg(
        payload,
        &[
            "provider",
            "providerId",
            "provider_id",
            "cloudProvider",
            "cloud_provider",
        ],
    )
    .map(|value| value.trim().to_string())
    .filter(|value| !value.is_empty())
}

fn resolve_browser_provider_for_session<'a>(
    providers: &'a [BrowserProvider],
    payload: &Value,
) -> AppResult<(&'a BrowserProvider, &'static str)> {
    let configured = configured_browser_provider_from_payload(payload);
    let (provider, reason) = resolve_hermes_browser_provider(providers, configured.as_deref());
    let Some(provider) = provider else {
        return Err(AppError::BadRequest(
            if configured
                .as_deref()
                .is_some_and(|value| value.eq_ignore_ascii_case("local"))
            {
                "browser_create_session cannot create a cloud session when provider is explicitly local"
                    .into()
            } else {
                "no available Hermes browser provider configured; set browser-use/browserbase credentials or pass an explicit provider".into()
            },
        ));
    };
    Ok((provider, reason))
}

fn browser_provider_summary(provider: &BrowserProvider) -> Value {
    let credential_configured = provider_api_key(&provider.api_key, &provider.api_key_env)
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false);
    json!({
        "id": provider.id,
        "name": provider.name,
        "providerType": provider.provider_type,
        "enabled": provider.enabled,
        "available": browser_provider_available(provider),
        "baseUrlConfigured": !provider.base_url.trim().is_empty(),
        "apiKeyEnv": provider.api_key_env,
        "credentialConfigured": credential_configured,
        "projectIdConfigured": !provider.project_id.trim().is_empty(),
        "recordSessions": provider.record_sessions,
        "timeoutSeconds": provider.timeout_seconds,
    })
}

fn browser_provider_setup_schema(provider: &BrowserProvider) -> Value {
    let mut env_vars = Vec::new();
    if !provider.api_key_env.trim().is_empty() {
        env_vars.push(json!({
            "key": provider.api_key_env.trim(),
            "prompt": format!("{} API key", provider.name.trim()),
        }));
    }
    json!({
        "id": provider.id,
        "name": if provider.name.trim().is_empty() { provider.provider_type.trim() } else { provider.name.trim() },
        "providerType": provider.provider_type,
        "badge": "cloud",
        "tag": browser_provider_setup_tag(provider),
        "env_vars": env_vars,
        "post_setup": "agent_browser",
        "recordSessions": provider.record_sessions,
    })
}

fn browser_provider_setup_tag(provider: &BrowserProvider) -> &'static str {
    match provider.provider_type.trim().to_ascii_lowercase().as_str() {
        "browserbase" => "Cloud browser with CDP session lifecycle",
        "browser-use" | "browser_use" => "Browser Use cloud browser session",
        "firecrawl" => "Firecrawl browser provider; explicit configuration recommended",
        _ => "Cloud browser provider",
    }
}

fn browser_provider_health_schema() -> Value {
    json!({
        "actions": ["status", "list", "resolve", "setup_schema", "lifecycle", "health_schema"],
        "sessionContract": {
            "createTool": "browser_create_session",
            "closeTool": "browser_close_session",
            "requiredCreateResultFields": ["sessionId", "cdpUrl"],
            "hermesLegacyMetadataFields": ["session_name", "bb_session_id", "cdp_url", "features", "external_call_id"],
            "closeInputFields": ["sessionId"]
        },
        "nonMutating": true,
        "networkCalls": false,
        "notes": [
            "health_schema and lifecycle preview request shapes only; they do not create or close cloud browser sessions.",
            "Explicit Hermes provider resolution preserves unavailable providers so credential errors remain visible."
        ]
    })
}

fn browser_provider_lifecycle_status(
    providers: &[BrowserProvider],
    configured: Option<&str>,
) -> Value {
    let configured = configured.map(str::trim).filter(|value| !value.is_empty());
    let synthchat_enabled = providers
        .iter()
        .find(|provider| {
            provider.enabled
                && !provider.provider_type.trim().is_empty()
                && !provider.base_url.trim().is_empty()
        })
        .map(|provider| browser_provider_lifecycle_preview(provider, "synthchat_enabled"));
    let (hermes_resolved, hermes_reason) = resolve_hermes_browser_provider(providers, configured);
    json!({
        "configured": configured.unwrap_or(""),
        "synthchatSelectedProvider": synthchat_enabled,
        "hermesSelectedProvider": hermes_resolved
            .map(|provider| browser_provider_lifecycle_preview(provider, hermes_reason)),
        "providers": providers
            .iter()
            .map(|provider| browser_provider_lifecycle_preview(provider, "registered"))
            .collect::<Vec<_>>(),
        "selectionNotes": [
            "SynthChat session creation uses the same explicit-provider and legacy-preference resolution shown here.",
            "Hermes explicit config returns the named provider even when unavailable; auto-detect only walks browser-use then browserbase."
        ]
    })
}

fn browser_provider_lifecycle_preview(provider: &BrowserProvider, selected_by: &str) -> Value {
    let diagnostics = browser_provider_lifecycle_diagnostics(provider);
    let create_url = browser_session_create_url(provider)
        .map(|url| url.to_string())
        .map_err(|error| error.to_string());
    let create_request = browser_session_create_request(provider, "diagnostic-task", &json!({}));
    let close_request = browser_session_close_request(provider, "diagnostic-session")
        .map(|request| {
            json!({
                "method": request.method,
                "url": request.url.to_string(),
                "body": request.body,
            })
        })
        .map_err(|error| error.to_string());
    json!({
        "provider": browser_provider_summary(provider),
        "selectedBy": selected_by,
        "available": browser_provider_available(provider),
        "diagnostics": diagnostics,
        "create": {
            "method": "POST",
            "url": match create_url {
                Ok(value) => json!(value),
                Err(error) => json!({"error": error}),
            },
            "body": create_request.body,
            "features": create_request.features,
            "fallbacks": {
                "browserbase402": [
                    "remove keepAlive and retry when Browserbase returns 402",
                    "remove proxies and retry when Browserbase still returns 402"
                ]
            },
            "auth": browser_provider_auth_preview(provider),
            "responseExtraction": {
                "sessionIdKeys": ["id", "sessionId", "session_id", "bbSessionId", "browserId"],
                "cdpUrlKeys": [
                    "cdpUrl",
                    "cdp_url",
                    "connectUrl",
                    "connect_url",
                    "webSocketDebuggerUrl",
                    "wsEndpoint",
                    "wsUrl",
                    "ws_url"
                ],
                "recursive": true,
                "websocketSchemes": ["ws", "wss"]
            }
        },
        "close": match close_request {
            Ok(value) => value,
            Err(error) => json!({"error": error}),
        },
        "hermesContract": {
            "session_name": "SynthChat returns sessionId and registers a browser supervisor instead of launching agent-browser --session.",
            "bb_session_id": "Mapped from provider session id and accepted by browser_close_session as sessionId.",
            "cdp_url": "Mapped to cdpUrl.",
            "features": {},
            "external_call_id": null
        }
    })
}

fn browser_provider_lifecycle_diagnostics(provider: &BrowserProvider) -> Vec<Value> {
    let mut diagnostics = Vec::new();
    if !provider.enabled {
        diagnostics.push(json!({
            "level": "warning",
            "code": "provider_disabled",
            "message": "Provider is registered but disabled in SynthChat settings."
        }));
    }
    if provider.provider_type.trim().is_empty() {
        diagnostics.push(json!({
            "level": "error",
            "code": "missing_provider_type",
            "message": "providerType is required for session URL, close method, and auth shape."
        }));
    }
    if provider.base_url.trim().is_empty() {
        diagnostics.push(json!({
            "level": "error",
            "code": "missing_base_url",
            "message": "baseUrl is required before a cloud browser session can be created."
        }));
    } else if let Err(error) = reqwest::Url::parse(provider.base_url.trim()) {
        diagnostics.push(json!({
            "level": "error",
            "code": "invalid_base_url",
            "message": error.to_string()
        }));
    }
    if provider_api_key(&provider.api_key, &provider.api_key_env)
        .map(|value| value.trim().is_empty())
        .unwrap_or(true)
    {
        diagnostics.push(json!({
            "level": "error",
            "code": "missing_credentials",
            "message": "No non-empty API key is configured directly or via apiKeyEnv.",
            "env": provider.api_key_env
        }));
    }
    let provider_type = normalize_browser_provider_name(&provider.provider_type);
    if provider_type == "firecrawl" {
        diagnostics.push(json!({
            "level": "info",
            "code": "not_legacy_auto_selected",
            "message": "Matches Hermes behavior: Firecrawl is only selected when explicitly configured, never by the legacy browser-use/browserbase auto-detect walk."
        }));
    } else if !matches!(provider_type.as_str(), "browserbase" | "browser-use") {
        diagnostics.push(json!({
            "level": "warning",
            "code": "generic_lifecycle_adapter",
            "message": "Unknown provider type uses the generic /sessions and /sessions/{id}/close lifecycle adapter."
        }));
    }
    diagnostics
}

fn browser_provider_auth_preview(provider: &BrowserProvider) -> Value {
    let credential_configured = provider_api_key(&provider.api_key, &provider.api_key_env)
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false);
    let provider_type = provider.provider_type.trim().to_lowercase();
    if provider_type == "browserbase" {
        json!({
            "type": "header",
            "header": "x-bb-api-key",
            "credentialConfigured": credential_configured,
            "value": if credential_configured { "<redacted>" } else { "" }
        })
    } else if provider_type == "browser-use" || provider_type == "browser_use" {
        json!({
            "type": "header",
            "header": "X-Browser-Use-API-Key",
            "credentialConfigured": credential_configured,
            "value": if credential_configured { "<redacted>" } else { "" }
        })
    } else {
        json!({
            "type": "bearer",
            "header": "Authorization",
            "credentialConfigured": credential_configured,
            "value": if credential_configured { "Bearer <redacted>" } else { "" }
        })
    }
}

fn browser_provider_available(provider: &BrowserProvider) -> bool {
    provider.enabled
        && !provider.provider_type.trim().is_empty()
        && !provider.base_url.trim().is_empty()
        && provider_api_key(&provider.api_key, &provider.api_key_env)
            .map(|value| !value.trim().is_empty())
            .unwrap_or(false)
}

fn browser_provider_matches(provider: &BrowserProvider, name: &str) -> bool {
    let name = normalize_browser_provider_name(name);
    [
        provider.id.as_str(),
        provider.name.as_str(),
        provider.provider_type.as_str(),
    ]
    .iter()
    .any(|candidate| normalize_browser_provider_name(candidate) == name)
}

fn normalize_browser_provider_name(value: &str) -> String {
    value.trim().to_ascii_lowercase().replace('_', "-")
}

pub(super) async fn browser_navigate_tool(
    store: &AppStore,
    agent: &AgentDefinition,
    run_id: &str,
    payload: &Value,
) -> AppResult<String> {
    let url = payload
        .get("url")
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::BadRequest("browser_navigate requires payload.url".into()))?;
    remember_browser_url(agent, url)?;
    let html = fetch_url_text_for_store(store, url).await?;
    let supervisor = if let Some(cdp_url) = payload
        .get("cdpUrl")
        .or_else(|| payload.get("cdp_url"))
        .and_then(Value::as_str)
    {
        Some(register_browser_supervisor(
            store, run_id, payload, cdp_url,
        )?)
    } else {
        None
    };
    let supervisor_text = supervisor
        .map(|value| {
            format!(
                "\nsupervisor:\n{}",
                serde_json::to_string_pretty(&value).unwrap_or_default()
            )
        })
        .unwrap_or_default();
    let snapshot = append_supervisor_snapshot(
        store,
        run_id,
        payload,
        build_browser_snapshot(url, &html, false),
    )?;
    Ok(format!("navigated: {url}{supervisor_text}\n\n{}", snapshot))
}

pub(super) async fn browser_snapshot_tool(
    store: &AppStore,
    agent: &AgentDefinition,
    run_id: &str,
    payload: &Value,
) -> AppResult<String> {
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
    remember_browser_url(agent, &url)?;
    let full = payload
        .get("full")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let html = fetch_url_text_for_store(store, &url).await?;
    append_supervisor_snapshot(
        store,
        run_id,
        payload,
        build_browser_snapshot(&url, &html, full),
    )
}

pub(super) async fn browser_back_tool(
    store: &AppStore,
    agent: &AgentDefinition,
) -> AppResult<String> {
    let previous = pop_browser_history(agent)?;
    let Some(url) = previous else {
        return Err(AppError::BadRequest(
            "browser_back requires at least one previous browser_navigate URL".into(),
        ));
    };
    let html = fetch_url_text_for_store(store, &url).await?;
    Ok(format!(
        "navigatedBack: {url}\n\n{}",
        build_browser_snapshot(&url, &html, false)
    ))
}

pub(super) async fn browser_get_images_tool(
    store: &AppStore,
    agent: &AgentDefinition,
    payload: &Value,
) -> AppResult<String> {
    let url = payload
        .get("url")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| last_browser_url(agent).ok().flatten())
        .ok_or_else(|| {
            AppError::BadRequest(
                "browser_get_images requires payload.url until a page has been navigated".into(),
            )
        })?;
    remember_browser_url(agent, &url)?;
    let html = fetch_url_text_for_store(store, &url).await?;
    let images = extract_images(&html, 100);
    Ok(format!("url: {url}\nimages:\n{}", format_list(images)))
}

pub(super) async fn browser_create_session_tool(
    store: &AppStore,
    run_id: &str,
    payload: &Value,
) -> AppResult<String> {
    let providers = store.browser_providers()?;
    let (provider, selection_reason) = resolve_browser_provider_for_session(&providers, payload)?;
    let task_id = string_arg(payload, &["taskId", "task_id"])
        .unwrap_or_else(|| format!("synthchat-{run_id}"));
    let api_key = provider_api_key(&provider.api_key, &provider.api_key_env);
    let create_url = browser_session_create_url(&provider)?;
    let create_request = browser_session_create_request(&provider, &task_id, payload);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(provider.timeout_seconds.max(1)))
        .user_agent("SynthChat-agent/1.0")
        .build()
        .map_err(|error| {
            AppError::BadRequest(format!("failed to build browser client: {error}"))
        })?;
    let create_response = send_browser_session_create_request(
        &client,
        &provider,
        api_key.as_deref(),
        create_url.clone(),
        create_request,
    )
    .await?;
    let status = create_response.status;
    let text = create_response.text;
    if !status.is_success() {
        return Err(AppError::BadRequest(format!(
            "browser_create_session returned HTTP {}: {}",
            status.as_u16(),
            truncate_output(&text, 2000)
        )));
    }
    let value = serde_json::from_str::<Value>(&text)
        .map_err(|error| AppError::BadRequest(format!("invalid browser session JSON: {error}")))?;
    let session_id = extract_first_string_key(
        &value,
        &["id", "sessionId", "session_id", "bbSessionId", "browserId"],
    )
    .unwrap_or_else(|| new_id("browser-session"));
    let cdp_url = extract_browser_cdp_url(&value).ok_or_else(|| {
        AppError::BadRequest(format!(
            "browser_create_session response missing cdp/connect websocket URL: {}",
            truncate_output(&text, 2000)
        ))
    })?;
    let supervisor = register_browser_supervisor(
        store,
        run_id,
        &json!({
            "sessionId": session_id,
            "providerType": provider.provider_type,
        }),
        &cdp_url,
    )?;
    let auto_recording = if browser_auto_record_enabled(&provider, payload) {
        let record_payload = json!({
            "action": "start",
            "runId": run_id,
            "sessionId": session_id,
            "cdpUrl": cdp_url,
            "quality": payload.get("recordQuality").or_else(|| payload.get("record_quality")).and_then(Value::as_u64).unwrap_or(80),
            "everyNthFrame": payload.get("recordEveryNthFrame").or_else(|| payload.get("record_every_nth_frame")).and_then(Value::as_u64).unwrap_or(1)
        });
        match browser_record_start(store, run_id, &record_payload).await {
            Ok(text) => serde_json::from_str::<Value>(&text)
                .unwrap_or_else(|_| json!({"ok": true, "raw": text})),
            Err(error) => json!({"ok": false, "error": error.to_string()}),
        }
    } else {
        Value::Null
    };
    Ok(serde_json::to_string_pretty(&json!({
        "providerId": provider.id,
        "providerType": provider.provider_type,
        "providerSelectionReason": selection_reason,
        "sessionId": session_id,
        "cdpUrl": cdp_url,
        "createUrl": create_url,
        "features": create_response.features,
        "fallbacks": create_response.fallbacks,
        "external_call_id": create_response.external_call_id,
        "externalCallId": create_response.external_call_id,
        "supervisor": supervisor,
        "autoRecording": auto_recording,
        "raw": value
    }))?)
}

pub(super) async fn browser_close_session_tool(
    store: &AppStore,
    payload: &Value,
) -> AppResult<String> {
    let session_id =
        string_arg(payload, &["sessionId", "session_id", "bbSessionId"]).ok_or_else(|| {
            AppError::BadRequest("browser_close_session requires payload.sessionId".into())
        })?;
    let auto_recording = stop_and_export_recording_for_close(store, &session_id)?;
    let removed = store.remove_browser_supervisor_session(&session_id)?;
    let providers = store.browser_providers()?;
    let resolved_provider = resolve_browser_provider_for_session(&providers, payload).ok();
    let Some((provider, selection_reason)) = resolved_provider else {
        return Ok(serde_json::to_string_pretty(&json!({
            "sessionId": session_id,
            "removedSupervisor": removed,
            "autoRecording": auto_recording,
            "providerClosed": false,
            "reason": "no available Hermes browser provider configured"
        }))?);
    };
    let close_request = browser_session_close_request(&provider, &session_id)?;
    let api_key = provider_api_key(&provider.api_key, &provider.api_key_env);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(provider.timeout_seconds.max(1)))
        .user_agent("SynthChat-agent/1.0")
        .build()
        .map_err(|error| {
            AppError::BadRequest(format!("failed to build browser client: {error}"))
        })?;
    let mut request = match close_request.method.as_str() {
        "PATCH" => client
            .patch(close_request.url.clone())
            .json(&close_request.body),
        "POST" => client
            .post(close_request.url.clone())
            .json(&close_request.body),
        "DELETE" => client.delete(close_request.url.clone()),
        _ => client
            .post(close_request.url.clone())
            .json(&close_request.body),
    };
    request = apply_browser_provider_auth(request, &provider, api_key.as_deref())?;
    let response = request
        .send()
        .await
        .map_err(|error| AppError::BadRequest(format!("browser_close_session failed: {error}")))?;
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(AppError::BadRequest(format!(
            "browser_close_session returned HTTP {}: {}",
            status.as_u16(),
            truncate_output(&text, 2000)
        )));
    }
    Ok(serde_json::to_string_pretty(&json!({
        "sessionId": session_id,
        "removedSupervisor": removed,
        "autoRecording": auto_recording,
        "providerClosed": true,
        "status": status.as_u16(),
        "providerSelectionReason": selection_reason,
        "response": truncate_output(&text, 4000)
    }))?)
}

#[derive(Debug, Clone)]
pub(super) struct BrowserCloseRequest {
    pub(super) method: String,
    pub(super) url: reqwest::Url,
    pub(super) body: Value,
}

pub(super) fn browser_session_create_url(provider: &BrowserProvider) -> AppResult<reqwest::Url> {
    let mut url = reqwest::Url::parse(provider.base_url.trim())
        .map_err(|error| AppError::BadRequest(format!("invalid browser provider URL: {error}")))?;
    let provider_type = provider.provider_type.trim().to_lowercase();
    let path = url.path().trim_end_matches('/');
    if provider_type == "browserbase" {
        if path.ends_with("/v1") {
            url.set_path(&format!("{path}/sessions"));
        } else if !path.ends_with("/v1/sessions") {
            url.set_path(&format!("{path}/v1/sessions"));
        }
    } else if provider_type == "browser-use" || provider_type == "browser_use" {
        if !path.ends_with("/browsers") {
            url.set_path(&format!("{path}/browsers"));
        }
    } else if provider_type == "firecrawl" {
        if path.ends_with("/v2") {
            url.set_path(&format!("{path}/browser"));
        } else if !path.ends_with("/v2/browser") {
            url.set_path(&format!("{path}/v2/browser"));
        }
    }
    Ok(url)
}

#[derive(Debug, Clone)]
pub(super) struct BrowserSessionCreateRequest {
    pub(super) body: Value,
    pub(super) features: Value,
    pub(super) task_id: String,
    pub(super) idempotency_key: Option<String>,
}

#[derive(Debug)]
struct BrowserSessionCreateResponse {
    status: StatusCode,
    text: String,
    features: Value,
    fallbacks: Value,
    external_call_id: Option<String>,
}

pub(super) fn browser_session_create_request(
    provider: &BrowserProvider,
    task_id: &str,
    payload: &Value,
) -> BrowserSessionCreateRequest {
    let mut body = json!({
        "taskId": task_id,
        "name": task_id
    });
    let mut features = json!({});
    let provider_type = normalize_browser_provider_name(&provider.provider_type);
    if provider_type == "browserbase" {
        features = json!({
            "basic_stealth": true,
            "proxies": true,
            "advanced_stealth": false,
            "keep_alive": true,
            "custom_timeout": false,
        });
        body["keepAlive"] = json!(true);
        body["proxies"] = json!(true);
        if env_flag_enabled("BROWSERBASE_ADVANCED_STEALTH", false) {
            body["browserSettings"] = json!({"advancedStealth": true});
            features["advanced_stealth"] = json!(true);
        }
        if let Some(timeout) = browserbase_session_timeout(payload) {
            body["timeout"] = json!(timeout);
            features["custom_timeout"] = json!(true);
        }
        if !env_flag_enabled("BROWSERBASE_KEEP_ALIVE", true) {
            body.as_object_mut()
                .map(|object| object.remove("keepAlive"));
            features["keep_alive"] = json!(false);
        }
        if !env_flag_enabled("BROWSERBASE_PROXIES", true) {
            body.as_object_mut().map(|object| object.remove("proxies"));
            features["proxies"] = json!(false);
        }
    } else if provider_type == "browser-use" {
        let managed_mode = browser_use_managed_mode(provider, payload);
        let idempotency_key = managed_mode.then(|| browser_use_pending_create_key(task_id));
        features = json!({
            "browser_use": true,
            "managed_mode": managed_mode,
            "idempotency_key": idempotency_key.as_deref().unwrap_or("")
        });
        if managed_mode {
            body["timeout"] = json!(5);
            body["proxyCountryCode"] = json!("us");
            features["managed_timeout_minutes"] = json!(5);
            features["managed_proxy_country_code"] = json!("us");
        }
    } else if provider_type == "firecrawl" {
        body = json!({
            "ttl": firecrawl_browser_ttl(payload)
        });
        features = json!({
            "firecrawl": true,
            "ttl": body["ttl"]
        });
    }
    if !provider.project_id.trim().is_empty() {
        body["projectId"] = json!(provider.project_id.trim());
    }
    if let Some(extra) = payload.get("session").and_then(Value::as_object) {
        for (key, value) in extra {
            body[key] = value.clone();
        }
    }
    let idempotency_key = features
        .get("idempotency_key")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    BrowserSessionCreateRequest {
        body,
        features,
        task_id: task_id.to_string(),
        idempotency_key,
    }
}

async fn send_browser_session_create_request(
    client: &reqwest::Client,
    provider: &BrowserProvider,
    api_key: Option<&str>,
    create_url: reqwest::Url,
    mut create_request: BrowserSessionCreateRequest,
) -> AppResult<BrowserSessionCreateResponse> {
    let provider_type = normalize_browser_provider_name(&provider.provider_type);
    let mut fallbacks = Vec::new();
    let mut response = post_browser_session_create(
        client,
        provider,
        api_key,
        create_url.clone(),
        &create_request.body,
        create_request.idempotency_key.as_deref(),
    )
    .await?;

    if provider_type == "browserbase" && response.0 == StatusCode::PAYMENT_REQUIRED {
        if create_request.body.get("keepAlive").is_some() {
            if let Some(object) = create_request.body.as_object_mut() {
                object.remove("keepAlive");
            }
            create_request.features["keep_alive"] = json!(false);
            fallbacks.push(json!({"feature": "keepAlive", "reason": "browserbase_402"}));
            response = post_browser_session_create(
                client,
                provider,
                api_key,
                create_url.clone(),
                &create_request.body,
                create_request.idempotency_key.as_deref(),
            )
            .await?;
        }
        if response.0 == StatusCode::PAYMENT_REQUIRED
            && create_request.body.get("proxies").is_some()
        {
            if let Some(object) = create_request.body.as_object_mut() {
                object.remove("proxies");
            }
            create_request.features["proxies"] = json!(false);
            fallbacks.push(json!({"feature": "proxies", "reason": "browserbase_402"}));
            response = post_browser_session_create(
                client,
                provider,
                api_key,
                create_url,
                &create_request.body,
                create_request.idempotency_key.as_deref(),
            )
            .await?;
        }
    }
    if provider_type == "browser-use" && create_request.idempotency_key.is_some() {
        if response.0.is_success()
            || !browser_use_should_preserve_create_key(response.0, &response.1)
        {
            browser_use_clear_pending_create_key(&create_request.task_id);
        }
    }

    Ok(BrowserSessionCreateResponse {
        status: response.0,
        text: response.1,
        features: create_request.features,
        fallbacks: Value::Array(fallbacks),
        external_call_id: response.2,
    })
}

async fn post_browser_session_create(
    client: &reqwest::Client,
    provider: &BrowserProvider,
    api_key: Option<&str>,
    create_url: reqwest::Url,
    body: &Value,
    idempotency_key: Option<&str>,
) -> AppResult<(StatusCode, String, Option<String>)> {
    let mut request = client.post(create_url).json(body);
    request = apply_browser_provider_auth(request, provider, api_key)?;
    if let Some(idempotency_key) = idempotency_key.filter(|value| !value.trim().is_empty()) {
        request = request.header("X-Idempotency-Key", idempotency_key.trim());
    }
    let response = request
        .send()
        .await
        .map_err(|error| AppError::BadRequest(format!("browser_create_session failed: {error}")))?;
    let status = response.status();
    let external_call_id = response
        .headers()
        .get("x-external-call-id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let text = response.text().await.map_err(|error| {
        AppError::BadRequest(format!("failed to read browser session response: {error}"))
    })?;
    Ok((status, text, external_call_id))
}

fn browserbase_session_timeout(payload: &Value) -> Option<u64> {
    payload
        .get("session")
        .and_then(|session| session.get("timeout"))
        .and_then(Value::as_u64)
        .filter(|value| *value > 0)
        .or_else(|| {
            std::env::var("BROWSERBASE_SESSION_TIMEOUT")
                .ok()
                .and_then(|value| value.trim().parse::<u64>().ok())
                .filter(|value| *value > 0)
        })
}

fn firecrawl_browser_ttl(payload: &Value) -> u64 {
    payload
        .get("ttl")
        .or_else(|| payload.get("browserTtl"))
        .or_else(|| payload.get("browser_ttl"))
        .or_else(|| {
            payload
                .get("session")
                .and_then(|session| session.get("ttl"))
        })
        .and_then(Value::as_u64)
        .filter(|value| *value > 0)
        .or_else(|| {
            std::env::var("FIRECRAWL_BROWSER_TTL")
                .ok()
                .and_then(|value| value.trim().parse::<u64>().ok())
                .filter(|value| *value > 0)
        })
        .unwrap_or(300)
}

fn env_flag_enabled(name: &str, default: bool) -> bool {
    match std::env::var(name) {
        Ok(value) => {
            let value = value.trim().to_ascii_lowercase();
            if matches!(value.as_str(), "1" | "true" | "yes" | "on") {
                true
            } else if matches!(value.as_str(), "0" | "false" | "no" | "off") {
                false
            } else {
                default
            }
        }
        Err(_) => default,
    }
}

fn browser_use_managed_mode(provider: &BrowserProvider, payload: &Value) -> bool {
    if let Some(value) = payload
        .get("managedMode")
        .or_else(|| payload.get("managed_mode"))
        .and_then(Value::as_bool)
    {
        return value;
    }
    if let Some(value) = payload
        .get("session")
        .and_then(|session| {
            session
                .get("managedMode")
                .or_else(|| session.get("managed_mode"))
        })
        .and_then(Value::as_bool)
    {
        return value;
    }
    env_flag_enabled("BROWSER_USE_MANAGED_MODE", false)
        || env_flag_enabled("HERMES_BROWSER_USE_MANAGED_MODE", false)
        || normalize_browser_provider_name(&provider.id).contains("managed")
        || normalize_browser_provider_name(&provider.name).contains("managed")
        || provider
            .base_url
            .to_ascii_lowercase()
            .contains("managed-tool")
}

fn browser_use_pending_create_key(task_id: &str) -> String {
    let task_id = task_id.trim();
    let key = if task_id.is_empty() {
        "default"
    } else {
        task_id
    };
    let mut pending = BROWSER_USE_PENDING_CREATE_KEYS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap();
    pending
        .entry(key.to_string())
        .or_insert_with(|| format!("browser-use-session-create:{}", new_id("browser-use")))
        .clone()
}

fn browser_use_clear_pending_create_key(task_id: &str) {
    let task_id = task_id.trim();
    let key = if task_id.is_empty() {
        "default"
    } else {
        task_id
    };
    if let Some(pending) = BROWSER_USE_PENDING_CREATE_KEYS.get() {
        pending.lock().unwrap().remove(key);
    }
}

fn browser_use_should_preserve_create_key(status: StatusCode, text: &str) -> bool {
    if status.is_server_error() {
        return true;
    }
    if status != StatusCode::CONFLICT {
        return false;
    }
    serde_json::from_str::<Value>(text)
        .ok()
        .and_then(|value| {
            value
                .get("error")
                .and_then(|error| error.get("message"))
                .and_then(Value::as_str)
                .map(|message| message.to_ascii_lowercase())
        })
        .map(|message| message.contains("already in progress"))
        .unwrap_or(false)
}

pub(super) fn browser_session_close_request(
    provider: &BrowserProvider,
    session_id: &str,
) -> AppResult<BrowserCloseRequest> {
    let mut url = reqwest::Url::parse(provider.base_url.trim())
        .map_err(|error| AppError::BadRequest(format!("invalid browser provider URL: {error}")))?;
    let provider_type = provider.provider_type.trim().to_lowercase();
    let encoded = session_id.replace('/', "%2F");
    let path = url.path().trim_end_matches('/');
    if provider_type == "browserbase" {
        if path.ends_with("/v1") {
            url.set_path(&format!("{path}/sessions/{encoded}"));
        } else if path.ends_with("/v1/sessions") {
            url.set_path(&format!("{path}/{encoded}"));
        } else {
            url.set_path(&format!("{path}/v1/sessions/{encoded}"));
        }
        Ok(BrowserCloseRequest {
            method: "PATCH".into(),
            url,
            body: json!({"status": "REQUEST_RELEASE"}),
        })
    } else if provider_type == "browser-use" || provider_type == "browser_use" {
        url.set_path(&format!("{path}/browsers/{encoded}"));
        Ok(BrowserCloseRequest {
            method: "PATCH".into(),
            url,
            body: json!({"action": "stop"}),
        })
    } else if provider_type == "firecrawl" {
        if path.ends_with("/v2") {
            url.set_path(&format!("{path}/browser/{encoded}"));
        } else if path.ends_with("/v2/browser") {
            url.set_path(&format!("{path}/{encoded}"));
        } else {
            url.set_path(&format!("{path}/v2/browser/{encoded}"));
        }
        Ok(BrowserCloseRequest {
            method: "DELETE".into(),
            url,
            body: json!({}),
        })
    } else {
        url.set_path(&format!("{path}/sessions/{encoded}/close"));
        Ok(BrowserCloseRequest {
            method: "POST".into(),
            url,
            body: json!({}),
        })
    }
}

fn apply_browser_provider_auth(
    request: reqwest::RequestBuilder,
    provider: &BrowserProvider,
    api_key: Option<&str>,
) -> AppResult<reqwest::RequestBuilder> {
    let Some(api_key) = api_key.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(request);
    };
    let provider_type = provider.provider_type.trim().to_lowercase();
    let request = if provider_type == "browserbase" {
        request.header("x-bb-api-key", api_key)
    } else if provider_type == "browser-use" || provider_type == "browser_use" {
        request.header("X-Browser-Use-API-Key", api_key)
    } else {
        request.bearer_auth(api_key)
    };
    Ok(request)
}

fn browser_auto_record_enabled(provider: &BrowserProvider, payload: &Value) -> bool {
    if let Some(value) = payload
        .get("recordSessions")
        .or_else(|| payload.get("record_sessions"))
        .or_else(|| payload.get("record"))
        .and_then(Value::as_bool)
    {
        return value;
    }
    for name in [
        "SYNTHCHAT_BROWSER_RECORD_SESSIONS",
        "HERMES_BROWSER_RECORD_SESSIONS",
    ] {
        if let Ok(value) = std::env::var(name) {
            let value = value.trim().to_ascii_lowercase();
            if matches!(value.as_str(), "1" | "true" | "yes" | "on") {
                return true;
            }
            if matches!(value.as_str(), "0" | "false" | "no" | "off") {
                return false;
            }
        }
    }
    provider.record_sessions
}

fn stop_and_export_recording_for_close(store: &AppStore, session_id: &str) -> AppResult<Value> {
    let Some(state) = store.browser_supervisor_state_for_session(session_id)? else {
        return Ok(Value::Null);
    };
    let key = state
        .get("runId")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(session_id)
        .to_string();
    let stopped = BROWSER_RECORDERS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .map_err(|_| AppError::BadRequest("browser recorder lock poisoned".into()))?
        .remove(&key)
        .map(|handle| {
            handle.abort();
            true
        })
        .unwrap_or(false);
    if stopped
        || state
            .get("recording")
            .and_then(|value| value.get("active"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
    {
        let previous = state.get("recording").cloned().unwrap_or_else(|| json!({}));
        let _ = store.set_browser_supervisor_recording(
            &key,
            json!({
                "active": false,
                "mode": previous.get("mode").and_then(Value::as_str).unwrap_or("cdp_screencast"),
                "startedAt": previous.get("startedAt").cloned().unwrap_or(Value::Null),
                "stoppedAt": crate::models::now_iso(),
                "stoppedRecorderTask": stopped,
                "stoppedBy": "browser_close_session"
            }),
        );
    }
    let export_state = store.browser_supervisor_state(&key)?.unwrap_or(state);
    let export = if export_state
        .get("screencastFrames")
        .and_then(Value::as_array)
        .map(|frames| !frames.is_empty())
        .unwrap_or(false)
    {
        match browser_record_export_state(store, &key, &export_state, &json!({"maxFrames": 12})) {
            Ok(manifest) => json!({"ok": true, "manifest": manifest}),
            Err(error) => json!({"ok": false, "error": error.to_string()}),
        }
    } else {
        json!({"ok": false, "reason": "no recorded screencast frames"})
    };
    Ok(json!({
        "runId": key,
        "stoppedRecorderTask": stopped,
        "export": export
    }))
}

pub(super) fn extract_browser_cdp_url(value: &Value) -> Option<String> {
    extract_first_string_key(
        value,
        &[
            "cdpUrl",
            "cdp_url",
            "connectUrl",
            "connect_url",
            "webSocketDebuggerUrl",
            "wsUrl",
            "ws_url",
        ],
    )
    .filter(|url| url.starts_with("ws://") || url.starts_with("wss://"))
}

pub(super) fn extract_first_string_key(value: &Value, keys: &[&str]) -> Option<String> {
    match value {
        Value::Object(map) => {
            for key in keys {
                if let Some(found) = map
                    .get(*key)
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                {
                    return Some(found.to_string());
                }
            }
            map.values()
                .find_map(|nested| extract_first_string_key(nested, keys))
        }
        Value::Array(items) => items
            .iter()
            .find_map(|nested| extract_first_string_key(nested, keys)),
        _ => None,
    }
}

pub(super) async fn browser_cdp_tool(
    store: &AppStore,
    run_id: &str,
    payload: &Value,
) -> AppResult<String> {
    let cdp_url = cdp_url_from_payload(payload)?;
    if !(cdp_url.starts_with("ws://") || cdp_url.starts_with("wss://")) {
        return Err(AppError::BadRequest(
            "browser_cdp cdpUrl must start with ws:// or wss://".into(),
        ));
    }
    let action = payload
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("snapshot");
    let action = action.trim().to_lowercase();
    if payload.get("method").and_then(Value::as_str).is_some()
        && matches!(action.as_str(), "" | "raw" | "cdp" | "snapshot")
    {
        return browser_cdp_raw_tool(store, run_id, cdp_url, payload).await;
    }
    match action.as_str() {
        "" | "snapshot" => browser_cdp_snapshot_tool(cdp_url, payload).await,
        "navigate" => browser_cdp_navigate_tool(store, run_id, cdp_url, payload).await,
        "click" => browser_click_tool(payload).await,
        "type" => browser_type_tool(payload).await,
        "press" => browser_press_tool(payload).await,
        "scroll" => browser_scroll_tool(payload).await,
        "back" => browser_cdp_back_tool(cdp_url).await,
        "screenshot" => browser_cdp_screenshot_tool(store, run_id, cdp_url, payload).await,
        "console" | "evaluate" => browser_console_tool(store, run_id, payload).await,
        "dialog" => browser_dialog_with_cdp(cdp_url, payload).await,
        "frame_tree" | "frametree" => {
            let result = send_cdp_message(cdp_url, "Page.getFrameTree", json!({})).await?;
            Ok(serde_json::to_string_pretty(&result)?)
        }
        "raw" | "cdp" => browser_cdp_raw_tool(store, run_id, cdp_url, payload).await,
        other => Err(AppError::BadRequest(format!(
            "unsupported browser_cdp action: {other}"
        ))),
    }
}

async fn browser_cdp_raw_tool(
    store: &AppStore,
    run_id: &str,
    cdp_url: &str,
    payload: &Value,
) -> AppResult<String> {
    let method = payload
        .get("method")
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::BadRequest("browser_cdp requires payload.method".into()))?;
    let params = payload.get("params").cloned().unwrap_or_else(|| json!({}));
    if !params.is_object() {
        return Err(AppError::BadRequest(
            "browser_cdp payload.params must be an object".into(),
        ));
    }
    let timeout_ms = payload
        .get("timeoutMs")
        .or_else(|| payload.get("timeout_ms"))
        .and_then(Value::as_u64)
        .unwrap_or(10_000)
        .clamp(1_000, 120_000);
    let options = cdp_call_options_from_payload(store, run_id, payload)?;
    let target_id = options.target_id.clone();
    let session_id = options.session_id.clone();
    let result = await_browser_future_interruptible(
        store,
        run_id,
        timeout_ms,
        format!("browser_cdp timed out after {timeout_ms}ms"),
        send_cdp_message_with_options(cdp_url, method, params, options),
    )
    .await??;
    Ok(serde_json::to_string_pretty(&browser_cdp_success_payload(
        method, result, target_id, session_id,
    ))?)
}

pub(super) fn browser_cdp_success_payload(
    method: &str,
    result: Value,
    target_id: Option<String>,
    session_id: Option<String>,
) -> Value {
    let mut payload = json!({
        "success": true,
        "method": method,
        "result": result,
    });
    if let Some(target_id) = target_id.filter(|value| !value.trim().is_empty()) {
        payload["target_id"] = json!(target_id);
    }
    if let Some(session_id) = session_id.filter(|value| !value.trim().is_empty()) {
        payload["session_id"] = json!(session_id);
    }
    payload
}

async fn browser_cdp_navigate_tool(
    store: &AppStore,
    run_id: &str,
    cdp_url: &str,
    payload: &Value,
) -> AppResult<String> {
    let url = payload.get("url").and_then(Value::as_str).ok_or_else(|| {
        AppError::BadRequest("browser_cdp action=navigate requires payload.url".into())
    })?;
    validate_web_url(url)?;
    let navigate = send_cdp_message(cdp_url, "Page.navigate", json!({"url": url})).await?;
    let wait_ms = payload
        .get("waitMs")
        .or_else(|| payload.get("wait_ms"))
        .and_then(Value::as_u64)
        .unwrap_or(750)
        .clamp(0, 10_000);
    if wait_ms > 0 {
        wait_browser_run_interruptible(store, run_id, wait_ms).await?;
    }
    let snapshot = browser_cdp_snapshot_tool(cdp_url, payload).await?;
    Ok(format!(
        "navigate:\n{}\n\n{}",
        serde_json::to_string_pretty(&navigate)?,
        snapshot
    ))
}

pub(super) async fn wait_browser_run_interruptible(
    store: &AppStore,
    run_id: &str,
    wait_ms: u64,
) -> AppResult<()> {
    let started = tokio::time::Instant::now();
    let deadline = started + Duration::from_millis(wait_ms);
    loop {
        if browser_run_interrupted(store, run_id)? {
            return Err(AppError::BadRequest(
                "tool canceled because the agent run ended".into(),
            ));
        }
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return Ok(());
        }
        tokio::time::sleep((deadline - now).min(Duration::from_millis(100))).await;
    }
}

pub(super) async fn await_browser_future_interruptible<F, T>(
    store: &AppStore,
    run_id: &str,
    timeout_ms: u64,
    timeout_message: String,
    future: F,
) -> AppResult<T>
where
    F: std::future::Future<Output = T>,
{
    let deadline = tokio::time::Instant::now() + Duration::from_millis(timeout_ms);
    tokio::pin!(future);
    loop {
        tokio::select! {
            output = &mut future => return Ok(output),
            _ = tokio::time::sleep_until(deadline) => {
                return Err(AppError::BadRequest(timeout_message));
            }
            _ = tokio::time::sleep(Duration::from_millis(100)) => {
                if browser_run_interrupted(store, run_id)? {
                    return Err(AppError::BadRequest(
                        "tool canceled because the agent run ended".into(),
                    ));
                }
            }
        }
    }
}

fn browser_run_interrupted(store: &AppStore, run_id: &str) -> AppResult<bool> {
    match store.agent_run(run_id) {
        Ok(run) => Ok(matches!(
            run.state.as_str(),
            "completed" | "failed" | "aborted"
        )),
        Err(AppError::NotFound(_)) => Ok(false),
        Err(error) => Err(error),
    }
}

async fn browser_cdp_back_tool(cdp_url: &str) -> AppResult<String> {
    let history = send_cdp_message(cdp_url, "Page.getNavigationHistory", json!({})).await?;
    let current_index = history
        .get("currentIndex")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let entries = history
        .get("entries")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if current_index <= 0 {
        return Ok(serde_json::to_string_pretty(&json!({
            "ok": false,
            "error": "no previous history entry",
            "history": history
        }))?);
    }
    let target = entries
        .get((current_index - 1) as usize)
        .and_then(|entry| entry.get("id"))
        .and_then(Value::as_i64)
        .ok_or_else(|| AppError::BadRequest("previous history entry missing id".into()))?;
    let result = send_cdp_message(
        cdp_url,
        "Page.navigateToHistoryEntry",
        json!({"entryId": target}),
    )
    .await?;
    Ok(serde_json::to_string_pretty(&json!({
        "ok": true,
        "entryId": target,
        "result": result
    }))?)
}

async fn browser_cdp_screenshot_tool(
    store: &AppStore,
    run_id: &str,
    cdp_url: &str,
    payload: &Value,
) -> AppResult<String> {
    let format = payload
        .get("format")
        .and_then(Value::as_str)
        .unwrap_or("png")
        .to_lowercase();
    let image_format = if format == "jpeg" || format == "jpg" {
        "jpeg"
    } else {
        "png"
    };
    let mut params = json!({"format": image_format});
    if image_format == "jpeg" {
        if let Some(quality) = payload.get("quality").and_then(Value::as_u64) {
            params["quality"] = json!(quality.clamp(1, 100));
        }
    }
    if payload
        .get("fullPage")
        .or_else(|| payload.get("full_page"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        params["captureBeyondViewport"] = json!(true);
    }
    let result = send_cdp_message(cdp_url, "Page.captureScreenshot", params).await?;
    let data_len = result
        .get("data")
        .and_then(Value::as_str)
        .map(str::len)
        .unwrap_or(0);
    let screenshot_path = if let Some(data) = result.get("data").and_then(Value::as_str) {
        use base64::Engine;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(data)
            .map_err(|error| {
                AppError::BadRequest(format!("invalid browser screenshot base64: {error}"))
            })?;
        Some((
            store.save_tool_binary_artifact(
                run_id,
                "browser_cdp_screenshot",
                image_format,
                &bytes,
            )?,
            bytes.len(),
        ))
    } else {
        None
    };
    Ok(serde_json::to_string_pretty(&json!({
        "ok": true,
        "format": image_format,
        "base64Length": data_len,
        "sizeBytes": screenshot_path.as_ref().map(|(_, size)| *size).unwrap_or(0),
        "screenshotPath": screenshot_path.as_ref().map(|(path, _)| path.to_string_lossy().to_string()).unwrap_or_default(),
        "data": result.get("data").cloned().unwrap_or(Value::Null)
    }))?)
}

async fn browser_cdp_snapshot_tool(cdp_url: &str, payload: &Value) -> AppResult<String> {
    let max_items = payload
        .get("maxItems")
        .or_else(|| payload.get("max_items"))
        .and_then(Value::as_u64)
        .unwrap_or(60)
        .clamp(1, 120) as usize;
    let timeout_ms = payload
        .get("timeoutMs")
        .or_else(|| payload.get("timeout_ms"))
        .and_then(Value::as_u64)
        .unwrap_or(10_000)
        .clamp(1_000, 120_000);
    let expression = dynamic_browser_snapshot_expression(max_items);
    let result = tokio::time::timeout(
        Duration::from_millis(timeout_ms),
        send_cdp_message(
            cdp_url,
            "Runtime.evaluate",
            json!({"expression": expression, "awaitPromise": true, "returnByValue": true}),
        ),
    )
    .await
    .map_err(|_| {
        AppError::BadRequest(format!(
            "browser_cdp snapshot timed out after {timeout_ms}ms"
        ))
    })??;
    let value = result
        .get("result")
        .and_then(|result| result.get("value"))
        .cloned()
        .unwrap_or_else(|| json!({"ok": false, "error": "Runtime.evaluate returned no value"}));
    Ok(render_dynamic_browser_snapshot(&value)?)
}

pub(super) fn dynamic_browser_snapshot_expression(max_items: usize) -> String {
    let script = r#"
(() => {
  const maxItems = __MAX_ITEMS__;
  const trim = (value, limit = 240) => String(value ?? "").replace(/\s+/g, " ").trim().slice(0, limit);
  const cssEscape = (value) => {
    if (window.CSS && typeof window.CSS.escape === "function") return window.CSS.escape(String(value));
    return String(value).replace(/["\\#.:,[\]>+~*^$|=()\s]/g, "\\$&");
  };
  const selectorFor = (el) => {
    if (!el || !el.tagName) return "";
    const tag = el.tagName.toLowerCase();
    if (el.id) return `${tag}#${cssEscape(el.id)}`;
    const stableAttrs = ["name", "aria-label", "placeholder", "type", "role", "data-testid", "data-test", "href"];
    for (const attr of stableAttrs) {
      const value = el.getAttribute(attr);
      if (value) return `${tag}[${attr}="${cssEscape(value)}"]`;
    }
    const parts = [];
    let cur = el;
    while (cur && cur.nodeType === 1 && parts.length < 4) {
      let part = cur.tagName.toLowerCase();
      if (cur.id) {
        part += `#${cssEscape(cur.id)}`;
        parts.unshift(part);
        break;
      }
      const parent = cur.parentElement;
      if (parent) {
        const siblings = Array.from(parent.children).filter((child) => child.tagName === cur.tagName);
        if (siblings.length > 1) part += `:nth-of-type(${siblings.indexOf(cur) + 1})`;
      }
      parts.unshift(part);
      cur = parent;
    }
    return parts.join(" > ");
  };
  let refId = 0;
  const describeElement = (el) => {
    const rect = el.getBoundingClientRect ? el.getBoundingClientRect() : {x:0,y:0,width:0,height:0};
    return {
      ref: `@e${++refId}`,
      selector: selectorFor(el),
      tag: el.tagName ? el.tagName.toLowerCase() : "",
      type: el.getAttribute("type") || "",
      role: el.getAttribute("role") || "",
      name: el.getAttribute("name") || "",
      id: el.id || "",
      text: trim(el.innerText || el.value || el.getAttribute("aria-label") || el.getAttribute("placeholder") || el.getAttribute("title") || ""),
      visible: !!(rect.width || rect.height || el.getClientRects().length),
      disabled: !!el.disabled || el.getAttribute("aria-disabled") === "true",
      bounds: {x: Math.round(rect.x), y: Math.round(rect.y), width: Math.round(rect.width), height: Math.round(rect.height)}
    };
  };
  const controls = Array.from(document.querySelectorAll("input, textarea, select, button, [role=button], [contenteditable=true]"))
    .slice(0, maxItems)
    .map((el) => ({
      ...describeElement(el),
      value: el.tagName && el.tagName.toLowerCase() === "input" && /password/i.test(el.type || "") ? "" : trim(el.value || "", 120),
      placeholder: el.getAttribute("placeholder") || "",
      required: !!el.required || el.getAttribute("aria-required") === "true",
      form: el.form ? selectorFor(el.form) : ""
    }));
  const buttons = Array.from(document.querySelectorAll("button, input[type=button], input[type=submit], input[type=reset], [role=button]"))
    .slice(0, maxItems)
    .map(describeElement);
  const links = Array.from(document.querySelectorAll("a[href]"))
    .slice(0, maxItems)
    .map((el) => ({...describeElement(el), href: el.href || el.getAttribute("href") || ""}));
  const images = Array.from(document.querySelectorAll("img[src]"))
    .slice(0, maxItems)
    .map((el) => ({...describeElement(el), src: el.currentSrc || el.src || el.getAttribute("src") || "", alt: el.alt || ""}));
  const forms = Array.from(document.querySelectorAll("form"))
    .slice(0, maxItems)
    .map((form) => ({
      ...describeElement(form),
      method: (form.method || "GET").toUpperCase(),
      action: form.action || location.href,
      inputs: Array.from(form.querySelectorAll("input, textarea, select")).slice(0, maxItems).map((el) => describeElement(el)),
      buttons: Array.from(form.querySelectorAll("button, input[type=submit], input[type=button], input[type=reset]")).slice(0, maxItems).map((el) => describeElement(el))
    }));
  const requestClues = [];
  for (const form of forms) {
    requestClues.push({kind: "form", method: form.method || "GET", url: form.action || location.href, ref: form.ref, selector: form.selector});
    if (requestClues.length >= maxItems) break;
  }
  if (requestClues.length < maxItems) {
    for (const entry of performance.getEntriesByType("resource")) {
      const type = entry.initiatorType || "resource";
      if (!["fetch", "xmlhttprequest", "script", "link", "img", "beacon"].includes(type)) continue;
      requestClues.push({kind: "performance", initiatorType: type, url: entry.name, durationMs: Math.round(entry.duration || 0)});
      if (requestClues.length >= maxItems) break;
    }
  }
  if (requestClues.length < maxItems) {
    const scriptText = Array.from(document.scripts).map((script) => script.src ? `src=${script.src}` : script.textContent || "").join("\n");
    for (const needle of ["fetch(", "XMLHttpRequest", ".open(", "method:", "method="]) {
      const lower = scriptText.toLowerCase();
      let offset = 0;
      while (requestClues.length < maxItems) {
        const index = lower.indexOf(needle.toLowerCase(), offset);
        if (index < 0) break;
        requestClues.push({kind: "script", marker: needle, snippet: trim(scriptText.slice(Math.max(0, index - 160), index + 320), 420)});
        offset = index + needle.length;
      }
    }
  }
  return {
    ok: true,
    mode: "dynamic_cdp",
    url: location.href,
    title: document.title || "",
    readyState: document.readyState,
    forms,
    controls,
    buttons,
    links,
    images,
    requestClues,
    textPreview: trim(document.body ? document.body.innerText : "", 2000)
  };
})()
"#;
    script.replace("__MAX_ITEMS__", &max_items.to_string())
}

pub(super) fn render_dynamic_browser_snapshot(snapshot: &Value) -> AppResult<String> {
    let mut lines = Vec::new();
    lines.push(format!(
        "Dynamic browser snapshot: {}",
        snapshot.get("url").and_then(Value::as_str).unwrap_or("")
    ));
    if let Some(title) = snapshot.get("title").and_then(Value::as_str) {
        if !title.is_empty() {
            lines.push(format!("title: {title}"));
        }
    }
    if let Some(ready_state) = snapshot.get("readyState").and_then(Value::as_str) {
        if !ready_state.is_empty() {
            lines.push(format!("readyState: {ready_state}"));
        }
    }
    for key in [
        "forms",
        "controls",
        "buttons",
        "links",
        "images",
        "requestClues",
    ] {
        lines.push(format!("\n{key}:"));
        if let Some(items) = snapshot.get(key).and_then(Value::as_array) {
            if items.is_empty() {
                lines.push("- none".into());
            }
            for (index, item) in items.iter().enumerate() {
                lines.push(format!("{}: {}", index + 1, compact_json_line(item)));
            }
        } else {
            lines.push("- none".into());
        }
    }
    if let Some(preview) = snapshot.get("textPreview").and_then(Value::as_str) {
        if !preview.is_empty() {
            lines.push(format!(
                "\ntextPreview:\n{}",
                truncate_for_prompt(preview, 1200)
            ));
        }
    }
    lines.push(format!(
        "\njson:\n{}",
        serde_json::to_string_pretty(snapshot)?
    ));
    Ok(lines.join("\n"))
}

fn compact_json_line(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| value.to_string())
}

pub(super) fn browser_target_from_payload(payload: &Value, tool_name: &str) -> AppResult<String> {
    payload
        .get("ref")
        .or_else(|| payload.get("selector"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|target| !target.is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            AppError::BadRequest(format!(
                "{tool_name} requires payload.ref from browser_cdp snapshot or payload.selector"
            ))
        })
}

pub(super) fn browser_target_resolver_script() -> &'static str {
    r#"
(target) => {
  const cssEscape = (value) => {
    if (window.CSS && typeof window.CSS.escape === "function") return window.CSS.escape(String(value));
    return String(value).replace(/["\\#.:,[\]>+~*^$|=()\s]/g, "\\$&");
  };
  const selectorFor = (el) => {
    if (!el || !el.tagName) return "";
    const tag = el.tagName.toLowerCase();
    if (el.id) return `${tag}#${cssEscape(el.id)}`;
    const stableAttrs = ["name", "aria-label", "placeholder", "type", "role", "data-testid", "data-test", "href"];
    for (const attr of stableAttrs) {
      const value = el.getAttribute(attr);
      if (value) return `${tag}[${attr}="${cssEscape(value)}"]`;
    }
    const parts = [];
    let cur = el;
    while (cur && cur.nodeType === 1 && parts.length < 4) {
      let part = cur.tagName.toLowerCase();
      if (cur.id) {
        part += `#${cssEscape(cur.id)}`;
        parts.unshift(part);
        break;
      }
      const parent = cur.parentElement;
      if (parent) {
        const siblings = Array.from(parent.children).filter((child) => child.tagName === cur.tagName);
        if (siblings.length > 1) part += `:nth-of-type(${siblings.indexOf(cur) + 1})`;
      }
      parts.unshift(part);
      cur = parent;
    }
    return parts.join(" > ");
  };
  const allElements = [];
  allElements.push(...Array.from(document.querySelectorAll("input, textarea, select, button, [role=button], [contenteditable=true]")));
  allElements.push(...Array.from(document.querySelectorAll("button, input[type=button], input[type=submit], input[type=reset], [role=button]")));
  allElements.push(...Array.from(document.querySelectorAll("a[href]")));
  allElements.push(...Array.from(document.querySelectorAll("img[src]")));
  for (const form of Array.from(document.querySelectorAll("form"))) {
    allElements.push(form);
    allElements.push(...Array.from(form.querySelectorAll("input, textarea, select")));
    allElements.push(...Array.from(form.querySelectorAll("button, input[type=submit], input[type=button], input[type=reset]")));
  }
  const normalized = String(target || "").trim();
  const refMatch = normalized.match(/^@?e(\d+)$/i);
  if (refMatch) {
    const index = Number(refMatch[1]) - 1;
    const element = allElements[index];
    if (!element) return {ok:false, error:"ref not found in current DOM snapshot", target, ref:`@e${index + 1}`, elementCount: allElements.length};
    return {ok:true, element, ref:`@e${index + 1}`, selector: selectorFor(element)};
  }
  const element = document.querySelector(normalized);
  if (!element) return {ok:false, error:"selector not found", target, selector: normalized};
  return {ok:true, element, selector: normalized};
}
"#
}

pub(super) async fn browser_click_tool(payload: &Value) -> AppResult<String> {
    let cdp_url = cdp_url_from_payload(payload)?;
    let target = browser_target_from_payload(payload, "browser_click")?;
    let target_json = serde_json::to_string(&target)?;
    let resolver_script = browser_target_resolver_script();
    let expression = format!(
        r#"
(() => {{
  const target = {target_json};
  const resolved = ({resolver_script})(target);
  if (!resolved.ok) return resolved;
  const el = resolved.element;
  el.scrollIntoView({{block:"center", inline:"center"}});
  el.click();
  return {{ok:true, target, ref: resolved.ref || "", selector: resolved.selector || "", tag:el.tagName, text:(el.innerText || el.value || "").slice(0,200)}};
}})()
"#
    );
    cdp_evaluate(cdp_url, &expression).await
}

pub(super) async fn browser_type_tool(payload: &Value) -> AppResult<String> {
    let cdp_url = cdp_url_from_payload(payload)?;
    let target = browser_target_from_payload(payload, "browser_type")?;
    let text = payload
        .get("text")
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::BadRequest("browser_type requires payload.text".into()))?;
    let clear = payload
        .get("clear")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let target_json = serde_json::to_string(&target)?;
    let text_json = serde_json::to_string(text)?;
    let resolver_script = browser_target_resolver_script();
    let expression = format!(
        r#"
(() => {{
  const target = {target_json};
  const resolved = ({resolver_script})(target);
  if (!resolved.ok) return resolved;
  const el = resolved.element;
  el.scrollIntoView({{block:"center", inline:"center"}});
  el.focus();
  if ({clear}) el.value = "";
  el.value = (el.value || "") + {text_json};
  el.dispatchEvent(new Event("input", {{bubbles:true}}));
  el.dispatchEvent(new Event("change", {{bubbles:true}}));
  return {{ok:true, target, ref: resolved.ref || "", selector: resolved.selector || "", tag:el.tagName, value:el.value}};
}})()
"#
    );
    cdp_evaluate(cdp_url, &expression).await
}

pub(super) async fn browser_press_tool(payload: &Value) -> AppResult<String> {
    let cdp_url = cdp_url_from_payload(payload)?;
    let key = payload
        .get("key")
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::BadRequest("browser_press requires payload.key".into()))?;
    let params = json!({"type": "keyDown", "key": key});
    send_cdp_message(cdp_url, "Input.dispatchKeyEvent", params).await?;
    let params = json!({"type": "keyUp", "key": key});
    let result = send_cdp_message(cdp_url, "Input.dispatchKeyEvent", params).await?;
    Ok(serde_json::to_string_pretty(
        &json!({"ok": true, "key": key, "result": result}),
    )?)
}

pub(super) async fn browser_scroll_tool(payload: &Value) -> AppResult<String> {
    let cdp_url = cdp_url_from_payload(payload)?;
    let x = payload.get("x").and_then(Value::as_i64).unwrap_or(0);
    let y = payload
        .get("y")
        .and_then(Value::as_i64)
        .or_else(|| {
            payload
                .get("direction")
                .and_then(Value::as_str)
                .map(|direction| {
                    if direction.eq_ignore_ascii_case("up") {
                        -700
                    } else {
                        700
                    }
                })
        })
        .unwrap_or(700);
    let expression = format!(
        "(() => {{ window.scrollBy({}, {}); return {{ok:true, x: window.scrollX, y: window.scrollY}}; }})()",
        x, y
    );
    cdp_evaluate(cdp_url, &expression).await
}

pub(super) async fn browser_dialog_tool(
    store: &AppStore,
    run_id: &str,
    payload: &Value,
) -> AppResult<String> {
    let (cdp_url, supervisor_dialog) = resolve_dialog_cdp_url(store, run_id, payload)?;
    let requested_run_id =
        string_arg(payload, &["runId", "run_id"]).unwrap_or_else(|| run_id.into());
    let result =
        browser_dialog_with_resolved_cdp(&cdp_url, payload, supervisor_dialog.clone()).await?;
    if let Some(dialog) = supervisor_dialog.as_ref() {
        if dialog_bridge_request_id(dialog).is_some() {
            let dialog_id = dialog
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if !dialog_id.is_empty() {
                let accept = dialog_accept_value(payload);
                let prompt_text = dialog_prompt_text(payload, dialog);
                let _ = store.update_browser_supervisor_state(
                    &requested_run_id,
                    None,
                    None,
                    vec![json!({
                        "method": "Supervisor.bridgeDialogFulfilled",
                        "params": {
                            "dialogId": dialog_id,
                            "dialog_id": dialog_id,
                            "accept": accept,
                            "promptText": prompt_text,
                            "prompt_text": prompt_text
                        }
                    })],
                );
            }
        }
    }
    Ok(result)
}

async fn browser_dialog_with_cdp(cdp_url: &str, payload: &Value) -> AppResult<String> {
    browser_dialog_with_resolved_cdp(cdp_url, payload, None).await
}

async fn browser_dialog_with_resolved_cdp(
    cdp_url: &str,
    payload: &Value,
    supervisor_dialog: Option<Value>,
) -> AppResult<String> {
    let accept = dialog_accept_value(payload);
    let prompt_text = supervisor_dialog
        .as_ref()
        .map(|dialog| dialog_prompt_text(payload, dialog))
        .or_else(|| {
            payload
                .get("promptText")
                .or_else(|| payload.get("prompt_text"))
                .and_then(Value::as_str)
                .map(str::to_string)
        });
    if let Some(dialog) = supervisor_dialog.as_ref() {
        if let Some(request_id) = dialog_bridge_request_id(dialog) {
            let prompt_body = if dialog.get("type").and_then(Value::as_str) == Some("prompt") {
                prompt_text.clone().unwrap_or_default()
            } else {
                String::new()
            };
            let body = json!({
                "accept": accept,
                "prompt_text": prompt_body,
                "dialog_id": dialog.get("id").and_then(Value::as_str).unwrap_or_default()
            });
            use base64::Engine;
            let encoded_body =
                base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(&body)?);
            let options = dialog_cdp_call_options(dialog)?;
            let result = send_cdp_message_with_options(
                cdp_url,
                "Fetch.fulfillRequest",
                json!({
                    "requestId": request_id,
                    "responseCode": 200,
                    "responseHeaders": [
                        {"name": "Content-Type", "value": "application/json"},
                        {"name": "Access-Control-Allow-Origin", "value": "*"}
                    ],
                    "body": encoded_body
                }),
                options,
            )
            .await?;
            return Ok(serde_json::to_string_pretty(&json!({
                "ok": true,
                "action": if accept { "accept" } else { "dismiss" },
                "transport": "fetch_bridge",
                "dialog": supervisor_dialog,
                "result": result
            }))?);
        }
    }
    let mut params = json!({"accept": accept});
    if let Some(prompt_text) = prompt_text {
        params["promptText"] = json!(prompt_text);
    }
    let options = supervisor_dialog
        .as_ref()
        .map(dialog_cdp_call_options)
        .transpose()?
        .unwrap_or_default();
    let result =
        send_cdp_message_with_options(cdp_url, "Page.handleJavaScriptDialog", params, options)
            .await?;
    Ok(serde_json::to_string_pretty(&json!({
        "ok": true,
        "action": if accept { "accept" } else { "dismiss" },
        "dialog": supervisor_dialog,
        "result": result
    }))?)
}

fn dialog_accept_value(payload: &Value) -> bool {
    payload
        .get("accept")
        .and_then(Value::as_bool)
        .unwrap_or_else(|| {
            !payload
                .get("action")
                .and_then(Value::as_str)
                .map(|action| action.eq_ignore_ascii_case("dismiss"))
                .unwrap_or(false)
        })
}

fn dialog_prompt_text(payload: &Value, dialog: &Value) -> String {
    payload
        .get("promptText")
        .or_else(|| payload.get("prompt_text"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            dialog
                .get("defaultPrompt")
                .or_else(|| dialog.get("default_prompt"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_default()
}

fn dialog_bridge_request_id(dialog: &Value) -> Option<String> {
    dialog
        .get("bridgeRequestId")
        .or_else(|| dialog.get("bridge_request_id"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
}

fn dialog_cdp_call_options(dialog: &Value) -> AppResult<CdpCallOptions> {
    let session_id = dialog
        .get("sessionId")
        .or_else(|| dialog.get("session_id"))
        .or_else(|| dialog.get("cdpSessionId"))
        .or_else(|| dialog.get("cdp_session_id"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string);
    Ok(CdpCallOptions {
        target_id: None,
        session_id,
    })
}

fn resolve_dialog_cdp_url(
    store: &AppStore,
    run_id: &str,
    payload: &Value,
) -> AppResult<(String, Option<Value>)> {
    if let Ok(cdp_url) = cdp_url_from_payload(payload) {
        return Ok((cdp_url.to_string(), None));
    }
    let requested_run_id = payload
        .get("runId")
        .or_else(|| payload.get("run_id"))
        .and_then(Value::as_str)
        .unwrap_or(run_id);
    let state = store
        .browser_supervisor_state(requested_run_id)?
        .ok_or_else(|| {
            AppError::BadRequest(
                "browser_dialog requires payload.cdpUrl or an active browser supervisor for this run"
                    .into(),
            )
        })?;
    let cdp_url = state
        .get("cdpUrl")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            AppError::BadRequest(
                "browser_dialog supervisor state does not include a usable cdpUrl".into(),
            )
        })?;
    let dialog = resolve_pending_dialog_from_state(&state, payload)?;
    Ok((cdp_url.to_string(), Some(dialog)))
}

fn resolve_pending_dialog_from_state(state: &Value, payload: &Value) -> AppResult<Value> {
    let dialogs = state
        .get("pendingDialogs")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if dialogs.is_empty() {
        return Err(AppError::BadRequest(
            "browser_dialog found no pending dialog in supervisor state; call browser_snapshot or browser_supervisor_state first".into(),
        ));
    }
    let requested_id = payload
        .get("dialogId")
        .or_else(|| payload.get("dialog_id"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty());
    if let Some(requested_id) = requested_id {
        return dialogs
            .into_iter()
            .find(|dialog| dialog.get("id").and_then(Value::as_str) == Some(requested_id))
            .ok_or_else(|| {
                AppError::BadRequest(format!(
                    "browser_dialog dialog_id '{requested_id}' not found in pending dialogs"
                ))
            });
    }
    if dialogs.len() > 1 {
        let ids = dialogs
            .iter()
            .filter_map(|dialog| dialog.get("id").and_then(Value::as_str))
            .collect::<Vec<_>>();
        return Err(AppError::BadRequest(format!(
            "{} pending dialogs; specify payload.dialogId/dialog_id. candidates: {}",
            dialogs.len(),
            ids.join(", ")
        )));
    }
    Ok(dialogs.into_iter().next().unwrap_or(Value::Null))
}

pub(super) async fn browser_record_tool(
    store: &AppStore,
    run_id: &str,
    payload: &Value,
) -> AppResult<String> {
    let action = string_arg(payload, &["action"])
        .unwrap_or_else(|| "status".into())
        .to_ascii_lowercase();
    match action.as_str() {
        "start" => browser_record_start(store, run_id, payload).await,
        "stop" => browser_record_stop(store, run_id, payload),
        "status" | "state" => browser_record_status(store, run_id, payload),
        "export" | "save" => browser_record_export(store, run_id, payload),
        "capabilities" | "capability" => Ok(serde_json::to_string_pretty(&json!({
            "ok": true,
            "action": "capabilities",
            "capabilities": browser_record_capabilities()
        }))?),
        other => Err(AppError::BadRequest(format!(
            "unsupported browser_record action: {other}"
        ))),
    }
}

async fn browser_record_start(
    store: &AppStore,
    run_id: &str,
    payload: &Value,
) -> AppResult<String> {
    let key = browser_record_key(run_id, payload);
    let cdp_url = resolve_browser_record_cdp_url(store, &key, payload)?;
    store.update_browser_supervisor_state(&key, Some(&cdp_url), None, Vec::new())?;
    let mut params = json!({
        "format": string_arg(payload, &["format"]).unwrap_or_else(|| "png".into()),
        "everyNthFrame": payload
            .get("everyNthFrame")
            .or_else(|| payload.get("every_nth_frame"))
            .and_then(Value::as_u64)
            .unwrap_or(1)
            .clamp(1, 60),
    });
    if let Some(quality) = payload.get("quality").and_then(Value::as_u64) {
        params["quality"] = json!(quality.clamp(1, 100));
    }
    let recorders = BROWSER_RECORDERS.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(previous) = recorders
        .lock()
        .map_err(|_| AppError::BadRequest("browser recorder lock poisoned".into()))?
        .remove(&key)
    {
        previous.abort();
    }
    let recorder_store = store.clone();
    let recorder_key = key.clone();
    let recorder_cdp_url = cdp_url.clone();
    let started_at = crate::models::now_iso();
    store.set_browser_supervisor_recording(
        &key,
        json!({
            "active": true,
            "mode": "cdp_screencast",
            "startedAt": started_at,
            "stoppedAt": null,
            "cdpUrl": cdp_url,
            "capabilities": browser_record_capabilities(),
            "note": "CDP screencast frame recording; export saves PNG frame artifacts and a manifest."
        }),
    )?;
    let task = tokio::spawn(async move {
        let _ =
            run_browser_screencast_recorder(recorder_store, recorder_key, recorder_cdp_url, params)
                .await;
    });
    recorders
        .lock()
        .map_err(|_| AppError::BadRequest("browser recorder lock poisoned".into()))?
        .insert(key.clone(), task.abort_handle());
    Ok(serde_json::to_string_pretty(&json!({
        "ok": true,
        "action": "start",
        "runId": key,
        "mode": "cdp_screencast",
        "startedAt": started_at,
        "capabilities": browser_record_capabilities()
    }))?)
}

fn browser_record_stop(store: &AppStore, run_id: &str, payload: &Value) -> AppResult<String> {
    let key = browser_record_key(run_id, payload);
    let stopped = BROWSER_RECORDERS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .map_err(|_| AppError::BadRequest("browser recorder lock poisoned".into()))?
        .remove(&key)
        .map(|handle| {
            handle.abort();
            true
        })
        .unwrap_or(false);
    let state = store
        .browser_supervisor_state(&key)?
        .unwrap_or_else(|| json!({}));
    let previous = state.get("recording").cloned().unwrap_or_else(|| json!({}));
    let stopped_at = crate::models::now_iso();
    store.set_browser_supervisor_recording(
        &key,
        json!({
            "active": false,
            "mode": previous.get("mode").and_then(Value::as_str).unwrap_or("cdp_screencast"),
            "startedAt": previous.get("startedAt").cloned().unwrap_or(Value::Null),
            "stoppedAt": stopped_at,
            "stoppedRecorderTask": stopped
        }),
    )?;
    browser_record_status(store, &key, &json!({"runId": key, "stopped": stopped}))
}

fn browser_record_status(store: &AppStore, run_id: &str, payload: &Value) -> AppResult<String> {
    let key = browser_record_key(run_id, payload);
    let state = store
        .browser_supervisor_state(&key)?
        .unwrap_or_else(|| json!({"runId": key}));
    let active_task = BROWSER_RECORDERS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .map_err(|_| AppError::BadRequest("browser recorder lock poisoned".into()))?
        .contains_key(&key);
    Ok(serde_json::to_string_pretty(&json!({
        "ok": true,
        "action": "status",
        "runId": key,
        "activeTask": active_task,
        "recording": state.get("recording").cloned().unwrap_or(Value::Null),
        "capabilities": browser_record_capabilities(),
        "screencastFrameCount": state.get("screencastFrameCount").cloned().unwrap_or_else(|| json!(0)),
        "recentFrames": state.get("screencastFrames").and_then(Value::as_array).map(|frames| frames.len()).unwrap_or(0),
        "supervisor": summarize_browser_supervisor_state(&state)
    }))?)
}

fn browser_record_export(store: &AppStore, run_id: &str, payload: &Value) -> AppResult<String> {
    let key = browser_record_key(run_id, payload);
    let state = store
        .browser_supervisor_state(&key)?
        .ok_or_else(|| AppError::BadRequest(format!("browser_record found no state for {key}")))?;
    let manifest = browser_record_export_state(store, &key, &state, payload)?;
    Ok(serde_json::to_string_pretty(&json!({
        "ok": true,
        "action": "export",
        "runId": key,
        "manifestPath": manifest.get("manifestPath").and_then(Value::as_str).unwrap_or_default(),
        "frameCount": manifest.get("frames").and_then(Value::as_array).map(|items| items.len()).unwrap_or(0),
        "manifest": manifest
    }))?)
}

fn browser_record_export_state(
    store: &AppStore,
    key: &str,
    state: &Value,
    payload: &Value,
) -> AppResult<Value> {
    let frames = state
        .get("screencastFrames")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if frames.is_empty() {
        return Err(AppError::BadRequest(
            "browser_record export found no screencast frames; call browser_record start first and allow frames to arrive".into(),
        ));
    }
    let max_frames = payload
        .get("maxFrames")
        .or_else(|| payload.get("max_frames"))
        .and_then(Value::as_u64)
        .unwrap_or(12)
        .clamp(1, 24) as usize;
    let selected = frames
        .into_iter()
        .skip(
            state
                .get("screencastFrames")
                .and_then(Value::as_array)
                .map(|items| items.len().saturating_sub(max_frames))
                .unwrap_or(0),
        )
        .collect::<Vec<_>>();
    let mut exported_frames = Vec::new();
    let mut exported_frame_bytes = Vec::new();
    for (index, frame) in selected.iter().enumerate() {
        let data = frame
            .get("data")
            .and_then(Value::as_str)
            .ok_or_else(|| AppError::BadRequest("browser_record frame missing data".into()))?;
        use base64::Engine;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(data)
            .map_err(|error| {
                AppError::BadRequest(format!("invalid screencast frame base64: {error}"))
            })?;
        let path = store.save_tool_binary_artifact(&key, "browser_record_frame", "png", &bytes)?;
        exported_frame_bytes.push(bytes.clone());
        exported_frames.push(json!({
            "index": index,
            "path": path.to_string_lossy(),
            "sizeBytes": bytes.len(),
            "metadata": frame.get("metadata").cloned().unwrap_or(Value::Null),
            "capturedAt": frame.get("capturedAt").cloned().unwrap_or(Value::Null)
        }));
    }
    let video_export = browser_record_export_webm(store, key, payload, &exported_frame_bytes)?;
    let manifest = json!({
        "runId": key,
        "mode": "cdp_screencast",
        "exportedAt": crate::models::now_iso(),
        "recording": state.get("recording").cloned().unwrap_or(Value::Null),
        "exportFormat": if video_export.get("ok").and_then(Value::as_bool) == Some(true) {
            "png_frames_manifest_webm"
        } else {
            "png_frames_manifest"
        },
        "capabilities": browser_record_capabilities(),
        "screencastFrameCount": state.get("screencastFrameCount").cloned().unwrap_or_else(|| json!(0)),
        "frames": exported_frames,
        "video": video_export,
        "networkArchive": state.get("networkArchive").cloned().unwrap_or(Value::Null),
        "consoleErrors": state.get("consoleErrors").cloned().unwrap_or_else(|| json!([])),
    });
    let manifest_path = store.save_tool_artifact(
        &key,
        "browser_record_manifest",
        &serde_json::to_string_pretty(&manifest)?,
    )?;
    let mut manifest = manifest;
    manifest["manifestPath"] = json!(manifest_path.to_string_lossy());
    Ok(manifest)
}

fn browser_record_key(run_id: &str, payload: &Value) -> String {
    string_arg(payload, &["runId", "run_id", "sessionId", "session_id"])
        .unwrap_or_else(|| run_id.to_string())
}

fn browser_record_export_webm(
    store: &AppStore,
    key: &str,
    payload: &Value,
    frames: &[Vec<u8>],
) -> AppResult<Value> {
    let requested_format = string_arg(payload, &["format", "exportFormat", "export_format"])
        .unwrap_or_else(|| "auto".into())
        .to_ascii_lowercase();
    if matches!(requested_format.as_str(), "png" | "png_frames" | "manifest") {
        return Ok(json!({
            "ok": false,
            "format": "webm",
            "skipped": true,
            "reason": "Video export was not requested."
        }));
    }
    if frames.len() < 2 {
        return Ok(json!({
            "ok": false,
            "format": "webm",
            "skipped": true,
            "reason": "At least two frames are required for WebM export."
        }));
    }
    let Some(ffmpeg_path) = find_executable_on_path("ffmpeg") else {
        return Ok(json!({
            "ok": false,
            "format": "webm",
            "skipped": true,
            "reason": "ffmpeg was not found on PATH."
        }));
    };
    let fps = payload
        .get("fps")
        .or_else(|| payload.get("framerate"))
        .or_else(|| payload.get("frameRate"))
        .and_then(Value::as_u64)
        .unwrap_or(4)
        .clamp(1, 30);
    let temp_dir =
        std::env::temp_dir().join(format!("synthchat-browser-record-{}", new_id("webm")));
    fs::create_dir_all(&temp_dir)?;
    let output_path = temp_dir.join("recording.webm");
    for (index, bytes) in frames.iter().enumerate() {
        fs::write(temp_dir.join(format!("frame-{index:06}.png")), bytes)?;
    }
    let mut command = Command::new(&ffmpeg_path);
    command.hide_window();
    let output = command
        .arg("-y")
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-framerate")
        .arg(fps.to_string())
        .arg("-i")
        .arg(temp_dir.join("frame-%06d.png"))
        .arg("-c:v")
        .arg("libvpx-vp9")
        .arg("-pix_fmt")
        .arg("yuv420p")
        .arg(&output_path)
        .output();
    let result = match output {
        Ok(output) if output.status.success() => {
            let bytes = fs::read(&output_path)?;
            let artifact_path =
                store.save_tool_binary_artifact(key, "browser_record", "webm", &bytes)?;
            json!({
                "ok": true,
                "format": "webm",
                "path": artifact_path.to_string_lossy(),
                "sizeBytes": bytes.len(),
                "frameCount": frames.len(),
                "fps": fps,
                "ffmpegPath": ffmpeg_path.to_string_lossy()
            })
        }
        Ok(output) => json!({
            "ok": false,
            "format": "webm",
            "ffmpegPath": ffmpeg_path.to_string_lossy(),
            "error": truncate_output(&String::from_utf8_lossy(&output.stderr), 2000)
        }),
        Err(error) => json!({
            "ok": false,
            "format": "webm",
            "ffmpegPath": ffmpeg_path.to_string_lossy(),
            "error": error.to_string()
        }),
    };
    let _ = fs::remove_dir_all(&temp_dir);
    Ok(result)
}

fn browser_record_capabilities() -> Value {
    let ffmpeg_path = find_executable_on_path("ffmpeg");
    json!({
        "captureMode": "cdp_screencast",
        "exportFormats": ["png_frames_manifest", "webm"],
        "videoExport": {
            "webm": {
                "supported": ffmpeg_path.is_some(),
                "ffmpegAvailable": ffmpeg_path.is_some(),
                "ffmpegPath": ffmpeg_path
                    .as_ref()
                    .map(|path| path.to_string_lossy().to_string())
                    .unwrap_or_default(),
                "reason": if ffmpeg_path.is_some() {
                    Value::Null
                } else {
                    json!("ffmpeg was not found on PATH; export still saves PNG frames plus a JSON manifest.")
                }
            }
        }
    })
}

fn find_executable_on_path(name: &str) -> Option<std::path::PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    let extensions = executable_extensions();
    for dir in std::env::split_paths(&path_var) {
        for extension in &extensions {
            let candidate = dir.join(format!("{name}{extension}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

fn executable_extensions() -> Vec<String> {
    #[cfg(windows)]
    {
        let mut extensions = std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".EXE;.CMD;.BAT;.COM".into())
            .split(';')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| {
                if value.starts_with('.') {
                    value.to_ascii_lowercase()
                } else {
                    format!(".{}", value.to_ascii_lowercase())
                }
            })
            .collect::<Vec<_>>();
        extensions.insert(0, String::new());
        extensions
    }
    #[cfg(not(windows))]
    {
        vec![String::new()]
    }
}

fn resolve_browser_record_cdp_url(
    store: &AppStore,
    key: &str,
    payload: &Value,
) -> AppResult<String> {
    if let Ok(cdp_url) = cdp_url_from_payload(payload) {
        return Ok(cdp_url.to_string());
    }
    store
        .browser_supervisor_state(key)?
        .and_then(|state| {
            state
                .get("cdpUrl")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .map(str::to_string)
        })
        .ok_or_else(|| {
            AppError::BadRequest(
                "browser_record start requires payload.cdpUrl or an active browser supervisor for this run/session".into(),
            )
        })
}

async fn run_browser_screencast_recorder(
    store: AppStore,
    key: String,
    cdp_url: String,
    params: Value,
) -> AppResult<()> {
    let (mut ws, _) = connect_async(&cdp_url).await.map_err(|error| {
        AppError::BadRequest(format!("browser_record CDP connect failed: {error}"))
    })?;
    let mut next_id = 1_u64;
    send_cdp_request(&mut ws, &mut next_id, "Page.enable", json!({})).await?;
    send_cdp_request(&mut ws, &mut next_id, "Page.startScreencast", params).await?;
    while let Some(message) = ws.next().await {
        let message = message.map_err(|error| {
            AppError::BadRequest(format!("browser_record CDP receive failed: {error}"))
        })?;
        let Message::Text(text) = message else {
            continue;
        };
        let value = serde_json::from_str::<Value>(&text)?;
        if value.get("method").and_then(Value::as_str) != Some("Page.screencastFrame") {
            continue;
        }
        if let Some(session_id) = value
            .get("params")
            .and_then(|params| params.get("sessionId"))
            .and_then(Value::as_u64)
        {
            let _ = send_cdp_fire_and_forget(
                &mut ws,
                &mut next_id,
                "Page.screencastFrameAck",
                json!({"sessionId": session_id}),
            )
            .await;
        }
        let _ = store.update_browser_supervisor_state(&key, Some(&cdp_url), None, vec![value]);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_store() -> AppStore {
        let dir = std::env::temp_dir().join(new_id("synthchat-browser-test"));
        std::fs::create_dir_all(&dir).unwrap();
        AppStore::new(dir.join("state.json")).unwrap()
    }

    fn test_browser_provider(
        id: &str,
        provider_type: &str,
        enabled: bool,
        api_key: Option<&str>,
    ) -> BrowserProvider {
        BrowserProvider {
            id: id.into(),
            name: id.into(),
            provider_type: provider_type.into(),
            base_url: format!("https://{id}.example.test"),
            api_key_env: format!("{}_API_KEY", id.to_ascii_uppercase().replace('-', "_")),
            api_key: api_key.map(str::to_string),
            project_id: String::new(),
            record_sessions: false,
            enabled,
            timeout_seconds: 30,
        }
    }

    #[test]
    fn browser_provider_resolution_preserves_explicit_unavailable_provider() {
        let providers = vec![test_browser_provider(
            "browserbase-main",
            "browserbase",
            true,
            None,
        )];

        let status = browser_provider_registry_status(&providers, Some("browserbase"));

        assert_eq!(
            status["hermesResolutionReason"].as_str(),
            Some("explicit_config")
        );
        assert_eq!(
            status["hermesResolvedProvider"]["providerType"].as_str(),
            Some("browserbase")
        );
        assert_eq!(
            status["hermesResolvedProvider"]["available"].as_bool(),
            Some(false)
        );
    }

    #[test]
    fn browser_provider_resolution_does_not_auto_pick_firecrawl() {
        let providers = vec![test_browser_provider(
            "firecrawl-main",
            "firecrawl",
            true,
            Some("key"),
        )];

        let status = browser_provider_registry_status(&providers, None);

        assert_eq!(
            status["hermesResolutionReason"].as_str(),
            Some("no_available_legacy_provider")
        );
        assert!(status["hermesResolvedProvider"].is_null());
        assert!(status["activeProvider"].is_null());
        assert_eq!(
            status["synthchatEnabledProvider"]["providerType"].as_str(),
            Some("firecrawl")
        );
    }

    #[test]
    fn browser_session_resolution_uses_hermes_legacy_order() {
        let providers = vec![
            test_browser_provider("firecrawl-main", "firecrawl", true, Some("key")),
            test_browser_provider("browserbase-main", "browserbase", true, Some("key")),
            test_browser_provider("browser-use-main", "browser-use", true, Some("key")),
        ];

        let (provider, reason) =
            resolve_browser_provider_for_session(&providers, &json!({})).unwrap();
        assert_eq!(provider.provider_type, "browser-use");
        assert_eq!(reason, "legacy_available");

        let (provider, reason) = resolve_browser_provider_for_session(
            &providers,
            &json!({"provider": "firecrawl-main"}),
        )
        .unwrap();
        assert_eq!(provider.provider_type, "firecrawl");
        assert_eq!(reason, "explicit_config");
    }

    #[test]
    fn browser_provider_lifecycle_previews_browserbase_contract() {
        let provider = BrowserProvider {
            base_url: "https://api.browserbase.com/v1".into(),
            project_id: "project-123".into(),
            ..test_browser_provider("browserbase-main", "browserbase", true, Some("key"))
        };

        let preview = browser_provider_lifecycle_preview(&provider, "test");

        assert_eq!(
            preview["create"]["url"].as_str(),
            Some("https://api.browserbase.com/v1/sessions")
        );
        assert_eq!(
            preview["create"]["auth"]["header"].as_str(),
            Some("x-bb-api-key")
        );
        assert_eq!(
            preview["create"]["body"]["projectId"].as_str(),
            Some("project-123")
        );
        assert_eq!(preview["close"]["method"].as_str(), Some("PATCH"));
        assert_eq!(
            preview["close"]["body"]["status"].as_str(),
            Some("REQUEST_RELEASE")
        );
    }

    #[test]
    fn browser_provider_lifecycle_previews_browser_use_contract() {
        let provider = BrowserProvider {
            base_url: "https://api.browser-use.com".into(),
            ..test_browser_provider("browser-use-main", "browser-use", true, Some("key"))
        };

        let preview = browser_provider_lifecycle_preview(&provider, "test");

        assert_eq!(
            preview["create"]["url"].as_str(),
            Some("https://api.browser-use.com/browsers")
        );
        assert_eq!(
            preview["create"]["auth"]["header"].as_str(),
            Some("X-Browser-Use-API-Key")
        );
        assert_eq!(
            preview["create"]["auth"]["value"].as_str(),
            Some("<redacted>")
        );
        assert_eq!(preview["close"]["method"].as_str(), Some("PATCH"));
        assert_eq!(
            preview["close"]["url"].as_str(),
            Some("https://api.browser-use.com/browsers/diagnostic-session")
        );
        assert_eq!(preview["close"]["body"]["action"].as_str(), Some("stop"));
    }

    #[test]
    fn browser_provider_lifecycle_reports_missing_credentials() {
        let provider = test_browser_provider("browserbase-main", "browserbase", true, None);

        let preview = browser_provider_lifecycle_preview(&provider, "test");
        let codes = preview["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|item| item.get("code").and_then(Value::as_str))
            .collect::<Vec<_>>();

        assert!(codes.contains(&"missing_credentials"));
        assert_eq!(
            preview["create"]["auth"]["credentialConfigured"].as_bool(),
            Some(false)
        );
    }

    #[test]
    fn browser_record_capabilities_report_webm_support_from_ffmpeg() {
        let capabilities = browser_record_capabilities();

        assert_eq!(capabilities["captureMode"].as_str(), Some("cdp_screencast"));
        assert_eq!(
            capabilities["exportFormats"][0].as_str(),
            Some("png_frames_manifest")
        );
        assert_eq!(capabilities["exportFormats"][1].as_str(), Some("webm"));
        let ffmpeg_available = find_executable_on_path("ffmpeg").is_some();
        assert_eq!(
            capabilities["videoExport"]["webm"]["supported"].as_bool(),
            Some(ffmpeg_available)
        );
    }

    #[test]
    fn resolves_single_pending_dialog_without_id() {
        let state = json!({
            "pendingDialogs": [
                {"id": "d-1", "type": "alert", "message": "hello"}
            ]
        });

        let dialog = resolve_pending_dialog_from_state(&state, &json!({})).unwrap();

        assert_eq!(dialog.get("id").and_then(Value::as_str), Some("d-1"));
    }

    #[test]
    fn requires_dialog_id_when_multiple_pending_dialogs_exist() {
        let state = json!({
            "pendingDialogs": [
                {"id": "d-1", "type": "alert"},
                {"id": "d-2", "type": "confirm"}
            ]
        });

        let error = resolve_pending_dialog_from_state(&state, &json!({}))
            .expect_err("multiple dialogs should require dialogId");

        assert!(error.to_string().contains("specify payload.dialogId"));
    }

    #[test]
    fn extracts_raw_cdp_target_and_session_options() {
        let store = test_store();
        let options = cdp_call_options_from_payload(
            &store,
            "run-1",
            &json!({
            "target_id": "target-1",
            "sessionId": "session-1"
            }),
        )
        .unwrap();

        assert_eq!(options.target_id.as_deref(), Some("target-1"));
        assert_eq!(options.session_id.as_deref(), Some("session-1"));
    }

    #[test]
    fn resolves_raw_cdp_frame_id_to_supervisor_session() {
        let store = test_store();
        store
            .update_browser_supervisor_state(
                "run-frame",
                Some("ws://example.test/devtools/browser/1"),
                None,
                vec![json!({
                    "method": "Target.attachedToTarget",
                    "params": {
                        "sessionId": "session-oopif",
                        "targetInfo": {
                            "targetId": "frame-oopif",
                            "type": "iframe",
                            "url": "https://child.example.test/"
                        }
                    }
                })],
            )
            .unwrap();

        let options =
            cdp_call_options_from_payload(&store, "run-frame", &json!({"frame_id": "frame-oopif"}))
                .unwrap();

        assert_eq!(options.target_id, None);
        assert_eq!(options.session_id.as_deref(), Some("session-oopif"));
    }

    #[test]
    fn dialog_cdp_options_use_dialog_session() {
        let options = dialog_cdp_call_options(&json!({
            "id": "d-1",
            "cdp_session_id": "session-dialog"
        }))
        .unwrap();

        assert_eq!(options.target_id, None);
        assert_eq!(options.session_id.as_deref(), Some("session-dialog"));
    }

    #[test]
    fn bridge_dialog_helpers_extract_request_session_and_prompt() {
        let dialog = json!({
            "id": "d-bridge",
            "type": "prompt",
            "bridge_request_id": "request-1",
            "cdp_session_id": "session-dialog",
            "default_prompt": "seed"
        });

        assert_eq!(
            dialog_bridge_request_id(&dialog).as_deref(),
            Some("request-1")
        );
        assert!(dialog_accept_value(&json!({"action": "accept"})));
        assert!(!dialog_accept_value(&json!({"action": "dismiss"})));
        assert_eq!(dialog_prompt_text(&json!({}), &dialog), "seed");
        assert_eq!(
            dialog_prompt_text(&json!({"promptText": "typed"}), &dialog),
            "typed"
        );
        let options = dialog_cdp_call_options(&dialog).unwrap();
        assert_eq!(options.session_id.as_deref(), Some("session-dialog"));
    }

    #[test]
    fn browser_supervisor_config_accepts_hermes_dialog_policy() {
        let config = browser_supervisor_config_from_payload(&json!({
            "dialog_policy": "auto_dismiss",
            "dialog_timeout_s": 42.5
        }));

        assert_eq!(config["dialogPolicy"].as_str(), Some("auto_dismiss"));
        assert_eq!(config["dialog_policy"].as_str(), Some("auto_dismiss"));
        assert_eq!(config["dialogTimeoutSeconds"].as_f64(), Some(42.5));
        assert_eq!(config["dialog_timeout_s"].as_f64(), Some(42.5));
    }

    #[test]
    fn browser_supervisor_config_falls_back_to_hermes_defaults() {
        let config = browser_supervisor_config_from_payload(&json!({
            "dialogPolicy": "invalid",
            "dialogTimeoutSeconds": -1
        }));

        assert_eq!(config["dialogPolicy"].as_str(), Some("must_respond"));
        assert_eq!(config["dialogTimeoutSeconds"].as_f64(), Some(300.0));
    }

    #[test]
    fn detects_cdp_evaluate_serialization_errors() {
        assert!(is_cdp_evaluate_serialization_error(
            "CDP Runtime.evaluate failed: Object reference chain is too long"
        ));
        assert!(cdp_evaluate_serialization_guidance().contains("browser_snapshot"));
    }

    #[test]
    fn browser_console_formats_supervisor_history_like_hermes() {
        let state = json!({
            "consoleHistory": [
                {
                    "method": "Runtime.consoleAPICalled",
                    "level": "log",
                    "text": "ready"
                },
                {
                    "method": "Runtime.exceptionThrown",
                    "level": "exception",
                    "text": "boom"
                }
            ]
        });
        let messages = browser_console_messages_from_state(&state);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["type"].as_str(), Some("log"));
        assert_eq!(messages[0]["source"].as_str(), Some("console"));
        assert_eq!(messages[1]["source"].as_str(), Some("exception"));
    }
}

pub(super) async fn browser_vision_tool(
    store: &AppStore,
    agent: &AgentDefinition,
    run_id: &str,
    payload: &Value,
) -> AppResult<String> {
    let cdp_url = cdp_url_from_payload(payload)?;
    let question = string_arg(payload, &["question", "prompt"]).unwrap_or_else(|| {
        "Describe the visible browser page and call out important UI state.".into()
    });
    let format = browser_screenshot_format(payload);
    let screenshot = capture_browser_screenshot(&cdp_url, payload).await?;
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&screenshot)
        .map_err(|error| {
            AppError::BadRequest(format!("invalid browser screenshot base64: {error}"))
        })?;
    let path = store.save_tool_binary_artifact(run_id, "browser_vision", &format, &bytes)?;
    let mime = if format == "jpeg" {
        "image/jpeg"
    } else {
        "image/png"
    };
    let image_url = format!("data:{mime};base64,{screenshot}");
    let Some(provider) = resolve_vision_provider(store)? else {
        return Ok(serde_json::to_string_pretty(&json!({
            "ok": false,
            "screenshotPath": path.to_string_lossy(),
            "format": format,
            "sizeBytes": bytes.len(),
            "error": "no enabled vision provider configured"
        }))?);
    };
    match provider.provider_type.trim().to_lowercase().as_str() {
        "openai" | "openai-compatible" | "openai_compatible" | "compatible" | "custom" | "" => {
            let analysis = openai_compatible_vision_analyze(
                store, agent, run_id, &provider, &question, &image_url, payload,
            )
            .await;
            match analysis {
                Ok(text) => Ok(serde_json::to_string_pretty(&json!({
                    "ok": true,
                    "screenshotPath": path.to_string_lossy(),
                    "format": format,
                    "sizeBytes": bytes.len(),
                    "vision": serde_json::from_str::<Value>(&text).unwrap_or_else(|_| json!({"analysis": text}))
                }))?),
                Err(error) => Ok(serde_json::to_string_pretty(&json!({
                    "ok": false,
                    "screenshotPath": path.to_string_lossy(),
                    "format": format,
                    "sizeBytes": bytes.len(),
                    "error": error.to_string()
                }))?),
            }
        }
        other => Ok(serde_json::to_string_pretty(&json!({
            "ok": false,
            "screenshotPath": path.to_string_lossy(),
            "format": format,
            "sizeBytes": bytes.len(),
            "error": format!("unsupported vision provider type: {other}")
        }))?),
    }
}

pub(super) fn browser_screenshot_format(payload: &Value) -> String {
    let format = payload
        .get("format")
        .and_then(Value::as_str)
        .unwrap_or("png")
        .trim()
        .to_lowercase();
    if format == "jpeg" || format == "jpg" {
        "jpeg".into()
    } else {
        "png".into()
    }
}

async fn capture_browser_screenshot(cdp_url: &str, payload: &Value) -> AppResult<String> {
    let image_format = browser_screenshot_format(payload);
    let mut params = json!({"format": image_format});
    if image_format == "jpeg" {
        if let Some(quality) = payload.get("quality").and_then(Value::as_u64) {
            params["quality"] = json!(quality.clamp(1, 100));
        }
    }
    if payload
        .get("fullPage")
        .or_else(|| payload.get("full_page"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        params["captureBeyondViewport"] = json!(true);
    }
    let result = send_cdp_message(cdp_url, "Page.captureScreenshot", params).await?;
    result
        .get("data")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| AppError::BadRequest("browser screenshot response missing data".into()))
}

pub(super) async fn browser_console_tool(
    store: &AppStore,
    run_id: &str,
    payload: &Value,
) -> AppResult<String> {
    if let Some(expression) = payload.get("expression").and_then(Value::as_str) {
        let cdp_url = cdp_url_from_payload(payload)?;
        return cdp_evaluate(cdp_url, expression).await;
    }
    let cdp_url = cdp_url_from_payload(payload).ok();
    browser_console_from_supervisor_state(store, run_id, payload, cdp_url)
}

fn browser_console_from_supervisor_state(
    store: &AppStore,
    run_id: &str,
    payload: &Value,
    cdp_url: Option<&str>,
) -> AppResult<String> {
    let requested_run_id =
        string_arg(payload, &["runId", "run_id"]).unwrap_or_else(|| run_id.into());
    let session_id = string_arg(payload, &["sessionId", "session_id"]);
    let state = if let Some(session_id) = session_id.as_deref() {
        store.browser_supervisor_state_for_session(session_id)?
    } else {
        store.browser_supervisor_state(&requested_run_id)?
    };
    let Some(state) = state else {
        return Ok(serde_json::to_string_pretty(&json!({
            "success": false,
            "error": "no active browser supervisor console buffer; call browser_supervisor_register or browser_create_session first, or pass browser_console.expression to evaluate JavaScript",
            "runId": requested_run_id,
            "sessionId": session_id,
            "cdpUrl": cdp_url.unwrap_or("")
        }))?);
    };
    let console_messages = browser_console_messages_from_state(&state);
    let js_errors = state
        .get("consoleErrors")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let clear = payload
        .get("clear")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let cleared = if clear {
        store
            .clear_browser_supervisor_console(session_id.as_deref(), Some(&requested_run_id))?
            .is_some()
    } else {
        false
    };
    Ok(serde_json::to_string_pretty(&json!({
        "success": true,
        "console_messages": console_messages,
        "js_errors": js_errors,
        "total_messages": console_messages.len(),
        "total_errors": js_errors.len(),
        "cleared": cleared,
        "runId": state.get("runId").cloned().unwrap_or_else(|| json!(requested_run_id)),
        "sessionId": state.get("sessionId").cloned().unwrap_or_else(|| json!(session_id.unwrap_or_default())),
    }))?)
}

fn browser_console_messages_from_state(state: &Value) -> Vec<Value> {
    state
        .get("consoleHistory")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|entry| {
            json!({
                "type": entry.get("level").and_then(Value::as_str).unwrap_or("log"),
                "text": entry.get("text").and_then(Value::as_str).unwrap_or(""),
                "source": if entry.get("method").and_then(Value::as_str) == Some("Runtime.exceptionThrown") { "exception" } else { "console" },
                "raw": entry
            })
        })
        .collect()
}

pub(super) async fn browser_supervisor_register_tool(
    store: &AppStore,
    run_id: &str,
    payload: &Value,
) -> AppResult<String> {
    let cdp_url = cdp_url_from_payload(payload)?;
    let state = register_browser_supervisor(store, run_id, payload, cdp_url)?;
    Ok(serde_json::to_string_pretty(&state)?)
}

pub(super) async fn browser_supervisor_state_tool(
    store: &AppStore,
    run_id: &str,
    payload: &Value,
) -> AppResult<String> {
    let requested_run_id = payload
        .get("runId")
        .or_else(|| payload.get("run_id"))
        .and_then(Value::as_str)
        .unwrap_or(run_id);
    let state = store.browser_supervisor_state(requested_run_id)?;
    Ok(serde_json::to_string_pretty(&json!({
        "runId": requested_run_id,
        "state": state,
        "summary": state.as_ref().map(summarize_browser_supervisor_state),
        "capabilities": browser_supervisor_capabilities()
    }))?)
}

pub(super) async fn browser_supervisor_remove_tool(
    store: &AppStore,
    payload: &Value,
) -> AppResult<String> {
    let session_id = payload
        .get("sessionId")
        .or_else(|| payload.get("session_id"))
        .and_then(Value::as_str)
        .ok_or_else(|| {
            AppError::BadRequest("browser_supervisor_remove requires payload.sessionId".into())
        })?;
    let removed = store.remove_browser_supervisor_session(session_id)?;
    Ok(serde_json::to_string_pretty(&json!({
        "sessionId": session_id,
        "removed": removed
    }))?)
}

fn register_browser_supervisor(
    store: &AppStore,
    run_id: &str,
    payload: &Value,
    cdp_url: &str,
) -> AppResult<Value> {
    let session_id = payload
        .get("sessionId")
        .or_else(|| payload.get("session_id"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| new_id("browser-session"));
    let provider_type = payload
        .get("providerType")
        .or_else(|| payload.get("provider_type"))
        .and_then(Value::as_str)
        .unwrap_or("cdp");
    store.register_browser_supervisor_session_with_config(
        run_id,
        &session_id,
        cdp_url,
        provider_type,
        Some(browser_supervisor_config_from_payload(payload)),
    )
}

fn browser_supervisor_capabilities() -> Value {
    json!({
        "dialogPolicies": ["must_respond", "auto_dismiss", "auto_accept"],
        "defaultDialogPolicy": "must_respond",
        "defaultDialogTimeoutSeconds": 300.0,
        "stateFields": [
            "pendingDialogs",
            "recentDialogs",
            "frameTree",
            "frameSessions",
            "consoleHistory",
            "consoleErrors",
            "networkArchive",
            "screencastFrames"
        ],
        "hermesStyleFields": [
            "pending_dialogs",
            "recent_dialogs",
            "frame_tree",
            "frame_sessions",
            "dialog_policy",
            "dialog_timeout_s"
        ],
        "notes": [
            "Policy defaults and auto_accept/auto_dismiss behavior mirror Hermes CDPSupervisor for native and bridge-captured dialogs.",
            "browser_dialog can answer pending dialogs captured by the supervisor."
        ]
    })
}

fn browser_supervisor_config_from_payload(payload: &Value) -> Value {
    let raw_policy = payload
        .get("dialogPolicy")
        .or_else(|| payload.get("dialog_policy"))
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("must_respond");
    let policy = match raw_policy {
        "must_respond" | "auto_dismiss" | "auto_accept" => raw_policy,
        _ => "must_respond",
    };
    let timeout = payload
        .get("dialogTimeoutSeconds")
        .or_else(|| payload.get("dialog_timeout_s"))
        .or_else(|| payload.get("dialogTimeoutS"))
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite() && *value > 0.0)
        .unwrap_or(300.0)
        .clamp(1.0, 21_600.0);
    json!({
        "dialogPolicy": policy,
        "dialog_policy": policy,
        "dialogTimeoutSeconds": timeout,
        "dialog_timeout_s": timeout
    })
}

pub(super) fn cdp_url_from_payload(payload: &Value) -> AppResult<&str> {
    find_browser_cdp_url_value(payload)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .filter(|value| value.starts_with("ws://") || value.starts_with("wss://"))
        .ok_or_else(|| AppError::BadRequest("CDP browser tool requires payload.cdpUrl".into()))
}

fn find_browser_cdp_url_value(value: &Value) -> Option<&Value> {
    match value {
        Value::Object(map) => {
            for key in [
                "cdpUrl",
                "cdp_url",
                "connectUrl",
                "connect_url",
                "webSocketDebuggerUrl",
                "wsUrl",
                "ws_url",
            ] {
                if let Some(found) = map.get(key) {
                    return Some(found);
                }
            }
            map.values().find_map(find_browser_cdp_url_value)
        }
        Value::Array(items) => items.iter().find_map(find_browser_cdp_url_value),
        _ => None,
    }
}

async fn cdp_evaluate(cdp_url: &str, expression: &str) -> AppResult<String> {
    let result = match send_cdp_message(
        cdp_url,
        "Runtime.evaluate",
        json!({"expression": expression, "awaitPromise": true, "returnByValue": true}),
    )
    .await
    {
        Ok(result) => result,
        Err(error) if is_cdp_evaluate_serialization_error(&error.to_string()) => {
            match send_cdp_message(
                cdp_url,
                "Runtime.evaluate",
                json!({"expression": expression, "awaitPromise": true, "returnByValue": false}),
            )
            .await
            {
                Ok(result) => result,
                Err(_) => {
                    return Ok(serde_json::to_string_pretty(&json!({
                        "success": false,
                        "error": cdp_evaluate_serialization_guidance()
                    }))?)
                }
            }
        }
        Err(error) => return Err(error),
    };
    Ok(serde_json::to_string_pretty(&result)?)
}

fn is_cdp_evaluate_serialization_error(error: &str) -> bool {
    let lowered = error.to_lowercase();
    lowered.contains("reference chain is too long")
        || lowered.contains("object reference chain")
        || lowered.contains("could not serialize")
}

fn cdp_evaluate_serialization_guidance() -> &'static str {
    "Expression returned a live DOM node / NodeList / Window that cannot be serialized. Extract a primitive value such as .innerText, .href, .src, or .value, or use JSON.stringify() / browser_snapshot instead."
}

#[derive(Default)]
struct CdpCallOptions {
    target_id: Option<String>,
    session_id: Option<String>,
}

fn cdp_call_options_from_payload(
    store: &AppStore,
    run_id: &str,
    payload: &Value,
) -> AppResult<CdpCallOptions> {
    let target_id = string_arg(payload, &["targetId", "target_id"]);
    let session_id = string_arg(payload, &["sessionId", "session_id"]);
    if target_id.is_some() || session_id.is_some() {
        return Ok(CdpCallOptions {
            target_id,
            session_id,
        });
    }
    let frame_id = string_arg(payload, &["frameId", "frame_id"]);
    let Some(frame_id) = frame_id else {
        return Ok(CdpCallOptions::default());
    };
    let supervisor_run_id =
        string_arg(payload, &["runId", "run_id"]).unwrap_or_else(|| run_id.into());
    let state = store
        .browser_supervisor_state(&supervisor_run_id)?
        .ok_or_else(|| {
            AppError::BadRequest(format!(
                "browser_cdp frameId '{frame_id}' requires an active browser supervisor for run '{supervisor_run_id}'"
            ))
        })?;
    let frame = find_browser_supervisor_frame(&state, &frame_id).ok_or_else(|| {
        AppError::BadRequest(format!(
            "browser_cdp frameId '{frame_id}' not found in supervisor frameTree; call browser_snapshot or browser_supervisor_state first"
        ))
    })?;
    let Some(session_id) = frame
        .get("session_id")
        .or_else(|| frame.get("sessionId"))
        .or_else(|| frame.get("cdp_session_id"))
        .or_else(|| frame.get("cdpSessionId"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
    else {
        return Err(AppError::BadRequest(format!(
            "browser_cdp frameId '{frame_id}' has no CDP session in supervisor frameTree; for same-process frames use top-level Runtime.evaluate, or pass targetId/sessionId for an OOPIF target"
        )));
    };
    Ok(CdpCallOptions {
        target_id: None,
        session_id: Some(session_id.to_string()),
    })
}

fn find_browser_supervisor_frame(state: &Value, frame_id: &str) -> Option<Value> {
    if let Some(sessions) = state.get("frameSessions").and_then(Value::as_array) {
        for session in sessions {
            if session
                .get("frameId")
                .or_else(|| session.get("frame_id"))
                .and_then(Value::as_str)
                == Some(frame_id)
            {
                return Some(session.clone());
            }
        }
    }
    let frame_tree = state
        .get("frameTree")
        .or_else(|| state.get("frame_tree"))
        .unwrap_or(state);
    find_browser_frame_value(frame_tree, frame_id)
}

fn find_browser_frame_value(value: &Value, frame_id: &str) -> Option<Value> {
    if let Some(frame) = value.get("frame") {
        if frame
            .get("id")
            .or_else(|| frame.get("frameId"))
            .or_else(|| frame.get("frame_id"))
            .and_then(Value::as_str)
            == Some(frame_id)
        {
            return Some(frame.clone());
        }
    }
    if value
        .get("frame_id")
        .or_else(|| value.get("frameId"))
        .or_else(|| value.get("id"))
        .and_then(Value::as_str)
        == Some(frame_id)
    {
        return Some(value.clone());
    }
    for key in ["children", "childFrames"] {
        if let Some(children) = value.get(key).and_then(Value::as_array) {
            for child in children {
                if let Some(found) = find_browser_frame_value(child, frame_id) {
                    return Some(found);
                }
            }
        }
    }
    if let Some(object) = value.as_object() {
        for child in object.values() {
            if child.is_object() {
                if let Some(found) = find_browser_frame_value(child, frame_id) {
                    return Some(found);
                }
            }
        }
    }
    None
}

async fn send_cdp_message(cdp_url: &str, method: &str, params: Value) -> AppResult<Value> {
    send_cdp_message_with_options(cdp_url, method, params, CdpCallOptions::default()).await
}

async fn send_cdp_request(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    next_id: &mut u64,
    method: &str,
    params: Value,
) -> AppResult<Value> {
    let id = *next_id;
    *next_id += 1;
    ws.send(Message::Text(
        json!({"id": id, "method": method, "params": params}).to_string(),
    ))
    .await
    .map_err(|error| AppError::BadRequest(format!("CDP send failed: {error}")))?;
    wait_for_cdp_response(ws, id, method).await
}

async fn send_cdp_fire_and_forget(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    next_id: &mut u64,
    method: &str,
    params: Value,
) -> AppResult<()> {
    let id = *next_id;
    *next_id += 1;
    ws.send(Message::Text(
        json!({"id": id, "method": method, "params": params}).to_string(),
    ))
    .await
    .map_err(|error| AppError::BadRequest(format!("CDP send failed: {error}")))?;
    Ok(())
}

async fn send_cdp_message_with_options(
    cdp_url: &str,
    method: &str,
    params: Value,
    options: CdpCallOptions,
) -> AppResult<Value> {
    let (mut ws, _) = connect_async(cdp_url)
        .await
        .map_err(|error| AppError::BadRequest(format!("CDP connect failed: {error}")))?;
    let mut next_id = 1_u64;
    let mut session_id = options.session_id;
    if let Some(target_id) = options.target_id {
        let attach_id = next_id;
        next_id += 1;
        ws.send(Message::Text(
            json!({
                "id": attach_id,
                "method": "Target.attachToTarget",
                "params": {"targetId": target_id, "flatten": true}
            })
            .to_string(),
        ))
        .await
        .map_err(|error| AppError::BadRequest(format!("CDP attach send failed: {error}")))?;
        session_id = Some(
            wait_for_cdp_response(&mut ws, attach_id, "Target.attachToTarget")
                .await?
                .get("sessionId")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    AppError::BadRequest(
                        "CDP Target.attachToTarget response missing sessionId".into(),
                    )
                })?
                .to_string(),
        );
    }
    let id = next_id;
    let mut request = json!({"id": id, "method": method, "params": params});
    if let Some(session_id) = session_id {
        request["sessionId"] = json!(session_id);
    }
    ws.send(Message::Text(request.to_string()))
        .await
        .map_err(|error| AppError::BadRequest(format!("CDP send failed: {error}")))?;
    wait_for_cdp_response(&mut ws, id, method).await
}

async fn wait_for_cdp_response(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    id: u64,
    method: &str,
) -> AppResult<Value> {
    while let Some(message) = ws.next().await {
        let message = message
            .map_err(|error| AppError::BadRequest(format!("CDP receive failed: {error}")))?;
        let Message::Text(text) = message else {
            continue;
        };
        let value = serde_json::from_str::<Value>(&text)?;
        if value.get("id").and_then(Value::as_u64) != Some(id) {
            continue;
        }
        if let Some(error) = value.get("error") {
            return Err(AppError::BadRequest(format!(
                "CDP {method} failed: {error}"
            )));
        }
        return Ok(value.get("result").cloned().unwrap_or_else(|| json!({})));
    }
    Err(AppError::BadRequest(format!(
        "CDP connection closed before {method} returned"
    )))
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
    let summary = summarize_browser_supervisor_state(&state);
    Ok(format!(
        "{snapshot}\n\nsupervisorState:\n{}",
        serde_json::to_string_pretty(&summary)?
    ))
}

fn remember_browser_url(agent: &AgentDefinition, url: &str) -> AppResult<()> {
    let state = LAST_BROWSER_URLS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut state = state
        .lock()
        .map_err(|_| AppError::BadRequest("browser state lock poisoned".into()))?;
    state.insert(agent.id.clone(), url.to_string());
    let histories = BROWSER_HISTORIES.get_or_init(|| Mutex::new(HashMap::new()));
    let mut histories = histories
        .lock()
        .map_err(|_| AppError::BadRequest("browser history lock poisoned".into()))?;
    let history = histories.entry(agent.id.clone()).or_default();
    if history.last().map(|last| last != url).unwrap_or(true) {
        history.push(url.to_string());
        let overflow = history.len().saturating_sub(50);
        if overflow > 0 {
            history.drain(0..overflow);
        }
    }
    Ok(())
}

fn last_browser_url(agent: &AgentDefinition) -> AppResult<Option<String>> {
    let state = LAST_BROWSER_URLS.get_or_init(|| Mutex::new(HashMap::new()));
    let state = state
        .lock()
        .map_err(|_| AppError::BadRequest("browser state lock poisoned".into()))?;
    Ok(state.get(&agent.id).cloned())
}

fn pop_browser_history(agent: &AgentDefinition) -> AppResult<Option<String>> {
    let histories = BROWSER_HISTORIES.get_or_init(|| Mutex::new(HashMap::new()));
    let mut histories = histories
        .lock()
        .map_err(|_| AppError::BadRequest("browser history lock poisoned".into()))?;
    let Some(history) = histories.get_mut(&agent.id) else {
        return Ok(None);
    };
    if history.len() < 2 {
        return Ok(None);
    }
    history.pop();
    let previous = history.last().cloned();
    if let Some(previous) = &previous {
        let state = LAST_BROWSER_URLS.get_or_init(|| Mutex::new(HashMap::new()));
        let mut state = state
            .lock()
            .map_err(|_| AppError::BadRequest("browser state lock poisoned".into()))?;
        state.insert(agent.id.clone(), previous.clone());
    }
    Ok(previous)
}
