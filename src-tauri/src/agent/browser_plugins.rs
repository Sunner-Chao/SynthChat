use serde_json::{json, Value};

use crate::{error::AppResult, models::BrowserProvider, store::AppStore};

use super::provider_api_key;

const HERMES_BROWSER_PROVIDER_PLUGINS: &[HermesBrowserProviderPlugin] = &[
    HermesBrowserProviderPlugin {
        manifest_name: "browser-browserbase",
        provider_type: "browserbase",
        label: "Browserbase",
        manifest_path: "plugins/browser/browserbase/plugin.yaml",
        module_path: "plugins/browser/browserbase/__init__.py",
        provider_path: "plugins/browser/browserbase/provider.py",
        required_env: &["BROWSERBASE_API_KEY", "BROWSERBASE_PROJECT_ID"],
        base_url_env: "BROWSERBASE_BASE_URL",
        default_base_url: "https://api.browserbase.com",
        description: "Browserbase cloud browser backend with stealth, proxies, and keep-alive sessions.",
        auto_selected_by_legacy: true,
    },
    HermesBrowserProviderPlugin {
        manifest_name: "browser-browser-use",
        provider_type: "browser-use",
        label: "Browser Use",
        manifest_path: "plugins/browser/browser_use/plugin.yaml",
        module_path: "plugins/browser/browser_use/__init__.py",
        provider_path: "plugins/browser/browser_use/provider.py",
        required_env: &["BROWSER_USE_API_KEY"],
        base_url_env: "BROWSER_USE_BASE_URL",
        default_base_url: "https://api.browser-use.com/api/v3",
        description: "Browser Use cloud browser backend with direct API-key or managed Nous tool-gateway auth.",
        auto_selected_by_legacy: true,
    },
    HermesBrowserProviderPlugin {
        manifest_name: "browser-firecrawl",
        provider_type: "firecrawl",
        label: "Firecrawl",
        manifest_path: "plugins/browser/firecrawl/plugin.yaml",
        module_path: "plugins/browser/firecrawl/__init__.py",
        provider_path: "plugins/browser/firecrawl/provider.py",
        required_env: &["FIRECRAWL_API_KEY"],
        base_url_env: "FIRECRAWL_API_URL",
        default_base_url: "https://api.firecrawl.dev",
        description: "Firecrawl cloud browser backend using /v2/browser, distinct from Firecrawl web search/extract.",
        auto_selected_by_legacy: false,
    },
];

pub(super) fn browser_plugins_tool(store: &AppStore, payload: &Value) -> AppResult<String> {
    let action = payload
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("status")
        .trim()
        .to_ascii_lowercase();
    if !matches!(
        action.as_str(),
        "status" | "manifest" | "providers" | "readiness" | "diagnostics"
    ) {
        return Ok(serde_json::to_string_pretty(&json!({
            "schema": "hermes_browser_plugins_desktop_v1",
            "status": "unsupported_action",
            "supportedActions": ["status", "manifest", "providers", "readiness", "diagnostics"],
        }))?);
    }
    Ok(serde_json::to_string_pretty(&browser_plugins_snapshot(
        store, &action,
    ))?)
}

pub(super) fn browser_plugins_snapshot(store: &AppStore, action: &str) -> Value {
    let providers = store.browser_providers().unwrap_or_default();
    let plugin_snapshots = HERMES_BROWSER_PROVIDER_PLUGINS
        .iter()
        .map(|plugin| browser_provider_plugin_snapshot(plugin, &providers))
        .collect::<Vec<_>>();
    let legacy_selected = ["browser-use", "browserbase"]
        .iter()
        .find_map(|provider_type| {
            providers
                .iter()
                .find(|provider| {
                    normalize_browser_provider_name(&provider.provider_type) == *provider_type
                        && browser_provider_ready(provider)
                })
                .map(browser_provider_snapshot)
        });

    json!({
        "schema": "hermes_browser_plugins_desktop_v1",
        "status": "ok",
        "action": action,
        "hermesReference": {
            "category": "plugins/browser",
            "kind": "backend",
            "pluginCount": HERMES_BROWSER_PROVIDER_PLUGINS.len(),
            "providers": ["browserbase", "browser-use", "firecrawl"],
            "registration": "Each plugin register(ctx) calls ctx.register_browser_provider(<Provider>()).",
            "baseClass": "agent.browser_provider.BrowserProvider",
            "legacyPreference": ["browser-use", "browserbase"],
            "firecrawlLegacyAutoSelected": false,
            "selectionBoundary": "Hermes legacy auto-detect walks browser-use then browserbase; Firecrawl is only used when explicitly configured as the cloud browser provider."
        },
        "plugins": plugin_snapshots,
        "synthChatNativeAdaptation": {
            "statusTool": "browser_provider",
            "sessionTools": ["browser_create_session", "browser_close_session"],
            "supervisorTools": ["browser_supervisor_register", "browser_supervisor_state", "browser_supervisor_remove"],
            "registeredProviderCount": providers.len(),
            "registeredProviders": providers.iter().map(browser_provider_snapshot).collect::<Vec<_>>(),
            "legacySelectedProvider": legacy_selected,
            "lifecyclePreviewAvailable": true,
            "networkProbePerformed": false,
            "sessionCreated": false
        },
        "boundary": {
            "pythonPluginLoaderExecuted": false,
            "cloudBrowserSessionCreated": false,
            "cloudBrowserSessionClosed": false,
            "networkProbePerformed": false,
            "boundary": "SynthChat maps Hermes browser provider plugin manifests to its native BrowserProvider registry and lifecycle/status tools. This diagnostic does not execute Python provider plugins or create/close cloud browser sessions."
        }
    })
}

