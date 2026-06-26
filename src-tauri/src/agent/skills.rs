use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

use serde_json::{json, Value};

use crate::{
    error::{AppError, AppResult},
    models::{AgentDefinition, EnhancedSkillSummary},
    skills as skill_library,
    store::AppStore,
};

use super::{list_python_plugin_skills, string_arg, truncate_for_prompt};
pub(super) fn skills_list_tool(
    store: &AppStore,
    agent: &AgentDefinition,
    payload: &Value,
) -> AppResult<String> {
    let query = payload
        .get("query")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_lowercase();
    let enabled_only = payload
        .get("enabledOnly")
        .or_else(|| payload.get("enabled_only"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let category = payload
        .get("category")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_lowercase);
    let mut skills = skills_with_python_plugins(store, Some(agent))?;
    skills.sort_by(|left, right| {
        skill_category(left)
            .cmp(&skill_category(right))
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
    });
    let rows = skills
        .into_iter()
        .filter(|skill| !enabled_only || skill.enabled)
        .filter(|skill| {
            category
                .as_deref()
                .is_none_or(|category| skill_category(skill).eq_ignore_ascii_case(category))
        })
        .filter(|skill| {
            query.is_empty()
                || skill.name.to_lowercase().contains(&query)
                || skill.id.to_lowercase().contains(&query)
                || skill.description.to_lowercase().contains(&query)
                || skill.source.to_lowercase().contains(&query)
        })
        .map(|skill| {
            let category = skill_category(&skill);
            json!({
                "id": skill.id,
                "name": skill.name,
                "description": truncate_for_prompt(&skill.description, 800),
                "category": category,
                "enabled": skill.enabled,
                "source": skill.source,
                "version": skill.version,
                "author": skill.author,
                "path": skill.path,
                "hint": "Use skill_view with this id or name to load full instructions."
            })
        })
        .collect::<Vec<_>>();
    let mut categories = rows
        .iter()
        .filter_map(|row| row.get("category").and_then(Value::as_str))
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    categories.sort();
    categories.dedup();
    Ok(serde_json::to_string_pretty(&json!({
        "ok": true,
        "success": true,
        "count": rows.len(),
        "query": query,
        "category": category.unwrap_or_default(),
        "categories": categories,
        "enabledOnly": enabled_only,
        "skills": rows
    }))?)
}

fn skill_category(skill: &EnhancedSkillSummary) -> String {
    if let Some((category, _)) = skill.id.split_once('/') {
        let category = category.trim();
        if !category.is_empty() {
            return category.to_string();
        }
    }
    let path = skill.path.replace('\\', "/");
    path.split('/')
        .find(|part| {
            !part.trim().is_empty()
                && *part != "."
                && !part.eq_ignore_ascii_case("skill.md")
                && !part.eq_ignore_ascii_case("skills")
        })
        .unwrap_or("")
        .to_string()
}

pub(super) fn skill_view_tool(
    store: &AppStore,
    agent: &AgentDefinition,
    payload: &Value,
) -> AppResult<String> {
    let name = string_arg(payload, &["name", "id", "skill", "skillId", "skill_id"])
        .ok_or_else(|| AppError::BadRequest("skill_view requires payload.name".into()))?;
    let file_path = string_arg(payload, &["filePath", "file_path", "path"]);
    let max_chars = payload
        .get("maxChars")
        .or_else(|| payload.get("max_chars"))
        .and_then(Value::as_u64)
        .unwrap_or(20_000)
        .clamp(500, 80_000) as usize;
    let skills = skills_with_python_plugins(store, Some(agent))?;
    let skill = find_skill_by_name_or_id(&skills, &name).ok_or_else(|| {
        AppError::BadRequest(format!(
            "skill_view could not find skill '{name}'. Use skills_list first."
        ))
    })?;
    let skill_md = PathBuf::from(skill.path.trim());
    let skill_md = if skill_md.is_absolute() {
        skill_md
    } else {
        store.data_dir().join(skill_md)
    };
    let skill_dir = skill_md
        .parent()
        .ok_or_else(|| AppError::BadRequest("skill path has no parent directory".into()))?
        .to_path_buf();
    let target = if let Some(file_path) = file_path {
        resolve_skill_relative_path(&skill_dir, &file_path)?
    } else {
        skill_md.clone()
    };
    let content = fs::read_to_string(&target).map_err(|error| {
        AppError::BadRequest(format!(
            "failed to read skill file {}: {error}",
            target.display()
        ))
    })?;
    let relative_root = skill_dir
        .canonicalize()
        .unwrap_or_else(|_| skill_dir.clone());
    let relative_path = target
        .strip_prefix(&relative_root)
        .or_else(|_| target.strip_prefix(&skill_dir))
        .unwrap_or(&target)
        .display()
        .to_string();
    let _ = skill_library::record_skill_usage(store, skill, "view", Some("foreground"));
    Ok(serde_json::to_string_pretty(&json!({
        "ok": true,
        "success": true,
        "id": skill.id,
        "name": skill.name,
        "description": skill.description,
        "enabled": skill.enabled,
        "source": skill.source,
        "path": skill.path,
        "filePath": relative_path,
        "truncated": content.chars().count() > max_chars,
        "content": truncate_for_prompt(&content, max_chars)
    }))?)
}

fn find_skill_by_name_or_id<'a>(
    skills: &'a [EnhancedSkillSummary],
    name: &str,
) -> Option<&'a EnhancedSkillSummary> {
    let needle = name.trim().to_lowercase();
    skills
        .iter()
        .find(|skill| skill.id.to_lowercase() == needle || skill.name.to_lowercase() == needle)
        .or_else(|| {
            skills
                .iter()
                .find(|skill| skill.name.to_lowercase().contains(&needle))
        })
}

