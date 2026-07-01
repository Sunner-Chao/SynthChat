use std::{
    collections::HashMap,
    fs,
    path::PathBuf,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    error::{AppError, AppResult},
    models::LlmProvider,
};

const MODELS_DEV_URL: &str = "https://models.dev/api.json";
const MEMORY_CACHE_TTL: Duration = Duration::from_secs(60 * 60);
const DEEPSEEK_MODEL_LIST_BASE_URL: &str = "https://api.deepseek.com";
const XIAOMI_ANTHROPIC_MODEL_LIST_DEFAULT_BASE_URL: &str = "https://token-plan-sgp.xiaomimimo.com";

static MODELS_DEV_CACHE: OnceLock<Mutex<Option<CachedCatalog>>> = OnceLock::new();

#[derive(Debug, Clone)]
struct CachedCatalog {
    loaded_at: Instant,
    data: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ModelCapabilities {
    pub provider_id: String,
    pub model_id: String,
    pub models_dev_provider_id: String,
    pub supports_tools: bool,
    pub supports_vision: bool,
    pub supports_reasoning: bool,
    pub supports_pdf: bool,
    pub supports_audio_input: bool,
    pub supports_structured_output: bool,
    pub open_weights: bool,
    pub input_modalities: Vec<String>,
    pub output_modalities: Vec<String>,
    pub context_window: Option<u64>,
    pub max_output_tokens: Option<u64>,
    pub model_family: String,
    pub status: String,
    pub knowledge_cutoff: String,
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderCatalogInfo {
    pub id: String,
    pub name: String,
    pub api: String,
    pub doc: String,
    pub env: Vec<String>,
    pub model_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCatalogEntry {
    pub id: String,
    pub name: String,
    pub family: String,
    pub capabilities: ModelCapabilities,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DetectedModelList {
    pub ok: bool,
    pub source: String,
    pub provider_id: String,
    pub provider_type: String,
    pub base_url: String,
    pub models: Vec<ModelCatalogEntry>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct PartialModelCapabilities {
    supports_tools: Option<bool>,
    supports_vision: Option<bool>,
    supports_reasoning: Option<bool>,
    supports_pdf: Option<bool>,
    supports_audio_input: Option<bool>,
    supports_structured_output: Option<bool>,
    open_weights: Option<bool>,
    input_modalities: Option<Vec<String>>,
    output_modalities: Option<Vec<String>>,
    context_window: Option<u64>,
    max_output_tokens: Option<u64>,
    model_family: Option<String>,
    status: Option<String>,
    knowledge_cutoff: Option<String>,
}

fn provider_mapping() -> &'static HashMap<&'static str, &'static str> {
    static MAPPING: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    MAPPING.get_or_init(|| {
        HashMap::from([
            ("openrouter", "openrouter"),
            ("novita", "novita-ai"),
            ("novita-ai", "novita-ai"),
            ("novitaai", "novita-ai"),
            ("anthropic", "anthropic"),
            ("claude", "anthropic"),
            ("claude-code", "anthropic"),
            ("nous", "nous"),
            ("openai", "openai"),
            ("openai-api", "openai"),
            ("openai-codex", "openai"),
            ("zai", "zai"),
            ("glm", "zai"),
            ("z-ai", "zai"),
            ("z.ai", "zai"),
            ("zhipu", "zai"),
            ("kimi-for-coding", "kimi-for-coding"),
            ("kimi", "kimi-for-coding"),
            ("kimi-coding", "kimi-for-coding"),
            ("moonshot", "kimi-for-coding"),
            ("stepfun", "stepfun"),
            ("step", "stepfun"),
            ("stepfun-coding-plan", "stepfun"),
            ("kimi-coding-cn", "kimi-for-coding"),
            ("minimax", "minimax"),
            ("minimax-oauth", "minimax"),
            ("minimax-cn", "minimax-cn"),
            ("minimax-china", "minimax-cn"),
            ("minimax_cn", "minimax-cn"),
            ("deepseek", "deepseek"),
            ("deep-seek", "deepseek"),
            ("alibaba", "alibaba"),
            ("dashscope", "alibaba"),
            ("aliyun", "alibaba"),
            ("qwen", "alibaba"),
            ("alibaba-cloud", "alibaba"),
            ("qwen-oauth", "alibaba"),
            ("alibaba-coding-plan", "alibaba-coding-plan"),
            ("alibaba-coding", "alibaba-coding-plan"),
            ("alibaba_coding", "alibaba-coding-plan"),
            ("alibaba_coding_plan", "alibaba-coding-plan"),
            ("copilot", "github-copilot"),
            ("github", "github-copilot"),
            ("github-copilot", "github-copilot"),
            ("copilot-acp", "github-copilot"),
            ("github-copilot-acp", "github-copilot"),
            ("copilot-acp-agent", "github-copilot"),
            ("opencode", "opencode"),
            ("opencode-zen", "opencode"),
            ("zen", "opencode"),
            ("opencode-go", "opencode-go"),
            ("go", "opencode-go"),
            ("opencode-go-sub", "opencode-go"),
            ("kilocode", "kilo"),
            ("kilo-code", "kilo"),
            ("kilo-gateway", "kilo"),
            ("kilo", "kilo"),
            ("fireworks", "fireworks-ai"),
            ("huggingface", "huggingface"),
            ("hf", "huggingface"),
            ("hugging-face", "huggingface"),
            ("huggingface-hub", "huggingface"),
            ("gemini", "google"),
            ("google", "google"),
            ("google-gemini-cli", "google"),
            ("gemini-cli", "google"),
            ("gemini-oauth", "google"),
            ("xai", "xai"),
            ("x-ai", "xai"),
            ("x.ai", "xai"),
            ("grok", "xai"),
            ("xai-oauth", "xai"),
            ("grok-oauth", "xai"),
            ("x-ai-oauth", "xai"),
            ("xai-grok-oauth", "xai"),
            ("xiaomi", "xiaomi"),
            ("mimo", "xiaomi"),
            ("xiaomi-mimo", "xiaomi"),
            ("tencent-tokenhub", "tencent-tokenhub"),
            ("tencent", "tencent-tokenhub"),
            ("tokenhub", "tencent-tokenhub"),
            ("tencent-cloud", "tencent-tokenhub"),
            ("tencentmaas", "tencent-tokenhub"),
            ("nvidia", "nvidia"),
            ("nim", "nvidia"),
            ("nvidia-nim", "nvidia"),
            ("build-nvidia", "nvidia"),
            ("nemotron", "nvidia"),
            ("arcee", "arcee"),
            ("arcee-ai", "arcee"),
            ("arceeai", "arcee"),
            ("gmi", "gmi"),
            ("gmi-cloud", "gmi"),
            ("gmicloud", "gmi"),
            ("groq", "groq"),
            ("mistral", "mistral"),
            ("togetherai", "togetherai"),
            ("perplexity", "perplexity"),
            ("cohere", "cohere"),
            ("azure-foundry", "azure-foundry"),
            ("lmstudio", "lmstudio"),
            ("lm-studio", "lmstudio"),
            ("lm_studio", "lmstudio"),
            ("ollama-cloud", "ollama-cloud"),
            ("ollama", "ollama"),
            ("vllm", "local"),
            ("llamacpp", "local"),
            ("llama.cpp", "local"),
            ("llama-cpp", "local"),
            ("bedrock", "bedrock"),
            ("aws", "bedrock"),
            ("aws-bedrock", "bedrock"),
            ("amazon-bedrock", "bedrock"),
            ("amazon", "bedrock"),
        ])
    })
}

pub fn models_dev_provider_id(provider_id: &str) -> String {
    let key = provider_id
        .trim()
        .split_once(":cred-")
        .map(|(base, _)| base)
        .unwrap_or(provider_id)
        .to_ascii_lowercase();
    provider_mapping()
        .get(key.as_str())
        .copied()
        .unwrap_or(key.as_str())
        .to_string()
}

fn catalog_cache_path() -> PathBuf {
    let base = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(|parent| parent.to_path_buf()))
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("synthchat-data").join("models_dev_cache.json")
}

fn memory_cached_catalog() -> Option<Value> {
    let cache = MODELS_DEV_CACHE.get_or_init(|| Mutex::new(None));
    let guard = cache.lock().ok()?;
    let cached = guard.as_ref()?;
    if cached.loaded_at.elapsed() < MEMORY_CACHE_TTL {
        Some(cached.data.clone())
    } else {
        None
    }
}

fn set_memory_cache(data: Value) {
    let cache = MODELS_DEV_CACHE.get_or_init(|| Mutex::new(None));
    if let Ok(mut guard) = cache.lock() {
        *guard = Some(CachedCatalog {
            loaded_at: Instant::now(),
            data,
        });
    }
}

fn load_disk_catalog() -> Option<Value> {
    let bytes = fs::read(catalog_cache_path()).ok()?;
    serde_json::from_slice::<Value>(&bytes).ok()
}

fn save_disk_catalog(data: &Value) -> AppResult<()> {
    let path = catalog_cache_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| AppError::BadRequest(format!("cannot create model cache dir: {err}")))?;
    }
    let tmp = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec(data)
        .map_err(|err| AppError::BadRequest(format!("cannot serialize model cache: {err}")))?;
    fs::write(&tmp, bytes)
        .map_err(|err| AppError::BadRequest(format!("cannot write model cache: {err}")))?;
    fs::rename(&tmp, &path)
        .map_err(|err| AppError::BadRequest(format!("cannot replace model cache: {err}")))?;
    Ok(())
}

pub async fn fetch_models_dev_catalog(force_refresh: bool) -> AppResult<Value> {
    if !force_refresh {
        if let Some(data) = memory_cached_catalog() {
            return Ok(data);
        }
        if let Some(data) = load_disk_catalog() {
            set_memory_cache(data.clone());
            return Ok(data);
        }
    }

    let response = reqwest::Client::new()
        .get(MODELS_DEV_URL)
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .map_err(|err| AppError::BadRequest(format!("cannot fetch models.dev catalog: {err}")))?;
    let status = response.status();
    if !status.is_success() {
        return Err(AppError::BadRequest(format!(
            "models.dev catalog returned HTTP {status}"
        )));
    }
    let data = response
        .json::<Value>()
        .await
        .map_err(|err| AppError::BadRequest(format!("cannot parse models.dev catalog: {err}")))?;
    if !data.is_object() {
        return Err(AppError::BadRequest(
            "models.dev catalog did not return an object".into(),
        ));
    }
    save_disk_catalog(&data)?;
    set_memory_cache(data.clone());
    Ok(data)
}

fn catalog_for_lookup() -> Option<Value> {
    memory_cached_catalog().or_else(load_disk_catalog)
}

fn provider_models<'a>(
    catalog: &'a Value,
    provider_id: &str,
) -> Option<&'a serde_json::Map<String, Value>> {
    let mdev_id = models_dev_provider_id(provider_id);
    catalog.get(&mdev_id)?.get("models")?.as_object()
}

