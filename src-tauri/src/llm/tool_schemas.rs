use std::collections::HashSet;

use serde_json::{json, Map, Value};

use crate::models::ToolDefinition;

pub(super) fn openai_tool_schemas(tools: &[ToolDefinition]) -> Vec<Value> {
    let mut used_names = HashSet::new();
    tools
        .iter()
        .map(|tool| {
            let name = provider_safe_tool_name(tool, &mut used_names);
            json!({
                "type": "function",
                "function": {
                    "name": name,
                    "description": tool.description,
                    "parameters": normalize_tool_parameters(&tool.input_schema)
                }
            })
        })
        .collect()
}

pub(super) fn responses_tool_schemas(tools: &[ToolDefinition]) -> Vec<Value> {
    let mut used_names = HashSet::new();
    tools
        .iter()
        .map(|tool| {
            let name = provider_safe_tool_name(tool, &mut used_names);
            json!({
                "type": "function",
                "name": name,
                "description": tool.description,
                "parameters": normalize_tool_parameters(&tool.input_schema)
            })
        })
        .collect()
}

pub(super) fn anthropic_tool_schemas(tools: &[ToolDefinition]) -> Vec<Value> {
    let mut used_names = HashSet::new();
    tools
        .iter()
        .map(|tool| {
            let name = provider_safe_tool_name(tool, &mut used_names);
            json!({
                "name": name,
                "description": tool.description,
                "input_schema": normalize_tool_parameters(&tool.input_schema)
            })
        })
        .collect()
}

pub(super) fn provider_safe_tool_name(
    tool: &ToolDefinition,
    used_names: &mut HashSet<String>,
) -> String {
    let mut name = sanitize_provider_tool_name(&tool.name);
    if name.is_empty() {
        name = sanitize_provider_tool_name(&tool.display_name);
    }
    if name.is_empty() {
        name = sanitize_provider_tool_name(&tool.tool_name);
    }
    if name.is_empty() {
        name = "tool".into();
    }
    if used_names.insert(name.clone()) {
        return name;
    }

    let base = name;
    let hash = stable_tool_name_hash(tool);
    for suffix in [
        format!("_{hash:08x}"),
        format!("_{}", sanitize_provider_tool_name(&tool.server_id)),
        format!("_{}", sanitize_provider_tool_name(&tool.tool_name)),
    ] {
        let suffix = sanitize_provider_tool_suffix(&suffix);
        let candidate = truncate_tool_name_with_suffix(&base, &suffix);
        if used_names.insert(candidate.clone()) {
            return candidate;
        }
    }

    let mut index = 2usize;
    loop {
        let suffix = format!("_{hash:08x}_{index}");
        let candidate = truncate_tool_name_with_suffix(&base, &suffix);
        if used_names.insert(candidate.clone()) {
            return candidate;
        }
        index += 1;
    }
}

pub(super) fn provider_tool_name_map(tools: &[ToolDefinition]) -> Map<String, Value> {
    let mut used_names = HashSet::new();
    tools
        .iter()
        .filter_map(|tool| {
            let safe = provider_safe_tool_name(tool, &mut used_names);
            (safe != tool.name).then(|| (safe, json!(tool.name)))
        })
        .collect()
}

pub(super) fn anthropic_tool_name_map(tools: &[ToolDefinition]) -> Map<String, Value> {
    provider_tool_name_map(tools)
}

pub(super) fn original_anthropic_tool_name(name: &str, name_map: &Map<String, Value>) -> String {
    original_provider_tool_name(name, name_map)
}

pub(super) fn original_provider_tool_name(name: &str, name_map: &Map<String, Value>) -> String {
    name_map
        .get(name)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(name)
        .to_string()
}

pub(super) fn safe_anthropic_tool_name_for_original(
    original: &str,
    name_map: &Map<String, Value>,
) -> String {
    safe_provider_tool_name_for_original(original, name_map)
}

pub(super) fn safe_provider_tool_name_for_original(
    original: &str,
    name_map: &Map<String, Value>,
) -> String {
    name_map
        .iter()
        .find_map(|(safe, value)| {
            (value.as_str() == Some(original)).then(|| safe.clone())
        })
        .unwrap_or_else(|| original.to_string())
}

fn sanitize_provider_tool_name(value: &str) -> String {
    value
        .trim()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_matches('_')
        .to_string()
}

fn sanitize_provider_tool_suffix(value: &str) -> String {
    let suffix = value
        .trim()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_end_matches('_')
        .to_string();
    if suffix.starts_with('_') {
        suffix
    } else {
        format!("_{suffix}")
    }
}

fn truncate_tool_name_with_suffix(base: &str, suffix: &str) -> String {
    const MAX_TOOL_NAME_LEN: usize = 64;
    if suffix.is_empty() {
        return base.chars().take(MAX_TOOL_NAME_LEN).collect();
    }
    let suffix_len = suffix.chars().count();
    if suffix_len >= MAX_TOOL_NAME_LEN {
        return suffix
            .chars()
            .take(MAX_TOOL_NAME_LEN)
            .collect::<String>()
            .trim_matches('_')
            .to_string();
    }
    let base_len = MAX_TOOL_NAME_LEN - suffix_len;
    format!(
        "{}{}",
        base.chars().take(base_len).collect::<String>(),
        suffix
    )
}

