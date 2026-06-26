use std::{
    env, fs,
    net::{IpAddr, ToSocketAddrs},
    path::{Path, PathBuf},
    time::Duration,
};

use serde_json::{json, Value};

use crate::{
    error::{AppError, AppResult},
    models::{AppConfig, ChatMessage, LlmProvider, Persona, SearchProvider},
    store::AppStore,
};

use super::{
    complete_chat_with_provider_failover, list_agent_auxiliary_task_assignments, string_arg,
    tool_registry::truncate_for_prompt, truncate_output,
};

const HERMES_WEB_LEGACY_PREFERENCE: [&str; 7] = [
    "firecrawl",
    "parallel",
    "tavily",
    "exa",
    "searxng",
    "brave-free",
    "ddgs",
];

#[derive(Clone, Debug, Default)]
pub(super) struct WebsitePolicy {
    rules: Vec<String>,
}

pub(super) async fn web_provider_tool(store: &AppStore, payload: &Value) -> AppResult<String> {
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
            "backend",
            "searchBackend",
            "search_backend",
        ],
    );
    let capability = string_arg(payload, &["capability", "mode"])
        .unwrap_or_else(|| "search".into())
        .trim()
        .to_ascii_lowercase();
    let providers = store.search_providers()?;
    let status =
        web_provider_registry_status(&providers, configured.as_deref(), capability.as_str());
    let value = match action.as_str() {
        "" | "status" | "list" | "resolve" => status,
        "setup_schema" | "setup-schema" | "schema" => json!({
            "setupSchema": web_provider_setup_schema(),
            "status": status
        }),
        "lifecycle" | "health" | "health_schema" | "health-schema" => json!({
            "healthSchema": web_provider_health_schema(),
            "status": status
        }),
        other => {
            return Err(AppError::BadRequest(format!(
                "unsupported web_provider action: {other}"
            )))
        }
    };
    Ok(serde_json::to_string_pretty(&value)?)
}

fn web_provider_registry_status(
    providers: &[SearchProvider],
    configured: Option<&str>,
    capability: &str,
) -> Value {
    let capability = if capability == "extract" {
        "extract"
    } else {
        "search"
    };
    let configured = configured.map(str::trim).filter(|value| !value.is_empty());
    let synthchat_enabled = providers
        .iter()
        .find(|provider| web_provider_available(provider))
        .map(web_provider_summary);
    let (hermes_resolved, hermes_reason) =
        resolve_hermes_web_provider(providers, configured, capability);
    json!({
        "configured": configured.unwrap_or(""),
        "capability": capability,
        "legacyPreference": HERMES_WEB_LEGACY_PREFERENCE,
        "providers": providers.iter().map(web_provider_summary).collect::<Vec<_>>(),
        "synthchatEnabledProvider": synthchat_enabled,
        "hermesResolvedProvider": hermes_resolved.map(web_provider_summary),
        "hermesResolutionReason": hermes_reason,
        "builtinExtract": {
            "implemented": true,
            "providerBacked": true,
            "fallbackFetch": true,
            "note": "SynthChat web_extract resolves Hermes-style extract providers first, then falls back to builtin HTTP(S) fetch when no extract provider is configured."
        },
        "adapterParity": {
            "implementedSearchProviderTypes": ["searxng", "searx", "firecrawl", "parallel", "tavily", "exa", "brave-free", "ddgs"],
            "implementedExtractProviderTypes": ["firecrawl", "parallel", "tavily", "exa"],
            "pendingHermesProviderTypes": []
        },
        "notes": [
            "Hermes resolves web providers by capability: explicit config, single available provider, then legacy preference.",
            "SynthChat dispatches web_search and web_extract through Hermes-style provider resolution; web_extract falls back to builtin URL fetch when no extract provider is configured."
        ]
    })
}

fn resolve_hermes_web_provider<'a>(
    providers: &'a [SearchProvider],
    configured: Option<&str>,
    capability: &str,
) -> (Option<&'a SearchProvider>, &'static str) {
    let configured = configured.unwrap_or("").trim();
    if !configured.is_empty() {
        if let Some(provider) = providers.iter().find(|provider| {
            web_provider_matches(provider, configured) && web_provider_capable(provider, capability)
        }) {
            return (Some(provider), "explicit_config");
        }
    }
    let eligible = providers
        .iter()
        .filter(|provider| {
            web_provider_capable(provider, capability) && web_provider_available(provider)
        })
        .collect::<Vec<_>>();
    if eligible.len() == 1 {
        return (Some(eligible[0]), "single_available_provider");
    }
    for legacy in HERMES_WEB_LEGACY_PREFERENCE {
        if let Some(provider) = providers.iter().find(|provider| {
            web_provider_matches(provider, legacy)
                && web_provider_capable(provider, capability)
                && web_provider_available(provider)
        }) {
            return (Some(provider), "legacy_available");
        }
    }
    if configured.is_empty() {
        (None, "no_available_provider")
    } else {
        (
            None,
            "explicit_not_registered_or_not_capable_then_no_available_provider",
        )
    }
}

fn web_provider_summary(provider: &SearchProvider) -> Value {
    json!({
        "id": provider.id,
        "name": provider.name,
        "providerType": provider.provider_type,
        "enabled": provider.enabled,
        "available": web_provider_available(provider),
        "baseUrlConfigured": !provider.base_url.trim().is_empty(),
        "credentialEnv": web_provider_effective_env_key(provider).unwrap_or_default(),
        "credentialConfigured": web_provider_api_key(provider).is_some(),
        "supportsSearch": web_provider_capable(provider, "search"),
        "supportsExtract": web_provider_capable(provider, "extract"),
        "adapterImplemented": web_search_adapter_implemented(&provider.provider_type),
        "extractAdapterImplemented": web_extract_adapter_implemented(&provider.provider_type),
        "timeoutSeconds": provider.timeout_seconds,
    })
}

fn web_provider_setup_schema() -> Value {
    json!({
        "providerTypes": [
            {"id": "searxng", "supportsSearch": true, "supportsExtract": false, "implemented": true, "env_vars": []},
            {"id": "firecrawl", "supportsSearch": true, "supportsExtract": true, "implemented": true, "env_vars": [{"key": "FIRECRAWL_API_KEY"}, {"key": "FIRECRAWL_BASE_URL", "optional": true}]},
            {"id": "parallel", "supportsSearch": true, "supportsExtract": true, "implemented": true, "env_vars": [{"key": "PARALLEL_API_KEY"}]},
            {"id": "tavily", "supportsSearch": true, "supportsExtract": true, "implemented": true, "env_vars": [{"key": "TAVILY_API_KEY"}, {"key": "TAVILY_BASE_URL", "optional": true}]},
            {"id": "exa", "supportsSearch": true, "supportsExtract": true, "implemented": true, "env_vars": [{"key": "EXA_API_KEY"}, {"key": "EXA_BASE_URL", "optional": true}]},
            {"id": "brave-free", "supportsSearch": true, "supportsExtract": false, "implemented": true, "env_vars": [{"key": "BRAVE_SEARCH_API_KEY"}]},
            {"id": "ddgs", "supportsSearch": true, "supportsExtract": false, "implemented": true}
        ],
        "post_setup": "web_search"
    })
}

fn web_provider_health_schema() -> Value {
    json!({
        "actions": ["status", "list", "resolve", "setup_schema", "lifecycle", "health_schema"],
        "capabilities": ["search", "extract"],
        "legacyPreference": HERMES_WEB_LEGACY_PREFERENCE,
        "nonMutating": true,
        "networkCalls": false
    })
}

fn web_provider_available(provider: &SearchProvider) -> bool {
    if !provider.enabled {
        return false;
    }
    let provider_type = normalize_web_provider_name(&provider.provider_type);
    if matches!(provider_type.as_str(), "ddgs" | "duckduckgo-html") {
        return true;
    }
    if matches!(provider_type.as_str(), "" | "searxng" | "searx") {
        return !provider.base_url.trim().is_empty();
    }
    web_provider_api_key(provider).is_some()
}

fn web_provider_capable(provider: &SearchProvider, capability: &str) -> bool {
    let provider_type = normalize_web_provider_name(&provider.provider_type);
    match capability {
        "extract" => matches!(
            provider_type.as_str(),
            "firecrawl" | "parallel" | "tavily" | "exa"
        ),
        _ => matches!(
            provider_type.as_str(),
            "" | "searxng"
                | "searx"
                | "firecrawl"
                | "parallel"
                | "tavily"
                | "exa"
                | "brave-free"
                | "ddgs"
                | "duckduckgo-html"
        ),
    }
}

fn web_provider_matches(provider: &SearchProvider, name: &str) -> bool {
    let name = normalize_web_provider_name(name);
    [
        provider.id.as_str(),
        provider.name.as_str(),
        provider.provider_type.as_str(),
    ]
    .iter()
    .any(|candidate| normalize_web_provider_name(candidate) == name)
}

fn normalize_web_provider_name(value: &str) -> String {
    value.trim().to_ascii_lowercase().replace('_', "-")
}

fn web_search_adapter_implemented(provider_type: &str) -> bool {
    matches!(
        normalize_web_provider_name(provider_type).as_str(),
        "searxng"
            | "searx"
            | "firecrawl"
            | "parallel"
            | "tavily"
            | "exa"
            | "brave-free"
            | "ddgs"
            | "duckduckgo-html"
    )
}

fn web_extract_adapter_implemented(provider_type: &str) -> bool {
    matches!(
        normalize_web_provider_name(provider_type).as_str(),
        "firecrawl" | "parallel" | "tavily" | "exa"
    )
}

fn web_provider_env_key(provider_type: &str) -> Option<&'static str> {
    match normalize_web_provider_name(provider_type).as_str() {
        "firecrawl" => Some("FIRECRAWL_API_KEY"),
        "tavily" => Some("TAVILY_API_KEY"),
        "exa" => Some("EXA_API_KEY"),
        "brave-free" => Some("BRAVE_SEARCH_API_KEY"),
        "parallel" => Some("PARALLEL_API_KEY"),
        _ => None,
    }
}

fn web_provider_effective_env_key(provider: &SearchProvider) -> Option<String> {
    let configured = provider.api_key_env.trim();
    if !configured.is_empty() {
        Some(configured.to_string())
    } else {
        web_provider_env_key(&provider.provider_type).map(str::to_string)
    }
}