fn find_model_entry<'a>(
    models: &'a serde_json::Map<String, Value>,
    model_id: &str,
) -> Option<(&'a str, &'a Value)> {
    if let Some(entry) = models.get_key_value(model_id) {
        return Some((entry.0.as_str(), entry.1));
    }
    let lower = model_id.to_ascii_lowercase();
    for (id, entry) in models {
        if id.to_ascii_lowercase() == lower {
            return Some((id.as_str(), entry));
        }
    }
    for suffix in [":cloud", "-cloud"] {
        let suffixed = format!("{model_id}{suffix}");
        if let Some(entry) = models.get_key_value(&suffixed) {
            return Some((entry.0.as_str(), entry.1));
        }
        let suffixed_lower = suffixed.to_ascii_lowercase();
        for (id, entry) in models {
            if id.to_ascii_lowercase() == suffixed_lower {
                return Some((id.as_str(), entry));
            }
        }
    }
    None
}

fn string_vec(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn lower_string_vec(value: Option<&Value>) -> Vec<String> {
    string_vec(value)
        .into_iter()
        .map(|item| item.trim().to_ascii_lowercase())
        .filter(|item| !item.is_empty())
        .collect()
}

fn positive_u64(value: Option<&Value>) -> Option<u64> {
    value
        .and_then(Value::as_u64)
        .filter(|value| *value > 0)
        .or_else(|| {
            value
                .and_then(Value::as_f64)
                .filter(|value| *value > 0.0)
                .map(|value| value as u64)
        })
}

fn object_bool(object: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<bool> {
    keys.iter()
        .find_map(|key| object.get(*key))
        .and_then(Value::as_bool)
}

fn object_u64(object: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<u64> {
    keys.iter().find_map(|key| positive_u64(object.get(*key)))
}

fn object_string(object: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| object.get(*key))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn object_string_vec(
    object: &serde_json::Map<String, Value>,
    keys: &[&str],
) -> Option<Vec<String>> {
    keys.iter().find_map(|key| {
        let values = lower_string_vec(object.get(*key));
        if values.is_empty() {
            None
        } else {
            Some(values)
        }
    })
}

fn object_modalities(object: &serde_json::Map<String, Value>) -> (Option<Vec<String>>, Option<Vec<String>>) {
    let direct_input = object_string_vec(object, &["inputModalities", "input_modalities"]);
    let direct_output = object_string_vec(object, &["outputModalities", "output_modalities"]);
    let nested = object.get("modalities").and_then(Value::as_object);
    let nested_input = nested
        .and_then(|value| {
            let modalities = lower_string_vec(value.get("input"));
            if modalities.is_empty() {
                None
            } else {
                Some(modalities)
            }
        });
    let nested_output = nested
        .and_then(|value| {
            let modalities = lower_string_vec(value.get("output"));
            if modalities.is_empty() {
                None
            } else {
                Some(modalities)
            }
        });
    (direct_input.or(nested_input), direct_output.or(nested_output))
}

fn partial_capabilities_from_object(
    object: &serde_json::Map<String, Value>,
) -> Option<PartialModelCapabilities> {
    let (input_modalities, output_modalities) = object_modalities(object);
    let partial = PartialModelCapabilities {
        supports_tools: object_bool(object, &["supportsTools", "supports_tools", "tool_call"]),
        supports_vision: object_bool(object, &["supportsVision", "supports_vision", "attachment"]),
        supports_reasoning: object_bool(object, &["supportsReasoning", "supports_reasoning", "reasoning"]),
        supports_pdf: object_bool(object, &["supportsPdf", "supports_pdf"]),
        supports_audio_input: object_bool(object, &["supportsAudioInput", "supports_audio_input"]),
        supports_structured_output: object_bool(
            object,
            &["supportsStructuredOutput", "supports_structured_output", "structured_output"],
        ),
        open_weights: object_bool(object, &["openWeights", "open_weights"]),
        input_modalities,
        output_modalities,
        context_window: object_u64(object, &["contextWindow", "context_window", "inputTokenLimit"]),
        max_output_tokens: object_u64(object, &["maxOutputTokens", "max_output_tokens", "outputTokenLimit"]),
        model_family: object_string(object, &["modelFamily", "model_family", "family"]),
        status: object_string(object, &["status"]),
        knowledge_cutoff: object_string(object, &["knowledgeCutoff", "knowledge_cutoff", "knowledge"]),
    };
    let has_any = partial.supports_tools.is_some()
        || partial.supports_vision.is_some()
        || partial.supports_reasoning.is_some()
        || partial.supports_pdf.is_some()
        || partial.supports_audio_input.is_some()
        || partial.supports_structured_output.is_some()
        || partial.open_weights.is_some()
        || partial.input_modalities.is_some()
        || partial.output_modalities.is_some()
        || partial.context_window.is_some()
        || partial.max_output_tokens.is_some()
        || partial.model_family.is_some()
        || partial.status.is_some()
        || partial.knowledge_cutoff.is_some();
    has_any.then_some(partial)
}

fn normalize_modalities(mut values: Vec<String>) -> Vec<String> {
    values.sort();
    values.dedup();
    values
}

fn apply_partial_capabilities(
    mut base: ModelCapabilities,
    partial: PartialModelCapabilities,
    source: &str,
) -> ModelCapabilities {
    let explicit_input_modalities = partial.input_modalities.clone();
    let explicit_output_modalities = partial.output_modalities.clone();
    if let Some(input_modalities) = explicit_input_modalities.clone() {
        base.input_modalities = normalize_modalities(input_modalities);
    }
    if let Some(output_modalities) = explicit_output_modalities {
        base.output_modalities = normalize_modalities(output_modalities);
    }
    if let Some(value) = partial.supports_tools {
        base.supports_tools = value;
    }
    if let Some(value) = partial.supports_reasoning {
        base.supports_reasoning = value;
    }
    if let Some(value) = partial.supports_structured_output {
        base.supports_structured_output = value;
    }
    if let Some(value) = partial.open_weights {
        base.open_weights = value;
    }
    if let Some(value) = partial.context_window {
        base.context_window = Some(value);
    }
    if let Some(value) = partial.max_output_tokens {
        base.max_output_tokens = Some(value);
    }
    if let Some(value) = partial.model_family {
        base.model_family = value;
    }
    if let Some(value) = partial.status {
        base.status = value;
    }
    if let Some(value) = partial.knowledge_cutoff {
        base.knowledge_cutoff = value;
    }
    if explicit_input_modalities.is_some() {
        if partial.supports_vision.is_none() {
            base.supports_vision = base.input_modalities.iter().any(|item| item == "image");
        }
        if partial.supports_pdf.is_none() {
            base.supports_pdf = base.input_modalities.iter().any(|item| item == "pdf");
        }
        if partial.supports_audio_input.is_none() {
            base.supports_audio_input = base.input_modalities.iter().any(|item| item == "audio");
        }
    }
    if let Some(value) = partial.supports_vision {
        base.supports_vision = value;
    }
    if let Some(value) = partial.supports_pdf {
        base.supports_pdf = value;
    }
    if let Some(value) = partial.supports_audio_input {
        base.supports_audio_input = value;
    }
    if base.supports_vision && !base.input_modalities.iter().any(|item| item == "image") {
        base.input_modalities.push("image".into());
        base.input_modalities = normalize_modalities(base.input_modalities);
    }
    if !base.supports_vision {
        base.input_modalities.retain(|item| item != "image");
    }
    if base.supports_pdf && !base.input_modalities.iter().any(|item| item == "pdf") {
        base.input_modalities.push("pdf".into());
        base.input_modalities = normalize_modalities(base.input_modalities);
    }
    if base.supports_audio_input && !base.input_modalities.iter().any(|item| item == "audio") {
        base.input_modalities.push("audio".into());
        base.input_modalities = normalize_modalities(base.input_modalities);
    }
    base.source = source.to_string();
    base
}

fn provider_model_config<'a>(
    provider: &'a LlmProvider,
    model_id: &str,
) -> Option<&'a serde_json::Map<String, Value>> {
    let model_id = model_id.trim();
    if model_id.is_empty() {
        return None;
    }
    provider
        .models
        .as_object()
        .and_then(|models| models.get(model_id))
        .and_then(Value::as_object)
}

fn provider_model_override(
    provider: &LlmProvider,
    model_id: &str,
    base: &ModelCapabilities,
) -> Option<ModelCapabilities> {
    let config = provider_model_config(provider, model_id)?;
    let partial = config
        .get("capabilities")
        .and_then(Value::as_object)
        .and_then(partial_capabilities_from_object)
        .or_else(|| partial_capabilities_from_object(config))?;
    Some(apply_partial_capabilities(
        base.clone(),
        partial,
        "configured",
    ))
}

fn curated_gateway_capabilities(provider: &LlmProvider, model_id: &str) -> Option<ModelCapabilities> {
    let host = provider.base_url.trim().to_ascii_lowercase();
    let model = model_id.trim().to_ascii_lowercase();
    if host.contains("xiaomimimo.com") && model == "mimo-v2.5" {
        return Some(ModelCapabilities {
            provider_id: if provider.id.trim().is_empty() {
                provider.provider_type.clone()
            } else {
                provider.id.clone()
            },
            model_id: model_id.to_string(),
            models_dev_provider_id: models_dev_provider_id("xiaomi"),
            supports_tools: true,
            supports_vision: true,
            supports_reasoning: false,
            supports_pdf: false,
            supports_audio_input: false,
            supports_structured_output: true,
            open_weights: false,
            input_modalities: vec!["text".into(), "image".into()],
            output_modalities: vec!["text".into()],
            context_window: None,
            max_output_tokens: None,
            model_family: "mimo".into(),
            status: String::new(),
            knowledge_cutoff: String::new(),
            source: "curated".into(),
        });
    }
    None
}

fn capabilities_from_entry(
    provider_id: &str,
    model_id: &str,
    resolved_model_id: &str,
    entry: &Value,
    source: &str,
) -> ModelCapabilities {
    let input_modalities = string_vec(entry.pointer("/modalities/input"));
    let output_modalities = string_vec(entry.pointer("/modalities/output"));
    let supports_vision = if input_modalities.is_empty() {
        entry
            .get("attachment")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    } else {
        input_modalities.iter().any(|item| item == "image")
    };
    let supports_pdf = input_modalities.iter().any(|item| item == "pdf");
    let supports_audio_input = input_modalities.iter().any(|item| item == "audio");
    ModelCapabilities {
        provider_id: provider_id.to_string(),
        model_id: resolved_model_id.to_string(),
        models_dev_provider_id: models_dev_provider_id(provider_id),
        supports_tools: entry
            .get("tool_call")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        supports_vision,
        supports_reasoning: entry
            .get("reasoning")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        supports_pdf,
        supports_audio_input,
        supports_structured_output: entry
            .get("structured_output")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        open_weights: entry
            .get("open_weights")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        input_modalities,
        output_modalities,
        context_window: positive_u64(entry.pointer("/limit/context")),
        max_output_tokens: positive_u64(entry.pointer("/limit/output")),
        model_family: entry
            .get("family")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        status: entry
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        knowledge_cutoff: entry
            .get("knowledge")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        source: if resolved_model_id == model_id {
            source.to_string()
        } else {
            format!("{source}:matched:{model_id}")
        },
    }
}

pub fn lookup_model_capabilities(provider_id: &str, model_id: &str) -> Option<ModelCapabilities> {
    let catalog = catalog_for_lookup()?;
    let models = provider_models(&catalog, provider_id)?;
    let (resolved_id, entry) = find_model_entry(models, model_id)?;
    Some(capabilities_from_entry(
        provider_id,
        model_id,
        resolved_id,
        entry,
        "models.dev",
    ))
}

pub fn infer_model_capabilities(provider: &LlmProvider) -> ModelCapabilities {
    let provider_type = provider.provider_type.trim().to_ascii_lowercase();
    let provider_id = if provider.id.trim().is_empty() {
        provider_type.as_str()
    } else {
        provider.id.trim()
    };
    let model = provider.model.trim().to_ascii_lowercase();
    let supports_vision = model.contains("vision")
        || model.contains("gpt-4o")
        || model.contains("gpt-4.1")
        || model.contains("gemini")
        || model.contains("claude-3")
        || model.contains("claude-4")
        || model.contains("qwen-vl")
        || model.contains("vl-")
        || model.contains("omni");
    let supports_reasoning = model.contains("reason")
        || model.contains("thinking")
        || model.contains("o1")
        || model.contains("o3")
        || model.contains("o4")
        || model.contains("r1")
        || model.contains("gpt-5")
        || model.contains("claude-4")
        || model.contains("gemini-2.5");
    let supports_tools = !matches!(provider_type.as_str(), "echo" | "completion");
    ModelCapabilities {
        provider_id: provider_id.to_string(),
        model_id: provider.model.clone(),
        models_dev_provider_id: models_dev_provider_id(provider_id),
        supports_tools,
        supports_vision,
        supports_reasoning,
        supports_pdf: supports_vision,
        supports_audio_input: false,
        supports_structured_output: supports_tools,
        open_weights: false,
        input_modalities: if supports_vision {
            vec!["text".into(), "image".into()]
        } else {
            vec!["text".into()]
        },
        output_modalities: vec!["text".into()],
        context_window: None,
        max_output_tokens: None,
        model_family: String::new(),
        status: String::new(),
        knowledge_cutoff: String::new(),
        source: "heuristic".into(),
    }
}

pub fn provider_model_capabilities(provider: &LlmProvider) -> ModelCapabilities {
    let provider_id = if provider.id.trim().is_empty() {
        provider.provider_type.as_str()
    } else {
        provider.id.as_str()
    };
    let model_id = provider.model.trim();
    let mut base = lookup_model_capabilities(provider_id, model_id)
        .or_else(|| lookup_model_capabilities(&provider.provider_type, model_id))
        .or_else(|| curated_gateway_capabilities(provider, model_id))
        .unwrap_or_else(|| infer_model_capabilities(provider));
    if let Some(overridden) = provider_model_override(provider, model_id, &base) {
        base = overridden;
    }
    base
}

pub fn model_capability_prompt_block(provider: &LlmProvider) -> String {
    let caps = provider_model_capabilities(provider);
    let mut flags = Vec::new();
    if caps.supports_tools {
        flags.push("tools");
    }
    if caps.supports_reasoning {
        flags.push("reasoning");
    }
    if caps.supports_vision {
        flags.push("vision");
    }
    if caps.supports_pdf {
        flags.push("pdf");
    }
    if caps.supports_audio_input {
        flags.push("audio-input");
    }
    if caps.supports_structured_output {
        flags.push("structured-output");
    }
    let context = caps
        .context_window
        .map(|value| value.to_string())
        .unwrap_or_else(|| "unknown".into());
    let output = caps
        .max_output_tokens
        .map(|value| value.to_string())
        .unwrap_or_else(|| "unknown".into());
    format!(
        "Current LLM model metadata: provider={}, model={}, source={}, capabilities={}, contextWindowTokens={}, maxOutputTokens={}.",
        caps.provider_id,
        caps.model_id,
        caps.source,
        if flags.is_empty() { "basic".into() } else { flags.join(",") },
        context,
        output
    )
}

pub fn provider_catalog_info(provider_id: &str) -> Option<ProviderCatalogInfo> {
    let catalog = catalog_for_lookup()?;
    let mdev_id = models_dev_provider_id(provider_id);
    let provider = catalog.get(&mdev_id)?.as_object()?;
    let env = string_vec(provider.get("env"));
    let model_count = provider
        .get("models")
        .and_then(Value::as_object)
        .map(|models| models.len())
        .unwrap_or(0);
    Some(ProviderCatalogInfo {
        id: mdev_id.clone(),
        name: provider
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or(&mdev_id)
            .to_string(),
        api: provider
            .get("api")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        doc: provider
            .get("doc")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        env,
        model_count,
    })
}

fn should_hide_from_provider_catalog(provider_id: &str, model_id: &str) -> bool {
    let provider = provider_id.trim().to_ascii_lowercase();
    let model = model_id.trim().to_ascii_lowercase();
    if matches!(provider.as_str(), "gemini" | "google") {
        matches!(
            model.as_str(),
            "gemini-1.5-flash"
                | "gemini-1.5-pro"
                | "gemini-1.5-flash-8b"
                | "gemini-2.0-flash"
                | "gemini-2.0-flash-lite"
                | "gemma-4-31b-it"
                | "gemma-4-26b-it"
                | "gemma-4-26b-a4b-it"
                | "gemma-3-1b"
                | "gemma-3-1b-it"
                | "gemma-3-2b"
                | "gemma-3-2b-it"
                | "gemma-3-4b"
                | "gemma-3-4b-it"
                | "gemma-3-12b"
                | "gemma-3-12b-it"
                | "gemma-3-27b"
                | "gemma-3-27b-it"
        )
    } else {
        false
    }
}

fn looks_like_noise_model(model_id: &str) -> bool {
    let model = model_id.to_ascii_lowercase();
    model.contains("embedding")
        || model.contains("-tts")
        || model.contains("live-")
        || model.contains("-image")
        || model.contains("-customtools")
        || model.contains("-preview-")
        || model.contains("-exp-")
}

pub async fn detect_provider_models(provider: LlmProvider) -> AppResult<DetectedModelList> {
    let provider_id = provider.id.trim().to_string();
    let provider_type = provider.provider_type.trim().to_ascii_lowercase();
    let use_deepseek_model_endpoint = uses_deepseek_model_endpoint(&provider);
    let use_xiaomi_anthropic_model_endpoint = uses_xiaomi_anthropic_model_endpoint(&provider);
    let base_url = live_model_base_url(&provider);
    let api_key = live_model_api_key(&provider);
    let static_provider_id = if use_deepseek_model_endpoint {
        "deepseek"
    } else if use_xiaomi_anthropic_model_endpoint {
        "xiaomi"
    } else {
        match provider_type.as_str() {
            "openai_responses" | "openai-responses" => "openai",
            "" => provider_id.as_str(),
            _ => provider_type.as_str(),
        }
    };
    let fallback = list_agentic_models(static_provider_id);

    if base_url.trim().is_empty() {
        return Ok(DetectedModelList {
            ok: !fallback.is_empty(),
            source: "catalog".into(),
            provider_id,
            provider_type,
            base_url,
            models: fallback,
            error: Some("baseUrl is empty; using built-in catalog".into()),
        });
    }

    let live = if use_deepseek_model_endpoint {
        fetch_openai_compatible_models(&provider, &base_url, api_key.as_deref()).await
    } else {
        match provider_type.as_str() {
            "anthropic" => fetch_anthropic_models(&provider, &base_url, api_key.as_deref()).await,
            "gemini" | "google" => fetch_gemini_models(&provider, &base_url, api_key.as_deref()).await,
            "echo" => Ok(Vec::new()),
            _ => fetch_openai_compatible_models(&provider, &base_url, api_key.as_deref()).await,
        }
    };

    match live {
        Ok(models) if !models.is_empty() => Ok(DetectedModelList {
            ok: true,
            source: "live".into(),
            provider_id,
            provider_type,
            base_url,
            models,
            error: None,
        }),
        Ok(_) => Ok(DetectedModelList {
            ok: !fallback.is_empty(),
            source: "catalog".into(),
            provider_id,
            provider_type,
            base_url,
            models: fallback,
            error: Some("live model endpoint returned no models; using built-in catalog".into()),
        }),
        Err(error) => Ok(DetectedModelList {
            ok: !fallback.is_empty(),
            source: "catalog".into(),
            provider_id,
            provider_type,
            base_url,
            models: fallback,
            error: Some(error),
        }),
    }
}

fn live_model_base_url(provider: &LlmProvider) -> String {
    if uses_deepseek_model_endpoint(provider) {
        return DEEPSEEK_MODEL_LIST_BASE_URL.into();
    }
    if uses_xiaomi_anthropic_model_endpoint(provider) {
        return xiaomi_anthropic_model_base_url(provider);
    }
    let configured = provider.base_url.trim().trim_end_matches('/');
    if !configured.is_empty() {
        return configured.to_string();
    }
    match provider.provider_type.trim().to_ascii_lowercase().as_str() {
        "anthropic" => "https://api.anthropic.com".into(),
        "gemini" | "google" => "https://generativelanguage.googleapis.com/v1beta".into(),
        "openai" | "openai_compatible" | "openai_responses" | "openai-responses" => {
            "https://api.openai.com/v1".into()
        }
        _ => String::new(),
    }
}

fn uses_deepseek_model_endpoint(provider: &LlmProvider) -> bool {
    let preset = provider
        .preset
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let haystack = format!(
        "{} {} {} {} {}",
        provider.id.to_ascii_lowercase(),
        provider.name.to_ascii_lowercase(),
        provider.provider_type.to_ascii_lowercase(),
        preset,
        provider.base_url.to_ascii_lowercase()
    );
    haystack.contains("deepseek")
        || haystack.contains("deep-seek")
        || haystack.contains("api.deepseek.com")
}

fn uses_xiaomi_anthropic_model_endpoint(provider: &LlmProvider) -> bool {
    let provider_type = provider.provider_type.trim().to_ascii_lowercase();
    if provider_type != "anthropic" {
        return false;
    }
    let preset = provider
        .preset
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let haystack = format!(
        "{} {} {} {}",
        provider.id.to_ascii_lowercase(),
        provider.name.to_ascii_lowercase(),
        preset,
        provider.base_url.to_ascii_lowercase()
    );
    haystack.contains("xiaomi")
        || haystack.contains("mimo")
        || haystack.contains("xiaomimimo.com")
}

fn xiaomi_anthropic_model_base_url(provider: &LlmProvider) -> String {
    let configured = provider.base_url.trim().trim_end_matches('/');
    if configured.is_empty() {
        return XIAOMI_ANTHROPIC_MODEL_LIST_DEFAULT_BASE_URL.into();
    }
    configured
        .strip_suffix("/anthropic")
        .unwrap_or(configured)
        .to_string()
}

fn live_model_api_key(provider: &LlmProvider) -> Option<String> {
    provider
        .api_key
        .as_ref()
        .map(|value| value.trim().to_string())
        .filter(|value| usable_live_secret(value))
        .or_else(|| {
            let env_name = provider.api_key_env.trim();
            if env_name.is_empty() {
                None
            } else if usable_live_secret(env_name) && looks_like_inline_live_key(env_name) {
                Some(env_name.to_string())
            } else {
                std::env::var(env_name)
                    .ok()
                    .map(|value| value.trim().to_string())
                    .filter(|value| usable_live_secret(value))
            }
        })
}

fn usable_live_secret(value: &str) -> bool {
    let trimmed = value.trim();
    !trimmed.is_empty()
        && !matches!(
            trimmed.to_ascii_lowercase().as_str(),
            "none" | "null" | "undefined" | "your_api_key" | "your_api_key_here"
        )
}

fn looks_like_inline_live_key(value: &str) -> bool {
    let trimmed = value.trim();
    trimmed.starts_with("sk-")
        || trimmed.starts_with("AIza")
        || trimmed.starts_with("eyJ")
        || trimmed.len() > 32 && !trimmed.chars().any(char::is_whitespace)
}

fn live_model_entry(provider_id: &str, model_id: &str, name: Option<&str>) -> ModelCatalogEntry {
    let fallback = infer_model_capabilities(&LlmProvider {
        id: provider_id.to_string(),
        provider_type: provider_id.to_string(),
        model: model_id.to_string(),
        ..LlmProvider::default()
    });
    ModelCatalogEntry {
        id: model_id.to_string(),
        name: name.unwrap_or(model_id).to_string(),
        family: fallback.model_family.clone(),
        capabilities: ModelCapabilities {
            source: "live".into(),
            ..fallback
        },
    }
}

fn partial_capabilities_from_live_item(item: &Value) -> Option<PartialModelCapabilities> {
    let object = item.as_object()?;
    let (input_modalities, output_modalities) = object_modalities(object);
    let mut partial = PartialModelCapabilities {
        supports_tools: object_bool(object, &["supportsTools", "supports_tools", "tool_call"]),
        supports_vision: object_bool(object, &["supportsVision", "supports_vision"]),
        supports_reasoning: object_bool(object, &["supportsReasoning", "supports_reasoning", "reasoning"]),
        supports_pdf: object_bool(object, &["supportsPdf", "supports_pdf"]),
        supports_audio_input: object_bool(object, &["supportsAudioInput", "supports_audio_input"]),
        supports_structured_output: object_bool(
            object,
            &["supportsStructuredOutput", "supports_structured_output", "structured_output"],
        ),
        open_weights: object_bool(object, &["openWeights", "open_weights"]),
        input_modalities,
        output_modalities,
        context_window: object_u64(object, &["contextWindow", "context_window", "inputTokenLimit"]),
        max_output_tokens: object_u64(object, &["maxOutputTokens", "max_output_tokens", "outputTokenLimit"]),
        model_family: object_string(object, &["modelFamily", "model_family", "family"]),
        status: object_string(object, &["status"]),
        knowledge_cutoff: object_string(object, &["knowledgeCutoff", "knowledge_cutoff", "knowledge"]),
    };
    if partial.input_modalities.is_none() {
        let methods = lower_string_vec(object.get("supportedGenerationMethods"));
        if !methods.is_empty() {
            partial.supports_tools = partial.supports_tools.or(Some(
                methods
                    .iter()
                    .any(|method| matches!(method.as_str(), "generatecontent" | "streamgeneratecontent")),
            ));
        }
    }
    if partial.supports_vision.is_none() {
        if let Some(input_modalities) = partial.input_modalities.as_ref() {
            partial.supports_vision = Some(input_modalities.iter().any(|item| item == "image"));
        }
    }
    if partial.supports_pdf.is_none() {
        if let Some(input_modalities) = partial.input_modalities.as_ref() {
            partial.supports_pdf = Some(input_modalities.iter().any(|item| item == "pdf"));
        }
    }
    if partial.supports_audio_input.is_none() {
        if let Some(input_modalities) = partial.input_modalities.as_ref() {
            partial.supports_audio_input = Some(input_modalities.iter().any(|item| item == "audio"));
        }
    }
    let has_any = partial.supports_tools.is_some()
        || partial.supports_vision.is_some()
        || partial.supports_reasoning.is_some()
        || partial.supports_pdf.is_some()
        || partial.supports_audio_input.is_some()
        || partial.supports_structured_output.is_some()
        || partial.open_weights.is_some()
        || partial.input_modalities.is_some()
        || partial.output_modalities.is_some()
        || partial.context_window.is_some()
        || partial.max_output_tokens.is_some()
        || partial.model_family.is_some()
        || partial.status.is_some()
        || partial.knowledge_cutoff.is_some();
    has_any.then_some(partial)
}

async fn fetch_openai_compatible_models(
    provider: &LlmProvider,
    base_url: &str,
    api_key: Option<&str>,
) -> Result<Vec<ModelCatalogEntry>, String> {
    let url = if base_url.ends_with("/models") {
        base_url.to_string()
    } else if base_url.ends_with("/v1") {
        format!("{base_url}/models")
    } else {
        format!("{base_url}/v1/models")
    };
    let mut headers = HeaderMap::new();
    headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
    if let Some(key) = api_key {
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {key}"))
                .map_err(|error| format!("invalid Authorization header: {error}"))?,
        );
    }
    let body = fetch_model_json(&url, headers).await?;
    let items = body
        .get("data")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    Ok(models_from_live_items(provider, items))
}

