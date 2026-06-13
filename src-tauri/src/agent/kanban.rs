use serde_json::{json, Value};

use crate::{
    error::{AppError, AppResult},
    models::{new_id, now_iso, ChatMessage, LlmProvider, Persona},
    store::AppStore,
};

use super::{
    complete_chat_with_provider_failover, list_agent_auxiliary_task_assignments,
    required_string_arg, send_message_tool_async, string_arg, string_list_arg,
};

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
    let result_text = mutate_kanban_task(store, &task_id, |task| {
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
        enqueue_kanban_terminal_notifications(task, "completed");
    })?;
    spawn_kanban_notification_delivery(store, &result_text);
    Ok(result_text)
}

pub(super) fn kanban_block_tool(store: &AppStore, payload: &Value) -> AppResult<String> {
    let task_id = required_string_arg(payload, &["taskId", "task_id"], "kanban_block")?;
    let reason = required_string_arg(payload, &["reason", "summary"], "kanban_block")?;
    let result_text = mutate_kanban_task(store, &task_id, |task| {
        set_task_field(task, "status", json!("blocked"));
        set_task_field(task, "blockReason", json!(reason.clone()));
        set_task_field(task, "updatedAt", json!(now_iso()));
        push_kanban_event(task, "blocked", json!({"reason": reason}));
        enqueue_kanban_terminal_notifications(task, "blocked");
    })?;
    spawn_kanban_notification_delivery(store, &result_text);
    Ok(result_text)
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

pub(super) fn kanban_unlink_tool(store: &AppStore, payload: &Value) -> AppResult<String> {
    let parent_id = required_string_arg(payload, &["parentId", "parent_id"], "kanban_unlink")?;
    let child_id = required_string_arg(payload, &["childId", "child_id"], "kanban_unlink")?;
    let mut tasks = store.agent_kanban_tasks()?;
    if find_kanban_task(&tasks, &parent_id).is_err() || find_kanban_task(&tasks, &child_id).is_err()
    {
        return Err(AppError::BadRequest(
            "kanban_unlink requires existing parent and child tasks".into(),
        ));
    }
    for task in &mut tasks {
        if task.get("id").and_then(Value::as_str) == Some(parent_id.as_str()) {
            remove_string_field(task, "children", &child_id);
            push_kanban_event(task, "unlinked_child", json!({"childId": child_id}));
        }
        if task.get("id").and_then(Value::as_str) == Some(child_id.as_str()) {
            remove_string_field(task, "parents", &parent_id);
            push_kanban_event(task, "unlinked_parent", json!({"parentId": parent_id}));
        }
    }
    store.set_agent_kanban_tasks(tasks)?;
    Ok(serde_json::to_string_pretty(&json!({
        "ok": true,
        "parentId": parent_id,
        "childId": child_id
    }))?)
}

pub(super) fn kanban_update_tool(store: &AppStore, payload: &Value) -> AppResult<String> {
    let task_id = required_string_arg(payload, &["taskId", "task_id", "id"], "kanban_update")?;
    let author = string_arg(payload, &["author"]).unwrap_or_else(|| "dashboard".into());
    validate_kanban_update_payload(payload)?;
    mutate_kanban_task(store, &task_id, |task| {
        let patch = apply_kanban_task_patch(task, payload);
        set_task_field(task, "updatedAt", json!(now_iso()));
        push_kanban_event(task, "updated", json!({"author": author, "patch": patch}));
    })
}

pub(super) fn kanban_delete_tool(store: &AppStore, payload: &Value) -> AppResult<String> {
    let task_id = required_string_arg(payload, &["taskId", "task_id", "id"], "kanban_delete")?;
    let hard_delete = payload
        .get("hardDelete")
        .or_else(|| payload.get("hard_delete"))
        .or_else(|| payload.get("delete"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !hard_delete {
        return mutate_kanban_task(store, &task_id, |task| {
            set_task_field(task, "status", json!("archived"));
            set_task_field(task, "updatedAt", json!(now_iso()));
            push_kanban_event(task, "archived", json!({"source": "dashboard"}));
        });
    }

    let mut tasks = store.agent_kanban_tasks()?;
    let position = tasks
        .iter()
        .position(|task| task.get("id").and_then(Value::as_str) == Some(task_id.as_str()))
        .ok_or_else(|| AppError::BadRequest(format!("kanban task not found: {task_id}")))?;
    let removed = tasks.remove(position);
    for task in &mut tasks {
        remove_string_field(task, "parents", &task_id);
        remove_string_field(task, "children", &task_id);
    }
    store.set_agent_kanban_tasks(tasks)?;
    Ok(serde_json::to_string_pretty(&json!({
        "ok": true,
        "deleted": true,
        "task": removed
    }))?)
}

pub(super) fn kanban_bulk_update_tool(store: &AppStore, payload: &Value) -> AppResult<String> {
    let task_ids = string_list_arg(payload, &["taskIds", "task_ids", "tasks", "ids"]);
    if task_ids.is_empty() {
        return Err(AppError::BadRequest(
            "kanban_bulk_update requires taskIds".into(),
        ));
    }
    let author = string_arg(payload, &["author"]).unwrap_or_else(|| "dashboard".into());
    validate_kanban_update_payload(payload)?;
    let mut tasks = store.agent_kanban_tasks()?;
    let mut updated = Vec::new();
    for task_id in &task_ids {
        let task = tasks
            .iter_mut()
            .find(|task| task.get("id").and_then(Value::as_str) == Some(task_id.as_str()))
            .ok_or_else(|| AppError::BadRequest(format!("kanban task not found: {task_id}")))?;
        let patch = apply_kanban_task_patch(task, payload);
        set_task_field(task, "updatedAt", json!(now_iso()));
        push_kanban_event(
            task,
            "bulk_updated",
            json!({"author": author, "patch": patch}),
        );
        updated.push(task.clone());
    }
    store.set_agent_kanban_tasks(tasks)?;
    Ok(serde_json::to_string_pretty(&json!({
        "ok": true,
        "updated": updated,
        "count": updated.len()
    }))?)
}

pub(super) async fn kanban_specify_tool(store: &AppStore, payload: &Value) -> AppResult<String> {
    let task_id = required_string_arg(payload, &["taskId", "task_id", "id"], "kanban_specify")?;
    let author = string_arg(payload, &["author"]).unwrap_or_else(|| "specifier".into());
    let task = find_kanban_task(&store.agent_kanban_tasks()?, &task_id)?;
    let status = task
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if status != "triage" {
        return Err(AppError::BadRequest(format!(
            "kanban_specify requires a triage task, got status={status:?}"
        )));
    }
    let title = task
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    let body = task
        .get("body")
        .or_else(|| task.get("description"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();

    let (spec, source, model) =
        match specify_kanban_task_with_auxiliary(store, &task_id, &title, &body).await {
            Ok((spec, model)) => (spec, "auxiliary", model),
            Err(_) => (
                deterministic_kanban_specification(&title, &body),
                "deterministic",
                String::new(),
            ),
        };
    mutate_kanban_task(store, &task_id, |task| {
        set_task_field(task, "title", json!(spec.title.clone()));
        set_task_field(task, "body", json!(spec.body.clone()));
        set_task_field(task, "status", json!("todo"));
        set_task_field(task, "updatedAt", json!(now_iso()));
        let mut metadata = task
            .get("metadata")
            .filter(|value| value.is_object())
            .cloned()
            .unwrap_or_else(|| json!({}));
        if let Some(object) = metadata.as_object_mut() {
            object.insert("specifierSource".into(), json!(source));
            object.insert("specifierModel".into(), json!(model));
        }
        set_task_field(task, "metadata", metadata);
        push_kanban_event(
            task,
            "specified",
            json!({"author": author, "source": source, "model": model}),
        );
    })
    .and_then(|text| {
        let mut value = serde_json::from_str::<Value>(&text)?;
        value["source"] = json!(source);
        value["model"] = json!(model);
        Ok(serde_json::to_string_pretty(&value)?)
    })
}

pub(super) async fn kanban_decompose_tool(store: &AppStore, payload: &Value) -> AppResult<String> {
    let objective = required_string_arg(
        payload,
        &["objective", "task", "prompt"],
        "kanban_decompose",
    )?;
    let create = payload
        .get("create")
        .or_else(|| payload.get("createTasks"))
        .or_else(|| payload.get("create_tasks"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let max_tasks = payload
        .get("maxTasks")
        .or_else(|| payload.get("max_tasks"))
        .and_then(Value::as_u64)
        .unwrap_or(6)
        .clamp(1, 20) as usize;
    let parent_ids = string_list_arg(payload, &["parents", "parentIds", "parent_ids"]);
    let assignee = string_arg(payload, &["assignee"]);
    let tenant = string_arg(payload, &["tenant"]);
    let workspace_kind = string_arg(payload, &["workspaceKind", "workspace_kind"]);
    let workspace_path = string_arg(payload, &["workspacePath", "workspace_path"]);

    let (cards, source, model) =
        match decompose_kanban_cards_with_auxiliary(store, &objective, max_tasks).await {
            Ok((cards, model)) if !cards.is_empty() => (cards, "auxiliary", model),
            _ => (
                deterministic_kanban_decomposition(&objective, max_tasks),
                "deterministic",
                String::new(),
            ),
        };
    let cards = cards.into_iter().take(max_tasks).collect::<Vec<_>>();
    let mut created = Vec::new();
    if create {
        for card in &cards {
            let mut create_payload = json!({
                "title": card.title.clone(),
                "body": card.body.clone(),
                "priority": card.priority,
                "parents": parent_ids.clone(),
                "createdBy": "kanban_decompose",
                "metadata": {
                    "decomposedFrom": objective.clone(),
                    "decomposerSource": source,
                    "decomposerModel": model.clone(),
                }
            });
            if let Some(assignee) = assignee.as_deref() {
                create_payload["assignee"] = json!(assignee);
            }
            if let Some(tenant) = tenant.as_deref() {
                create_payload["tenant"] = json!(tenant);
            }
            if let Some(workspace_kind) = workspace_kind.as_deref() {
                create_payload["workspaceKind"] = json!(workspace_kind);
            }
            if let Some(workspace_path) = workspace_path.as_deref() {
                create_payload["workspacePath"] = json!(workspace_path);
            }
            let created_text = kanban_create_tool(store, &create_payload)?;
            created.push(serde_json::from_str::<Value>(&created_text)?["task"].clone());
        }
    }
    Ok(serde_json::to_string_pretty(&json!({
        "ok": true,
        "source": source,
        "model": model,
        "objective": objective,
        "created": create,
        "cards": cards.iter().map(KanbanDraftCard::to_json).collect::<Vec<_>>(),
        "createdTasks": created,
    }))?)
}

#[derive(Debug, Clone)]
struct KanbanDraftCard {
    title: String,
    body: String,
    priority: i64,
}

impl KanbanDraftCard {
    fn to_json(&self) -> Value {
        json!({
            "title": self.title.clone(),
            "body": self.body.clone(),
            "priority": self.priority,
        })
    }
}

#[derive(Debug, Clone)]
struct KanbanTaskSpec {
    title: String,
    body: String,
}

async fn specify_kanban_task_with_auxiliary(
    store: &AppStore,
    task_id: &str,
    title: &str,
    body: &str,
) -> AppResult<(KanbanTaskSpec, String)> {
    let Some((providers, persona, model_label)) =
        build_kanban_decomposer_provider_plan(store, "triage_specifier")?
    else {
        return Err(AppError::BadRequest(
            "triage_specifier auxiliary assignment is not configured".into(),
        ));
    };
    let system_prompt = "You turn rough kanban triage ideas into concrete task specs. Return only JSON with title and body. The body must include **Goal**, **Approach**, **Acceptance criteria**, and optionally **Out of scope** sections.".to_string();
    let user_prompt = format!(
        "Task id: {task_id}\nCurrent title: {title}\nCurrent body:\n{}\n\nReturn JSON only, like {{\"title\":\"imperative title <= 80 chars\",\"body\":\"**Goal** ...\"}}.",
        if body.trim().is_empty() { "(no body)" } else { body }
    );
    let history = vec![ChatMessage::new(
        "__kanban_specify__".into(),
        "user",
        user_prompt.clone(),
        "kanban_specify",
    )];
    let reply = complete_chat_with_provider_failover(
        store,
        None,
        &providers,
        &persona,
        system_prompt,
        history,
        &user_prompt,
        None,
        None,
    )
    .await?;
    parse_kanban_specification(&reply.content)
        .map(|spec| (spec, model_label))
        .ok_or_else(|| AppError::BadRequest("triage_specifier returned no usable spec".into()))
}

fn parse_kanban_specification(text: &str) -> Option<KanbanTaskSpec> {
    let value = parse_json_object_from_text(text)?;
    let title = value
        .get("title")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    let body = value
        .get("body")
        .or_else(|| value.get("description"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    Some(KanbanTaskSpec {
        title: truncate_title(title),
        body: body.to_string(),
    })
}

fn deterministic_kanban_specification(title: &str, body: &str) -> KanbanTaskSpec {
    let title = truncate_title(title);
    let seed = if body.trim().is_empty() {
        title.as_str()
    } else {
        body.trim()
    };
    KanbanTaskSpec {
        title: title.clone(),
        body: format!(
            "**Goal**\nDeliver: {title}.\n\n**Approach**\n- Clarify the current state and constraints.\n- Implement the smallest coherent change that satisfies the request.\n- Record important decisions and evidence.\n\n**Acceptance criteria**\n- The requested outcome is implemented or explicitly blocked with evidence.\n- Relevant checks pass or failures are documented.\n\n**Out of scope**\n- Unrelated refactors or behavior changes.\n\nSource note: {seed}"
        ),
    }
}

async fn decompose_kanban_cards_with_auxiliary(
    store: &AppStore,
    objective: &str,
    max_tasks: usize,
) -> AppResult<(Vec<KanbanDraftCard>, String)> {
    let Some((providers, persona, model_label)) =
        build_kanban_decomposer_provider_plan(store, "kanban_decomposer")?
    else {
        return Err(AppError::BadRequest(
            "kanban_decomposer auxiliary assignment is not configured".into(),
        ));
    };
    let system_prompt = "You decompose software agent work into concise kanban cards. Return only JSON with a top-level tasks array. Each task must have title, body, and priority integer.".to_string();
    let user_prompt = format!(
        "Objective:\n{objective}\n\nCreate up to {max_tasks} actionable kanban cards. Return JSON only, like {{\"tasks\":[{{\"title\":\"...\",\"body\":\"...\",\"priority\":0}}]}}."
    );
    let history = vec![ChatMessage::new(
        "__kanban_decompose__".into(),
        "user",
        user_prompt.clone(),
        "kanban_decompose",
    )];
    let reply = complete_chat_with_provider_failover(
        store,
        None,
        &providers,
        &persona,
        system_prompt,
        history,
        &user_prompt,
        None,
        None,
    )
    .await?;
    let cards = parse_kanban_decomposition_cards(&reply.content, max_tasks);
    Ok((cards, model_label))
}

fn build_kanban_decomposer_provider_plan(
    store: &AppStore,
    task_key: &str,
) -> AppResult<Option<(Vec<LlmProvider>, Persona, String)>> {
    let config = store.config()?;
    let assignment_configured = config
        .chat
        .auxiliary_task_assignments
        .as_object()
        .map(|assignments| assignments.contains_key(task_key))
        .unwrap_or(false);
    if !assignment_configured {
        return Ok(None);
    }
    let assignment = list_agent_auxiliary_task_assignments(store)?
        .into_iter()
        .find(|assignment| assignment.key == task_key)
        .ok_or_else(|| AppError::BadRequest(format!("unknown auxiliary task: {task_key}")))?;
    let provider_choice = assignment.provider.trim();
    let provider_id = if provider_choice.is_empty() || provider_choice.eq_ignore_ascii_case("auto")
    {
        None
    } else {
        Some(provider_choice)
    };
    let model = assignment.model.trim();
    let base_url = assignment.base_url.trim();
    let custom_model = if model.is_empty() {
        store
            .provider(None)
            .ok()
            .map(|provider| provider.model)
            .unwrap_or_default()
    } else {
        model.to_string()
    };
    let mut providers = if base_url.is_empty() {
        store.provider_candidates(provider_id)?
    } else {
        vec![LlmProvider {
            id: format!("auxiliary-{task_key}-custom"),
            name: format!("{task_key} auxiliary"),
            provider_type: "openai_compatible".into(),
            base_url: base_url.into(),
            append_chat_path: true,
            api_key: (!assignment.api_key.trim().is_empty())
                .then(|| assignment.api_key.trim().to_string()),
            model: custom_model,
            enabled: true,
            timeout_seconds: assignment.timeout.max(1),
            ..LlmProvider::default()
        }]
    };
    if providers.is_empty() {
        return Err(AppError::NotFound(format!("{task_key} auxiliary provider")));
    }
    if !model.is_empty() {
        for provider in &mut providers {
            provider.model = model.to_string();
        }
    }
    for provider in &mut providers {
        provider.timeout_seconds = assignment.timeout.max(1);
    }
    let mut persona = store.persona(None)?;
    persona.temperature = 0.2;
    persona.max_tokens = 2_000;
    if let Some(provider_id) = provider_id {
        persona.llm_provider = provider_id.to_string();
    }
    if !model.is_empty() {
        persona.llm_model = model.to_string();
    }
    let label = if model.is_empty() {
        providers
            .first()
            .map(|provider| provider.model.clone())
            .unwrap_or_else(|| "default".into())
    } else {
        model.to_string()
    };
    Ok(Some((providers, persona, label)))
}

fn parse_kanban_decomposition_cards(text: &str, max_tasks: usize) -> Vec<KanbanDraftCard> {
    let Some(value) = parse_json_object_from_text(text) else {
        return Vec::new();
    };
    let tasks = value
        .get("tasks")
        .or_else(|| value.get("cards"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    tasks
        .into_iter()
        .take(max_tasks)
        .filter_map(|task| {
            let title = task.get("title").and_then(Value::as_str)?.trim();
            if title.is_empty() {
                return None;
            }
            Some(KanbanDraftCard {
                title: title.to_string(),
                body: task
                    .get("body")
                    .or_else(|| task.get("description"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim()
                    .to_string(),
                priority: task.get("priority").and_then(Value::as_i64).unwrap_or(0),
            })
        })
        .collect()
}

fn parse_json_object_from_text(text: &str) -> Option<Value> {
    serde_json::from_str::<Value>(text).ok().or_else(|| {
        let start = text.find('{')?;
        let end = text.rfind('}')?;
        serde_json::from_str::<Value>(&text[start..=end]).ok()
    })
}

fn deterministic_kanban_decomposition(objective: &str, max_tasks: usize) -> Vec<KanbanDraftCard> {
    let objective = objective.trim();
    let mut parts = objective
        .split(['\n', ';'])
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .take(max_tasks)
        .map(|part| KanbanDraftCard {
            title: truncate_title(part),
            body: part.to_string(),
            priority: 0,
        })
        .collect::<Vec<_>>();
    if parts.is_empty() {
        parts.push(KanbanDraftCard {
            title: "Clarify and plan task".into(),
            body: objective.to_string(),
            priority: 0,
        });
    }
    parts
}

fn truncate_title(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.chars().count() <= 80 {
        return trimmed.to_string();
    }
    format!("{}...", trimmed.chars().take(77).collect::<String>())
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

fn enqueue_kanban_terminal_notifications(task: &mut Value, event_kind: &str) {
    if !matches!(
        event_kind,
        "completed" | "blocked" | "gave_up" | "crashed" | "timed_out"
    ) {
        return;
    }
    let Some(task_id) = task
        .get("id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|value| !value.is_empty())
    else {
        return;
    };
    let mut metadata = task
        .get("metadata")
        .filter(|value| value.is_object())
        .cloned()
        .unwrap_or_else(|| json!({}));
    let mut subs = metadata
        .get("notifySubs")
        .or_else(|| metadata.get("notify_subs"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if subs.is_empty() {
        return;
    }
    let event_cursor = task
        .get("events")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0) as u64;
    if event_cursor == 0 {
        return;
    }
    let mut outbox = metadata
        .get("notificationOutbox")
        .or_else(|| metadata.get("notification_outbox"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut archived = metadata
        .get("notifySubsArchived")
        .or_else(|| metadata.get("notify_subs_archived"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut kept_subs = Vec::new();
    let now = now_iso();
    for mut sub in subs.drain(..) {
        let last_event_id = sub
            .get("lastEventId")
            .or_else(|| sub.get("last_event_id"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        if last_event_id >= event_cursor {
            kept_subs.push(sub);
            continue;
        }
        let platform = sub
            .get("platform")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        let chat_id = sub
            .get("chat_id")
            .or_else(|| sub.get("chatId"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_string();
        if platform.is_empty() || chat_id.is_empty() {
            kept_subs.push(sub);
            continue;
        }
        if let Some(object) = sub.as_object_mut() {
            object.insert("lastEventId".into(), json!(event_cursor));
            object.insert("last_event_id".into(), json!(event_cursor));
            object.insert("lastEventKind".into(), json!(event_kind));
            object.insert("last_event_kind".into(), json!(event_kind));
            object.insert("updatedAt".into(), json!(now.clone()));
            object.insert("updated_at".into(), json!(now.clone()));
        }
        let message = format_kanban_terminal_notification(task, event_kind);
        let delivery = json!({
            "id": new_id("kbnotif"),
            "taskId": task_id,
            "task_id": task_id,
            "eventKind": event_kind,
            "event_kind": event_kind,
            "eventCursor": event_cursor,
            "event_cursor": event_cursor,
            "platform": platform,
            "chatId": chat_id,
            "chat_id": chat_id,
            "threadId": sub.get("thread_id").or_else(|| sub.get("threadId")).cloned().unwrap_or(Value::Null),
            "thread_id": sub.get("thread_id").or_else(|| sub.get("threadId")).cloned().unwrap_or(Value::Null),
            "message": message,
            "status": "queued",
            "createdAt": now,
            "created_at": now,
            "source": "synthchat_kanban_notifier_desktop_v1"
        });
        outbox.push(delivery);
        if event_kind == "completed" {
            if let Some(object) = sub.as_object_mut() {
                object.insert("archivedAt".into(), json!(now_iso()));
                object.insert("archived_at".into(), json!(now_iso()));
                object.insert("archiveReason".into(), json!("task_completed"));
                object.insert("archive_reason".into(), json!("task_completed"));
            }
            archived.push(sub);
        } else {
            kept_subs.push(sub);
        }
    }
    if let Some(object) = metadata.as_object_mut() {
        object.insert("notifySubs".into(), Value::Array(kept_subs));
        object.insert("notificationOutbox".into(), Value::Array(outbox));
        object.insert("notifySubsArchived".into(), Value::Array(archived));
    }
    set_task_field(task, "metadata", metadata);
    push_kanban_event(
        task,
        "notification_queued",
        json!({
            "eventKind": event_kind,
            "eventCursor": event_cursor,
            "source": "synthchat_kanban_notifier_desktop_v1"
        }),
    );
}

fn format_kanban_terminal_notification(task: &Value, event_kind: &str) -> String {
    let task_id = task.get("id").and_then(Value::as_str).unwrap_or("task");
    let title = task
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or(task_id)
        .chars()
        .take(120)
        .collect::<String>();
    match event_kind {
        "completed" => {
            let handoff = task
                .get("summary")
                .or_else(|| task.get("result"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|value| {
                    value
                        .lines()
                        .next()
                        .unwrap_or(value)
                        .chars()
                        .take(200)
                        .collect::<String>()
                })
                .unwrap_or_default();
            if handoff.is_empty() {
                format!("[Kanban] {task_id} done - {title}")
            } else {
                format!("[Kanban] {task_id} done - {title}\n{handoff}")
            }
        }
        "blocked" => {
            let reason = task
                .get("blockReason")
                .or_else(|| task.get("block_reason"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .chars()
                .take(160)
                .collect::<String>();
            if reason.is_empty() {
                format!("[Kanban] {task_id} blocked - {title}")
            } else {
                format!("[Kanban] {task_id} blocked: {reason}")
            }
        }
        "gave_up" => format!("[Kanban] {task_id} gave up after repeated failures - {title}"),
        "crashed" => format!("[Kanban] {task_id} worker crashed; dispatcher will retry - {title}"),
        "timed_out" => format!("[Kanban] {task_id} timed out; dispatcher will retry - {title}"),
        _ => format!("[Kanban] {task_id} {event_kind} - {title}"),
    }
}

fn spawn_kanban_notification_delivery(store: &AppStore, result_text: &str) {
    let Ok(value) = serde_json::from_str::<Value>(result_text) else {
        return;
    };
    let Some(task) = value.get("task").cloned() else {
        return;
    };
    let deliveries = task
        .get("metadata")
        .and_then(|metadata| metadata.get("notificationOutbox"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|delivery| delivery.get("status").and_then(Value::as_str) == Some("queued"))
        .collect::<Vec<_>>();
    if deliveries.is_empty() {
        return;
    }
    let store = store.clone();
    tauri::async_runtime::spawn(async move {
        for delivery in deliveries {
            deliver_kanban_notification(&store, &delivery).await;
        }
    });
}

async fn deliver_kanban_notification(store: &AppStore, delivery: &Value) {
    let delivery_id = delivery
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let task_id = delivery
        .get("taskId")
        .or_else(|| delivery.get("task_id"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if delivery_id.is_empty() || task_id.is_empty() {
        return;
    }
    let platform = delivery
        .get("platform")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let chat_id = delivery
        .get("chatId")
        .or_else(|| delivery.get("chat_id"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let thread_id = delivery
        .get("threadId")
        .or_else(|| delivery.get("thread_id"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty());
    let message = delivery
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if platform.is_empty() || chat_id.is_empty() || message.is_empty() {
        mark_kanban_notification_delivery(
            store,
            &task_id,
            &delivery_id,
            "failed",
            Some("delivery missing platform, chat id, or message".into()),
        );
        return;
    }
    let target = thread_id
        .map(|thread_id| format!("{platform}:{chat_id}:{thread_id}"))
        .unwrap_or_else(|| format!("{platform}:{chat_id}"));
    let mut payload = json!({
        "target": target,
        "platform": platform,
        "message": message,
        "metadata": {
            "notify": true,
            "kanbanTaskId": task_id,
            "kanbanNotificationId": delivery_id
        }
    });
    if let Some(thread_id) = thread_id {
        payload["thread_id"] = json!(thread_id);
        payload["threadId"] = json!(thread_id);
    }
    let status = match send_message_tool_async(store, "__kanban_notifier__", &payload).await {
        Ok(result) => ("delivered", Some(result)),
        Err(error) => ("failed", Some(error.to_string())),
    };
    mark_kanban_notification_delivery(store, &task_id, &delivery_id, status.0, status.1);
}

fn mark_kanban_notification_delivery(
    store: &AppStore,
    task_id: &str,
    delivery_id: &str,
    status: &str,
    detail: Option<String>,
) {
    let Ok(mut tasks) = store.agent_kanban_tasks() else {
        return;
    };
    let Some(task) = tasks
        .iter_mut()
        .find(|task| task.get("id").and_then(Value::as_str) == Some(task_id))
    else {
        return;
    };
    let Some(outbox) = task
        .get_mut("metadata")
        .and_then(|metadata| metadata.get_mut("notificationOutbox"))
        .and_then(Value::as_array_mut)
    else {
        return;
    };
    for delivery in outbox {
        if delivery.get("id").and_then(Value::as_str) != Some(delivery_id) {
            continue;
        }
        if let Some(object) = delivery.as_object_mut() {
            object.insert("status".into(), json!(status));
            object.insert("updatedAt".into(), json!(now_iso()));
            object.insert("updated_at".into(), json!(now_iso()));
            if status == "delivered" {
                object.insert("deliveredAt".into(), json!(now_iso()));
                object.insert("delivered_at".into(), json!(now_iso()));
            }
            if let Some(detail) = detail {
                object.insert(
                    if status == "delivered" {
                        "result"
                    } else {
                        "error"
                    }
                    .into(),
                    json!(detail),
                );
            }
        }
        break;
    }
    let _ = store.set_agent_kanban_tasks(tasks);
}

fn set_task_field(task: &mut Value, key: &str, value: Value) {
    if let Some(object) = task.as_object_mut() {
        object.insert(key.into(), value);
    }
}

fn validate_kanban_update_payload(payload: &Value) -> AppResult<()> {
    if let Some(metadata) = payload.get("metadata") {
        if !metadata.is_object() {
            return Err(AppError::BadRequest(format!(
                "kanban_update metadata must be an object, got {}",
                value_type_name(metadata)
            )));
        }
    }
    Ok(())
}

fn apply_kanban_task_patch(task: &mut Value, payload: &Value) -> Value {
    let mut patch = json!({});
    if let Some(title) = string_arg(payload, &["title"]) {
        set_task_field(task, "title", json!(title));
        set_task_field(&mut patch, "title", task["title"].clone());
    }
    if let Some(body) = string_arg(payload, &["body", "description"]) {
        set_task_field(task, "body", json!(body));
        set_task_field(&mut patch, "body", task["body"].clone());
    }
    if payload.get("assignee").is_some() {
        let assignee = string_arg(payload, &["assignee"]);
        set_task_field(
            task,
            "assignee",
            assignee.map(Value::String).unwrap_or(Value::Null),
        );
        set_task_field(&mut patch, "assignee", task["assignee"].clone());
    }
    if let Some(status) = string_arg(payload, &["status", "state"]) {
        let status = normalize_kanban_status_for_store(&status);
        set_task_field(task, "status", json!(status));
        if status == "completed" {
            set_task_field(task, "completedAt", json!(now_iso()));
        }
        if status == "running" || status == "in_progress" {
            let started = task.get("startedAt").cloned().unwrap_or(Value::Null);
            if started.is_null() {
                set_task_field(task, "startedAt", json!(now_iso()));
            }
        }
        set_task_field(&mut patch, "status", task["status"].clone());
    }
    if let Some(priority) = payload.get("priority").and_then(Value::as_i64) {
        set_task_field(task, "priority", json!(priority));
        set_task_field(&mut patch, "priority", task["priority"].clone());
    }
    if payload.get("tenant").is_some() {
        let tenant = string_arg(payload, &["tenant"]);
        set_task_field(
            task,
            "tenant",
            tenant.map(Value::String).unwrap_or(Value::Null),
        );
        set_task_field(&mut patch, "tenant", task["tenant"].clone());
    }
    if let Some(reason) = string_arg(payload, &["blockReason", "block_reason", "reason"]) {
        set_task_field(task, "blockReason", json!(reason));
        set_task_field(&mut patch, "blockReason", task["blockReason"].clone());
    }
    if let Some(metadata_patch) = payload.get("metadata").and_then(Value::as_object) {
        let mut metadata = task
            .get("metadata")
            .filter(|value| value.is_object())
            .cloned()
            .unwrap_or_else(|| json!({}));
        if let Some(target) = metadata.as_object_mut() {
            for (key, value) in metadata_patch {
                target.insert(key.clone(), value.clone());
            }
        }
        set_task_field(task, "metadata", metadata);
        set_task_field(&mut patch, "metadata", task["metadata"].clone());
    }
    patch
}

fn normalize_kanban_status_for_store(status: &str) -> String {
    match status.trim().to_ascii_lowercase().as_str() {
        "done" => "completed".into(),
        "running" => "in_progress".into(),
        "" => "ready".into(),
        other => other.into(),
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

fn remove_string_field(task: &mut Value, key: &str, value: &str) {
    if let Some(items) = task.get_mut(key).and_then(Value::as_array_mut) {
        items.retain(|item| item.as_str() != Some(value));
    }
}
