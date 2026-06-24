use std::{
    collections::{HashMap, HashSet},
    env, fs,
    path::{Path, PathBuf},
    process::Command as StdCommand,
};

use serde_json::Value;

use crate::{
    error::AppResult, models::PluginSummary, process_utils::CommandWindowExt, store::AppStore,
};

pub fn list_plugins(store: &AppStore) -> AppResult<Vec<PluginSummary>> {
    let discovered = discover_plugins();
    let existing = store.plugins()?;
    let enabled_by_id = existing
        .iter()
        .map(|plugin| (plugin.id.clone(), plugin.enabled))
        .collect::<HashMap<_, _>>();

    let mut merged = discovered
        .into_iter()
        .map(|mut plugin| {
            if let Some(enabled) = enabled_by_id.get(&plugin.id) {
                plugin.enabled = *enabled;
            }
            enrich_plugin_readiness(plugin)
        })
        .collect::<Vec<_>>();
    merged.sort_by(|a, b| a.name.cmp(&b.name));
    store.set_plugins(merged)
}

pub fn toggle_plugin(
    store: &AppStore,
    plugin_id: &str,
    enabled: bool,
) -> AppResult<Vec<PluginSummary>> {
    if store.plugins()?.iter().all(|plugin| plugin.id != plugin_id) {
        let _ = list_plugins(store)?;
    }
    store.set_plugin_enabled(plugin_id, enabled)
}

fn discover_plugins() -> Vec<PluginSummary> {
    let mut plugins = Vec::new();
    let mut seen = HashSet::new();
    for root in discover_plugin_roots() {
        for manifest in find_plugin_manifests(&root.path) {
            if let Some(plugin) = parse_plugin_manifest(&root.path, &manifest, &root.source) {
                if seen.insert(plugin.id.clone()) {
                    plugins.push(plugin);
                }
            }
        }
    }
    for plugin in scan_python_entry_point_plugins() {
        if seen.insert(plugin.id.clone()) {
            plugins.push(plugin);
        }
    }
    plugins
}

#[derive(Debug, Clone)]
struct PluginRoot {
    path: PathBuf,
    source: String,
}

fn discover_plugin_roots() -> Vec<PluginRoot> {
    let mut roots = Vec::new();
    if let Ok(current) = env::current_dir() {
        roots.push(plugin_root(current.join("plugins"), "synthchat"));
        roots.push(plugin_root(current.join("..").join("plugins"), "synthchat"));
        if env_enabled("HERMES_ENABLE_PROJECT_PLUGINS") {
            roots.push(plugin_root(
                current.join(".hermes").join("plugins"),
                "project",
            ));
            roots.push(plugin_root(
                current.join("..").join(".hermes").join("plugins"),
                "project",
            ));
        }
        roots.push(plugin_root(
            current
                .join("..")
                .join("..")
                .join("hermes-agent")
                .join("plugins"),
            "bundled",
        ));
    }
    if let Some(home) = user_home_dir() {
        roots.push(plugin_root(home.join(".hermes").join("plugins"), "user"));
    }
    if let Ok(override_dir) = env::var("HERMES_BUNDLED_PLUGINS") {
        roots.push(plugin_root(PathBuf::from(override_dir), "bundled"));
    }
    roots.push(plugin_root(
        PathBuf::from(r"D:\pro_sunner\demo_vscode\hermes-agent\plugins"),
        "bundled",
    ));

    let mut seen = HashSet::new();
    roots
        .into_iter()
        .filter_map(|root| {
            root.path.canonicalize().ok().map(|path| PluginRoot {
                path,
                source: root.source,
            })
        })
        .filter(|root| root.path.is_dir() && seen.insert(root.path.clone()))
        .collect()
}

fn plugin_root(path: PathBuf, source: &str) -> PluginRoot {
    PluginRoot {
        path,
        source: source.into(),
    }
}

fn env_enabled(name: &str) -> bool {
    env::var(name)
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

fn user_home_dir() -> Option<PathBuf> {
    env::var_os("USERPROFILE")
        .or_else(|| env::var_os("HOME"))
        .map(PathBuf::from)
}

fn find_plugin_manifests(root: &Path) -> Vec<PathBuf> {
    let mut manifests = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let name = path
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("");
            if matches!(name, "plugin.yaml" | "plugin.yml" | "manifest.json") {
                manifests.push(path);
            }
        }
    }
    manifests
}