fn skills_with_python_plugins(
    store: &AppStore,
    agent: Option<&AgentDefinition>,
) -> AppResult<Vec<EnhancedSkillSummary>> {
    let mut skills = if let Some(agent) = agent {
        skill_library::list_skills_for_agent(store, &agent.id)?
    } else {
        store.skills()?
    };
    for skill in list_python_plugin_skills(store)? {
        let id = format!("{}:{}", skill.plugin_id, skill.name);
        if skills
            .iter()
            .any(|existing| existing.id == id || existing.name == id)
        {
            continue;
        }
        skills.push(EnhancedSkillSummary {
            id: id.clone(),
            name: id,
            description: skill.description,
            enabled: agent.map(|value| value.skills_enabled).unwrap_or(true),
            path: skill.path.to_string_lossy().to_string(),
            version: String::new(),
            author: String::new(),
            icon: "sparkles".into(),
            is_core: false,
            is_bundled: false,
            source: format!("python-plugin:{}", skill.plugin_name),
            agent_id: agent.map(|value| value.id.clone()).unwrap_or_default(),
            config: HashMap::new(),
            required_environment_variables: Vec::new(),
            required_credential_files: Vec::new(),
        });
    }
    Ok(skills)
}

fn resolve_skill_relative_path(skill_dir: &Path, relative: &str) -> AppResult<PathBuf> {
    let relative_path = PathBuf::from(relative.trim());
    if relative_path.is_absolute() {
        return Err(AppError::BadRequest(
            "skill_view filePath must be relative to the skill directory".into(),
        ));
    }
    let root = skill_dir.canonicalize()?;
    let target = root.join(relative_path).canonicalize()?;
    if !target.starts_with(&root) {
        return Err(AppError::BadRequest(
            "skill_view filePath must stay inside the skill directory".into(),
        ));
    }
    Ok(target)
}

pub(super) fn skill_manage_tool(store: &AppStore, payload: &Value) -> AppResult<String> {
    let action = string_arg(payload, &["action"])
        .ok_or_else(|| AppError::BadRequest("skill_manage requires payload.action".into()))?
        .trim()
        .to_lowercase();
    let name = string_arg(payload, &["name", "id", "skill", "skillId", "skill_id"]);
    let mut result = match action.as_str() {
        "status" | "hub_status" | "hub-status" => skill_manage_status(store)?,
        "list_installs" | "list-installs" | "provenance" => skill_manage_list_installs(store)?,
        "usage" | "usage_log" | "usage-log" => skill_manage_usage(store, payload)?,
        "audit" | "guard_scan" | "guard-scan" => {
            skill_manage_audit(store, name.as_deref(), payload)?
        }
        "audit_log" | "audit-log" => skill_manage_audit_log(store, payload)?,
        "check_updates" | "check-updates" | "updates" => {
            skill_manage_check_updates(store, name.as_deref())?
        }
        "update" | "sync" => skill_manage_update(store, name.as_deref(), payload)?,
        "uninstall" => skill_manage_uninstall(store, name.as_deref(), payload)?,
        "list_taps" | "list-taps" => skill_manage_list_taps(store)?,
        "add_tap" | "add-tap" => skill_manage_add_tap(store, payload)?,
        "remove_tap" | "remove-tap" => skill_manage_remove_tap(store, payload)?,
        "curator_report" | "curator-report" => skill_manage_curator_report(store)?,
        "curator_status" | "curator-status" => skill_manage_curator_status(store)?,
        "curator_pause" | "curator-pause" => skill_manage_curator_pause(store, true)?,
        "curator_resume" | "curator-resume" => skill_manage_curator_pause(store, false)?,
        "export_snapshot" | "export-snapshot" => skill_manage_export_snapshot(store, payload)?,
        "import_snapshot" | "import-snapshot" => skill_manage_import_snapshot(store, payload)?,
        "install_file" | "install-file" => skill_manage_install_file(store, payload)?,
        "install_content" | "install-content" => skill_manage_install_content(store, payload)?,
        "create" => skill_manage_create(store, required_skill_name(name.as_deref())?, payload)?,
        "edit" => skill_manage_edit(store, required_skill_name(name.as_deref())?, payload)?,
        "patch" => skill_manage_patch(store, required_skill_name(name.as_deref())?, payload)?,
        "pin" => skill_manage_pin(store, required_skill_name(name.as_deref())?)?,
        "unpin" | "un-pin" => skill_manage_unpin(store, required_skill_name(name.as_deref())?)?,
        "archive" => skill_manage_archive(store, required_skill_name(name.as_deref())?, payload)?,
        "restore" => skill_manage_restore(store, required_skill_name(name.as_deref())?)?,
        "delete" => skill_manage_delete(store, required_skill_name(name.as_deref())?)?,
        "write_file" | "write-file" | "writefile" => {
            skill_manage_write_file(store, required_skill_name(name.as_deref())?, payload)?
        }
        "remove_file" | "remove-file" | "removefile" => {
            skill_manage_remove_file(store, required_skill_name(name.as_deref())?, payload)?
        }
        other => {
            return Err(AppError::BadRequest(format!(
                "unsupported skill_manage action '{other}'. Use status, list_installs, usage, audit, audit_log, check_updates, update, uninstall, list_taps, add_tap, remove_tap, curator_report, curator_status, curator_pause, curator_resume, export_snapshot, import_snapshot, install_file, install_content, create, edit, patch, pin, unpin, archive, restore, delete, write_file, or remove_file."
            )));
        }
    };
    if let Some(object) = result.as_object_mut() {
        if !object.contains_key("success") {
            let ok = object.get("ok").and_then(Value::as_bool).unwrap_or(true);
            object.insert("success".into(), json!(ok));
        }
    }
    Ok(serde_json::to_string_pretty(&result)?)
}