struct HermesBrowserProviderPlugin {
    manifest_name: &'static str,
    provider_type: &'static str,
    label: &'static str,
    manifest_path: &'static str,
    module_path: &'static str,
    provider_path: &'static str,
    required_env: &'static [&'static str],
    base_url_env: &'static str,
    default_base_url: &'static str,
    description: &'static str,
    auto_selected_by_legacy: bool,
}

fn browser_provider_plugin_snapshot(
    plugin: &HermesBrowserProviderPlugin,
    providers: &[BrowserProvider],
) -> Value {
    let matching = providers
        .iter()
        .find(|provider| {
            normalize_browser_provider_name(&provider.provider_type) == plugin.provider_type
        })
        .map(browser_provider_snapshot);
    let env_readiness = plugin
        .required_env
        .iter()
        .map(|name| {
            json!({
                "name": name,
                "configured": std::env::var(name)
                    .ok()
                    .filter(|value| !value.trim().is_empty())
                    .is_some()
            })
        })
        .collect::<Vec<_>>();
    json!({
        "manifest": {
            "name": plugin.manifest_name,
            "version": "1.0.0",
            "kind": "backend",
            "author": "NousResearch",
            "description": plugin.description,
            "manifestPath": plugin.manifest_path,
            "modulePath": plugin.module_path,
            "providerPath": plugin.provider_path,
            "providesBrowserProviders": [plugin.provider_type],
            "autoLoaded": true
        },
        "provider": {
            "type": plugin.provider_type,
            "label": plugin.label,
            "requiredEnv": plugin.required_env,
            "baseUrlEnv": plugin.base_url_env,
            "defaultBaseUrl": plugin.default_base_url,
            "autoSelectedByHermesLegacyPreference": plugin.auto_selected_by_legacy,
            "envReadiness": env_readiness
        },
        "synthChatMapping": {
            "registered": matching.is_some(),
            "provider": matching,
            "mappedStatusTool": "browser_provider",
            "mappedCreateTool": "browser_create_session",
            "mappedCloseTool": "browser_close_session"
        }
    })
}

fn browser_provider_snapshot(provider: &BrowserProvider) -> Value {
    json!({
        "id": provider.id,
        "name": provider.name,
        "providerType": provider.provider_type,
        "enabled": provider.enabled,
        "baseUrlConfigured": !provider.base_url.trim().is_empty(),
        "apiKeyEnv": provider.api_key_env,
        "credentialConfigured": provider_api_key(&provider.api_key, &provider.api_key_env)
            .map(|value| !value.trim().is_empty())
            .unwrap_or(false),
        "projectIdConfigured": !provider.project_id.trim().is_empty(),
        "recordSessions": provider.record_sessions,
        "timeoutSeconds": provider.timeout_seconds,
        "ready": browser_provider_ready(provider)
    })
}

fn browser_provider_ready(provider: &BrowserProvider) -> bool {
    provider.enabled
        && !provider.provider_type.trim().is_empty()
        && !provider.base_url.trim().is_empty()
        && provider_api_key(&provider.api_key, &provider.api_key_env)
            .map(|value| !value.trim().is_empty())
            .unwrap_or(false)
}

fn normalize_browser_provider_name(value: &str) -> String {
    value.trim().to_ascii_lowercase().replace('_', "-")
}
