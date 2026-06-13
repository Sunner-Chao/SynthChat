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
    if acp_provider_looks_like_bedrock(provider) && acp_has_static_aws_credentials() {
        return true;
    }
    provider
        .api_key_env
        .split(',')
        .chain(acp_provider_runtime_credential_env_candidates(provider))
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .any(|name| {
            env::var(name)
                .ok()
                .is_some_and(|value| !value.trim().is_empty())
        })
}

fn acp_provider_looks_like_bedrock(provider: &LlmProvider) -> bool {
    let haystack = format!(
        "{} {} {} {}",
        provider.id,
        provider.provider_type,
        provider.preset.as_deref().unwrap_or_default(),
        provider.base_url
    )
    .to_ascii_lowercase();
    haystack.contains("bedrock") || haystack.contains("aws")
}

fn acp_has_static_aws_credentials() -> bool {
    let env_pair = env::var("AWS_ACCESS_KEY_ID")
        .ok()
        .is_some_and(|value| !value.trim().is_empty())
        && env::var("AWS_SECRET_ACCESS_KEY")
            .ok()
            .is_some_and(|value| !value.trim().is_empty());
    env_pair || acp_static_aws_credentials_file_has_profile(&acp_aws_profile())
}

fn acp_aws_profile() -> String {
    env::var("AWS_PROFILE")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "default".into())
}

fn acp_static_aws_credentials_file_has_profile(profile: &str) -> bool {
    let Some(path) = acp_aws_shared_credentials_path() else {
        return false;
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    acp_static_aws_credentials_text_has_profile(profile, &text)
}

fn acp_aws_shared_credentials_path() -> Option<std::path::PathBuf> {
    env::var_os("AWS_SHARED_CREDENTIALS_FILE")
        .filter(|value| !value.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| {
            env::var_os("HOME")
                .filter(|value| !value.is_empty())
                .map(std::path::PathBuf::from)
                .or_else(|| {
                    env::var_os("USERPROFILE")
                        .filter(|value| !value.is_empty())
                        .map(std::path::PathBuf::from)
                })
                .map(|home| home.join(".aws").join("credentials"))
        })
}

fn acp_static_aws_credentials_text_has_profile(profile: &str, text: &str) -> bool {
    let target = profile.trim();
    if target.is_empty() {
        return false;
    }
    let mut in_target = false;
    let mut access_key = false;
    let mut secret_key = false;
    for raw_line in text.lines() {
        let mut line = raw_line;
        if let Some((before, _)) = line.split_once('#') {
            line = before;
        }
        if let Some((before, _)) = line.split_once(';') {
            line = before;
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            if in_target && access_key && secret_key {
                return true;
            }
            let section = line.trim_start_matches('[').trim_end_matches(']').trim();
            in_target = section == target;
            access_key = false;
            secret_key = false;
            continue;
        }
        if !in_target {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if value.trim().is_empty() {
            continue;
        }
        match key.trim().to_ascii_lowercase().as_str() {
            "aws_access_key_id" => access_key = true,
            "aws_secret_access_key" => secret_key = true,
            _ => {}
        }
    }
    in_target && access_key && secret_key
}

fn acp_provider_runtime_credential_env_candidates(provider: &LlmProvider) -> Vec<&'static str> {
    let id = provider.id.to_ascii_lowercase();
    let provider_type = provider.provider_type.to_ascii_lowercase();
    let preset = provider
        .preset
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let base = provider.base_url.to_ascii_lowercase();
    let model = provider.model.to_ascii_lowercase();
    let haystack = format!("{id} {provider_type} {preset} {base} {model}");

    if haystack.contains("openrouter") {
        return vec!["OPENROUTER_API_KEY", "OPENAI_API_KEY"];
    }
    if haystack.contains("anthropic") || model.contains("claude") {
        return vec![
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_TOKEN",
            "CLAUDE_CODE_OAUTH_TOKEN",
        ];
    }
    if haystack.contains("gemini") || haystack.contains("google") {
        return vec!["GOOGLE_API_KEY", "GEMINI_API_KEY"];
    }
    if haystack.contains("kimi") || haystack.contains("moonshot") {
        return vec![
            "KIMI_API_KEY",
            "KIMI_CODING_API_KEY",
            "KIMI_CN_API_KEY",
            "MOONSHOT_API_KEY",
        ];
    }
    if haystack.contains("minimax") {
        return vec!["MINIMAX_API_KEY", "MINIMAX_CN_API_KEY"];
    }
    if haystack.contains("xai") || haystack.contains("x.ai") || haystack.contains("grok") {
        return vec!["XAI_API_KEY"];
    }
    if haystack.contains("zai") || haystack.contains("z.ai") || haystack.contains("glm") {
        return vec!["GLM_API_KEY", "ZAI_API_KEY", "Z_AI_API_KEY"];
    }
    if haystack.contains("deepseek") {
        return vec!["DEEPSEEK_API_KEY"];
    }
    if haystack.contains("stepfun") || haystack.contains("step-plan") {
        return vec!["STEPFUN_API_KEY"];
    }
    if haystack.contains("copilot") || haystack.contains("github") {
        return vec!["COPILOT_GITHUB_TOKEN", "GH_TOKEN", "GITHUB_TOKEN"];
    }
    if haystack.contains("opencode") {
        return vec!["OPENCODE_API_KEY"];
    }
    if haystack.contains("kilo") {
        return vec!["KILOCODE_API_KEY"];
    }
    if haystack.contains("huggingface") || haystack.contains("hugging-face") {
        return vec!["HF_TOKEN", "HF_API_KEY", "HUGGINGFACE_API_KEY"];
    }
    if haystack.contains("novita") {
        return vec!["NOVITA_API_KEY"];
    }
    if haystack.contains("nvidia") || haystack.contains("nemotron") {
        return vec!["NVIDIA_API_KEY"];
    }
    if haystack.contains("xiaomi") || haystack.contains("mimo") {
        return vec!["XIAOMI_API_KEY"];
    }
    if haystack.contains("tencent") || haystack.contains("tokenhub") {
        return vec!["TOKENHUB_API_KEY"];
    }
    if haystack.contains("arcee") {
        return vec!["ARCEE_API_KEY"];
    }
    if haystack.contains("gmi") {
        return vec!["GMI_API_KEY"];
    }
    if haystack.contains("cohere") {
        return vec!["COHERE_API_KEY"];
    }
    if haystack.contains("dashscope") || haystack.contains("alibaba") || haystack.contains("qwen") {
        return vec!["DASHSCOPE_API_KEY", "ALIBABA_CODING_PLAN_API_KEY"];
    }
    if provider_type == "openai" || haystack.contains("openai") {
        return vec!["OPENAI_API_KEY", "OPENROUTER_API_KEY"];
    }
    Vec::new()
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