async fn fetch_anthropic_models(
    provider: &LlmProvider,
    base_url: &str,
    api_key: Option<&str>,
) -> Result<Vec<ModelCatalogEntry>, String> {
    let url = if base_url.ends_with("/models") {
        base_url.to_string()
    } else if base_url.ends_with("/v1") {
        format!("{base_url}/models")
    } else {
        format!("{base_url}/v1/models")
    };
    let mut headers = HeaderMap::new();
    headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
    if let Some(key) = api_key {
        headers.insert(
            "x-api-key",
            HeaderValue::from_str(key)
                .map_err(|error| format!("invalid x-api-key header: {error}"))?,
        );
        headers.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
    }
    let body = fetch_model_json(&url, headers).await?;
    let items = body
        .get("data")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    Ok(models_from_live_items(provider, items))
}

async fn fetch_gemini_models(
    provider: &LlmProvider,
    base_url: &str,
    api_key: Option<&str>,
) -> Result<Vec<ModelCatalogEntry>, String> {
    let mut url = if base_url.ends_with("/models") {
        base_url.to_string()
    } else {
        format!("{base_url}/models")
    };
    if let Some(key) = api_key {
        let separator = if url.contains('?') { '&' } else { '?' };
        url = format!("{url}{separator}key={key}");
    }
    let mut headers = HeaderMap::new();
    headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
    let body = fetch_model_json(&url, headers).await?;
    let items = body
        .get("models")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    Ok(models_from_live_items(provider, items))
}

