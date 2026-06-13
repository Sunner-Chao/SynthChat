use std::env;

use serde_json::{json, Value};

use crate::{
    error::AppResult,
    models::{ImageProvider, LlmProvider, SearchProvider, VideoProvider},
    store::AppStore,
};

#[derive(Clone, Copy)]
struct HermesProviderPlugin {
    family: &'static str,
    path: &'static str,
    name: &'static str,
    kind: &'static str,
    description: &'static str,
    provider: &'static str,
    requires_env: &'static [&'static str],
}

pub(super) fn provider_plugins_tool(store: &AppStore, payload: &Value) -> AppResult<String> {
    let family = payload
        .get("family")
        .and_then(Value::as_str)
        .unwrap_or("all")
        .trim()
        .to_ascii_lowercase();
    let provider = payload
        .get("provider")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let llm = store.providers().unwrap_or_default();
    let search = store.search_providers().unwrap_or_default();
    let image = store.image_providers().unwrap_or_default();
    let video = store.video_providers().unwrap_or_default();
    let plugins = hermes_provider_plugins()
        .into_iter()
        .filter(|plugin| family == "all" || plugin.family == family)
        .filter(|plugin| provider.is_empty() || normalize(plugin.provider) == provider)
        .map(|plugin| provider_plugin_status(plugin, &llm, &search, &image, &video))
        .collect::<Vec<_>>();
    let mut by_family = serde_json::Map::new();
    for item in &plugins {
        let family = item
            .get("family")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let entry = by_family
            .entry(family.to_string())
            .or_insert_with(|| json!(0));
        *entry = json!(entry.as_u64().unwrap_or(0) + 1);
    }
    Ok(serde_json::to_string_pretty(&json!({
        "schema": "hermes_provider_plugins_desktop_v1",
        "status": "ok",
        "filter": {
            "family": family,
            "provider": provider,
        },
        "count": plugins.len(),
        "byFamily": by_family,
        "families": ["model", "web", "image", "video"],
        "plugins": plugins,
        "desktopAdaptation": {
            "model": "Hermes model-provider manifests map to SynthChat LLM provider presets/types and credential pool resolution.",
            "web": "Hermes web provider manifests map to SynthChat SearchProvider entries and web_provider status plus web_search/web_extract execution.",
            "image": "Hermes image_gen provider manifests map to SynthChat ImageProvider entries and image_generate execution.",
            "video": "Hermes video_gen provider manifests map to SynthChat VideoProvider entries and video_generate execution.",
            "boundary": "This tool reports plugin-level discovery/readiness without making network calls or creating provider records."
        }
    }))?)
}

fn provider_plugin_status(
    plugin: HermesProviderPlugin,
    llm: &[LlmProvider],
    search: &[SearchProvider],
    image: &[ImageProvider],
    video: &[VideoProvider],
) -> Value {
    let configured = match plugin.family {
        "model" => llm
            .iter()
            .any(|provider| provider.enabled && llm_provider_matches(provider, plugin.provider)),
        "web" => search.iter().any(|provider| {
            provider.enabled && provider_matches(&provider.provider_type, plugin.provider)
        }),
        "image" => image.iter().any(|provider| {
            provider.enabled && provider_matches(&provider.provider_type, plugin.provider)
        }),
        "video" => video.iter().any(|provider| {
            provider.enabled && provider_matches(&provider.provider_type, plugin.provider)
        }),
        _ => false,
    };
    let env_ready = plugin.requires_env.iter().all(|name| {
        env::var(name)
            .ok()
            .is_some_and(|value| !value.trim().is_empty())
    });
    let missing_env = plugin
        .requires_env
        .iter()
        .filter(|name| {
            env::var(name)
                .ok()
                .is_none_or(|value| value.trim().is_empty())
        })
        .copied()
        .collect::<Vec<_>>();
    let credential_ready = configured || env_ready || plugin.requires_env.is_empty();
    json!({
        "family": plugin.family,
        "path": plugin.path,
        "name": plugin.name,
        "kind": plugin.kind,
        "description": plugin.description,
        "provider": plugin.provider,
        "configuredInSynthChat": configured,
        "requiresEnv": plugin.requires_env,
        "missingEnv": missing_env,
        "envReady": env_ready,
        "ready": credential_ready,
        "status": if configured {
            "configured"
        } else if credential_ready {
            "available_by_env_or_no_key"
        } else {
            "needs_configuration"
        },
        "boundary": provider_boundary(plugin),
    })
}

