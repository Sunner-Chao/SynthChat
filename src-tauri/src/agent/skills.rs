use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

use serde_json::{json, Value};

use crate::{
    error::{AppError, AppResult},
    models::EnhancedSkillSummary,
    store::AppStore,
};

use super::{list_python_plugin_skills, string_arg, truncate_for_prompt};
pub(super) fn skills_list_tool(store: &AppStore, payload: &Value) -> AppResult<String> {
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
    let mut skills = skills_with_python_plugins(store)?;
    skills.sort_by(|left, right| {
        left.source
            .cmp(&right.source)
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
    });
    let rows = skills
        .into_iter()
        .filter(|skill| !enabled_only || skill.enabled)
        .filter(|skill| {
            query.is_empty()
                || skill.name.to_lowercase().contains(&query)
                || skill.id.to_lowercase().contains(&query)
                || skill.description.to_lowercase().contains(&query)
                || skill.source.to_lowercase().contains(&query)
        })
        .map(|skill| {
            json!({
                "id": skill.id,
                "name": skill.name,
                "description": truncate_for_prompt(&skill.description, 800),
                "enabled": skill.enabled,
                "source": skill.source,
                "version": skill.version,
                "author": skill.author,
                "path": skill.path,
                "hint": "Use skill_view with this id or name to load full instructions."
            })
        })
        .collect::<Vec<_>>();
    Ok(serde_json::to_string_pretty(&json!({
        "ok": true,
        "count": rows.len(),
        "query": query,
        "enabledOnly": enabled_only,
        "skills": rows
    }))?)
}

pub(super) fn skill_view_tool(store: &AppStore, payload: &Value) -> AppResult<String> {
    let name = string_arg(payload, &["name", "id", "skill", "skillId", "skill_id"])
        .ok_or_else(|| AppError::BadRequest("skill_view requires payload.name".into()))?;
    let file_path = string_arg(payload, &["filePath", "file_path", "path"]);
    let max_chars = payload
        .get("maxChars")
        .or_else(|| payload.get("max_chars"))
        .and_then(Value::as_u64)
        .unwrap_or(20_000)
        .clamp(500, 80_000) as usize;
    let skills = skills_with_python_plugins(store)?;
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
    Ok(serde_json::to_string_pretty(&json!({
        "ok": true,
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

fn skills_with_python_plugins(store: &AppStore) -> AppResult<Vec<EnhancedSkillSummary>> {
    let mut skills = store.skills()?;
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
            enabled: true,
            path: skill.path.to_string_lossy().to_string(),
            version: String::new(),
            author: String::new(),
            icon: "sparkles".into(),
            is_core: false,
            is_bundled: false,
            source: format!("python-plugin:{}", skill.plugin_name),
            agent_id: String::new(),
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
    let name = string_arg(payload, &["name", "id", "skill", "skillId", "skill_id"])
        .ok_or_else(|| AppError::BadRequest("skill_manage requires payload.name".into()))?;
    let result = match action.as_str() {
        "create" => skill_manage_create(store, &name, payload)?,
        "edit" => skill_manage_edit(store, &name, payload)?,
        "patch" => skill_manage_patch(store, &name, payload)?,
        "delete" => skill_manage_delete(store, &name)?,
        "write_file" | "write-file" | "writefile" => {
            skill_manage_write_file(store, &name, payload)?
        }
        "remove_file" | "remove-file" | "removefile" => {
            skill_manage_remove_file(store, &name, payload)?
        }
        other => {
            return Err(AppError::BadRequest(format!(
                "unsupported skill_manage action '{other}'. Use create, edit, patch, delete, write_file, or remove_file."
            )));
        }
    };
    Ok(serde_json::to_string_pretty(&result)?)
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
        skills[index] = updated;
        store.set_skills(skills)?;
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
    if skill.is_core || skill.is_bundled {
        return Err(AppError::BadRequest(format!(
            "skill_manage delete refuses bundled/core skill '{}'",
            skill.id
        )));
    }
    let skill_dir = skill_dir_for_summary(store, skill)?;
    fs::remove_dir_all(&skill_dir)?;
    store.remove_skill(&skill.id)?;
    Ok(json!({
        "ok": true,
        "action": "delete",
        "id": skill.id,
        "path": skill_dir.display().to_string()
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