fn required_skill_name(name: Option<&str>) -> AppResult<&str> {
    name.map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| AppError::BadRequest("skill_manage action requires payload.name".into()))
}

fn optional_selector(name: Option<&str>) -> Option<&str> {
    name.map(str::trim).filter(|value| !value.is_empty())
}

fn bool_arg(payload: &Value, keys: &[&str], default: bool) -> bool {
    keys.iter()
        .find_map(|key| payload.get(*key).and_then(Value::as_bool))
        .unwrap_or(default)
}

fn usize_arg(payload: &Value, keys: &[&str], default: usize) -> usize {
    keys.iter()
        .find_map(|key| payload.get(*key).and_then(Value::as_u64))
        .map(|value| value as usize)
        .unwrap_or(default)
}

fn skill_manage_status(store: &AppStore) -> AppResult<Value> {
    let skills = skills_with_python_plugins(store, None)?;
    let installs = skill_library::skill_install_records(store)?;
    let usage = skill_library::skill_usage_records(store)?;
    let taps = skill_library::list_skill_taps(store)?;
    let curator = skill_library::skill_curator_state(store)?;
    Ok(json!({
        "ok": true,
        "action": "status",
        "totalSkills": skills.len(),
        "enabledSkills": skills.iter().filter(|skill| skill.enabled).count(),
        "bundledSkills": skills.iter().filter(|skill| skill.is_bundled).count(),
        "externalSkills": skills.iter().filter(|skill| !skill.is_bundled).count(),
        "installRecords": installs.len(),
        "usageRecords": usage.len(),
        "taps": taps,
        "curator": curator
    }))
}

fn skill_manage_list_installs(store: &AppStore) -> AppResult<Value> {
    let records = skill_library::skill_install_records(store)?;
    Ok(json!({
        "ok": true,
        "action": "list_installs",
        "count": records.len(),
        "records": records
    }))
}

fn skill_manage_usage(store: &AppStore, payload: &Value) -> AppResult<Value> {
    let limit = usize_arg(payload, &["limit", "max"], 50).clamp(1, 500);
    let mut records = skill_library::skill_usage_records(store)?;
    records.truncate(limit);
    Ok(json!({
        "ok": true,
        "action": "usage",
        "count": records.len(),
        "records": records
    }))
}

fn skill_manage_audit(store: &AppStore, name: Option<&str>, payload: &Value) -> AppResult<Value> {
    let selector_arg = string_arg(payload, &["selector"]);
    let selector = optional_selector(name).or_else(|| selector_arg.as_deref());
    let reports = skill_library::audit_skills(store, selector)?;
    Ok(json!({
        "ok": true,
        "action": "audit",
        "selector": selector.unwrap_or_default(),
        "count": reports.len(),
        "reports": reports
    }))
}

fn skill_manage_audit_log(store: &AppStore, payload: &Value) -> AppResult<Value> {
    let limit = usize_arg(payload, &["limit", "max"], 50).clamp(1, 500);
    let entries = skill_library::skill_audit_log(store, Some(limit))?;
    Ok(json!({
        "ok": true,
        "action": "audit_log",
        "count": entries.len(),
        "entries": entries
    }))
}

fn skill_manage_check_updates(store: &AppStore, name: Option<&str>) -> AppResult<Value> {
    let checks = skill_library::check_skill_updates(store, optional_selector(name))?;
    Ok(json!({
        "ok": true,
        "action": "check_updates",
        "count": checks.len(),
        "updates": checks
    }))
}