fn provider_boundary(plugin: HermesProviderPlugin) -> &'static str {
    match plugin.family {
        "model" => "LLM execution is handled by SynthChat provider transports; plugin manifest parity is represented as provider type/preset readiness.",
        "web" => "Web execution is handled by web_provider/web_search/web_extract; this entry reports manifest-level provider readiness.",
        "image" => "Image generation is handled by image_generate; this entry reports manifest-level provider readiness.",
        "video" => "Video generation is handled by video_generate; this entry reports manifest-level provider readiness.",
        _ => "Provider plugin readiness boundary.",
    }
}

fn llm_provider_matches(provider: &LlmProvider, expected: &str) -> bool {
    [
        provider.id.as_str(),
        provider.name.as_str(),
        provider.provider_type.as_str(),
        provider.preset.as_deref().unwrap_or(""),
        provider.base_url.as_str(),
        provider.model.as_str(),
    ]
    .into_iter()
    .any(|value| provider_matches(value, expected))
}

fn provider_matches(value: &str, expected: &str) -> bool {
    let value = normalize(value);
    let expected = normalize(expected);
    value == expected || value.contains(&expected) || expected.contains(&value)
}

fn normalize(value: &str) -> String {
    value
        .trim()
        .to_ascii_lowercase()
        .replace(['_', '-', ' '], "")
}

fn hermes_provider_plugins() -> Vec<HermesProviderPlugin> {
    let mut items = Vec::new();
    items.extend(model_provider_plugins());
    items.extend(web_provider_plugins());
    items.extend(image_provider_plugins());
    items.extend(video_provider_plugins());
    items
}

fn model_provider_plugins() -> Vec<HermesProviderPlugin> {
    vec![
        model(
            "alibaba",
            "alibaba-provider",
            "Alibaba DashScope (international)",
        ),
        model(
            "alibaba-coding-plan",
            "alibaba-coding-plan-provider",
            "Alibaba Cloud Coding Plan",
        ),
        model("anthropic", "anthropic-provider", "Anthropic (Claude)"),
        model("arcee", "arcee-provider", "Arcee AI"),
        model(
            "azure-foundry",
            "azure-foundry-provider",
            "Microsoft Foundry",
        ),
        model("bedrock", "bedrock-provider", "AWS Bedrock"),
        model("copilot", "copilot-provider", "GitHub Copilot"),
        model(
            "copilot-acp",
            "copilot-acp-provider",
            "GitHub Copilot via ACP subprocess",
        ),
        model(
            "custom",
            "custom-provider",
            "Custom / Ollama / local OpenAI-compatible endpoint",
        ),
        model("deepseek", "deepseek-provider", "DeepSeek"),
        model(
            "gemini",
            "gemini-provider",
            "Google Gemini (API key + Cloud Code OAuth)",
        ),
        model("gmi", "gmi-provider", "GMI Cloud"),
        model(
            "huggingface",
            "huggingface-provider",
            "HuggingFace Inference Providers",
        ),
        model("kilocode", "kilocode-provider", "Kilo Code"),
        model(
            "kimi-coding",
            "kimi-coding-provider",
            "Moonshot Kimi Coding (global + China)",
        ),
        model(
            "minimax",
            "minimax-provider",
            "MiniMax M-series (global + China + OAuth)",
        ),
        model("nous", "nous-provider", "Nous Research Portal"),
        model(
            "novita",
            "novita-provider",
            "NovitaAI AI-native cloud for builders and agents",
        ),
        model("nvidia", "nvidia-provider", "NVIDIA NIM"),
        model("ollama-cloud", "ollama-cloud-provider", "Ollama Cloud"),
        model(
            "openai-codex",
            "openai-codex-provider",
            "OpenAI Codex (Responses API)",
        ),
        model(
            "opencode-zen",
            "opencode-zen-provider",
            "OpenCode (Zen + Go)",
        ),
        model("openrouter", "openrouter-provider", "OpenRouter aggregator"),
        model("qwen-oauth", "qwen-oauth-provider", "Qwen Portal (OAuth)"),
        model("stepfun", "stepfun-provider", "StepFun Step Plan"),
        model("xai", "xai-provider", "xAI Grok (Responses API)"),
        model("xiaomi", "xiaomi-provider", "Xiaomi MiMo"),
        model("zai", "zai-provider", "Z.AI / GLM"),
    ]
}

