use serde_json::{json, Value};

use crate::{
    error::{AppError, AppResult},
    models::{new_id, now_iso},
    store::AppStore,
};

use super::{required_string_arg, string_arg, string_list_arg};

pub(super) fn kanban_create_tool(store: &AppStore, payload: &Value) -> AppResult<String> {
    let title = required_string_arg(payload, &["title"], "kanban_create")?;
    let now = now_iso();
    let id = string_arg(payload, &["taskId", "task_id", "id"]).unwrap_or_else(|| new_id("kb"));
    let parents = string_list_arg(payload, &["parents", "parentIds", "parent_ids"]);
    let mut tasks = store.agent_kanban_tasks()?;
    if tasks
        .iter()
        .any(|task| task.get("id").and_then(Value::as_str) == Some(id.as_str()))
    {
        return Err(AppError::BadRequest(format!(
            "kanban task already exists: {id}"
        )));
    }
    let task = json!({
        "id": id,
        "title": title,
        "body": string_arg(payload, &["body", "description"]).unwrap_or_default(),
        "assignee": string_arg(payload, &["assignee"]),
        "status": string_arg(payload, &["status"]).unwrap_or_else(|| "ready".into()),
        "priority": payload.get("priority").and_then(Value::as_i64).unwrap_or(0),
        "tenant": string_arg(payload, &["tenant"]),
        "workspaceKind": string_arg(payload, &["workspaceKind", "workspace_kind"]),
        "workspacePath": string_arg(payload, &["workspacePath", "workspace_path"]),
        "createdBy": string_arg(payload, &["createdBy", "created_by"]).unwrap_or_else(|| "agent".into()),
        "createdAt": now,
        "updatedAt": now,
        "startedAt": Value::Null,
        "completedAt": Value::Null,
        "lastHeartbeatAt": Value::Null,
        "result": Value::Null,
        "blockReason": Value::Null,
        "metadata": payload.get("metadata").cloned().unwrap_or_else(|| json!({})),
        "parents": parents,
        "children": [],
        "comments": [],
        "events": [kanban_event("created", json!({}))]
    });
    tasks.push(task.clone());
    store.set_agent_kanban_tasks(tasks)?;
    Ok(serde_json::to_string_pretty(
        &json!({"ok": true, "task": task}),
    )?)
}