fn skill_manage_update(store: &AppStore, name: Option<&str>, payload: &Value) -> AppResult<Value> {
    let agent_id = string_arg(payload, &["agentId", "agent_id"]);
    let force = bool_arg(payload, &["force"], false);
    let updated = skill_library::update_skills_from_sources(
        store,
        optional_selector(name),
        agent_id.as_deref(),
        force,
    )?;
    Ok(json!({
        "ok": true,
        "action": "update",
        "count": updated.len(),
        "skills": updated
    }))
}

fn skill_manage_uninstall(
    store: &AppStore,
    name: Option<&str>,
    payload: &Value,
) -> AppResult<Value> {
    let selector = required_skill_name(name)?;
    let remove_files = bool_arg(payload, &["removeFiles", "remove_files"], true);
    let removed = skill_library::uninstall_external_skills(store, Some(selector), remove_files)?;
    Ok(json!({
        "ok": true,
        "action": "uninstall",
        "count": removed.len(),
        "records": removed,
        "removeFiles": remove_files
    }))
}

fn skill_manage_list_taps(store: &AppStore) -> AppResult<Value> {
    let taps = skill_library::list_skill_taps(store)?;
    Ok(json!({
        "ok": true,
        "action": "list_taps",
        "count": taps.len(),
        "taps": taps
    }))
}

fn skill_manage_add_tap(store: &AppStore, payload: &Value) -> AppResult<Value> {
    let repo = string_arg(payload, &["repo", "repository"])
        .ok_or_else(|| AppError::BadRequest("skill_manage add_tap requires payload.repo".into()))?;
    let path = string_arg(payload, &["path"]);
    let tap = skill_library::add_skill_tap(store, &repo, path.as_deref())?;
    Ok(json!({
        "ok": true,
        "action": "add_tap",
        "tap": tap
    }))
}

fn skill_manage_remove_tap(store: &AppStore, payload: &Value) -> AppResult<Value> {
    let repo = string_arg(payload, &["repo", "repository"]).ok_or_else(|| {
        AppError::BadRequest("skill_manage remove_tap requires payload.repo".into())
    })?;
    let removed = skill_library::remove_skill_tap(store, &repo)?;
    Ok(json!({
        "ok": true,
        "action": "remove_tap",
        "repo": repo,
        "removed": removed
    }))
}

fn skill_manage_curator_report(store: &AppStore) -> AppResult<Value> {
    let report = skill_library::curate_skills_report(store)?;
    Ok(json!({
        "ok": true,
        "action": "curator_report",
        "report": report
    }))
}

fn skill_manage_curator_status(store: &AppStore) -> AppResult<Value> {
    let state = skill_library::skill_curator_state(store)?;
    Ok(json!({
        "ok": true,
        "action": "curator_status",
        "state": state
    }))
}

fn skill_manage_curator_pause(store: &AppStore, paused: bool) -> AppResult<Value> {
    let state = skill_library::set_skill_curator_paused(store, paused)?;
    Ok(json!({
        "ok": true,
        "action": if paused { "curator_pause" } else { "curator_resume" },
        "state": state
    }))
}

fn skill_manage_export_snapshot(store: &AppStore, payload: &Value) -> AppResult<Value> {
    let path = string_arg(payload, &["path"]).ok_or_else(|| {
        AppError::BadRequest("skill_manage export_snapshot requires payload.path".into())
    })?;
    let path = skill_library::export_skill_snapshot(store, &path)?;
    Ok(json!({
        "ok": true,
        "action": "export_snapshot",
        "path": path
    }))
}

fn skill_manage_import_snapshot(store: &AppStore, payload: &Value) -> AppResult<Value> {
    let path = string_arg(payload, &["path"]).ok_or_else(|| {
        AppError::BadRequest("skill_manage import_snapshot requires payload.path".into())
    })?;
    let count = skill_library::import_skill_snapshot(store, &path)?;
    Ok(json!({
        "ok": true,
        "action": "import_snapshot",
        "count": count
    }))
}

fn skill_manage_install_file(store: &AppStore, payload: &Value) -> AppResult<Value> {
    let path = string_arg(payload, &["path", "sourcePath", "source_path"]).ok_or_else(|| {
        AppError::BadRequest("skill_manage install_file requires payload.path".into())
    })?;
    let name = string_arg(payload, &["name", "nameOverride", "name_override"]);
    let category = string_arg(payload, &["category"]);
    let agent_id = string_arg(payload, &["agentId", "agent_id"]);
    let force = bool_arg(payload, &["force"], false);
    let skill = skill_library::install_external_skill_file(
        store,
        &path,
        name.as_deref(),
        category.as_deref(),
        agent_id.as_deref(),
        force,
    )?;
    Ok(json!({
        "ok": true,
        "action": "install_file",
        "skill": skill
    }))
}