async fn fetch_model_json(url: &str, headers: HeaderMap) -> Result<Value, String> {
    let response = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .default_headers(headers)
        .build()
        .map_err(|error| error.to_string())?
        .get(url)
        .send()
        .await
        .map_err(|error| error.to_string())?;
    let status = response.status();
    let body = response.text().await.map_err(|error| error.to_string())?;
    if !status.is_success() {
        return Err(format!(
            "model endpoint returned {status}: {}",
            body.chars().take(300).collect::<String>()
        ));
    }
    serde_json::from_str(&body).map_err(|error| format!("invalid model endpoint JSON: {error}"))
}

fn models_from_live_items(provider: &LlmProvider, items: Vec<Value>) -> Vec<ModelCatalogEntry> {
    let provider_key = if provider.id.trim().is_empty() {
        provider.provider_type.as_str()
    } else {
        provider.id.as_str()
    };
    let mut entries = Vec::new();
    for item in items {
        let Some(raw_id) = live_item_model_id(&item).map(str::to_string) else {
            continue;
        };
        if looks_like_noise_model(&raw_id) {
            continue;
        }
        let name = item
            .get("displayName")
            .or_else(|| item.get("display_name"))
            .or_else(|| item.get("name"))
            .and_then(Value::as_str)
            .map(|value| value.trim_start_matches("models/"));
        let mut entry = live_model_entry(
            provider_key,
            raw_id.trim_start_matches("models/"),
            name,
        );
        if let Some(partial) = partial_capabilities_from_live_item(&item) {
            entry.capabilities =
                apply_partial_capabilities(entry.capabilities.clone(), partial, "live");
            if entry.family.trim().is_empty() {
                entry.family = entry.capabilities.model_family.clone();
            }
        }
        entries.push(entry);
    }
    entries.sort_by(|left, right| left.id.cmp(&right.id));
    entries.dedup_by(|left, right| left.id == right.id);
    entries
}

