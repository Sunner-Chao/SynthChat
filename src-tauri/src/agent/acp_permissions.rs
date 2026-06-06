use std::{
    fs,
    path::{Path, PathBuf},
};

use serde_json::{json, Value};

use super::{
    acp_edit_approval::{acp_should_auto_approve_edit, AcpEditProposal},
    acp_events::{
        acp_text_content_items, acp_tool_diff_content_items, acp_tool_event_kind,
        acp_tool_event_title, acp_tool_start_content_items,
    },
    delegation::acp_string_text,
};

#[derive(Debug, Clone, Default)]
pub(super) struct AcpPermissionApprovalContext {
    pub auto_approve: bool,
    pub edit_policy: Option<String>,
    pub cwd: Option<PathBuf>,
}

pub(super) fn acp_permission_decision(message: &Value, auto_approve: bool) -> Value {
    acp_permission_decision_with_context(
        message,
        &AcpPermissionApprovalContext {
            auto_approve,
            edit_policy: None,
            cwd: None,
        },
    )
}

pub(super) fn acp_permission_decision_with_context(
    message: &Value,
    context: &AcpPermissionApprovalContext,
) -> Value {
    let params = acp_permission_params_with_context(message, Some(context));
    if acp_permission_should_auto_approve(message, context) {
        json!({
            "method": "session/request_permission",
            "decision": "approved",
            "outcome": "selected",
            "optionId": "allow_once",
            "params": params
        })
    } else {
        json!({
            "method": "session/request_permission",
            "decision": "denied",
            "outcome": "cancelled",
            "params": params
        })
    }
}

fn acp_permission_params_with_context(
    message: &Value,
    context: Option<&AcpPermissionApprovalContext>,
) -> Value {
    let mut params = message.get("params").cloned().unwrap_or(Value::Null);
    let tool_call = params
        .get("toolCall")
        .or_else(|| params.get("tool_call"))
        .and_then(|tool_call| acp_normalized_permission_tool_call(tool_call, context));
    if let (Some(target), Some(tool_call)) = (params.as_object_mut(), tool_call) {
        target.insert("toolCall".into(), tool_call.clone());
        target.insert("tool_call".into(), tool_call);
    }
    params
}

fn acp_normalized_permission_tool_call(
    tool_call: &Value,
    context: Option<&AcpPermissionApprovalContext>,
) -> Option<Value> {
    let mut normalized = tool_call.clone();
    let target = normalized.as_object_mut()?;
    target
        .entry("sessionUpdate")
        .or_insert_with(|| Value::String("tool_call_update".into()));
    target
        .entry("session_update")
        .or_insert_with(|| Value::String("tool_call_update".into()));
    target
        .entry("status")
        .or_insert_with(|| Value::String("pending".into()));

    let raw_input = tool_call
        .get("rawInput")
        .or_else(|| tool_call.get("raw_input"))
        .cloned()
        .unwrap_or_else(|| json!({}));
    if !raw_input.is_null() {
        target
            .entry("rawInput")
            .or_insert_with(|| raw_input.clone());
        target.entry("raw_input").or_insert(raw_input.clone());
    }

    let tool_name = acp_permission_tool_name(&raw_input);
    if !tool_name.is_empty() {
        target
            .entry("kind")
            .or_insert_with(|| Value::String(acp_tool_event_kind("__internal", &tool_name)));
        target
            .entry("title")
            .or_insert_with(|| Value::String(acp_permission_tool_title(&tool_name, &raw_input)));
    }
    let content_missing = tool_call
        .get("content")
        .map(|value| value.is_null() || value.as_array().is_some_and(|items| items.is_empty()))
        .unwrap_or(true);
    if content_missing {
        if let Some(content) = acp_permission_tool_call_content(&tool_name, &raw_input, context) {
            target.insert("content".into(), content);
        }
    }
    Some(normalized)
}

fn acp_permission_tool_name(raw_input: &Value) -> String {
    let direct = acp_string_text(raw_input, &["tool", "toolName", "tool_name", "name"]);
    if !direct.is_empty() {
        return direct;
    }
    let args = raw_input
        .get("arguments")
        .or_else(|| raw_input.get("args"))
        .unwrap_or(raw_input);
    let nested = acp_string_text(args, &["tool", "toolName", "tool_name", "name"]);
    if !nested.is_empty() {
        return nested;
    }
    let command = acp_string_text(args, &["command"]);
    if !command.is_empty() {
        return "terminal".into();
    }
    String::new()
}

fn acp_permission_tool_arguments<'a>(raw_input: &'a Value) -> &'a Value {
    raw_input
        .get("arguments")
        .or_else(|| raw_input.get("args"))
        .unwrap_or(raw_input)
}

fn acp_permission_tool_title(tool_name: &str, raw_input: &Value) -> String {
    let args = acp_permission_tool_arguments(raw_input);
    if tool_name == "terminal" {
        let command = acp_string_text(args, &["command"]);
        let description = acp_string_text(args, &["description"]);
        if !command.is_empty() && !description.is_empty() {
            return format!("{description}: {command}");
        }
    }
    if matches!(tool_name, "patch" | "write_file") {
        let path = acp_string_text(args, &["path", "file", "target"]);
        if !path.is_empty() {
            return format!("Approve edit: {path}");
        }
    }
    acp_tool_event_title(tool_name, args)
}