fn web_provider_plugins() -> Vec<HermesProviderPlugin> {
    vec![
        web(
            "brave-free",
            "web-brave-free",
            "Brave Search free tier",
            &["BRAVE_SEARCH_API_KEY"],
        ),
        web(
            "ddgs",
            "web-ddgs",
            "DuckDuckGo web search via ddgs Python package",
            &[],
        ),
        web(
            "exa",
            "web-exa",
            "Exa web search and content extraction",
            &["EXA_API_KEY"],
        ),
        web(
            "firecrawl",
            "web-firecrawl",
            "Firecrawl web search + content extraction",
            &["FIRECRAWL_API_KEY"],
        ),
        web(
            "parallel",
            "web-parallel",
            "Parallel.ai web search + content extraction",
            &["PARALLEL_API_KEY"],
        ),
        web(
            "searxng",
            "web-searxng",
            "SearXNG web search",
            &["SEARXNG_URL"],
        ),
        web(
            "tavily",
            "web-tavily",
            "Tavily web search + content extraction + crawl",
            &["TAVILY_API_KEY"],
        ),
        web(
            "xai",
            "web-xai",
            "xAI Web Search via Grok Responses API",
            &["XAI_API_KEY"],
        ),
    ]
}

fn image_provider_plugins() -> Vec<HermesProviderPlugin> {
    vec![
        image(
            "fal",
            "fal",
            "FAL.ai image generation backend",
            &["FAL_KEY"],
        ),
        image(
            "krea",
            "krea",
            "Krea image generation backend",
            &["KREA_API_KEY"],
        ),
        image(
            "openai",
            "openai",
            "OpenAI image generation backend",
            &["OPENAI_API_KEY"],
        ),
        image(
            "openai-codex",
            "openai-codex",
            "OpenAI image generation backed by ChatGPT/Codex OAuth",
            &[],
        ),
        image(
            "xai",
            "xai",
            "xAI image generation backend",
            &["XAI_API_KEY"],
        ),
    ]
}

fn video_provider_plugins() -> Vec<HermesProviderPlugin> {
    vec![
        video(
            "fal",
            "fal",
            "FAL.ai video generation backend",
            &["FAL_KEY"],
        ),
        video(
            "xai",
            "xai",
            "xAI Grok-Imagine video generation backend",
            &["XAI_API_KEY"],
        ),
    ]
}

fn model(
    provider: &'static str,
    name: &'static str,
    description: &'static str,
) -> HermesProviderPlugin {
    HermesProviderPlugin {
        family: "model",
        path: provider,
        name,
        kind: "model-provider",
        description,
        provider,
        requires_env: &[],
    }
}

fn web(
    provider: &'static str,
    name: &'static str,
    description: &'static str,
    requires_env: &'static [&'static str],
) -> HermesProviderPlugin {
    HermesProviderPlugin {
        family: "web",
        path: provider,
        name,
        kind: "backend",
        description,
        provider,
        requires_env,
    }
}

fn image(
    provider: &'static str,
    name: &'static str,
    description: &'static str,
    requires_env: &'static [&'static str],
) -> HermesProviderPlugin {
    HermesProviderPlugin {
        family: "image",
        path: provider,
        name,
        kind: "backend",
        description,
        provider,
        requires_env,
    }
}

fn video(
    provider: &'static str,
    name: &'static str,
    description: &'static str,
    requires_env: &'static [&'static str],
) -> HermesProviderPlugin {
    HermesProviderPlugin {
        family: "video",
        path: provider,
        name,
        kind: "backend",
        description,
        provider,
        requires_env,
    }
}