fn live_item_model_id(item: &Value) -> Option<&str> {
    item.get("id")
        .or_else(|| item.get("model"))
        .or_else(|| item.get("name"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

pub fn list_agentic_models(provider_id: &str) -> Vec<ModelCatalogEntry> {
    let catalog = match catalog_for_lookup() {
        Some(catalog) => catalog,
        None => return Vec::new(),
    };
    let models = match provider_models(&catalog, provider_id) {
        Some(models) => models,
        None => return Vec::new(),
    };
    let mut entries = Vec::new();
    for (model_id, entry) in models {
        if should_hide_from_provider_catalog(provider_id, model_id)
            || looks_like_noise_model(model_id)
        {
            continue;
        }
        if !entry
            .get("tool_call")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            continue;
        }
        let caps = capabilities_from_entry(provider_id, model_id, model_id, entry, "models.dev");
        entries.push(ModelCatalogEntry {
            id: model_id.clone(),
            name: entry
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or(model_id)
                .to_string(),
            family: entry
                .get("family")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            capabilities: caps,
        });
    }
    entries.sort_by(|left, right| left.id.cmp(&right.id));
    entries
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn model_capabilities_parse_modalities_and_limits() {
        let entry = json!({
            "family": "gpt-4.1",
            "reasoning": true,
            "tool_call": true,
            "structured_output": true,
            "modalities": {"input": ["text", "image", "pdf"], "output": ["text"]},
            "limit": {"context": 1048576, "output": 32768}
        });
        let caps = capabilities_from_entry("openai", "gpt-4.1", "gpt-4.1", &entry, "test");
        assert!(caps.supports_tools);
        assert!(caps.supports_vision);
        assert!(caps.supports_pdf);
        assert!(caps.supports_reasoning);
        assert_eq!(caps.context_window, Some(1_048_576));
        assert_eq!(caps.max_output_tokens, Some(32_768));
    }

    #[test]
    fn provider_mapping_strips_credential_suffix() {
        assert_eq!(models_dev_provider_id("openai:cred-2"), "openai");
        assert_eq!(models_dev_provider_id("gemini"), "google");
        assert_eq!(models_dev_provider_id("custom"), "custom");
    }

    #[test]
    fn provider_mapping_covers_hermes_runtime_aliases() {
        let cases = [
            ("google-gemini-cli", "google"),
            ("gemini-cli", "google"),
            ("gemini-oauth", "google"),
            ("copilot", "github-copilot"),
            ("copilot-acp", "github-copilot"),
            ("github-copilot-acp", "github-copilot"),
            ("qwen-oauth", "alibaba"),
            ("minimax-oauth", "minimax"),
            ("grok-oauth", "xai"),
            ("x-ai-oauth", "xai"),
            ("aws-bedrock", "bedrock"),
            ("alibaba-coding", "alibaba-coding-plan"),
            ("tencent", "tencent-tokenhub"),
            ("kimi-for-coding", "kimi-for-coding"),
            ("opencode", "opencode"),
            ("azure-foundry", "azure-foundry"),
            ("nous", "nous"),
            ("arcee-ai", "arcee"),
            ("gmi-cloud", "gmi"),
            ("lm-studio", "lmstudio"),
            ("vllm", "local"),
            ("llama.cpp", "local"),
            ("zen", "opencode"),
            ("go", "opencode-go"),
        ];
        for (input, expected) in cases {
            assert_eq!(models_dev_provider_id(input), expected, "{input}");
        }
    }

    #[test]
    fn provider_model_capabilities_honors_model_overrides() {
        let mut provider = LlmProvider::default();
        provider.id = "provider-test".into();
        provider.provider_type = "anthropic".into();
        provider.model = "mimo-v2.5".into();
        provider.models = json!({
            "mimo-v2.5": {
                "capabilities": {
                    "supportsVision": true,
                    "supportsReasoning": true,
                    "supportsStructuredOutput": true,
                    "inputModalities": ["text", "image"],
                    "contextWindow": 131072
                }
            }
        });

        let caps = provider_model_capabilities(&provider);
        assert!(caps.supports_vision);
        assert!(caps.supports_reasoning);
        assert!(caps.supports_structured_output);
        assert_eq!(caps.context_window, Some(131072));
        assert_eq!(caps.input_modalities, vec!["image", "text"]);
        assert_eq!(caps.source, "configured");
    }

    #[test]
    fn provider_model_capabilities_uses_curated_gateway_hint() {
        let mut provider = LlmProvider::default();
        provider.id = "provider-mimo".into();
        provider.provider_type = "anthropic".into();
        provider.base_url = "https://token-plan-sgp.xiaomimimo.com/anthropic".into();
        provider.model = "mimo-v2.5".into();

        let caps = provider_model_capabilities(&provider);
        assert!(caps.supports_vision);
        assert!(caps.supports_tools);
        assert_eq!(caps.source, "curated");
        assert!(caps.input_modalities.iter().any(|item| item == "image"));
    }

    #[test]
    fn live_items_parse_capabilities_from_metadata() {
        let mut provider = LlmProvider::default();
        provider.id = "provider-google".into();
        provider.provider_type = "gemini".into();
        let items = vec![json!({
            "name": "models/gemini-2.5-pro",
            "displayName": "Gemini 2.5 Pro",
            "supportedGenerationMethods": ["generateContent", "streamGenerateContent"],
            "inputModalities": ["TEXT", "IMAGE", "PDF"],
            "outputModalities": ["TEXT"],
            "supportsReasoning": true,
            "inputTokenLimit": 1048576,
            "outputTokenLimit": 65536
        })];

        let entries = models_from_live_items(&provider, items);
        assert_eq!(entries.len(), 1);
        let caps = &entries[0].capabilities;
        assert!(caps.supports_tools);
        assert!(caps.supports_vision);
        assert!(caps.supports_pdf);
        assert!(caps.supports_reasoning);
        assert_eq!(caps.context_window, Some(1_048_576));
        assert_eq!(caps.max_output_tokens, Some(65_536));
        assert_eq!(caps.source, "live");
    }
}