fn acp_permission_tool_call_content(
    tool_name: &str,
    raw_input: &Value,
    context: Option<&AcpPermissionApprovalContext>,
) -> Option<Value> {
    let args = acp_permission_tool_arguments(raw_input);
    match tool_name {
        "terminal" => {
            let command = acp_string_text(args, &["command"]);
            if command.is_empty() {
                return None;
            }
            let description = acp_string_text(args, &["description"]);
            let text = if description.is_empty() {
                format!("$ {command}")
            } else {
                format!("{description}\n$ {command}")
            };
            Some(acp_text_content_items(&text))
        }
        "patch" | "write_file" => {
            let path = acp_string_text(args, &["path", "file", "target"]);
            if tool_name == "patch" {
                if let Some((old_text, new_text)) =
                    acp_permission_patch_full_file_diff(args, context)
                {
                    return Some(acp_tool_diff_content_items(
                        &path,
                        Some(&old_text),
                        &new_text,
                    ));
                }
            }
            let old_text = acp_raw_string_text(args, &["oldString", "old_string", "oldText"]);
            let new_text = acp_raw_string_text(
                args,
                &[
                    "newString",
                    "new_string",
                    "newText",
                    "content",
                    "file_content",
                ],
            );
            if path.is_empty() || new_text.is_empty() {
                return None;
            }
            Some(acp_tool_diff_content_items(
                &path,
                (!old_text.is_empty()).then_some(old_text.as_str()),
                &new_text,
            ))
        }
        _ => acp_tool_start_content_items(tool_name, args),
    }
}

fn acp_permission_patch_full_file_diff(
    args: &Value,
    context: Option<&AcpPermissionApprovalContext>,
) -> Option<(String, String)> {
    let cwd = context?.cwd.as_deref()?;
    let path = acp_string_text(args, &["path", "file", "target"]);
    let old_string = acp_raw_string_text(args, &["oldString", "old_string", "oldText"]);
    let new_string = acp_raw_string_text(args, &["newString", "new_string", "newText"]);
    if path.is_empty() || old_string.is_empty() || new_string.is_empty() {
        return None;
    }
    let target = Path::new(&path);
    let target = if target.is_absolute() {
        target.to_path_buf()
    } else {
        cwd.join(target)
    };
    let old_text = fs::read_to_string(target).ok()?;
    if !old_text.contains(&old_string) {
        return None;
    }
    let replace_all = args
        .get("replaceAll")
        .or_else(|| args.get("replace_all"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let new_text = if replace_all {
        old_text.replace(&old_string, &new_string)
    } else {
        old_text.replacen(&old_string, &new_string, 1)
    };
    Some((old_text, new_text))
}

fn acp_raw_string_text(value: &Value, keys: &[&str]) -> String {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_str))
        .filter(|text| !text.is_empty())
        .unwrap_or("")
        .to_string()
}

pub(super) fn acp_permission_response(message: &Value, auto_approve: bool) -> Value {
    acp_permission_response_with_context(
        message,
        &AcpPermissionApprovalContext {
            auto_approve,
            edit_policy: None,
            cwd: None,
        },
    )
}

pub(super) fn acp_permission_response_with_context(
    message: &Value,
    context: &AcpPermissionApprovalContext,
) -> Value {
    let id = message.get("id").cloned().unwrap_or(Value::Null);
    if acp_permission_should_auto_approve(message, context) {
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "outcome": {
                    "outcome": "selected",
                    "optionId": "allow_once",
                    "option_id": "allow_once"
                }
            }
        })
    } else {
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "outcome": {
                    "outcome": "cancelled"
                }
            }
        })
    }
}

fn acp_permission_should_auto_approve(
    message: &Value,
    context: &AcpPermissionApprovalContext,
) -> bool {
    let raw_input = message
        .get("params")
        .and_then(|params| params.get("toolCall").or_else(|| params.get("tool_call")))
        .and_then(|tool_call| {
            tool_call
                .get("rawInput")
                .or_else(|| tool_call.get("raw_input"))
        })
        .unwrap_or(&Value::Null);
    let tool_name = acp_permission_tool_name(raw_input);
    if matches!(tool_name.as_str(), "patch" | "write_file") {
        if let Some(policy) = context.edit_policy.as_deref() {
            return acp_permission_edit_proposal(&tool_name, raw_input)
                .map(|proposal| {
                    acp_should_auto_approve_edit(&proposal, policy, context.cwd.as_deref())
                })
                .unwrap_or(false);
        }
    }
    context.auto_approve
}

fn acp_permission_edit_proposal(tool_name: &str, raw_input: &Value) -> Option<AcpEditProposal> {
    let args = acp_permission_tool_arguments(raw_input);
    let path = acp_string_text(args, &["path", "file", "target"]);
    let new_text = acp_raw_string_text(
        args,
        &[
            "newString",
            "new_string",
            "newText",
            "content",
            "file_content",
        ],
    );
    if path.is_empty() || new_text.is_empty() {
        return None;
    }
    let old_text = acp_raw_string_text(args, &["oldString", "old_string", "oldText"]);
    Some(AcpEditProposal {
        tool_name: tool_name.to_string(),
        path: Path::new(&path).to_path_buf(),
        old_text: (!old_text.is_empty()).then_some(old_text),
        new_text,
    })
}
