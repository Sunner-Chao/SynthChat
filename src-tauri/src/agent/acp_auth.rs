use std::env;

use serde_json::{json, Value};

use crate::{error::AppResult, models::LlmProvider, store::AppStore};

pub(super) const ACP_TERMINAL_SETUP_AUTH_METHOD_ID: &str = "synthchat-setup";

pub(super) fn acp_server_authenticate(store: &AppStore, params: &Value) -> AppResult<Value> {
    let method_id = acp_auth_string_text(params, &["methodId", "method_id", "id"]).to_lowercase();
    if method_id.is_empty() {
        return Ok(Value::Null);
    }
    let auth_methods = acp_auth_methods_for_store(store)?;
    Ok(acp_authenticate_result_from_methods(
        &method_id,
        &auth_methods,
    ))
}

pub(super) fn acp_authenticate_result_from_methods(
    method_id: &str,
    auth_methods: &[Value],
) -> Value {
    if method_id == ACP_TERMINAL_SETUP_AUTH_METHOD_ID {
        let has_provider_method = auth_methods
            .iter()
            .filter_map(|method| method.get("id").and_then(Value::as_str))
            .any(|id| id != ACP_TERMINAL_SETUP_AUTH_METHOD_ID);
        return if has_provider_method {
            json!({})
        } else {
            Value::Null
        };
    }
    let accepted = auth_methods
        .iter()
        .filter_map(|method| method.get("id").and_then(Value::as_str))
        .any(|id| id.eq_ignore_ascii_case(method_id));
    if accepted {
        json!({})
    } else {
        Value::Null
    }
}

pub(super) fn acp_auth_methods_for_store(store: &AppStore) -> AppResult<Vec<Value>> {
    let mut methods = Vec::new();
    for provider in store.providers()? {
        if !provider.enabled || !acp_provider_has_runtime_credentials(&provider) {
            continue;
        }
        let method_id = acp_provider_auth_method_id(&provider);
        if method_id.is_empty()
            || methods
                .iter()
                .any(|method: &Value| method.get("id").and_then(Value::as_str) == Some(&method_id))
        {
            continue;
        }
        methods.push(json!({
            "id": method_id,
            "name": format!("{} runtime credentials", provider.name.trim()),
            "description": format!(
                "Authenticate SynthChat using the configured {} provider credentials.",
                provider.name.trim()
            )
        }));
    }
    methods.push(acp_terminal_setup_auth_method());
    Ok(methods)
}

pub(super) fn acp_terminal_setup_auth_method() -> Value {
    json!({
        "id": ACP_TERMINAL_SETUP_AUTH_METHOD_ID,
        "name": "Configure SynthChat provider",
        "description": "Open SynthChat's interactive model/provider setup in a terminal. Use this when SynthChat has not been configured on this machine yet.",
        "type": "terminal",
        "args": ["--setup"]
    })
}

fn acp_provider_has_runtime_credentials(provider: &LlmProvider) -> bool {
    if matches!(
        provider.provider_type.trim().to_lowercase().as_str(),
        "echo" | "local" | "ollama"
    ) {
        return true;
    }
    if provider
        .api_key
        .as_deref()
        .map(str::trim)
        .is_some_and(|value| !value.is_empty())
    {
        return true;
    }
    provider
        .api_key_env
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .any(|name| {
            env::var(name)
                .ok()
                .is_some_and(|value| !value.trim().is_empty())
        })
}

fn acp_provider_auth_method_id(provider: &LlmProvider) -> String {
    for candidate in [
        provider.preset.as_deref().unwrap_or(""),
        provider.provider_type.as_str(),
        provider.id.as_str(),
    ] {
        let normalized = acp_normalize_auth_method_id(candidate);
        if !normalized.is_empty() {
            return normalized;
        }
    }
    String::new()
}

fn acp_normalize_auth_method_id(value: &str) -> String {
    value
        .trim()
        .to_lowercase()
        .replace('_', "-")
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '-')
        .collect()
}

fn acp_auth_string_text(value: &Value, keys: &[&str]) -> String {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .unwrap_or("")
        .to_string()
}