fn parse_plugin_manifest(root: &Path, manifest: &Path, source: &str) -> Option<PluginSummary> {
    let raw = fs::read_to_string(manifest).ok()?;
    let rel = manifest
        .parent()
        .unwrap_or(manifest)
        .strip_prefix(root)
        .unwrap_or(manifest);
    let fallback_id = path_id(rel);
    if manifest.extension().and_then(|value| value.to_str()) == Some("json") {
        let value = serde_json::from_str::<Value>(&raw).ok()?;
        let name = string_field(&value, &["label", "name"]).unwrap_or_else(|| fallback_id.clone());
        return Some(PluginSummary {
            id: string_field(&value, &["name", "id"]).unwrap_or(fallback_id),
            name,
            description: string_field(&value, &["description"]).unwrap_or_default(),
            enabled: false,
            provided_tools: string_array_field(&value, &["providesTools", "provides_tools"]),
            provided_capabilities: json_plugin_capabilities(&value),
            provided_hooks: string_array_field(
                &value,
                &["hooks", "providesHooks", "provides_hooks"],
            ),
            requires_env: string_array_field(&value, &["requiresEnv", "requires_env"]),
            missing_env: Vec::new(),
            env_configured: false,
            version: string_field(&value, &["version"]).unwrap_or_default(),
            author: string_field(&value, &["author"]).unwrap_or_default(),
            source: source.into(),
            homepage_url: string_field(&value, &["homepageUrl", "homepage", "url"])
                .unwrap_or_default(),
            kind: normalize_plugin_kind(string_field(&value, &["kind"]).as_deref()),
            path: manifest
                .parent()
                .unwrap_or(manifest)
                .to_string_lossy()
                .to_string(),
            manifest_path: manifest.to_string_lossy().to_string(),
            entry_point: String::new(),
        });
    }

    let fields = parse_simple_yaml(&raw);
    let name = fields
        .get("label")
        .or_else(|| fields.get("name"))
        .cloned()
        .unwrap_or(fallback_id.clone());
    Some(PluginSummary {
        id: fallback_id,
        name,
        description: fields.get("description").cloned().unwrap_or_default(),
        enabled: false,
        provided_tools: parse_yaml_list(&raw, "provides_tools"),
        provided_capabilities: yaml_plugin_capabilities(&raw),
        provided_hooks: merge_lists(
            parse_yaml_list(&raw, "hooks"),
            parse_yaml_list(&raw, "provides_hooks"),
        ),
        requires_env: parse_yaml_list(&raw, "requires_env"),
        missing_env: Vec::new(),
        env_configured: false,
        version: fields.get("version").cloned().unwrap_or_default(),
        author: fields.get("author").cloned().unwrap_or_default(),
        source: source.into(),
        homepage_url: fields
            .get("homepage")
            .or_else(|| fields.get("homepage_url"))
            .cloned()
            .unwrap_or_default(),
        kind: normalize_plugin_kind(fields.get("kind").map(String::as_str)),
        path: manifest
            .parent()
            .unwrap_or(manifest)
            .to_string_lossy()
            .to_string(),
        manifest_path: manifest.to_string_lossy().to_string(),
        entry_point: String::new(),
    })
}

fn scan_python_entry_point_plugins() -> Vec<PluginSummary> {
    let Some(python) = find_python_command() else {
        return Vec::new();
    };
    let script = r#"
import importlib.metadata
import json

GROUP = "hermes_agent.plugins"

try:
    eps = importlib.metadata.entry_points()
    if hasattr(eps, "select"):
        group_eps = eps.select(group=GROUP)
    elif isinstance(eps, dict):
        group_eps = eps.get(GROUP, [])
    else:
        group_eps = [ep for ep in eps if getattr(ep, "group", "") == GROUP]
    print(json.dumps([
        {"name": ep.name, "value": ep.value}
        for ep in group_eps
    ]))
except Exception:
    print("[]")
"#;
    let output = StdCommand::new(python)
        .hide_window()
        .arg("-c")
        .arg(script)
        .output()
        .ok();
    let Some(output) = output.filter(|output| output.status.success()) else {
        return Vec::new();
    };
    serde_json::from_slice::<Vec<Value>>(&output.stdout)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|value| {
            let id = value.get("name").and_then(Value::as_str)?.trim();
            if id.is_empty() {
                return None;
            }
            let entry_point = value
                .get("value")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string();
            Some(PluginSummary {
                id: id.to_string(),
                name: id.to_string(),
                description: "Python entry point plugin".into(),
                enabled: false,
                provided_tools: Vec::new(),
                provided_capabilities: Vec::new(),
                provided_hooks: Vec::new(),
                requires_env: Vec::new(),
                missing_env: Vec::new(),
                env_configured: true,
                version: String::new(),
                author: String::new(),
                source: "entrypoint".into(),
                homepage_url: String::new(),
                kind: "standalone".into(),
                path: entry_point.clone(),
                manifest_path: String::new(),
                entry_point,
            })
        })
        .collect()
}