pub(super) fn kanban_list_tool(store: &AppStore, payload: &Value) -> AppResult<String> {
    let status = string_arg(payload, &["status"]);
    let assignee = string_arg(payload, &["assignee"]);
    let tenant = string_arg(payload, &["tenant"]);
    let include_archived = payload
        .get("includeArchived")
        .or_else(|| payload.get("include_archived"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let limit = payload
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(50)
        .clamp(1, 200) as usize;
    let mut items = Vec::new();
    for task in store.agent_kanban_tasks()? {
        if !include_archived && task.get("status").and_then(Value::as_str) == Some("archived") {
            continue;
        }
        if let Some(status) = status.as_deref() {
            if task.get("status").and_then(Value::as_str) != Some(status) {
                continue;
            }
        }
        if let Some(assignee) = assignee.as_deref() {
            if task.get("assignee").and_then(Value::as_str) != Some(assignee) {
                continue;
            }
        }
        if let Some(tenant) = tenant.as_deref() {
            if task.get("tenant").and_then(Value::as_str) != Some(tenant) {
                continue;
            }
        }
        items.push(kanban_task_summary(&task));
        if items.len() >= limit {
            break;
        }
    }
    Ok(serde_json::to_string_pretty(&json!({
        "ok": true,
        "tasks": items,
        "count": items.len(),
        "limit": limit
    }))?)
}

pub(super) fn kanban_show_tool(store: &AppStore, payload: &Value) -> AppResult<String> {
    let task_id = required_string_arg(payload, &["taskId", "task_id"], "kanban_show")?;
    let task = find_kanban_task(&store.agent_kanban_tasks()?, &task_id)?;
    Ok(serde_json::to_string_pretty(
        &json!({"ok": true, "task": task}),
    )?)
}

pub(super) fn kanban_complete_tool(store: &AppStore, payload: &Value) -> AppResult<String> {
    let task_id = required_string_arg(payload, &["taskId", "task_id"], "kanban_complete")?;
    let summary = string_arg(payload, &["summary"]);
    let result = string_arg(payload, &["result"]);
    if summary.is_none() && result.is_none() {
        return Err(AppError::BadRequest(
            "kanban_complete requires payload.summary or payload.result".into(),
        ));
    }
    let metadata_patch = kanban_metadata_arg(payload)?;
    let artifacts = kanban_artifacts_arg(payload)?;
    let created_cards = kanban_created_cards_arg(payload)?;
    validate_kanban_created_cards(store, &task_id, &created_cards)?;
    mutate_kanban_task(store, &task_id, |task| {
        let now = now_iso();
        let metadata = merged_kanban_completion_metadata(task, &metadata_patch, artifacts.clone());
        set_task_field(task, "status", json!("completed"));
        set_task_field(task, "completedAt", json!(now.clone()));
        set_task_field(task, "updatedAt", json!(now));
        if let Some(summary) = summary.clone() {
            set_task_field(task, "summary", json!(summary));
        }
        if let Some(result) = result.clone() {
            set_task_field(task, "result", json!(result));
        }
        if !created_cards.is_empty() {
            set_task_field(task, "createdCards", json!(created_cards.clone()));
            set_task_field(task, "created_cards", json!(created_cards.clone()));
        }
        set_task_field(task, "metadata", metadata.clone());
        push_kanban_event(
            task,
            "completed",
            json!({
                "summary": summary,
                "result": result,
                "metadata": metadata,
                "createdCards": created_cards.clone(),
                "created_cards": created_cards.clone()
            }),
        );
    })
}

pub(super) fn kanban_block_tool(store: &AppStore, payload: &Value) -> AppResult<String> {
    let task_id = required_string_arg(payload, &["taskId", "task_id"], "kanban_block")?;
    let reason = required_string_arg(payload, &["reason", "summary"], "kanban_block")?;
    mutate_kanban_task(store, &task_id, |task| {
        set_task_field(task, "status", json!("blocked"));
        set_task_field(task, "blockReason", json!(reason.clone()));
        set_task_field(task, "updatedAt", json!(now_iso()));
        push_kanban_event(task, "blocked", json!({"reason": reason}));
    })
}

pub(super) fn kanban_unblock_tool(store: &AppStore, payload: &Value) -> AppResult<String> {
    let task_id = required_string_arg(payload, &["taskId", "task_id"], "kanban_unblock")?;
    let note = string_arg(payload, &["note", "summary"]);
    mutate_kanban_task(store, &task_id, |task| {
        set_task_field(task, "status", json!("ready"));
        set_task_field(task, "blockReason", Value::Null);
        set_task_field(task, "updatedAt", json!(now_iso()));
        push_kanban_event(task, "unblocked", json!({"note": note}));
    })
}

pub(super) fn kanban_heartbeat_tool(store: &AppStore, payload: &Value) -> AppResult<String> {
    let task_id = required_string_arg(payload, &["taskId", "task_id"], "kanban_heartbeat")?;
    let note = string_arg(payload, &["note", "summary"]);
    mutate_kanban_task(store, &task_id, |task| {
        let now = now_iso();
        set_task_field(task, "lastHeartbeatAt", json!(now.clone()));
        set_task_field(task, "updatedAt", json!(now));
        push_kanban_event(task, "heartbeat", json!({"note": note}));
    })
}

pub(super) fn kanban_comment_tool(store: &AppStore, payload: &Value) -> AppResult<String> {
    let task_id = required_string_arg(payload, &["taskId", "task_id"], "kanban_comment")?;
    let body = required_string_arg(payload, &["body", "comment"], "kanban_comment")?;
    let author = string_arg(payload, &["author"]).unwrap_or_else(|| "agent".into());
    mutate_kanban_task(store, &task_id, |task| {
        let comment = json!({"author": author, "body": body, "createdAt": now_iso()});
        if let Some(comments) = task.get_mut("comments").and_then(Value::as_array_mut) {
            comments.push(comment.clone());
        }
        set_task_field(task, "updatedAt", json!(now_iso()));
        push_kanban_event(task, "commented", comment);
    })
}

pub(super) fn kanban_link_tool(store: &AppStore, payload: &Value) -> AppResult<String> {
    let parent_id = required_string_arg(payload, &["parentId", "parent_id"], "kanban_link")?;
    let child_id = required_string_arg(payload, &["childId", "child_id"], "kanban_link")?;
    if parent_id == child_id {
        return Err(AppError::BadRequest(
            "kanban_link cannot link a task to itself".into(),
        ));
    }
    let mut tasks = store.agent_kanban_tasks()?;
    if find_kanban_task(&tasks, &parent_id).is_err() || find_kanban_task(&tasks, &child_id).is_err()
    {
        return Err(AppError::BadRequest(
            "kanban_link requires existing parent and child tasks".into(),
        ));
    }
    for task in &mut tasks {
        if task.get("id").and_then(Value::as_str) == Some(parent_id.as_str()) {
            push_unique_string_field(task, "children", &child_id);
            push_kanban_event(task, "linked_child", json!({"childId": child_id}));
        }
        if task.get("id").and_then(Value::as_str) == Some(child_id.as_str()) {
            push_unique_string_field(task, "parents", &parent_id);
            push_kanban_event(task, "linked_parent", json!({"parentId": parent_id}));
        }
    }
    store.set_agent_kanban_tasks(tasks)?;
    Ok(serde_json::to_string_pretty(&json!({
        "ok": true,
        "parentId": parent_id,
        "childId": child_id
    }))?)
}

fn mutate_kanban_task<F>(store: &AppStore, task_id: &str, mut mutate: F) -> AppResult<String>
where
    F: FnMut(&mut Value),
{
    let mut tasks = store.agent_kanban_tasks()?;
    let task = tasks
        .iter_mut()
        .find(|task| task.get("id").and_then(Value::as_str) == Some(task_id))
        .ok_or_else(|| AppError::BadRequest(format!("kanban task not found: {task_id}")))?;
    mutate(task);
    let updated = task.clone();
    store.set_agent_kanban_tasks(tasks)?;
    Ok(serde_json::to_string_pretty(
        &json!({"ok": true, "task": updated}),
    )?)
}

fn find_kanban_task(tasks: &[Value], task_id: &str) -> AppResult<Value> {
    tasks
        .iter()
        .find(|task| task.get("id").and_then(Value::as_str) == Some(task_id))
        .cloned()
        .ok_or_else(|| AppError::BadRequest(format!("kanban task not found: {task_id}")))
}

fn kanban_task_summary(task: &Value) -> Value {
    json!({
        "id": task.get("id").cloned().unwrap_or(Value::Null),
        "title": task.get("title").cloned().unwrap_or(Value::Null),
        "assignee": task.get("assignee").cloned().unwrap_or(Value::Null),
        "status": task.get("status").cloned().unwrap_or(Value::Null),
        "priority": task.get("priority").cloned().unwrap_or(Value::Null),
        "tenant": task.get("tenant").cloned().unwrap_or(Value::Null),
        "parentCount": task.get("parents").and_then(Value::as_array).map(Vec::len).unwrap_or(0),
        "childCount": task.get("children").and_then(Value::as_array).map(Vec::len).unwrap_or(0),
        "updatedAt": task.get("updatedAt").cloned().unwrap_or(Value::Null),
    })
}

fn set_task_field(task: &mut Value, key: &str, value: Value) {
    if let Some(object) = task.as_object_mut() {
        object.insert(key.into(), value);
    }
}

fn kanban_event(kind: &str, payload: Value) -> Value {
    json!({"kind": kind, "payload": payload, "createdAt": now_iso()})
}

fn kanban_metadata_arg(payload: &Value) -> AppResult<Value> {
    let metadata = payload
        .get("metadata")
        .cloned()
        .unwrap_or_else(|| json!({}));
    if !metadata.is_object() {
        return Err(AppError::BadRequest(format!(
            "kanban_complete metadata must be an object, got {}",
            value_type_name(&metadata)
        )));
    }
    Ok(metadata)
}

fn merged_kanban_completion_metadata(
    task: &Value,
    metadata_patch: &Value,
    artifacts: Vec<String>,
) -> Value {
    let mut metadata = task
        .get("metadata")
        .filter(|value| value.is_object())
        .cloned()
        .unwrap_or_else(|| json!({}));
    if let (Some(target), Some(patch)) = (metadata.as_object_mut(), metadata_patch.as_object()) {
        for (key, value) in patch {
            target.insert(key.clone(), value.clone());
        }
    }
    if !artifacts.is_empty() {
        let object = metadata
            .as_object_mut()
            .expect("metadata is initialized as object");
        let mut merged = Vec::<String>::new();
        let mut seen = std::collections::HashSet::<String>::new();
        if let Some(existing) = object.get("artifacts") {
            if let Some(existing_items) = existing.as_array() {
                for item in existing_items {
                    if let Some(path) = item
                        .as_str()
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                    {
                        if seen.insert(path.to_string()) {
                            merged.push(path.to_string());
                        }
                    }
                }
            }
        }
        for artifact in artifacts {
            if seen.insert(artifact.clone()) {
                merged.push(artifact);
            }
        }
        object.insert(
            "artifacts".into(),
            Value::Array(merged.into_iter().map(Value::String).collect()),
        );
    }
    metadata
}

fn kanban_artifacts_arg(payload: &Value) -> AppResult<Vec<String>> {
    let Some(value) = payload.get("artifacts") else {
        return Ok(Vec::new());
    };
    if let Some(path) = value.as_str() {
        let path = path.trim();
        return Ok(if path.is_empty() {
            Vec::new()
        } else {
            vec![path.to_string()]
        });
    }
    let Some(items) = value.as_array() else {
        return Err(AppError::BadRequest(format!(
            "kanban_complete artifacts must be a string or array of strings, got {}",
            value_type_name(value)
        )));
    };
    let mut artifacts = Vec::new();
    for item in items {
        let Some(path) = item.as_str().map(str::trim) else {
            return Err(AppError::BadRequest(
                "kanban_complete artifacts must contain only strings".into(),
            ));
        };
        if !path.is_empty() {
            artifacts.push(path.to_string());
        }
    }
    Ok(artifacts)
}

fn kanban_created_cards_arg(payload: &Value) -> AppResult<Vec<String>> {
    let Some(value) = payload
        .get("createdCards")
        .or_else(|| payload.get("created_cards"))
    else {
        return Ok(Vec::new());
    };
    if let Some(task_id) = value.as_str() {
        let task_id = task_id.trim();
        return Ok(if task_id.is_empty() {
            Vec::new()
        } else {
            vec![task_id.to_string()]
        });
    }
    let Some(items) = value.as_array() else {
        return Err(AppError::BadRequest(format!(
            "kanban_complete created_cards must be a string or array of task ids, got {}",
            value_type_name(value)
        )));
    };
    let mut cards = Vec::new();
    let mut seen = std::collections::HashSet::<String>::new();
    for item in items {
        let Some(task_id) = item.as_str().map(str::trim) else {
            return Err(AppError::BadRequest(
                "kanban_complete created_cards must contain only strings".into(),
            ));
        };
        if !task_id.is_empty() && seen.insert(task_id.to_string()) {
            cards.push(task_id.to_string());
        }
    }
    Ok(cards)
}

fn validate_kanban_created_cards(
    store: &AppStore,
    task_id: &str,
    created_cards: &[String],
) -> AppResult<()> {
    if created_cards.is_empty() {
        return Ok(());
    }
    let tasks = store.agent_kanban_tasks()?;
    let current = find_kanban_task(&tasks, task_id)?;
    let current_creator = current
        .get("createdBy")
        .or_else(|| current.get("created_by"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let mut phantom = Vec::new();
    for card_id in created_cards {
        let Some(card) = tasks
            .iter()
            .find(|task| task.get("id").and_then(Value::as_str) == Some(card_id.as_str()))
        else {
            phantom.push(card_id.clone());
            continue;
        };
        if let Some(current_creator) = current_creator.as_deref() {
            let card_creator = card
                .get("createdBy")
                .or_else(|| card.get("created_by"))
                .and_then(Value::as_str);
            if card_creator.is_some() && card_creator != Some(current_creator) {
                phantom.push(card_id.clone());
            }
        }
    }
    if phantom.is_empty() {
        return Ok(());
    }
    Err(AppError::BadRequest(format!(
        "kanban_complete blocked: the following created_cards do not exist or were not created by this worker: {}. Your task is still in-flight (no state change). Retry kanban_complete with the same summary/metadata and either drop these ids from created_cards, or pass created_cards=[] to skip the card-claim check entirely.",
        phantom.join(", ")
    )))
}

fn value_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn push_kanban_event(task: &mut Value, kind: &str, payload: Value) {
    let event = kanban_event(kind, payload);
    if let Some(events) = task.get_mut("events").and_then(Value::as_array_mut) {
        events.push(event);
    }
}

fn push_unique_string_field(task: &mut Value, key: &str, value: &str) {
    if let Some(items) = task.get_mut(key).and_then(Value::as_array_mut) {
        if !items.iter().any(|item| item.as_str() == Some(value)) {
            items.push(Value::String(value.into()));
        }
    }
}