fn web_provider_api_key(provider: &SearchProvider) -> Option<String> {
    if let Some(value) = provider
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Some(value.to_string());
    }
    let env_key = web_provider_effective_env_key(provider)?;
    env::var(env_key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn web_provider_base_url(provider: &SearchProvider, default: &str) -> String {
    let configured = provider.base_url.trim().trim_end_matches('/');
    if configured.is_empty() {
        default.to_string()
    } else {
        configured.to_string()
    }
}

pub(super) async fn web_search_tool(store: &AppStore, payload: &Value) -> AppResult<String> {
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
    let configured = string_arg(
        payload,
        &[
            "provider",
            "providerId",
            "provider_id",
            "backend",
            "searchBackend",
            "search_backend",
        ],
    );
    let providers = store.search_providers()?;
    let (provider, _) = resolve_hermes_web_provider(&providers, configured.as_deref(), "search");
    let provider = provider
        .cloned()
        .ok_or_else(|| AppError::BadRequest("no enabled search provider configured".into()))?;
    match normalize_web_provider_name(&provider.provider_type).as_str() {
        "searxng" | "searx" | "" => searxng_search(&provider, query, limit, payload).await,
        "firecrawl" => firecrawl_search(&provider, query, limit).await,
        "parallel" => parallel_search(&provider, query, limit).await,
        "tavily" => tavily_search(&provider, query, limit).await,
        "exa" => exa_search(&provider, query, limit).await,
        "brave-free" => brave_search(&provider, query, limit).await,
        "ddgs" | "duckduckgo-html" => ddgs_search(&provider, query, limit).await,
        other => Err(AppError::BadRequest(format!(
            "unsupported search provider type: {other}"
        ))),
    }
}

pub(super) async fn x_search_tool(store: &AppStore, payload: &Value) -> AppResult<String> {
    let original_query = string_arg(payload, &["query", "q"])
        .ok_or_else(|| AppError::BadRequest("x_search requires payload.query".into()))?;
    let mode = string_arg(payload, &["mode", "backend"])
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
        .replace('-', "_");
    if !matches!(mode.as_str(), "web_search_bridge" | "bridge" | "web") {
        return xai_responses_x_search(store, payload, &original_query).await;
    }
    let search_query = build_x_search_query(payload)?;
    let mut bridged_payload = payload.clone();
    if let Some(map) = bridged_payload.as_object_mut() {
        map.insert("query".into(), json!(search_query));
    } else {
        bridged_payload = json!({"query": search_query});
    }
    let result_text = web_search_tool(store, &bridged_payload).await?;
    let result_json = serde_json::from_str::<Value>(&result_text).unwrap_or_else(|_| {
        json!({
            "raw": result_text
        })
    });
    Ok(serde_json::to_string_pretty(&json!({
        "tool": "x_search",
        "mode": "web_search_bridge",
        "query": original_query,
        "searchQuery": bridged_payload.get("query").and_then(Value::as_str).unwrap_or_default(),
        "result": result_json
    }))?)
}

#[derive(Clone, Debug)]
struct XaiSearchCredential {
    api_key: String,
    base_url: String,
    source: String,
}

fn resolve_xai_search_credential(store: &AppStore) -> AppResult<XaiSearchCredential> {
    let mut provider = LlmProvider::default();
    provider.id = "xai-oauth".into();
    provider.name = "xAI OAuth".into();
    provider.provider_type = "xai-oauth".into();
    provider.preset = Some("xai-oauth".into());
    if let Some(credential) = crate::hermes_auth::resolve_hermes_runtime_credential(&provider) {
        if !credential.api_key.trim().is_empty() {
            return Ok(XaiSearchCredential {
                api_key: credential.api_key,
                base_url: credential
                    .base_url
                    .unwrap_or_else(|| "https://api.x.ai/v1".into())
                    .trim_end_matches('/')
                    .to_string(),
                source: credential.source,
            });
        }
    }
    if let Some(api_key) = env::var("XAI_API_KEY")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        return Ok(XaiSearchCredential {
            api_key,
            base_url: env::var("XAI_BASE_URL")
                .ok()
                .map(|value| value.trim().trim_end_matches('/').to_string())
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| "https://api.x.ai/v1".into()),
            source: "env:XAI_API_KEY".into(),
        });
    }
    let config = store.config()?;
    if let Some(api_key) = config
        .messaging_gateway
        .get("dashboardEnv")
        .and_then(Value::as_object)
        .and_then(|env| env.get("XAI_API_KEY"))
        .and_then(Value::as_str)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        let base_url = config
            .messaging_gateway
            .get("dashboardEnv")
            .and_then(Value::as_object)
            .and_then(|env| env.get("XAI_BASE_URL"))
            .and_then(Value::as_str)
            .map(|value| value.trim().trim_end_matches('/').to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "https://api.x.ai/v1".into());
        return Ok(XaiSearchCredential {
            api_key,
            base_url,
            source: "dashboardEnv:XAI_API_KEY".into(),
        });
    }
    Err(AppError::BadRequest(
        "No xAI credentials available. Configure xAI OAuth or set XAI_API_KEY.".into(),
    ))
}

async fn xai_responses_x_search(
    store: &AppStore,
    payload: &Value,
    query: &str,
) -> AppResult<String> {
    let credential = resolve_xai_search_credential(store)?;
    let tool_def = xai_search_tool_definition(payload)?;
    let model = xai_search_model(&store.config()?);
    let body = json!({
        "model": model,
        "input": [{
            "role": "user",
            "content": query.trim()
        }],
        "tools": [tool_def],
        "store": false
    });
    let timeout_seconds = xai_search_timeout_seconds(&store.config()?);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(timeout_seconds))
        .user_agent("Hermes-Agent-SynthChat/1.0")
        .build()
        .map_err(|error| AppError::BadRequest(format!("failed to build xAI client: {error}")))?;
    let url = format!("{}/responses", credential.base_url);
    let response = client
        .post(&url)
        .bearer_auth(&credential.api_key)
        .json(&body)
        .send()
        .await
        .map_err(|error| AppError::BadRequest(format!("x_search failed: {error}")))?;
    let status = response.status();
    let text = response.text().await.map_err(|error| {
        AppError::BadRequest(format!("failed to read x_search response: {error}"))
    })?;
    if !status.is_success() {
        return Ok(serde_json::to_string_pretty(&json!({
            "success": false,
            "provider": "xai",
            "tool": "x_search",
            "error": truncate_output(&text, 2000),
            "httpStatus": status.as_u16(),
            "credential_source": credential.source,
        }))?);
    }
    let data = serde_json::from_str::<Value>(&text)
        .map_err(|error| AppError::BadRequest(format!("invalid x_search JSON: {error}")))?;
    let answer = extract_xai_response_text(&data);
    let citations = data.get("citations").cloned().unwrap_or_else(|| json!([]));
    let inline_citations = extract_xai_inline_citations(&data);
    let active_filters = xai_search_active_filters(payload);
    let degraded = !active_filters.is_empty()
        && citations.as_array().is_none_or(Vec::is_empty)
        && inline_citations.as_array().is_none_or(Vec::is_empty);
    Ok(serde_json::to_string_pretty(&json!({
        "success": true,
        "provider": "xai",
        "credential_source": credential.source,
        "tool": "x_search",
        "mode": "xai_responses",
        "model": model,
        "query": query.trim(),
        "answer": answer,
        "citations": citations,
        "inline_citations": inline_citations,
        "degraded": degraded,
        "degraded_reason": if degraded {
            Value::String(format!("no citations returned despite filters: {}", active_filters.join(", ")))
        } else {
            Value::Null
        },
        "raw": data,
    }))?)
}

pub(super) fn xai_search_tool_definition(payload: &Value) -> AppResult<Value> {
    let mut tool = json!({"type": "x_search"});
    let allowed = xai_search_handles(
        payload,
        &["allowed_x_handles", "allowedXHandles", "allowed", "from"],
    )?;
    let excluded = xai_search_handles(
        payload,
        &["excluded_x_handles", "excludedXHandles", "excluded"],
    )?;
    if !allowed.is_empty() && !excluded.is_empty() {
        return Err(AppError::BadRequest(
            "allowed_x_handles and excluded_x_handles cannot be used together".into(),
        ));
    }
    if !allowed.is_empty() {
        tool["allowed_x_handles"] = json!(allowed);
    }
    if !excluded.is_empty() {
        tool["excluded_x_handles"] = json!(excluded);
    }
    if let Some(from_date) = string_arg(
        payload,
        &["from_date", "fromDate", "since", "startDate", "start_date"],
    ) {
        xai_validate_date(&from_date, "from_date")?;
        tool["from_date"] = json!(from_date.trim());
    }
    if let Some(to_date) = string_arg(
        payload,
        &["to_date", "toDate", "until", "endDate", "end_date"],
    ) {
        xai_validate_date(&to_date, "to_date")?;
        tool["to_date"] = json!(to_date.trim());
    }
    if xai_bool_arg(
        payload,
        &["enable_image_understanding", "enableImageUnderstanding"],
    ) {
        tool["enable_image_understanding"] = json!(true);
    }
    if xai_bool_arg(
        payload,
        &["enable_video_understanding", "enableVideoUnderstanding"],
    ) {
        tool["enable_video_understanding"] = json!(true);
    }
    xai_validate_date_range(
        tool.get("from_date").and_then(Value::as_str),
        tool.get("to_date").and_then(Value::as_str),
    )?;
    Ok(tool)
}

fn xai_search_handles(payload: &Value, keys: &[&str]) -> AppResult<Vec<String>> {
    let mut handles = Vec::new();
    for key in keys {
        if let Some(value) = payload.get(*key) {
            if let Some(text) = value.as_str() {
                handles.extend(text.split(',').map(str::to_string));
            } else if let Some(items) = value.as_array() {
                handles.extend(items.iter().filter_map(Value::as_str).map(str::to_string));
            }
        }
    }
    let handles = handles
        .into_iter()
        .map(|value| value.trim().trim_start_matches('@').to_string())
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    if handles.len() > 10 {
        return Err(AppError::BadRequest(
            "x_search supports at most 10 handles".into(),
        ));
    }
    Ok(handles)
}

fn xai_validate_date(value: &str, field: &str) -> AppResult<()> {
    let raw = value.trim();
    if chrono::NaiveDate::parse_from_str(raw, "%Y-%m-%d").is_err() {
        Err(AppError::BadRequest(format!("{field} must be YYYY-MM-DD")))
    } else {
        Ok(())
    }
}

fn xai_validate_date_range(from_date: Option<&str>, to_date: Option<&str>) -> AppResult<()> {
    let parsed_from = from_date
        .filter(|value| !value.trim().is_empty())
        .map(|value| {
            chrono::NaiveDate::parse_from_str(value.trim(), "%Y-%m-%d")
                .map_err(|_| AppError::BadRequest("from_date must be YYYY-MM-DD".into()))
        })
        .transpose()?;
    let parsed_to = to_date
        .filter(|value| !value.trim().is_empty())
        .map(|value| {
            chrono::NaiveDate::parse_from_str(value.trim(), "%Y-%m-%d")
                .map_err(|_| AppError::BadRequest("to_date must be YYYY-MM-DD".into()))
        })
        .transpose()?;
    if let (Some(from), Some(to)) = (parsed_from, parsed_to) {
        if from > to {
            return Err(AppError::BadRequest(format!(
                "from_date ({}) must be on or before to_date ({})",
                from.format("%Y-%m-%d"),
                to.format("%Y-%m-%d")
            )));
        }
    }
    if let Some(from) = parsed_from {
        let today = chrono::Utc::now().date_naive();
        if from > today {
            return Err(AppError::BadRequest(format!(
                "from_date ({}) is in the future; X Search only indexes past posts (today UTC is {})",
                from.format("%Y-%m-%d"),
                today.format("%Y-%m-%d")
            )));
        }
    }
    Ok(())
}