fn skill_manage_install_content(store: &AppStore, payload: &Value) -> AppResult<Value> {
    let content = string_arg(payload, &["content", "skillMd", "skill_md"]).ok_or_else(|| {
        AppError::BadRequest("skill_manage install_content requires payload.content".into())
    })?;
    let fallback_name = string_arg(payload, &["name", "fallbackName", "fallback_name"])
        .unwrap_or_else(|| "external-skill".into());
    let name = string_arg(payload, &["nameOverride", "name_override"]);
    let category = string_arg(payload, &["category"]);
    let agent_id = string_arg(payload, &["agentId", "agent_id"]);
    let force = bool_arg(payload, &["force"], false);
    let identifier =
        string_arg(payload, &["identifier", "source"]).unwrap_or_else(|| "inline".into());
    let skill = skill_library::install_external_skill_content(
        store,
        &content,
        &fallback_name,
        name.as_deref(),
        category.as_deref(),
        agent_id.as_deref(),
        force,
        false,
        &identifier,
    )?;
    Ok(json!({
        "ok": true,
        "action": "install_content",
        "skill": skill
    }))
}

fn skill_manage_create(store: &AppStore, name: &str, payload: &Value) -> AppResult<Value> {
    validate_skill_name(name)?;
    let category = string_arg(payload, &["category"])
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    if let Some(category) = category.as_deref() {
        validate_skill_name(category)?;
    }
    let content = string_arg(payload, &["content", "skillMd", "skill_md"]).ok_or_else(|| {
        AppError::BadRequest("skill_manage create requires payload.content".into())
    })?;
    validate_skill_markdown(&content)?;
    let mut skills = store.skills()?;
    if find_skill_by_name_or_id(&skills, name).is_some() {
        return Err(AppError::BadRequest(format!(
            "a skill named '{name}' already exists"
        )));
    }
    let root = store.data_dir().join("skills").join("agent-managed");
    let skill_dir = if let Some(category) = category.as_deref() {
        root.join(category).join(name)
    } else {
        root.join(name)
    };
    fs::create_dir_all(&skill_dir)?;
    let skill_md = skill_dir.join("SKILL.md");
    fs::write(&skill_md, &content)?;
    let skill = summarize_managed_skill(store, &skill_md, true)?;
    skills.push(skill.clone());
    skills.sort_by(|left, right| left.id.cmp(&right.id));
    store.set_skills(skills)?;
    let _ = skill_library::record_skill_usage(store, &skill, "create", Some("foreground"));
    Ok(json!({
        "ok": true,
        "action": "create",
        "id": skill.id,
        "name": skill.name,
        "path": skill.path,
        "hint": "Use skill_manage action=write_file for references, templates, scripts, or assets."
    }))
}

fn skill_manage_edit(store: &AppStore, name: &str, payload: &Value) -> AppResult<Value> {
    let content = string_arg(payload, &["content", "skillMd", "skill_md"])
        .ok_or_else(|| AppError::BadRequest("skill_manage edit requires payload.content".into()))?;
    validate_skill_markdown(&content)?;
    let mut skills = store.skills()?;
    let index = skill_index_by_name_or_id(&skills, name).ok_or_else(|| {
        AppError::BadRequest(format!(
            "skill_manage could not find skill '{name}'. Use skills_list first."
        ))
    })?;
    let skill_md = skill_markdown_path(store, &skills[index])?;
    fs::write(&skill_md, &content)?;
    let mut updated = summarize_managed_skill(store, &skill_md, skills[index].enabled)?;
    updated.id = skills[index].id.clone();
    updated.is_core = skills[index].is_core;
    updated.is_bundled = skills[index].is_bundled;
    updated.source = skills[index].source.clone();
    updated.agent_id = skills[index].agent_id.clone();
    updated.config = skills[index].config.clone();
    skills[index] = updated.clone();
    store.set_skills(skills)?;
    let _ = skill_library::record_skill_usage(store, &updated, "edit", Some("foreground"));
    Ok(json!({
        "ok": true,
        "action": "edit",
        "id": updated.id,
        "name": updated.name,
        "path": updated.path
    }))
}