fn enrich_plugin_readiness(mut plugin: PluginSummary) -> PluginSummary {
    plugin.missing_env = plugin
        .requires_env
        .iter()
        .filter(|name| !plugin_env_value_configured(name))
        .cloned()
        .collect();
    plugin.env_configured = plugin.missing_env.is_empty();
    plugin
}

fn plugin_env_value_configured(name: &str) -> bool {
    env::var_os(name.trim())
        .and_then(|value| value.into_string().ok())
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false)
}

fn find_python_command() -> Option<&'static str> {
    ["python", "py", "python3"].into_iter().find(|command| {
        StdCommand::new(command)
            .hide_window()
            .arg("--version")
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
    })
}

fn parse_simple_yaml(raw: &str) -> HashMap<String, String> {
    let mut fields = HashMap::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('-') {
            continue;
        }
        let Some((key, value)) = trimmed.split_once(':') else {
            continue;
        };
        let value = value.trim();
        if value.is_empty() || value.starts_with('[') {
            continue;
        }
        fields.insert(key.trim().to_string(), clean_scalar(value));
    }
    fields
}

fn parse_yaml_list(raw: &str, key: &str) -> Vec<String> {
    let mut items = Vec::new();
    let mut in_list = false;
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with(&format!("{key}:")) {
            in_list = true;
            if let Some((_, inline)) = trimmed.split_once(':') {
                let inline = inline.trim();
                if inline.starts_with('[') && inline.ends_with(']') {
                    return inline
                        .trim_matches(|ch| ch == '[' || ch == ']')
                        .split(',')
                        .map(clean_scalar)
                        .filter(|value| !value.is_empty())
                        .collect();
                }
            }
            continue;
        }
        if in_list {
            if let Some(item) = trimmed.strip_prefix('-') {
                let item = clean_yaml_list_item(item.trim());
                if !item.is_empty() {
                    items.push(item);
                }
                continue;
            }
            if !trimmed.is_empty() && !line.starts_with(' ') && !line.starts_with('\t') {
                break;
            }
        }
    }
    items
}

fn merge_lists(primary: Vec<String>, secondary: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    primary
        .into_iter()
        .chain(secondary)
        .filter(|value| seen.insert(value.clone()))
        .collect()
}

fn yaml_plugin_capabilities(raw: &str) -> Vec<String> {
    let mut capabilities = Vec::new();
    for (key, prefix) in [
        ("provides_tools", "tool"),
        ("provides_web_providers", "web_provider"),
        ("provides_browser_providers", "browser_provider"),
        ("platforms", "platform"),
    ] {
        for item in parse_yaml_list(raw, key) {
            capabilities.push(format!("{prefix}:{item}"));
        }
    }
    dedupe_strings(capabilities)
}

fn json_plugin_capabilities(value: &Value) -> Vec<String> {
    let mut capabilities = Vec::new();
    for (keys, prefix) in [
        (&["providesTools", "provides_tools"][..], "tool"),
        (
            &["providesWebProviders", "provides_web_providers"][..],
            "web_provider",
        ),
        (
            &["providesBrowserProviders", "provides_browser_providers"][..],
            "browser_provider",
        ),
        (&["platforms"][..], "platform"),
    ] {
        for item in string_array_field(value, keys) {
            capabilities.push(format!("{prefix}:{item}"));
        }
    }
    dedupe_strings(capabilities)
}

fn dedupe_strings(values: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    values
        .into_iter()
        .filter(|value| seen.insert(value.clone()))
        .collect()
}

fn clean_yaml_list_item(value: &str) -> String {
    let value = clean_scalar(value);
    if let Some((key, rest)) = value.split_once(':') {
        if matches!(key.trim(), "name" | "key" | "var" | "env") {
            return clean_scalar(rest.trim());
        }
    }
    value
}

fn string_field(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_str))
        .map(ToOwned::to_owned)
}