fn xai_bool_arg(payload: &Value, keys: &[&str]) -> bool {
    keys.iter()
        .find_map(|key| payload.get(*key))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn xai_search_model(config: &AppConfig) -> String {
    string_arg(&config.web, &["xSearchModel", "x_search_model"])
        .or_else(|| {
            string_arg(
                &config.messaging_gateway,
                &["xSearchModel", "x_search_model"],
            )
        })
        .unwrap_or_else(|| "grok-4.20-reasoning".into())
}

fn xai_search_timeout_seconds(config: &AppConfig) -> u64 {
    config
        .web
        .get("xSearchTimeoutSeconds")
        .or_else(|| config.web.get("x_search_timeout_seconds"))
        .or_else(|| config.messaging_gateway.get("xSearchTimeoutSeconds"))
        .or_else(|| config.messaging_gateway.get("x_search_timeout_seconds"))
        .and_then(Value::as_u64)
        .unwrap_or(180)
        .max(30)
}

fn xai_search_active_filters(payload: &Value) -> Vec<String> {
    let mut filters = Vec::new();
    if xai_search_handles(
        payload,
        &["allowed_x_handles", "allowedXHandles", "allowed", "from"],
    )
    .map(|items| !items.is_empty())
    .unwrap_or(false)
    {
        filters.push("allowed_x_handles".into());
    }
    if xai_search_handles(
        payload,
        &["excluded_x_handles", "excludedXHandles", "excluded"],
    )
    .map(|items| !items.is_empty())
    .unwrap_or(false)
    {
        filters.push("excluded_x_handles".into());
    }
    if string_arg(
        payload,
        &["from_date", "fromDate", "since", "startDate", "start_date"],
    )
    .is_some()
    {
        filters.push("from_date".into());
    }
    if string_arg(
        payload,
        &["to_date", "toDate", "until", "endDate", "end_date"],
    )
    .is_some()
    {
        filters.push("to_date".into());
    }
    filters
}

fn extract_xai_response_text(value: &Value) -> String {
    if let Some(text) = value.get("output_text").and_then(Value::as_str) {
        return text.to_string();
    }
    value
        .get("output")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .flat_map(|item| {
            item.get("content")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
        })
        .filter_map(|content| {
            content
                .get("text")
                .or_else(|| content.get("output_text"))
                .and_then(Value::as_str)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn extract_xai_inline_citations(value: &Value) -> Value {
    fn walk(value: &Value, out: &mut Vec<Value>) {
        match value {
            Value::Object(map) => {
                if map.get("type").and_then(Value::as_str) == Some("url_citation") {
                    out.push(Value::Object(map.clone()));
                }
                for child in map.values() {
                    walk(child, out);
                }
            }
            Value::Array(items) => {
                for item in items {
                    walk(item, out);
                }
            }
            _ => {}
        }
    }
    let mut citations = Vec::new();
    walk(value, &mut citations);
    Value::Array(citations)
}

pub(super) fn build_x_search_query(payload: &Value) -> AppResult<String> {
    let query = string_arg(payload, &["query", "q"])
        .ok_or_else(|| AppError::BadRequest("x_search requires payload.query".into()))?;
    let mut parts = vec![format!("({query})")];
    if let Some(username) = string_arg(payload, &["from", "username", "user", "author"]) {
        let username = username.trim_start_matches('@');
        if !username.is_empty() {
            parts.push(format!("from:{username}"));
        }
    }
    if let Some(since) = string_arg(payload, &["since", "startDate", "start_date"]) {
        parts.push(format!("since:{since}"));
    }
    if let Some(until) = string_arg(payload, &["until", "endDate", "end_date"]) {
        parts.push(format!("until:{until}"));
    }
    if let Some(language) = string_arg(payload, &["language", "lang"]) {
        parts.push(format!("lang:{language}"));
    }
    if let Some(extra) = string_arg(payload, &["filters", "filter"]) {
        parts.push(extra);
    }
    parts.push("(site:x.com OR site:twitter.com)".into());
    Ok(parts.join(" "))
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
    let text = response.text().await.map_err(|error| {
        AppError::BadRequest(format!("failed to read search response: {error}"))
    })?;
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

async fn firecrawl_search(
    provider: &SearchProvider,
    query: &str,
    limit: usize,
) -> AppResult<String> {
    let api_key = web_provider_api_key(provider)
        .ok_or_else(|| AppError::BadRequest("FIRECRAWL_API_KEY is not set".into()))?;
    let url = format!(
        "{}/v1/search",
        web_provider_base_url(provider, "https://api.firecrawl.dev")
    );
    let value = post_json_with_bearer(
        provider,
        &url,
        &api_key,
        &json!({
            "query": query,
            "limit": limit.min(20)
        }),
    )
    .await?;
    let raw_results = value
        .get("data")
        .or_else(|| value.get("results"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    Ok(serde_json::to_string_pretty(
        &normalize_provider_search_results(
            provider,
            query,
            url,
            raw_results,
            limit,
            &["description", "content", "markdown"],
        ),
    )?)
}

async fn parallel_search(
    provider: &SearchProvider,
    query: &str,
    limit: usize,
) -> AppResult<String> {
    let api_key = web_provider_api_key(provider)
        .ok_or_else(|| AppError::BadRequest("PARALLEL_API_KEY is not set".into()))?;
    let url = format!(
        "{}/v1/search",
        web_provider_base_url(provider, "https://api.parallel.ai")
    );
    let value = post_json_with_api_key(
        provider,
        &url,
        &api_key,
        &json!({
            "search_queries": [query],
            "objective": query,
            "max_results": limit.min(20),
            "max_chars_per_result": 1200
        }),
    )
    .await?;
    let raw_results = value
        .get("results")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    Ok(serde_json::to_string_pretty(
        &normalize_provider_search_results(
            provider,
            query,
            url,
            raw_results,
            limit,
            &["excerpts", "full_content", "description"],
        ),
    )?)
}

async fn tavily_search(provider: &SearchProvider, query: &str, limit: usize) -> AppResult<String> {
    let api_key = web_provider_api_key(provider)
        .ok_or_else(|| AppError::BadRequest("TAVILY_API_KEY is not set".into()))?;
    let url = format!(
        "{}/search",
        web_provider_base_url(provider, "https://api.tavily.com")
    );
    let value = post_json(
        provider,
        &url,
        &json!({
            "api_key": api_key,
            "query": query,
            "max_results": limit.min(20),
            "include_raw_content": false,
            "include_images": false
        }),
    )
    .await?;
    let raw_results = value
        .get("results")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    Ok(serde_json::to_string_pretty(
        &normalize_provider_search_results(
            provider,
            query,
            url,
            raw_results,
            limit,
            &["content", "description"],
        ),
    )?)
}

async fn exa_search(provider: &SearchProvider, query: &str, limit: usize) -> AppResult<String> {
    let api_key = web_provider_api_key(provider)
        .ok_or_else(|| AppError::BadRequest("EXA_API_KEY is not set".into()))?;
    let url = format!(
        "{}/search",
        web_provider_base_url(provider, "https://api.exa.ai")
    );
    let value = post_json_with_bearer(
        provider,
        &url,
        &api_key,
        &json!({
            "query": query,
            "numResults": limit.min(20),
            "contents": {
                "highlights": true
            }
        }),
    )
    .await?;
    let raw_results = value
        .get("results")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    Ok(serde_json::to_string_pretty(
        &normalize_provider_search_results(
            provider,
            query,
            url,
            raw_results,
            limit,
            &["text", "highlights", "description"],
        ),
    )?)
}

async fn brave_search(provider: &SearchProvider, query: &str, limit: usize) -> AppResult<String> {
    let api_key = web_provider_api_key(provider)
        .ok_or_else(|| AppError::BadRequest("BRAVE_SEARCH_API_KEY is not set".into()))?;
    let mut url = reqwest::Url::parse(&web_provider_base_url(
        provider,
        "https://api.search.brave.com/res/v1/web/search",
    ))
    .map_err(|error| AppError::BadRequest(format!("invalid Brave Search URL: {error}")))?;
    {
        let mut query_pairs = url.query_pairs_mut();
        query_pairs.append_pair("q", query);
        query_pairs.append_pair("count", &limit.min(20).to_string());
    }
    let client = web_client(provider)?;
    let response = client
        .get(url.clone())
        .header("X-Subscription-Token", api_key)
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|error| AppError::BadRequest(format!("Brave Search failed: {error}")))?;
    let value = read_json_response(response, "Brave Search").await?;
    let raw_results = value
        .get("web")
        .and_then(|web| web.get("results"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    Ok(serde_json::to_string_pretty(
        &normalize_provider_search_results(
            provider,
            query,
            url.to_string(),
            raw_results,
            limit,
            &["description"],
        ),
    )?)
}

async fn ddgs_search(provider: &SearchProvider, query: &str, limit: usize) -> AppResult<String> {
    let mut url = reqwest::Url::parse(&web_provider_base_url(
        provider,
        "https://html.duckduckgo.com/html/",
    ))
    .map_err(|error| AppError::BadRequest(format!("invalid DuckDuckGo URL: {error}")))?;
    {
        let mut pairs = url.query_pairs_mut();
        pairs.append_pair("q", query);
    }
    let client = web_client(provider)?;
    let response = client
        .get(url.clone())
        .send()
        .await
        .map_err(|error| AppError::BadRequest(format!("DuckDuckGo search failed: {error}")))?;
    let status = response.status();
    let html = response.text().await.map_err(|error| {
        AppError::BadRequest(format!("failed to read DuckDuckGo response: {error}"))
    })?;
    if !status.is_success() {
        return Err(AppError::BadRequest(format!(
            "DuckDuckGo returned HTTP {}: {}",
            status.as_u16(),
            truncate_output(&html, 2000)
        )));
    }
    let results = ddgs_html_results(&html, limit);
    Ok(serde_json::to_string_pretty(&json!({
        "providerId": provider.id,
        "providerType": provider.provider_type,
        "query": query,
        "requestUrl": url.to_string(),
        "count": results.len(),
        "results": results,
    }))?)
}

fn ddgs_html_results(html: &str, limit: usize) -> Vec<Value> {
    let lower = html.to_ascii_lowercase();
    let mut results = Vec::new();
    let mut cursor = 0usize;
    while results.len() < limit {
        let Some(class_rel) = lower[cursor..].find("result__a") else {
            break;
        };
        let class_pos = cursor + class_rel;
        let Some(anchor_start) = lower[..class_pos].rfind("<a") else {
            cursor = class_pos + "result__a".len();
            continue;
        };
        let Some(anchor_tag_end_rel) = lower[anchor_start..].find('>') else {
            cursor = class_pos + "result__a".len();
            continue;
        };
        let anchor_tag_end = anchor_start + anchor_tag_end_rel;
        let Some(anchor_end_rel) = lower[anchor_tag_end..].find("</a>") else {
            cursor = anchor_tag_end + 1;
            continue;
        };
        let anchor_end = anchor_tag_end + anchor_end_rel + "</a>".len();
        let anchor_tag = &html[anchor_start..=anchor_tag_end];
        let title = clean_text(&strip_tags(&html[anchor_tag_end + 1..anchor_end]));
        let href = html_attr(anchor_tag, "href")
            .and_then(|href| duckduckgo_result_url(&href))
            .unwrap_or_default();
        let content = ddgs_snippet_after(html, &lower, anchor_end);
        if !href.is_empty() {
            results.push(json!({
                "title": title,
                "url": href,
                "content": truncate_for_prompt(&content, 1200),
                "position": results.len() + 1,
                "score": Value::Null
            }));
        }
        cursor = anchor_end;
    }
    results
}

fn ddgs_snippet_after(html: &str, lower: &str, from: usize) -> String {
    let search_end = (from + 3000).min(lower.len());
    let Some(snippet_rel) = lower[from..search_end].find("result__snippet") else {
        return String::new();
    };
    let snippet_pos = from + snippet_rel;
    let Some(tag_end_rel) = lower[snippet_pos..search_end].find('>') else {
        return String::new();
    };
    let content_start = snippet_pos + tag_end_rel + 1;
    let content_end = ["</a>", "</div>", "</span>"]
        .iter()
        .filter_map(|end| {
            lower[content_start..search_end]
                .find(end)
                .map(|rel| content_start + rel)
        })
        .min()
        .unwrap_or(search_end);
    clean_text(&strip_tags(&html[content_start..content_end]))
}

fn duckduckgo_result_url(href: &str) -> Option<String> {
    let absolute = if href.starts_with("//") {
        format!("https:{href}")
    } else if href.starts_with('/') {
        format!("https://duckduckgo.com{href}")
    } else {
        href.to_string()
    };
    let parsed = reqwest::Url::parse(&absolute).ok()?;
    if let Some((_, value)) = parsed.query_pairs().find(|(key, _)| key == "uddg") {
        let value = value.to_string();
        if value.starts_with("http://") || value.starts_with("https://") {
            return Some(value);
        }
    }
    if absolute.starts_with("http://") || absolute.starts_with("https://") {
        Some(absolute)
    } else {
        None
    }
}

fn normalize_provider_search_results(
    provider: &SearchProvider,
    query: &str,
    request_url: String,
    raw_results: Vec<Value>,
    limit: usize,
    content_keys: &[&str],
) -> Value {
    let results = raw_results
        .iter()
        .take(limit)
        .enumerate()
        .map(|(index, item)| {
            let content = content_keys
                .iter()
                .filter_map(|key| item.get(*key))
                .map(readable_json_text)
                .find(|value| !value.trim().is_empty())
                .unwrap_or_default();
            json!({
                "title": item.get("title").and_then(Value::as_str).unwrap_or_default(),
                "url": item.get("url").and_then(Value::as_str).unwrap_or_default(),
                "content": truncate_for_prompt(&content, 1200),
                "position": item.get("position").and_then(Value::as_u64).unwrap_or((index + 1) as u64),
                "score": item.get("score").cloned().unwrap_or(Value::Null)
            })
        })
        .collect::<Vec<_>>();
    json!({
        "providerId": provider.id,
        "providerType": provider.provider_type,
        "query": query,
        "requestUrl": request_url,
        "count": results.len(),
        "results": results,
    })
}

async fn provider_extract(
    store: &AppStore,
    provider: &SearchProvider,
    urls: &[String],
    max_chars: usize,
    payload: &Value,
) -> AppResult<String> {
    let provider_type = normalize_web_provider_name(&provider.provider_type);
    let rows = match provider_type.as_str() {
        "firecrawl" => firecrawl_extract(provider, urls, max_chars, payload).await?,
        "parallel" => parallel_extract(provider, urls, max_chars).await?,
        "tavily" => tavily_extract(provider, urls, max_chars).await?,
        "exa" => exa_extract(provider, urls, max_chars).await?,
        other => {
            return Err(AppError::BadRequest(format!(
                "unsupported web_extract provider type: {other}"
            )))
        }
    };
    let rows = process_web_extract_rows_with_llm(store, rows, payload).await?;
    format_extract_rows(provider, &rows)
}

async fn firecrawl_extract(
    provider: &SearchProvider,
    urls: &[String],
    max_chars: usize,
    payload: &Value,
) -> AppResult<Vec<Value>> {
    let api_key = web_provider_api_key(provider)
        .ok_or_else(|| AppError::BadRequest("FIRECRAWL_API_KEY is not set".into()))?;
    let base = web_provider_base_url(provider, "https://api.firecrawl.dev");
    let format = string_arg(payload, &["format"])
        .map(|value| value.to_ascii_lowercase())
        .filter(|value| matches!(value.as_str(), "markdown" | "html"));
    let formats = match format.as_deref() {
        Some("markdown") => json!(["markdown"]),
        Some("html") => json!(["html"]),
        _ => json!(["markdown", "html"]),
    };
    let mut rows = Vec::new();
    for url in urls.iter().take(5) {
        validate_web_url(url)?;
        let request_url = format!("{base}/v1/scrape");
        let row = match post_json_with_bearer(
            provider,
            &request_url,
            &api_key,
            &json!({
                "url": url,
                "formats": formats
            }),
        )
        .await
        {
            Ok(value) => {
                let data = value.get("data").unwrap_or(&value);
                let content = data
                    .get("markdown")
                    .or_else(|| data.get("html"))
                    .map(readable_json_text)
                    .unwrap_or_default();
                json!({
                    "url": data.get("url").and_then(Value::as_str).unwrap_or(url),
                    "ok": true,
                    "title": data.get("metadata").and_then(|metadata| metadata.get("title")).and_then(Value::as_str).unwrap_or_default(),
                    "content": truncate_for_prompt(&content, max_chars)
                })
            }
            Err(error) => json!({
                "url": url,
                "ok": false,
                "error": error.to_string()
            }),
        };
        rows.push(row);
    }
    Ok(rows)
}

async fn parallel_extract(
    provider: &SearchProvider,
    urls: &[String],
    max_chars: usize,
) -> AppResult<Vec<Value>> {
    let api_key = web_provider_api_key(provider)
        .ok_or_else(|| AppError::BadRequest("PARALLEL_API_KEY is not set".into()))?;
    for url in urls.iter().take(5) {
        validate_web_url(url)?;
    }
    let request_url = format!(
        "{}/v1/extract",
        web_provider_base_url(provider, "https://api.parallel.ai")
    );
    let value = post_json_with_api_key(
        provider,
        &request_url,
        &api_key,
        &json!({
            "urls": urls.iter().take(5).collect::<Vec<_>>(),
            "objective": "Extract the most relevant readable page content for an AI agent.",
            "advanced_settings": {
                "full_content": {
                    "max_chars": max_chars.min(50_000) as u64
                }
            }
        }),
    )
    .await?;
    Ok(normalize_extract_response_rows(
        &value,
        urls,
        max_chars,
        &["full_content", "excerpts", "content", "raw_content"],
    ))
}

async fn tavily_extract(
    provider: &SearchProvider,
    urls: &[String],
    max_chars: usize,
) -> AppResult<Vec<Value>> {
    let api_key = web_provider_api_key(provider)
        .ok_or_else(|| AppError::BadRequest("TAVILY_API_KEY is not set".into()))?;
    let request_url = format!(
        "{}/extract",
        web_provider_base_url(provider, "https://api.tavily.com")
    );
    let value = post_json(
        provider,
        &request_url,
        &json!({
            "api_key": api_key,
            "urls": urls.iter().take(5).collect::<Vec<_>>(),
            "include_images": false
        }),
    )
    .await?;
    Ok(normalize_extract_response_rows(
        &value,
        urls,
        max_chars,
        &["raw_content", "content"],
    ))
}

async fn exa_extract(
    provider: &SearchProvider,
    urls: &[String],
    max_chars: usize,
) -> AppResult<Vec<Value>> {
    let api_key = web_provider_api_key(provider)
        .ok_or_else(|| AppError::BadRequest("EXA_API_KEY is not set".into()))?;
    let request_url = format!(
        "{}/contents",
        web_provider_base_url(provider, "https://api.exa.ai")
    );
    let value = post_json_with_bearer(
        provider,
        &request_url,
        &api_key,
        &json!({
            "urls": urls.iter().take(5).collect::<Vec<_>>(),
            "text": true
        }),
    )
    .await?;
    Ok(normalize_extract_response_rows(
        &value,
        urls,
        max_chars,
        &["text", "content", "raw_content"],
    ))
}

fn normalize_extract_response_rows(
    value: &Value,
    fallback_urls: &[String],
    max_chars: usize,
    content_keys: &[&str],
) -> Vec<Value> {
    let results = value
        .get("results")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut rows = results
        .iter()
        .map(|item| {
            let content = content_keys
                .iter()
                .filter_map(|key| item.get(*key))
                .map(readable_json_text)
                .find(|value| !value.trim().is_empty())
                .unwrap_or_default();
            let url = item
                .get("url")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            json!({
                "url": url,
                "ok": !content.trim().is_empty(),
                "title": item.get("title").and_then(Value::as_str).unwrap_or_default(),
                "content": truncate_for_prompt(&content, max_chars)
            })
        })
        .collect::<Vec<_>>();
    for item in value
        .get("failed_results")
        .or_else(|| value.get("failedResults"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        rows.push(json!({
            "url": item.get("url").and_then(Value::as_str).unwrap_or_default(),
            "ok": false,
            "error": item.get("error").and_then(Value::as_str).unwrap_or("extraction failed")
        }));
    }
    if rows.is_empty() {
        rows.extend(fallback_urls.iter().take(5).map(|url| {
            json!({
                "url": url,
                "ok": false,
                "error": "provider returned no extract results"
            })
        }));
    }
    rows
}

fn format_extract_rows(provider: &SearchProvider, rows: &[Value]) -> AppResult<String> {
    let mut sections = Vec::new();
    for row in rows {
        let url = row.get("url").and_then(Value::as_str).unwrap_or("-");
        if row.get("ok").and_then(Value::as_bool).unwrap_or(false) {
            sections.push(format!(
                "URL: {url}\n{}",
                row.get("content").and_then(Value::as_str).unwrap_or("")
            ));
        } else {
            sections.push(format!(
                "URL: {url}\nERROR: {}",
                row.get("error").and_then(Value::as_str).unwrap_or("failed")
            ));
        }
    }
    let ok = rows
        .iter()
        .any(|row| row.get("ok").and_then(Value::as_bool).unwrap_or(false));
    sections.push(format!(
        "\nJSON:\n{}",
        serde_json::to_string_pretty(&json!({
            "providerId": provider.id,
            "providerType": provider.provider_type,
            "ok": ok,
            "results": rows,
        }))?
    ));
    Ok(sections.join("\n\n---\n\n"))
}

fn readable_json_text(value: &Value) -> String {
    if let Some(text) = value.as_str() {
        text.to_string()
    } else if let Some(items) = value.as_array() {
        items
            .iter()
            .map(readable_json_text)
            .filter(|value| !value.trim().is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    } else {
        String::new()
    }
}

fn web_client(provider: &SearchProvider) -> AppResult<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(provider.timeout_seconds.max(1)))
        .user_agent("SynthChat-agent/1.0")
        .build()
        .map_err(|error| AppError::BadRequest(format!("failed to build search client: {error}")))
}

async fn post_json(provider: &SearchProvider, url: &str, body: &Value) -> AppResult<Value> {
    let client = web_client(provider)?;
    let response =
        client.post(url).json(body).send().await.map_err(|error| {
            AppError::BadRequest(format!("web provider request failed: {error}"))
        })?;
    read_json_response(response, "web provider").await
}

async fn post_json_with_bearer(
    provider: &SearchProvider,
    url: &str,
    api_key: &str,
    body: &Value,
) -> AppResult<Value> {
    let client = web_client(provider)?;
    let response = client
        .post(url)
        .bearer_auth(api_key)
        .json(body)
        .send()
        .await
        .map_err(|error| AppError::BadRequest(format!("web provider request failed: {error}")))?;
    read_json_response(response, "web provider").await
}

async fn post_json_with_api_key(
    provider: &SearchProvider,
    url: &str,
    api_key: &str,
    body: &Value,
) -> AppResult<Value> {
    let client = web_client(provider)?;
    let response = client
        .post(url)
        .header("x-api-key", api_key)
        .header("parallel-beta", "search-extract-2025-10-10")
        .json(body)
        .send()
        .await
        .map_err(|error| AppError::BadRequest(format!("web provider request failed: {error}")))?;
    read_json_response(response, "web provider").await
}

async fn read_json_response(response: reqwest::Response, label: &str) -> AppResult<Value> {
    let status = response.status();
    let text = response.text().await.map_err(|error| {
        AppError::BadRequest(format!("failed to read {label} response: {error}"))
    })?;
    if !status.is_success() {
        return Err(AppError::BadRequest(format!(
            "{label} returned HTTP {}: {}",
            status.as_u16(),
            truncate_output(&text, 2000)
        )));
    }
    serde_json::from_str::<Value>(&text)
        .map_err(|error| AppError::BadRequest(format!("invalid {label} JSON: {error}")))
}

pub(super) fn normalize_search_results(
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

pub(super) async fn web_request_tool(store: &AppStore, payload: &Value) -> AppResult<String> {
    let url = payload
        .get("url")
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::BadRequest("web_request requires payload.url".into()))?;
    let policy = website_policy_from_store(store)?;
    validate_web_url_with_policy(url, &policy)?;
    let method = payload
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("GET")
        .to_uppercase();
    let method = reqwest::Method::from_bytes(method.as_bytes())
        .map_err(|error| AppError::BadRequest(format!("invalid HTTP method: {error}")))?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .redirect(safe_redirect_policy_with_policy(policy))
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

pub(super) async fn web_extract_tool(store: &AppStore, payload: &Value) -> AppResult<String> {
    let urls = web_extract_urls_from_payload(payload);
    if urls.is_empty() {
        return Err(AppError::BadRequest(
            "web_extract requires payload.url or payload.urls with HTTP(S) URLs".into(),
        ));
    }
    let max_chars = payload
        .get("maxChars")
        .or_else(|| payload.get("max_chars"))
        .and_then(Value::as_u64)
        .unwrap_or(6000)
        .clamp(500, 20_000) as usize;
    for url in urls.iter().take(5) {
        let policy = website_policy_from_store(store)?;
        validate_web_url_with_policy(url, &policy)?;
    }
    let configured = string_arg(
        payload,
        &[
            "provider",
            "providerId",
            "provider_id",
            "backend",
            "extractBackend",
            "extract_backend",
        ],
    );
    let providers = store.search_providers()?;
    let (provider, _) = resolve_hermes_web_provider(&providers, configured.as_deref(), "extract");
    if let Some(provider) =
        provider.filter(|provider| web_extract_adapter_implemented(&provider.provider_type))
    {
        return provider_extract(store, provider, &urls, max_chars, payload).await;
    }
    let mut rows = Vec::new();
    for url in urls.iter().take(5) {
        let policy = website_policy_from_store(store)?;
        validate_web_url_with_policy(url, &policy)?;
        let row = match fetch_url_text_with_policy(url, policy).await {
            Ok(body) => {
                let content = extract_readable_web_text(&body);
                json!({
                    "url": url,
                    "ok": true,
                    "content": truncate_for_prompt(&content, max_chars)
                })
            }
            Err(error) => json!({
                "url": url,
                "ok": false,
                "error": error.to_string()
            }),
        };
        rows.push(row);
    }
    rows = process_web_extract_rows_with_llm(store, rows, payload).await?;
    let mut sections = Vec::new();
    for row in &rows {
        let url = row.get("url").and_then(Value::as_str).unwrap_or("-");
        if row.get("ok").and_then(Value::as_bool).unwrap_or(false) {
            sections.push(format!(
                "URL: {url}\n{}",
                row.get("content").and_then(Value::as_str).unwrap_or("")
            ));
        } else {
            sections.push(format!(
                "URL: {url}\nERROR: {}",
                row.get("error").and_then(Value::as_str).unwrap_or("failed")
            ));
        }
    }
    let ok = rows
        .iter()
        .any(|row| row.get("ok").and_then(Value::as_bool).unwrap_or(false));
    sections.push(format!(
        "json:\n{}",
        serde_json::to_string_pretty(&json!({"ok": ok, "results": rows}))?
    ));
    Ok(sections.join("\n\n---\n\n"))
}

struct WebExtractSummaryPlan {
    providers: Vec<LlmProvider>,
    persona: Persona,
    model_label: String,
}

async fn process_web_extract_rows_with_llm(
    store: &AppStore,
    rows: Vec<Value>,
    payload: &Value,
) -> AppResult<Vec<Value>> {
    let min_length = payload
        .get("minLength")
        .or_else(|| payload.get("min_length"))
        .and_then(Value::as_u64)
        .unwrap_or(5_000)
        .clamp(500, 200_000) as usize;
    let Some(plan) = build_web_extract_summary_plan(store, payload)? else {
        return Ok(rows);
    };
    let mut processed = Vec::with_capacity(rows.len());
    for mut row in rows {
        let ok = row.get("ok").and_then(Value::as_bool).unwrap_or(false);
        let content = row
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if ok && content.chars().count() >= min_length {
            let url = row.get("url").and_then(Value::as_str).unwrap_or("");
            let title = row.get("title").and_then(Value::as_str).unwrap_or("");
            match summarize_web_extract_content(store, &plan, url, title, &content).await {
                Ok(summary) if !summary.trim().is_empty() => {
                    row["rawContent"] = Value::String(content.clone());
                    row["content"] = Value::String(truncate_for_prompt(&summary, 5_000));
                    row["llmProcessed"] = Value::Bool(true);
                    row["llmModel"] = Value::String(plan.model_label.clone());
                }
                Ok(_) | Err(_) => {
                    row["llmProcessed"] = Value::Bool(false);
                    row["llmModel"] = Value::String(plan.model_label.clone());
                }
            }
        }
        processed.push(row);
    }
    Ok(processed)
}

fn build_web_extract_summary_plan(
    store: &AppStore,
    payload: &Value,
) -> AppResult<Option<WebExtractSummaryPlan>> {
    if payload
        .get("useLlmProcessing")
        .or_else(|| payload.get("use_llm_processing"))
        .and_then(Value::as_bool)
        == Some(false)
    {
        return Ok(None);
    }
    let explicit_processing = payload
        .get("useLlmProcessing")
        .or_else(|| payload.get("use_llm_processing"))
        .and_then(Value::as_bool)
        == Some(true);
    let payload_model = string_arg(payload, &["model"]);
    let payload_provider = string_arg(payload, &["llmProvider", "llm_provider", "summaryProvider"]);
    let payload_base_url = string_arg(payload, &["baseUrl", "base_url"]);
    let config = store.config()?;
    let assignment_configured = config
        .chat
        .auxiliary_task_assignments
        .as_object()
        .map(|assignments| assignments.contains_key("web_extract"))
        .unwrap_or(false);
    if !explicit_processing
        && payload_model.as_deref().unwrap_or("").trim().is_empty()
        && payload_provider.as_deref().unwrap_or("").trim().is_empty()
        && payload_base_url.as_deref().unwrap_or("").trim().is_empty()
        && !assignment_configured
    {
        return Ok(None);
    }

    let assignment = list_agent_auxiliary_task_assignments(store)?
        .into_iter()
        .find(|assignment| assignment.key == "web_extract");
    let assignment_provider = assignment
        .as_ref()
        .map(|assignment| assignment.provider.trim())
        .unwrap_or("");
    let provider_id = payload_provider
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or_else(|| {
            if assignment_provider.eq_ignore_ascii_case("auto") || assignment_provider.is_empty() {
                None
            } else {
                Some(assignment_provider)
            }
        });
    let model = payload_model
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or_else(|| {
            assignment
                .as_ref()
                .map(|assignment| assignment.model.trim())
                .filter(|value| !value.is_empty())
        })
        .unwrap_or("");
    let base_url = payload_base_url
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or_else(|| {
            assignment
                .as_ref()
                .map(|assignment| assignment.base_url.trim())
                .filter(|value| !value.is_empty())
        })
        .unwrap_or("");
    let api_key = assignment
        .as_ref()
        .map(|assignment| assignment.api_key.trim())
        .unwrap_or("");
    let timeout = assignment
        .as_ref()
        .map(|assignment| assignment.timeout)
        .unwrap_or(60)
        .max(1);

    let custom_model = if model.is_empty() {
        store
            .provider(None)
            .ok()
            .map(|provider| provider.model)
            .unwrap_or_default()
    } else {
        model.to_string()
    };
    let mut providers = if !base_url.is_empty() {
        vec![LlmProvider {
            id: "auxiliary-web-extract-custom".into(),
            name: "Web extract auxiliary".into(),
            provider_type: "openai_compatible".into(),
            base_url: base_url.into(),
            append_chat_path: true,
            api_key: (!api_key.is_empty()).then(|| api_key.to_string()),
            model: custom_model,
            enabled: true,
            timeout_seconds: timeout,
            ..LlmProvider::default()
        }]
    } else {
        store.provider_candidates(provider_id)?
    };
    if providers.is_empty() {
        return Err(AppError::NotFound("web_extract auxiliary provider".into()));
    }
    if !model.is_empty() {
        for provider in &mut providers {
            provider.model = model.to_string();
        }
    }
    for provider in &mut providers {
        provider.timeout_seconds = timeout;
    }
    let mut persona = store.persona(None)?;
    persona.temperature = 0.1;
    persona.max_tokens = 4_000;
    if let Some(provider_id) = provider_id {
        persona.llm_provider = provider_id.to_string();
    }
    if !model.is_empty() {
        persona.llm_model = model.to_string();
    }
    let model_label = model.to_string();
    let model_label = if model_label.trim().is_empty() {
        providers
            .first()
            .map(|provider| provider.model.clone())
            .unwrap_or_else(|| "default".into())
    } else {
        model_label
    };
    Ok(Some(WebExtractSummaryPlan {
        providers,
        persona,
        model_label,
    }))
}

async fn summarize_web_extract_content(
    store: &AppStore,
    plan: &WebExtractSummaryPlan,
    url: &str,
    title: &str,
    content: &str,
) -> AppResult<String> {
    let system_prompt = "You are an expert content analyst. Summarize extracted web content for an AI agent. Preserve concrete facts, code snippets, named entities, dates, numbers, caveats, and actionable details. Use concise markdown.".to_string();
    let user_prompt = format!(
        "Source URL: {url}\nTitle: {title}\n\nExtracted content:\n{content}\n\nCreate a concise but comprehensive markdown summary. Include important quotes or code exactly when needed."
    );
    let history = vec![ChatMessage::new(
        "__web_extract__".into(),
        "user",
        user_prompt.clone(),
        "web_extract",
    )];
    let reply = complete_chat_with_provider_failover(
        store,
        None,
        &plan.providers,
        &plan.persona,
        system_prompt,
        history,
        &user_prompt,
        None,
        None,
    )
    .await?;
    Ok(reply.content)
}

pub(super) fn web_extract_urls_from_payload(payload: &Value) -> Vec<String> {
    let mut urls = Vec::new();
    if let Some(url) = payload.get("url").and_then(Value::as_str) {
        let url = url.trim();
        if !url.is_empty() {
            urls.push(url.to_string());
        }
    }
    if let Some(items) = payload.get("urls").and_then(Value::as_array) {
        for item in items {
            if let Some(url) = item.as_str().map(str::trim).filter(|url| !url.is_empty()) {
                urls.push(url.to_string());
            }
        }
    }
    let mut deduped = Vec::new();
    for url in urls {
        if (url.starts_with("http://") || url.starts_with("https://")) && !deduped.contains(&url) {
            deduped.push(url);
        }
    }
    deduped
}

pub(super) fn extract_readable_web_text(html: &str) -> String {
    let without_scripts = strip_html_blocks(html, &["script", "style", "noscript", "svg"]);
    visible_text_preview(&without_scripts)
}

fn strip_html_blocks(html: &str, tags: &[&str]) -> String {
    let mut output = html.to_string();
    for tag in tags {
        loop {
            let lower = output.to_ascii_lowercase();
            let open = format!("<{tag}");
            let close = format!("</{tag}>");
            let Some(start) = lower.find(&open) else {
                break;
            };
            let Some(open_end_rel) = lower[start..].find('>') else {
                break;
            };
            let body_start = start + open_end_rel + 1;
            let end = lower[body_start..]
                .find(&close)
                .map(|rel| body_start + rel + close.len())
                .unwrap_or(body_start);
            output.replace_range(start..end, " ");
        }
    }
    output
}

pub(super) async fn fetch_url_text(url: &str) -> AppResult<String> {
    fetch_url_text_with_policy(url, website_policy_from_env()).await
}

pub(super) async fn fetch_url_text_for_store(store: &AppStore, url: &str) -> AppResult<String> {
    let policy = website_policy_from_store(store)?;
    fetch_url_text_with_policy(url, policy).await
}

pub(super) async fn fetch_url_text_with_policy(
    url: &str,
    policy: WebsitePolicy,
) -> AppResult<String> {
    validate_web_url_with_policy(url, &policy)?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .redirect(safe_redirect_policy_with_policy(policy))
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

pub(super) fn build_browser_snapshot(url: &str, html: &str, full: bool) -> String {
    let title = extract_title(html).unwrap_or_else(|| "(untitled)".into());
    let forms = extract_forms(html, 12);
    let inputs = extract_simple_elements(
        html,
        "input",
        40,
        &["type", "name", "id", "placeholder", "value"],
    );
    let buttons = extract_button_like_elements(html, 30);
    let links = extract_links(html, 40);
    let request_clues = extract_simple_elements(
        html,
        "script",
        30,
        &["src", "type", "crossorigin", "integrity"],
    )
    .into_iter()
    .chain(extract_simple_elements(
        html,
        "img",
        30,
        &["src", "alt", "loading"],
    ))
    .chain(extract_simple_elements(
        html,
        "link",
        30,
        &["href", "rel", "as"],
    ))
    .chain(extract_request_method_clues(html, 30))
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

pub(super) fn validate_web_url(url: &str) -> AppResult<()> {
    validate_web_url_with_policy(url, &website_policy_from_env())
}

pub(super) fn validate_web_url_with_policy(url: &str, policy: &WebsitePolicy) -> AppResult<()> {
    let parsed = reqwest::Url::parse(url)
        .map_err(|error| AppError::BadRequest(format!("invalid URL: {error}")))?;
    validate_parsed_web_url(&parsed, policy)
}

fn validate_parsed_web_url(parsed: &reqwest::Url, policy: &WebsitePolicy) -> AppResult<()> {
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(AppError::BadRequest(
            "only http/https URLs are supported by recovered browser tools".into(),
        ));
    }
    let host = parsed
        .host_str()
        .map(str::to_lowercase)
        .ok_or_else(|| AppError::BadRequest("URL host is required".into()))?;
    if blocked_web_host_literal(&host) {
        return Err(AppError::BadRequest(format!(
            "blocked private/internal URL host: {host}"
        )));
    }
    if let Some(rule) = website_policy_matching_rule(&host, policy) {
        return Err(AppError::BadRequest(format!(
            "blocked by website policy: {host} matched rule {rule}"
        )));
    }
    let port = parsed
        .port_or_known_default()
        .ok_or_else(|| AppError::BadRequest("URL port could not be resolved".into()))?;
    let host_is_ip_literal = host.parse::<IpAddr>().is_ok();
    for ip in resolve_web_host_ips(&host, port)? {
        if blocked_web_ip(ip) && !allowed_web_dns_resolution_ip(ip, host_is_ip_literal) {
            return Err(AppError::BadRequest(format!(
                "blocked private/internal URL resolution: {host} -> {ip}"
            )));
        }
    }
    Ok(())
}

pub(super) fn safe_redirect_policy() -> reqwest::redirect::Policy {
    safe_redirect_policy_with_policy(website_policy_from_env())
}

pub(super) fn safe_redirect_policy_with_policy(policy: WebsitePolicy) -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(move |attempt| {
        if attempt.previous().len() >= 10 {
            return attempt.error("too many redirects");
        }
        match validate_parsed_web_url(attempt.url(), &policy) {
            Ok(()) => attempt.follow(),
            Err(error) => attempt.error(error.to_string()),
        }
    })
}

pub(super) fn website_policy_from_store(store: &AppStore) -> AppResult<WebsitePolicy> {
    let config = store.config()?;
    Ok(website_policy_from_config_with_base(
        &config,
        Some(&store.data_dir()),
    ))
}

pub(super) fn website_policy_from_config(config: &AppConfig) -> WebsitePolicy {
    website_policy_from_config_with_base(config, None)
}

fn website_policy_from_config_with_base(
    config: &AppConfig,
    base_dir: Option<&Path>,
) -> WebsitePolicy {
    let mut rules = Vec::new();
    collect_website_policy_rules_from_value(&config.web, base_dir, &mut rules);
    rules.extend(website_policy_from_env().rules);
    rules.sort();
    rules.dedup();
    WebsitePolicy { rules }
}

fn website_policy_from_env() -> WebsitePolicy {
    let mut rules = Vec::new();
    if let Ok(env_rules) = env::var("SYNTHCHAT_WEBSITE_BLOCKLIST") {
        for rule in env_rules.split([',', ';', '\n']) {
            if let Some(rule) = normalize_website_rule(rule) {
                rules.push(rule);
            }
        }
    }
    rules.sort();
    rules.dedup();
    WebsitePolicy { rules }
}

fn collect_website_policy_rules_from_value(
    value: &Value,
    base_dir: Option<&Path>,
    rules: &mut Vec<String>,
) {
    let Some(object) = value.as_object() else {
        return;
    };
    for key in ["websiteBlocklist", "website_blocklist"] {
        if let Some(blocklist) = object.get(key) {
            let enabled = blocklist
                .get("enabled")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if enabled {
                collect_website_rule_array(blocklist.get("domains"), rules);
                collect_website_rule_array(blocklist.get("rules"), rules);
                collect_website_shared_file_rules(blocklist.get("sharedFiles"), base_dir, rules);
                collect_website_shared_file_rules(blocklist.get("shared_files"), base_dir, rules);
            }
        }
    }
    collect_website_rule_array(object.get("blockedDomains"), rules);
    collect_website_rule_array(object.get("blocked_domains"), rules);
    collect_website_shared_file_rules(object.get("blockedDomainFiles"), base_dir, rules);
    collect_website_shared_file_rules(object.get("blocked_domain_files"), base_dir, rules);
}

fn collect_website_rule_array(value: Option<&Value>, rules: &mut Vec<String>) {
    if let Some(items) = value.and_then(Value::as_array) {
        for item in items {
            if let Some(rule) = item.as_str().and_then(normalize_website_rule) {
                rules.push(rule);
            }
        }
    }
}

fn collect_website_shared_file_rules(
    value: Option<&Value>,
    base_dir: Option<&Path>,
    rules: &mut Vec<String>,
) {
    let Some(items) = value.and_then(Value::as_array) else {
        return;
    };
    for item in items {
        let Some(path) = item
            .as_str()
            .and_then(|value| resolve_website_policy_file_path(value, base_dir))
        else {
            continue;
        };
        let Ok(content) = fs::read_to_string(&path) else {
            continue;
        };
        for line in content.lines() {
            if let Some(rule) = normalize_website_rule(line) {
                rules.push(rule);
            }
        }
    }
}

fn resolve_website_policy_file_path(raw: &str, base_dir: Option<&Path>) -> Option<PathBuf> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let expanded = expand_home_path(raw);
    let path = PathBuf::from(expanded);
    if path.is_absolute() {
        Some(path)
    } else {
        Some(base_dir.unwrap_or_else(|| Path::new(".")).join(path))
    }
}

fn expand_home_path(raw: &str) -> String {
    if raw == "~" {
        return env::var("USERPROFILE")
            .or_else(|_| env::var("HOME"))
            .unwrap_or_else(|_| raw.into());
    }
    if let Some(rest) = raw.strip_prefix("~/").or_else(|| raw.strip_prefix("~\\")) {
        if let Ok(home) = env::var("USERPROFILE").or_else(|_| env::var("HOME")) {
            return PathBuf::from(home).join(rest).to_string_lossy().to_string();
        }
    }
    raw.into()
}

fn normalize_website_rule(rule: &str) -> Option<String> {
    let mut value = rule.trim().to_ascii_lowercase();
    if value.is_empty() || value.starts_with('#') {
        return None;
    }
    if value.contains("://") {
        if let Ok(parsed) = reqwest::Url::parse(&value) {
            value = parsed.host_str().unwrap_or_default().to_string();
        }
    }
    value = value
        .trim_start_matches("www.")
        .trim_end_matches('.')
        .to_string();
    value = value.split('/').next().unwrap_or("").trim().to_string();
    (!value.is_empty()).then_some(value)
}

fn website_policy_matching_rule(host: &str, policy: &WebsitePolicy) -> Option<String> {
    let host = host
        .trim()
        .to_ascii_lowercase()
        .trim_end_matches('.')
        .to_string();
    let normalized_host = host.strip_prefix("www.").unwrap_or(&host);
    for rule in &policy.rules {
        if let Some(suffix) = rule.strip_prefix("*.") {
            if normalized_host.ends_with(&format!(".{suffix}")) {
                return Some(rule.clone());
            }
        } else if normalized_host == rule || normalized_host.ends_with(&format!(".{rule}")) {
            return Some(rule.clone());
        }
    }
    None
}

fn resolve_web_host_ips(host: &str, port: u16) -> AppResult<Vec<IpAddr>> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Ok(vec![ip]);
    }
    let addrs = (host, port).to_socket_addrs().map_err(|error| {
        AppError::BadRequest(format!(
            "blocked URL because DNS resolution failed for {host}: {error}"
        ))
    })?;
    let mut ips = Vec::new();
    for addr in addrs {
        let ip = addr.ip();
        if !ips.contains(&ip) {
            ips.push(ip);
        }
    }
    if ips.is_empty() {
        return Err(AppError::BadRequest(format!(
            "blocked URL because DNS returned no addresses for {host}"
        )));
    }
    Ok(ips)
}

fn blocked_web_host_literal(host: &str) -> bool {
    let host = host.trim().trim_matches(['[', ']']).trim_end_matches('.');
    if host.is_empty() {
        return true;
    }
    if matches!(
        host,
        "localhost" | "metadata.google.internal" | "metadata.goog"
    ) || host.ends_with(".localhost")
    {
        return true;
    }
    host.parse::<IpAddr>().is_ok_and(blocked_web_ip)
}

fn blocked_web_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let octets = ip.octets();
            ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_broadcast()
                || ip.is_documentation()
                || octets[0] == 0
                || (octets[0] == 100 && (64..=127).contains(&octets[1]))
                || (octets[0] == 169 && octets[1] == 254)
                || (octets[0] == 198 && matches!(octets[1], 18 | 19))
        }
        IpAddr::V6(ip) => {
            if let Some(mapped) = ip.to_ipv4_mapped() {
                return blocked_web_ip(IpAddr::V4(mapped));
            }
            ip.is_loopback()
                || ip.is_unspecified()
                || ip.is_unique_local()
                || ip.is_unicast_link_local()
                || ip.is_multicast()
        }
    }
}

fn allowed_web_dns_resolution_ip(ip: IpAddr, host_is_ip_literal: bool) -> bool {
    if host_is_ip_literal {
        return false;
    }
    is_proxy_fake_ip(ip)
}

fn is_proxy_fake_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let octets = ip.octets();
            // TUN/fake-ip proxies commonly synthesize public hostnames into
            // 198.18.0.0/15. Treat it as routable only for DNS results, not
            // when users pass the IP literal directly.
            octets[0] == 198 && matches!(octets[1], 18 | 19)
        }
        IpAddr::V6(_) => false,
    }
}