fn skill_manage_patch(store: &AppStore, name: &str, payload: &Value) -> AppResult<Value> {
    let old_string =
        string_arg(payload, &["oldString", "old_string", "search"]).ok_or_else(|| {
            AppError::BadRequest("skill_manage patch requires payload.oldString".into())
        })?;
    let new_string =
        string_arg(payload, &["newString", "new_string", "replace"]).ok_or_else(|| {
            AppError::BadRequest("skill_manage patch requires payload.newString".into())
        })?;
    if old_string.is_empty() {
        return Err(AppError::BadRequest(
            "skill_manage patch oldString cannot be empty".into(),
        ));
    }
    let replace_all = payload
        .get("replaceAll")
        .or_else(|| payload.get("replace_all"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut skills = store.skills()?;
    let index = skill_index_by_name_or_id(&skills, name).ok_or_else(|| {
        AppError::BadRequest(format!(
            "skill_manage could not find skill '{name}'. Use skills_list first."
        ))
    })?;
    let target = if let Some(file_path) = string_arg(payload, &["filePath", "file_path", "path"]) {
        let skill_dir = skill_dir_for_summary(store, &skills[index])?;
        validate_skill_support_file_path(&file_path)?;
        resolve_skill_write_path(&skill_dir, &file_path)?
    } else {
        skill_markdown_path(store, &skills[index])?
    };
    let content = fs::read_to_string(&target)?;
    let matches = content.matches(&old_string).count();
    if matches == 0 {
        return Err(AppError::BadRequest(format!(
            "skill_manage patch could not find oldString in {}",
            target.display()
        )));
    }
    if matches > 1 && !replace_all {
        return Err(AppError::BadRequest(format!(
            "skill_manage patch found {matches} matches; set replaceAll=true or provide a more specific oldString"
        )));
    }
    let updated_content = if replace_all {
        content.replace(&old_string, &new_string)
    } else {
        content.replacen(&old_string, &new_string, 1)
    };
    if target.file_name().and_then(|name| name.to_str()) == Some("SKILL.md") {
        validate_skill_markdown(&updated_content)?;
    } else {
        validate_skill_content_size(&updated_content, "supporting file")?;
    }
    fs::write(&target, updated_content)?;
    if target.file_name().and_then(|name| name.to_str()) == Some("SKILL.md") {
        let mut updated = summarize_managed_skill(store, &target, skills[index].enabled)?;
        updated.id = skills[index].id.clone();
        updated.is_core = skills[index].is_core;
        updated.is_bundled = skills[index].is_bundled;
        updated.source = skills[index].source.clone();
        updated.agent_id = skills[index].agent_id.clone();
        updated.config = skills[index].config.clone();
        let usage_skill = updated.clone();
        skills[index] = updated;
        store.set_skills(skills)?;
        let _ = skill_library::record_skill_usage(store, &usage_skill, "patch", Some("foreground"));
    } else {
        let _ =
            skill_library::record_skill_usage(store, &skills[index], "patch", Some("foreground"));
    }
    Ok(json!({
        "ok": true,
        "action": "patch",
        "path": target.display().to_string(),
        "replacements": if replace_all { matches } else { 1 }
    }))
}

fn skill_manage_delete(store: &AppStore, name: &str) -> AppResult<Value> {
    let skills = store.skills()?;
    let skill = find_skill_by_name_or_id(&skills, name).ok_or_else(|| {
        AppError::BadRequest(format!(
            "skill_manage could not find skill '{name}'. Use skills_list first."
        ))
    })?;
    let curator_state = skill_library::skill_curator_state(store)?;
    if curator_state
        .pinned_skill_ids
        .iter()
        .any(|id| id == &skill.id)
    {
        return Err(AppError::BadRequest(format!(
            "skill '{}' is pinned; unpin it before deleting",
            skill.id
        )));
    }
    if skill.is_core || skill.is_bundled {
        return Err(AppError::BadRequest(format!(
            "skill_manage delete refuses bundled/core skill '{}'",
            skill.id
        )));
    }
    let skill_dir = skill_dir_for_summary(store, skill)?;
    let _ = skill_library::record_skill_usage(store, skill, "delete", Some("foreground"));
    fs::remove_dir_all(&skill_dir)?;
    store.remove_skill(&skill.id)?;
    Ok(json!({
        "ok": true,
        "action": "delete",
        "id": skill.id,
        "path": skill_dir.display().to_string()
    }))
}

fn skill_manage_pin(store: &AppStore, name: &str) -> AppResult<Value> {
    let state = skill_library::pin_skill_for_curator(store, name)?;
    Ok(json!({
        "ok": true,
        "action": "pin",
        "name": name,
        "pinnedSkillIds": state.pinned_skill_ids
    }))
}

fn skill_manage_unpin(store: &AppStore, name: &str) -> AppResult<Value> {
    let state = skill_library::unpin_skill_for_curator(store, name)?;
    Ok(json!({
        "ok": true,
        "action": "unpin",
        "name": name,
        "pinnedSkillIds": state.pinned_skill_ids
    }))
}

fn skill_manage_archive(store: &AppStore, name: &str, payload: &Value) -> AppResult<Value> {
    let reason = string_arg(payload, &["reason", "archiveReason", "archive_reason"]);
    let archived = skill_library::archive_skill_for_curator(store, name, reason.as_deref())?;
    Ok(json!({
        "ok": true,
        "action": "archive",
        "archive": archived
    }))
}

fn skill_manage_restore(store: &AppStore, name: &str) -> AppResult<Value> {
    let restored = skill_library::restore_skill_for_curator(store, name)?;
    Ok(json!({
        "ok": true,
        "action": "restore",
        "archive": restored
    }))
}

fn skill_manage_write_file(store: &AppStore, name: &str, payload: &Value) -> AppResult<Value> {
    let file_path = string_arg(payload, &["filePath", "file_path", "path"]).ok_or_else(|| {
        AppError::BadRequest("skill_manage write_file requires payload.filePath".into())
    })?;
    let file_content = string_arg(payload, &["fileContent", "file_content", "content"])
        .ok_or_else(|| {
            AppError::BadRequest("skill_manage write_file requires payload.fileContent".into())
        })?;
    validate_skill_support_file_path(&file_path)?;
    validate_skill_content_size(&file_content, &file_path)?;
    let skills = store.skills()?;
    let skill = find_skill_by_name_or_id(&skills, name).ok_or_else(|| {
        AppError::BadRequest(format!(
            "skill_manage could not find skill '{name}'. Use skills_list first."
        ))
    })?;
    let skill_dir = skill_dir_for_summary(store, skill)?;
    let target = resolve_skill_write_path(&skill_dir, &file_path)?;
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&target, file_content)?;
    Ok(json!({
        "ok": true,
        "action": "write_file",
        "id": skill.id,
        "filePath": file_path,
        "path": target.display().to_string()
    }))
}

fn skill_manage_remove_file(store: &AppStore, name: &str, payload: &Value) -> AppResult<Value> {
    let file_path = string_arg(payload, &["filePath", "file_path", "path"]).ok_or_else(|| {
        AppError::BadRequest("skill_manage remove_file requires payload.filePath".into())
    })?;
    validate_skill_support_file_path(&file_path)?;
    let skills = store.skills()?;
    let skill = find_skill_by_name_or_id(&skills, name).ok_or_else(|| {
        AppError::BadRequest(format!(
            "skill_manage could not find skill '{name}'. Use skills_list first."
        ))
    })?;
    let skill_dir = skill_dir_for_summary(store, skill)?;
    let target = resolve_skill_write_path(&skill_dir, &file_path)?;
    if !target.exists() {
        return Err(AppError::BadRequest(format!(
            "skill_manage remove_file target does not exist: {}",
            target.display()
        )));
    }
    fs::remove_file(&target)?;
    Ok(json!({
        "ok": true,
        "action": "remove_file",
        "id": skill.id,
        "filePath": file_path
    }))
}

fn skill_markdown_path(store: &AppStore, skill: &EnhancedSkillSummary) -> AppResult<PathBuf> {
    let path = PathBuf::from(skill.path.trim());
    Ok(if path.is_absolute() {
        path
    } else {
        store.data_dir().join(path)
    })
}

fn skill_dir_for_summary(store: &AppStore, skill: &EnhancedSkillSummary) -> AppResult<PathBuf> {
    skill_markdown_path(store, skill)?
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| AppError::BadRequest("skill path has no parent directory".into()))
}