fn stable_tool_name_hash(tool: &ToolDefinition) -> u32 {
    let mut hash = 0x811c9dc5u32;
    for byte in format!(
        "{}\0{}\0{}\0{}",
        tool.source, tool.server_id, tool.tool_name, tool.name
    )
    .bytes()
    {
        hash ^= byte as u32;
        hash = hash.wrapping_mul(0x01000193);
    }
    hash
}

pub(super) fn bedrock_tool_config(tools: &[ToolDefinition]) -> Option<Value> {
    if tools.is_empty() {
        return None;
    }
    Some(json!({
        "tools": tools.iter().map(|tool| {
            json!({
                "toolSpec": {
                    "name": tool.name,
                    "description": tool.description,
                    "inputSchema": {
                        "json": normalize_tool_parameters(&tool.input_schema)
                    }
                }
            })
        }).collect::<Vec<_>>()
    }))
}

pub(super) fn gemini_tool_schemas(tools: &[ToolDefinition]) -> Vec<Value> {
    if tools.is_empty() {
        return Vec::new();
    }
    vec![json!({
        "functionDeclarations": tools.iter().map(|tool| {
            json!({
                "name": tool.name,
                "description": tool.description,
                "parameters": sanitize_gemini_parameters(&tool.input_schema)
            })
        }).collect::<Vec<_>>()
    })]
}

pub(super) fn normalize_tool_parameters(schema: &Value) -> Value {
    let mut normalized = strip_nullable_unions(schema);
    if !normalized.is_object() {
        normalized = json!({"type": "object", "properties": {}});
    }
    if let Some(object) = normalized.as_object_mut() {
        for key in ["oneOf", "allOf", "anyOf"] {
            object.remove(key);
        }
        object.remove("nullable");
        if object.get("type").and_then(Value::as_str).is_none() {
            object.insert("type".into(), json!("object"));
        }
        if object.get("type").and_then(Value::as_str) == Some("object")
            && !object.get("properties").is_some_and(Value::is_object)
        {
            object.insert("properties".into(), json!({}));
        }
    }
    sanitize_schema_node(&normalized, false)
}

fn sanitize_schema_node(value: &Value, strip_pattern_format: bool) -> Value {
    match strip_nullable_unions(value) {
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| sanitize_schema_node(item, strip_pattern_format))
                .collect(),
        ),
        Value::Object(object) => {
            let mut out = Map::new();
            for (key, item) in object {
                if key == "nullable" {
                    continue;
                }
                if strip_pattern_format && matches!(key.as_str(), "pattern" | "format") {
                    continue;
                }
                if matches!(key.as_str(), "oneOf" | "allOf" | "anyOf") {
                    continue;
                }
                out.insert(
                    key.clone(),
                    sanitize_schema_node(&item, strip_pattern_format),
                );
            }
            Value::Object(out)
        }
        other => other,
    }
}

fn strip_nullable_unions(value: &Value) -> Value {
    let Some(object) = value.as_object() else {
        return value.clone();
    };
    for key in ["anyOf", "oneOf"] {
        let Some(items) = object.get(key).and_then(Value::as_array) else {
            continue;
        };
        let non_null = items
            .iter()
            .filter(|item| item.get("type").and_then(Value::as_str) != Some("null"))
            .collect::<Vec<_>>();
        if non_null.len() == 1 && non_null.len() < items.len() {
            let mut replacement = strip_nullable_unions(non_null[0]);
            if let (Some(source), Some(target)) = (value.as_object(), replacement.as_object_mut()) {
                for carry_key in ["description", "title", "default"] {
                    if let Some(carry) = source.get(carry_key) {
                        target.entry(carry_key).or_insert_with(|| carry.clone());
                    }
                }
            }
            return replacement;
        }
    }
    value.clone()
}

fn sanitize_gemini_parameters(schema: &Value) -> Value {
    let normalized = normalize_tool_parameters(schema);
    let mut sanitized = sanitize_schema_node(&normalized, true);
    strip_gemini_unsupported_schema_keys(&mut sanitized, false);
    if !sanitized.is_object() {
        json!({"type": "object", "properties": {}})
    } else {
        sanitized
    }
}

fn strip_gemini_unsupported_schema_keys(value: &mut Value, inside_properties: bool) {
    match value {
        Value::Array(items) => {
            for item in items {
                strip_gemini_unsupported_schema_keys(item, false);
            }
        }
        Value::Object(object) => {
            let allowed = [
                "type",
                "description",
                "properties",
                "required",
                "items",
                "enum",
                "minimum",
                "maximum",
                "minItems",
                "maxItems",
            ];
            if !inside_properties {
                object.retain(|key, _| allowed.contains(&key.as_str()));
            }
            for (key, item) in object.iter_mut() {
                strip_gemini_unsupported_schema_keys(item, key == "properties");
            }
        }
        _ => {}
    }
}