fn extract_title(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let start = lower.find("<title")?;
    let content_start = lower[start..].find('>')? + start + 1;
    let end = lower[content_start..].find("</title>")? + content_start;
    Some(clean_text(&html[content_start..end]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn search_provider(
        id: &str,
        provider_type: &str,
        base_url: &str,
        enabled: bool,
    ) -> SearchProvider {
        SearchProvider {
            id: id.into(),
            name: id.into(),
            provider_type: provider_type.into(),
            base_url: base_url.into(),
            api_key_env: String::new(),
            api_key: None,
            enabled,
            timeout_seconds: 10,
        }
    }

    #[test]
    fn hermes_web_resolution_explicit_provider_ignores_availability() {
        let providers = vec![
            search_provider("searxng", "searxng", "http://localhost:8080", true),
            search_provider("firecrawl", "firecrawl", "", false),
        ];

        let (resolved, reason) =
            resolve_hermes_web_provider(&providers, Some("firecrawl"), "search");

        assert_eq!(
            resolved.map(|provider| provider.id.as_str()),
            Some("firecrawl")
        );
        assert_eq!(reason, "explicit_config");
    }

    #[test]
    fn hermes_web_resolution_uses_single_available_provider() {
        let providers = vec![
            search_provider("firecrawl", "firecrawl", "", true),
            search_provider("searxng", "searxng", "http://localhost:8080", true),
        ];

        let (resolved, reason) = resolve_hermes_web_provider(&providers, None, "search");

        assert_eq!(
            resolved.map(|provider| provider.id.as_str()),
            Some("searxng")
        );
        assert_eq!(reason, "single_available_provider");
    }

    #[test]
    fn hermes_web_resolution_prefers_legacy_order() {
        let providers = vec![
            search_provider("searxng", "searxng", "http://localhost:8080", true),
            search_provider("exa", "exa", "https://api.exa.ai", true),
            search_provider("ddgs", "ddgs", "https://ddgs.local", true),
        ];

        let (resolved, reason) = resolve_hermes_web_provider(&providers, None, "search");

        assert_eq!(resolved.map(|provider| provider.id.as_str()), Some("exa"));
        assert_eq!(reason, "legacy_available");
    }

    #[test]
    fn hermes_web_resolution_filters_extract_capability() {
        let providers = vec![search_provider(
            "searxng",
            "searxng",
            "http://localhost:8080",
            true,
        )];

        let (resolved, reason) = resolve_hermes_web_provider(&providers, None, "extract");

        assert!(resolved.is_none());
        assert_eq!(reason, "no_available_provider");
    }

    #[test]
    fn parallel_web_provider_is_marked_implemented_for_search_and_extract() {
        assert!(web_search_adapter_implemented("parallel"));
        assert!(web_extract_adapter_implemented("parallel"));

        let provider = search_provider("parallel", "parallel", "", true);
        assert!(web_provider_capable(&provider, "search"));
        assert!(web_provider_capable(&provider, "extract"));
    }

    #[test]
    fn web_provider_status_reports_provider_backed_extract() {
        let status = web_provider_registry_status(&[], None, "extract");

        assert_eq!(status["builtinExtract"]["implemented"], true);
        assert_eq!(status["builtinExtract"]["providerBacked"], true);
        assert_eq!(status["builtinExtract"]["fallbackFetch"], true);
        assert!(status["adapterParity"]["implementedExtractProviderTypes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value.as_str() == Some("parallel")));
    }

    #[test]
    fn web_extract_summary_plan_uses_auxiliary_assignment() {
        let dir = std::env::temp_dir().join(format!(
            "synthchat-web-extract-aux-{}",
            crate::models::new_id("test")
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let store = AppStore::new(dir.join("state.json")).unwrap();
        super::super::save_agent_auxiliary_task_assignment(
            &store,
            "web_extract",
            "custom",
            "web-summary-model",
            "https://summary.example/v1",
            "secret",
            Some(37),
            None,
        )
        .unwrap();

        let plan = build_web_extract_summary_plan(&store, &json!({}))
            .unwrap()
            .expect("web_extract auxiliary assignment should enable summarization");

        assert_eq!(plan.providers.len(), 1);
        assert_eq!(plan.providers[0].id, "auxiliary-web-extract-custom");
        assert_eq!(plan.providers[0].base_url, "https://summary.example/v1");
        assert_eq!(plan.providers[0].model, "web-summary-model");
        assert_eq!(plan.providers[0].timeout_seconds, 37);
        assert_eq!(plan.persona.llm_model, "web-summary-model");

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn ddgs_web_provider_is_search_only_without_credentials() {
        let provider = search_provider("ddgs", "ddgs", "", true);

        assert!(web_search_adapter_implemented("ddgs"));
        assert!(!web_extract_adapter_implemented("ddgs"));
        assert!(web_provider_available(&provider));
        assert!(web_provider_capable(&provider, "search"));
        assert!(!web_provider_capable(&provider, "extract"));
    }

    #[test]
    fn ddgs_html_results_parse_redirect_url_and_snippet() {
        let html = r#"
          <div class="result">
            <h2 class="result__title">
              <a rel="nofollow" class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fdocs&amp;rut=abc">Example &amp; Docs</a>
            </h2>
            <a class="result__snippet" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fdocs">A useful &lt;b&gt;snippet&lt;/b&gt;.</a>
          </div>
        "#;

        let results = ddgs_html_results(html, 5);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["title"], "Example & Docs");
        assert_eq!(results[0]["url"], "https://example.com/docs");
        assert_eq!(results[0]["content"], "A useful snippet.");
    }

    #[test]
    fn parallel_search_results_normalize_excerpts() {
        let provider = search_provider("parallel", "parallel", "https://api.parallel.ai", true);
        let value = normalize_provider_search_results(
            &provider,
            "rust agents",
            "https://api.parallel.ai/v1/search".into(),
            vec![json!({
                "title": "Parallel result",
                "url": "https://example.com/parallel",
                "excerpts": ["first excerpt", "second excerpt"],
                "position": 1
            })],
            5,
            &["excerpts", "full_content", "description"],
        );

        assert_eq!(value["providerType"], "parallel");
        assert_eq!(value["count"], 1);
        assert_eq!(
            value["results"][0]["content"],
            "first excerpt second excerpt"
        );
    }

    #[test]
    fn parallel_extract_results_normalize_full_content() {
        let rows = normalize_extract_response_rows(
            &json!({
                "results": [{
                    "url": "https://example.com/page",
                    "title": "Example",
                    "full_content": "full extracted text"
                }]
            }),
            &["https://example.com/page".into()],
            1000,
            &["full_content", "excerpts", "content", "raw_content"],
        );

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["ok"], true);
        assert_eq!(rows[0]["content"], "full extracted text");
    }

    #[test]
    fn validate_web_url_blocks_private_internal_hosts() {
        for url in [
            "http://localhost:8080",
            "http://127.0.0.1:8080",
            "http://10.0.0.1",
            "http://172.16.1.1",
            "http://192.168.1.1",
            "http://100.64.0.1",
            "http://169.254.169.254/latest/meta-data",
            "http://169.254.170.2/v2/credentials",
            "http://100.100.100.200/latest/meta-data",
            "http://metadata.google.internal/",
            "http://metadata.goog/",
            "http://[::1]/",
            "http://[fd00::1]/",
            "http://[::ffff:169.254.169.254]/",
        ] {
            assert!(validate_web_url(url).is_err(), "{url} should be blocked");
        }
    }

    #[test]
    fn validate_web_url_allows_public_http_urls() {
        assert!(validate_web_url("https://example.com/path").is_ok());
        assert!(validate_web_url("http://93.184.216.34/").is_ok());
    }

    #[test]
    fn validate_web_url_blocks_private_dns_resolution() {
        assert!(validate_web_url("http://localhost/").is_err());
        assert!(validate_web_url("http://localhost.localhost/").is_err());
    }

    #[test]
    fn proxy_fake_ip_policy_allows_dns_results_only() {
        assert!(allowed_web_dns_resolution_ip(
            "198.18.0.206".parse().unwrap(),
            false
        ));
        assert!(!allowed_web_dns_resolution_ip(
            "198.18.0.206".parse().unwrap(),
            true
        ));
        assert!(blocked_web_ip("198.18.0.206".parse().unwrap()));
        assert!(blocked_web_host_literal("198.18.0.206"));
    }

    #[test]
    fn website_policy_blocks_configured_domains() {
        let mut config = AppConfig::default();
        config.web = json!({
            "websiteBlocklist": {
                "enabled": true,
                "domains": ["blocked.example", "*.deny.test", "https://www.trimmed.test/path"]
            }
        });
        let policy = website_policy_from_config(&config);

        assert!(validate_web_url_with_policy("https://blocked.example/path", &policy).is_err());
        assert!(validate_web_url_with_policy("https://child.deny.test/path", &policy).is_err());
        assert!(validate_web_url_with_policy("https://trimmed.test/path", &policy).is_err());
        assert!(website_policy_matching_rule("example.com", &policy).is_none());
    }

    #[test]
    fn website_policy_blocks_shared_file_rules() {
        let dir = std::env::temp_dir().join(format!(
            "synthchat-website-policy-{}",
            crate::models::new_id("test")
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("blocklist.txt"),
            "# comment\n\nshared.example\n*.shared.test\nhttps://www.shared-url.test/path\n",
        )
        .unwrap();

        let mut config = AppConfig::default();
        config.web = json!({
            "websiteBlocklist": {
                "enabled": true,
                "sharedFiles": ["blocklist.txt", "missing.txt"]
            }
        });
        let policy = website_policy_from_config_with_base(&config, Some(&dir));

        assert!(validate_web_url_with_policy("https://shared.example/path", &policy).is_err());
        assert!(validate_web_url_with_policy("https://child.shared.test/path", &policy).is_err());
        assert!(validate_web_url_with_policy("https://shared-url.test/path", &policy).is_err());
        assert!(website_policy_matching_rule("allowed.example", &policy).is_none());

        let _ = std::fs::remove_dir_all(dir);
    }
}

fn extract_forms(html: &str, limit: usize) -> Vec<String> {
    collect_element_segments(html, "form", limit)
        .into_iter()
        .enumerate()
        .map(|(index, form)| {
            let tag = form
                .find('>')
                .map(|end| &form[..=end])
                .unwrap_or(form.as_str());
            let controls = extract_form_controls(&form, 10);
            format!(
                "@form{} method={} action={} id={} name={} controls=[{}]",
                index + 1,
                html_attr(&tag, "method").unwrap_or_else(|| "GET".into()),
                html_attr(&tag, "action").unwrap_or_default(),
                html_attr(&tag, "id").unwrap_or_default(),
                html_attr(&tag, "name").unwrap_or_default(),
                controls.join("; ")
            )
        })
        .collect()
}

fn extract_form_controls(form_html: &str, limit: usize) -> Vec<String> {
    let mut controls = Vec::new();
    for tag in ["input", "select", "textarea"] {
        for segment in collect_tag_segments(form_html, tag, limit.saturating_sub(controls.len())) {
            if controls.len() >= limit {
                break;
            }
            let fields = ["type", "name", "id", "placeholder", "value", "aria-label"]
                .iter()
                .filter_map(|attr| html_attr(&segment, attr).map(|value| format!("{attr}={value}")))
                .collect::<Vec<_>>();
            controls.push(format!("{tag} {}", fields.join(" ")).trim().to_string());
        }
    }
    if controls.len() < limit {
        for button in
            collect_element_segments(form_html, "button", limit.saturating_sub(controls.len()))
        {
            let tag = button
                .find('>')
                .map(|end| &button[..=end])
                .unwrap_or(button.as_str());
            let text = button
                .find('>')
                .and_then(|start| {
                    button
                        .to_ascii_lowercase()
                        .find("</button>")
                        .map(|end| clean_text(&strip_tags(&button[start + 1..end])))
                })
                .unwrap_or_default();
            let fields = ["type", "name", "id", "aria-label"]
                .iter()
                .filter_map(|attr| html_attr(tag, attr).map(|value| format!("{attr}={value}")))
                .chain((!text.is_empty()).then(|| format!("text={text}")))
                .collect::<Vec<_>>();
            controls.push(format!("button {}", fields.join(" ")).trim().to_string());
        }
    }
    controls
}

fn extract_simple_elements(html: &str, tag: &str, limit: usize, attrs: &[&str]) -> Vec<String> {
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

pub(super) fn extract_images(html: &str, limit: usize) -> Vec<String> {
    collect_tag_segments(html, "img", limit)
        .into_iter()
        .enumerate()
        .map(|(index, segment)| {
            format!(
                "@image{} src={} alt={} width={} height={} loading={}",
                index + 1,
                html_attr(&segment, "src").unwrap_or_default(),
                html_attr(&segment, "alt").unwrap_or_default(),
                html_attr(&segment, "width").unwrap_or_default(),
                html_attr(&segment, "height").unwrap_or_default(),
                html_attr(&segment, "loading").unwrap_or_default()
            )
        })
        .collect()
}

fn extract_button_like_elements(html: &str, limit: usize) -> Vec<String> {
    let mut buttons =
        extract_simple_elements(html, "button", limit, &["type", "name", "id", "aria-label"]);
    let remaining = limit.saturating_sub(buttons.len());
    if remaining > 0 {
        buttons.extend(
            collect_tag_segments(html, "input", remaining)
                .into_iter()
                .filter(|segment| {
                    html_attr(segment, "type")
                        .map(|value| {
                            matches!(value.to_lowercase().as_str(), "button" | "submit" | "reset")
                        })
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

fn extract_request_method_clues(html: &str, limit: usize) -> Vec<String> {
    let mut clues = Vec::new();
    let scripts = collect_element_segments(html, "script", 80);
    let markers = [
        "fetch(",
        "xmlhttprequest",
        ".open(",
        "axios.",
        "$.ajax",
        "method:",
    ];
    for script in scripts {
        let text = strip_tags(&script);
        let lower = text.to_ascii_lowercase();
        for marker in markers {
            let mut cursor = 0usize;
            while clues.len() < limit {
                let Some(pos_rel) = lower[cursor..].find(marker) else {
                    break;
                };
                let pos = cursor + pos_rel;
                let start = pos.saturating_sub(180);
                let end = (pos + 360).min(text.len());
                let snippet = clean_text(&text[start..end]);
                let method =
                    infer_request_method(&snippet, marker).unwrap_or_else(|| "UNKNOWN".into());
                let url = infer_request_url(&snippet, marker).unwrap_or_default();
                clues.push(format!(
                    "@request{} marker={} method={} url={} snippet={}",
                    clues.len() + 1,
                    marker,
                    method,
                    url,
                    truncate_output(&snippet, 420).replace('\n', " ")
                ));
                cursor = pos + marker.len();
            }
            if clues.len() >= limit {
                break;
            }
        }
        if clues.len() >= limit {
            break;
        }
    }
    clues
}

fn infer_request_method(snippet: &str, marker: &str) -> Option<String> {
    let lower = snippet.to_ascii_lowercase();
    if (marker.contains("open") || marker.contains("xmlhttprequest"))
        && lower.find("open(").is_some()
    {
        let pos = lower.find("open(")?;
        let rest = snippet[pos + "open(".len()..].trim_start();
        let mut chars = rest.chars();
        let first = chars.next()?;
        if first == '"' || first == '\'' {
            let method = chars
                .take_while(|ch| *ch != first)
                .collect::<String>()
                .to_ascii_uppercase();
            if matches!(
                method.as_str(),
                "GET" | "POST" | "PUT" | "PATCH" | "DELETE" | "HEAD"
            ) {
                return Some(method);
            }
        }
    }
    for method in ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD"] {
        let needle = method.to_ascii_lowercase();
        if lower.contains(&format!("method:{needle}"))
            || lower.contains(&format!("method: '{needle}'"))
            || lower.contains(&format!("method:\"{needle}\""))
            || lower.contains(&format!(".{}(", needle))
            || lower.contains(&format!("open('{method}'").to_ascii_lowercase())
            || lower.contains(&format!("open(\"{method}\"").to_ascii_lowercase())
            || ((lower.contains("open(") || lower.contains("method"))
                && (lower.contains(&format!("'{needle}'"))
                    || lower.contains(&format!("\"{needle}\""))))
        {
            return Some(method.into());
        }
    }
    None
}

fn infer_request_url(snippet: &str, marker: &str) -> Option<String> {
    if marker.contains("open") || marker.contains("xmlhttprequest") {
        let lower = snippet.to_ascii_lowercase();
        let pos = lower.find(".open(").or_else(|| lower.find("open("))?;
        let rest = &snippet[pos + lower[pos..].find('(')? + 1..];
        if let Some(comma) = rest.find(',') {
            let url_part = rest[comma + 1..].trim_start();
            let mut url_chars = url_part.chars();
            let quote = url_chars.next()?;
            if quote == '"' || quote == '\'' {
                return Some(url_chars.take_while(|ch| *ch != quote).collect::<String>());
            }
        }
        return None;
    }
    let markers = if marker.contains("axios") {
        vec![marker]
    } else {
        vec![
            "fetch(",
            "$.ajax",
            "axios.get(",
            "axios.post(",
            "axios.put(",
            "axios.patch(",
            "axios.delete(",
        ]
    };
    for marker in markers {
        if marker == "$.ajax" {
            continue;
        }
        let lower = snippet.to_ascii_lowercase();
        let Some(pos) = lower.find(marker) else {
            continue;
        };
        let rest = &snippet[pos + marker.len()..];
        let rest = rest.trim_start();
        let mut chars = rest.chars();
        let first = chars.next()?;
        if first == '"' || first == '\'' {
            let quote = first;
            return Some(chars.take_while(|ch| *ch != quote).collect::<String>());
        }
    }
    None
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
        links.push(format!(
            "@link{} href={} text={}",
            links.len() + 1,
            href,
            text
        ));
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

fn collect_element_segments(html: &str, tag: &str, limit: usize) -> Vec<String> {
    let lower = html.to_ascii_lowercase();
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut cursor = 0usize;
    let mut result = Vec::new();
    while result.len() < limit {
        let Some(start_rel) = lower[cursor..].find(&open) else {
            break;
        };
        let start = cursor + start_rel;
        let Some(open_end_rel) = lower[start..].find('>') else {
            break;
        };
        let open_end = start + open_end_rel + 1;
        let end = lower[open_end..]
            .find(&close)
            .map(|rel| open_end + rel + close.len())
            .unwrap_or(open_end);
        result.push(html[start..end].to_string());
        cursor = end;
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

pub(super) fn format_list(items: Vec<String>) -> String {
    if items.is_empty() {
        "(none)".into()
    } else {
        items.join("\n")
    }
}