fn skill_index_by_name_or_id(skills: &[EnhancedSkillSummary], name: &str) -> Option<usize> {
    let needle = name.trim().to_lowercase();
    skills
        .iter()
        .position(|skill| skill.id.to_lowercase() == needle || skill.name.to_lowercase() == needle)
        .or_else(|| {
            skills
                .iter()
                .position(|skill| skill.name.to_lowercase().contains(&needle))
        })
}

fn summarize_managed_skill(
    store: &AppStore,
    skill_md: &Path,
    enabled: bool,
) -> AppResult<EnhancedSkillSummary> {
    let raw = fs::read_to_string(skill_md)?;
    let metadata = parse_skill_frontmatter(&raw);
    let skill_dir = skill_md
        .parent()
        .ok_or_else(|| AppError::BadRequest("skill path has no parent directory".into()))?;
    let root = store.data_dir().join("skills");
    let rel = skill_dir.strip_prefix(&root).unwrap_or(skill_dir);
    let id = rel
        .components()
        .filter_map(|component| component.as_os_str().to_str())
        .collect::<Vec<_>>()
        .join("/");
    let name = metadata.get("name").cloned().unwrap_or_else(|| {
        rel.file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("skill")
            .into()
    });
    Ok(EnhancedSkillSummary {
        id,
        name,
        description: metadata.get("description").cloned().unwrap_or_default(),
        enabled,
        path: skill_md.to_string_lossy().to_string(),
        version: metadata
            .get("version")
            .cloned()
            .unwrap_or_else(|| "1.0.0".into()),
        author: metadata.get("author").cloned().unwrap_or_default(),
        icon: "sparkles".into(),
        is_core: false,
        is_bundled: false,
        source: "agent-managed".into(),
        agent_id: String::new(),
        config: HashMap::new(),
        required_environment_variables: parse_skill_frontmatter_list(
            &raw,
            "required_environment_variables",
        ),
        required_credential_files: parse_skill_frontmatter_list(&raw, "required_credential_files"),
    })
}

fn parse_skill_frontmatter(raw: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let mut lines = raw.lines();
    if lines.next() != Some("---") {
        return map;
    }
    for line in lines {
        if line.trim() == "---" {
            break;
        }
        if let Some((key, value)) = line.split_once(':') {
            map.insert(key.trim().to_string(), clean_skill_meta_value(value.trim()));
        }
    }
    map
}

fn clean_skill_meta_value(value: &str) -> String {
    value
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .to_string()
}

