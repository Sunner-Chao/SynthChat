use serde_json::{json, Map, Value};

use crate::models::ToolDefinition;

pub(super) fn openai_tool_schemas(tools: &[ToolDefinition]) -> Vec<Value> {
    tools
        .iter()
        .map(|tool| {
            json!({
                "type": "function",
                "function": {
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": normalize_tool_parameters(&tool.input_schema)
                }
            })
        })
        .collect()
}

pub(super) fn responses_tool_schemas(tools: &[ToolDefinition]) -> Vec<Value> {
    tools
        .iter()
        .map(|tool| {
            json!({
                "type": "function",
                "name": tool.name,
                "description": tool.description,
                "parameters": normalize_tool_parameters(&tool.input_schema)
            })
        })
        .collect()
}

pub(super) fn anthropic_tool_schemas(tools: &[ToolDefinition]) -> Vec<Value> {
    tools
        .iter()
        .map(|tool| {
            json!({
                "name": tool.name,
                "description": tool.description,
                "input_schema": normalize_tool_parameters(&tool.input_schema)
            })
        })
        .collect()
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