fn string_array_field(value: &Value, keys: &[&str]) -> Vec<String> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_array))
        .map(|values| {
            values
                .iter()
                .filter_map(|value| {
                    value.as_str().map(ToOwned::to_owned).or_else(|| {
                        value
                            .as_object()
                            .and_then(|object| {
                                ["name", "key", "var", "env"]
                                    .iter()
                                    .find_map(|key| object.get(*key).and_then(Value::as_str))
                            })
                            .map(ToOwned::to_owned)
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn normalize_plugin_kind(raw: Option<&str>) -> String {
    match raw
        .unwrap_or("standalone")
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "backend" => "backend".into(),
        "exclusive" => "exclusive".into(),
        "platform" => "platform".into(),
        "model-provider" => "model-provider".into(),
        _ => "standalone".into(),
    }
}

fn clean_scalar(value: &str) -> String {
    value
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .to_string()
}

fn path_id(path: &Path) -> String {
    path.components()
        .filter_map(|component| component.as_os_str().to_str())
        .map(|part| {
            part.chars()
                .map(|ch| {
                    if ch.is_ascii_alphanumeric() {
                        ch.to_ascii_lowercase()
                    } else {
                        '-'
                    }
                })
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn parses_hermes_manifest_hooks_env_and_kind() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be after epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("synthchat-plugin-test-{unique}"));
        let plugin_dir = root.join("observability").join("langfuse");
        fs::create_dir_all(&plugin_dir).expect("create plugin dir");
        let manifest = plugin_dir.join("plugin.yaml");
        fs::write(
            &manifest,
            r#"
name: langfuse
version: "1.0.0"
description: "Observability"
author: NousResearch
kind: backend
requires_env:
  - HERMES_LANGFUSE_PUBLIC_KEY
  - HERMES_LANGFUSE_SECRET_KEY
hooks:
  - pre_api_request
  - post_api_request
provides_tools:
  - trace_flush
"#,
        )
        .expect("write manifest");

        let plugin = parse_plugin_manifest(&root, &manifest, "bundled").expect("parse manifest");
        assert_eq!(plugin.id, "observability/langfuse");
        assert_eq!(plugin.name, "langfuse");
        assert_eq!(plugin.source, "bundled");
        assert_eq!(plugin.kind, "backend");
        assert_eq!(plugin.provided_tools, vec!["trace_flush"]);
        assert_eq!(plugin.provided_capabilities, vec!["tool:trace_flush"]);
        assert_eq!(
            plugin.provided_hooks,
            vec!["pre_api_request", "post_api_request"]
        );
        assert_eq!(
            plugin.requires_env,
            vec!["HERMES_LANGFUSE_PUBLIC_KEY", "HERMES_LANGFUSE_SECRET_KEY"]
        );
        assert!(plugin.path.ends_with(r"observability\langfuse"));
        assert!(plugin.manifest_path.ends_with("plugin.yaml"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn parses_platform_manifest_label_and_named_env_entries() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be after epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("synthchat-platform-plugin-test-{unique}"));
        let plugin_dir = root.join("platforms").join("discord");
        fs::create_dir_all(&plugin_dir).expect("create plugin dir");
        let manifest = plugin_dir.join("plugin.yaml");
        fs::write(
            &manifest,
            r#"
name: discord-platform
label: Discord
kind: platform
requires_env:
  - name: DISCORD_BOT_TOKEN
    description: "Discord bot token"
hooks:
  - pre_gateway_dispatch
provides_hooks:
  - post_approval_response
"#,
        )
        .expect("write manifest");

        let plugin = parse_plugin_manifest(&root, &manifest, "bundled").expect("parse manifest");
        assert_eq!(plugin.id, "platforms/discord");
        assert_eq!(plugin.name, "Discord");
        assert_eq!(plugin.kind, "platform");
        assert_eq!(plugin.requires_env, vec!["DISCORD_BOT_TOKEN"]);
        assert_eq!(plugin.provided_capabilities, Vec::<String>::new());
        assert_eq!(
            plugin.provided_hooks,
            vec!["pre_gateway_dispatch", "post_approval_response"]
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn parses_hermes_provider_capability_fields_and_env_readiness() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be after epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("synthchat-provider-plugin-test-{unique}"));
        let plugin_dir = root.join("web").join("exa");
        fs::create_dir_all(&plugin_dir).expect("create plugin dir");
        let manifest = plugin_dir.join("plugin.yaml");
        fs::write(
            &manifest,
            r#"
name: web-exa
kind: backend
description: Exa search provider
requires_env:
  - SYNTHCHAT_TEST_EXA_API_KEY_THAT_SHOULD_NOT_EXIST
provides_web_providers:
  - exa
provides_browser_providers:
  - browserbase
platforms:
  - teams
"#,
        )
        .expect("write manifest");

        let plugin = parse_plugin_manifest(&root, &manifest, "bundled").expect("parse manifest");
        assert_eq!(
            plugin.provided_capabilities,
            vec![
                "web_provider:exa",
                "browser_provider:browserbase",
                "platform:teams"
            ]
        );

        let enriched = enrich_plugin_readiness(plugin);
        assert_eq!(enriched.env_configured, false);
        assert_eq!(
            enriched.missing_env,
            vec!["SYNTHCHAT_TEST_EXA_API_KEY_THAT_SHOULD_NOT_EXIST"]
        );

        let _ = fs::remove_dir_all(root);
    }
}