fn parse_skill_frontmatter_list(raw: &str, key: &str) -> Vec<String> {
    let mut items = Vec::new();
    let mut lines = raw.lines();
    if lines.next() != Some("---") {
        return items;
    }
    let mut in_list = false;
    for line in lines {
        let trimmed = line.trim();
        if trimmed == "---" {
            break;
        }
        if trimmed.starts_with(&format!("{key}:")) {
            in_list = true;
            if let Some((_, inline)) = trimmed.split_once(':') {
                let inline = inline.trim();
                if inline.starts_with('[') && inline.ends_with(']') {
                    return inline
                        .trim_matches(|ch| ch == '[' || ch == ']')
                        .split(',')
                        .map(clean_skill_meta_value)
                        .filter(|value| !value.is_empty())
                        .collect();
                }
                if !inline.is_empty() {
                    return vec![clean_skill_meta_value(inline)];
                }
            }
            continue;
        }
        if in_list {
            if let Some(item) = trimmed.strip_prefix('-') {
                let item = clean_skill_meta_value(item.trim());
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

fn validate_skill_markdown(content: &str) -> AppResult<()> {
    validate_skill_content_size(content, "SKILL.md")?;
    if !content.trim_start().starts_with("---") {
        return Err(AppError::BadRequest(
            "SKILL.md must start with YAML frontmatter".into(),
        ));
    }
    let mut lines = content.lines();
    if lines.next() != Some("---") {
        return Err(AppError::BadRequest(
            "SKILL.md frontmatter must start with a standalone --- line".into(),
        ));
    }
    let mut closed = false;
    let mut frontmatter_lines = Vec::new();
    let mut body_lines = Vec::new();
    for line in lines {
        if !closed && line.trim() == "---" {
            closed = true;
            continue;
        }
        if closed {
            body_lines.push(line);
        } else {
            frontmatter_lines.push(line);
        }
    }
    if !closed {
        return Err(AppError::BadRequest(
            "SKILL.md frontmatter is not closed".into(),
        ));
    }
    let metadata = parse_skill_frontmatter(content);
    if !metadata.contains_key("name") {
        return Err(AppError::BadRequest(
            "SKILL.md frontmatter must include name".into(),
        ));
    }
    if !metadata.contains_key("description") {
        return Err(AppError::BadRequest(
            "SKILL.md frontmatter must include description".into(),
        ));
    }
    if metadata
        .get("description")
        .map(|value| value.chars().count() > 1024)
        .unwrap_or(false)
    {
        return Err(AppError::BadRequest(
            "SKILL.md description exceeds 1024 characters".into(),
        ));
    }
    if body_lines.iter().all(|line| line.trim().is_empty()) {
        return Err(AppError::BadRequest(
            "SKILL.md must include instructions after frontmatter".into(),
        ));
    }
    if frontmatter_lines
        .iter()
        .any(|line| !line.trim().is_empty() && !line.contains(':'))
    {
        return Err(AppError::BadRequest(
            "SKILL.md frontmatter lines must be key: value pairs".into(),
        ));
    }
    Ok(())
}

fn validate_skill_content_size(content: &str, label: &str) -> AppResult<()> {
    const MAX_SKILL_CONTENT_CHARS: usize = 100_000;
    if content.chars().count() > MAX_SKILL_CONTENT_CHARS {
        return Err(AppError::BadRequest(format!(
            "{label} exceeds {MAX_SKILL_CONTENT_CHARS} characters"
        )));
    }
    Ok(())
}

fn validate_skill_name(name: &str) -> AppResult<()> {
    let name = name.trim();
    if name.is_empty() {
        return Err(AppError::BadRequest("skill name is required".into()));
    }
    if name.chars().count() > 64 {
        return Err(AppError::BadRequest(
            "skill name exceeds 64 characters".into(),
        ));
    }
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return Err(AppError::BadRequest("skill name is required".into()));
    };
    if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
        return Err(AppError::BadRequest(
            "skill name must start with lowercase ASCII letter or digit".into(),
        ));
    }
    if !name
        .chars()
        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '-' | '_' | '.'))
    {
        return Err(AppError::BadRequest(
            "skill name may only use lowercase letters, digits, hyphens, underscores, and dots"
                .into(),
        ));
    }
    Ok(())
}

fn validate_skill_support_file_path(file_path: &str) -> AppResult<()> {
    let path = PathBuf::from(file_path.trim());
    if path.is_absolute() {
        return Err(AppError::BadRequest(
            "skill_manage filePath must be relative".into(),
        ));
    }
    let parts = path
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(value) => value.to_str(),
            _ => None,
        })
        .collect::<Vec<_>>();
    if parts.len() < 2 || path.components().count() != parts.len() {
        return Err(AppError::BadRequest(
            "skill_manage filePath must be a normal relative file path under references, templates, scripts, or assets".into(),
        ));
    }
    if !matches!(parts[0], "references" | "templates" | "scripts" | "assets") {
        return Err(AppError::BadRequest(
            "skill_manage filePath must be under references, templates, scripts, or assets".into(),
        ));
    }
    Ok(())
}

fn resolve_skill_write_path(skill_dir: &Path, file_path: &str) -> AppResult<PathBuf> {
    let root = skill_dir.canonicalize()?;
    let target = root.join(file_path.trim());
    let parent = target
        .parent()
        .ok_or_else(|| AppError::BadRequest("skill_manage filePath has no parent".into()))?;
    fs::create_dir_all(parent)?;
    let parent = parent.canonicalize()?;
    if !parent.starts_with(&root) {
        return Err(AppError::BadRequest(
            "skill_manage filePath must stay inside the skill directory".into(),
        ));
    }
    Ok(target)
}
